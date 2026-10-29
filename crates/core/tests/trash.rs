//! trash-list / trash-restore / trash-empty (dot-4uzgol.38): sessions trashed by `session-delete`
//! (`sessions.delete_mode = "trash"`) are listed, put back and purged through the control API.
//! One test in its own binary: it points `XDG_DATA_HOME` at a temp trash for the whole process.

use std::path::Path;
use std::time::Duration;

use oats_core::control::client::Client;
use oats_core::control::engine::{Engine, EngineOpts};
use oats_core::control::server;
use serde_json::{json, Map, Value};

fn p(pairs: &[(&str, Value)]) -> Map<String, Value> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.clone()))
        .collect()
}

fn fixture() -> String {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/openoats")
        .display()
        .to_string()
}

fn ids(r: &Value) -> Vec<String> {
    r["data"]["trash"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["id"].as_str().unwrap().to_string())
        .collect()
}

#[cfg(any(target_os = "linux", target_os = "windows"))]
#[test]
fn trashed_sessions_are_listed_restored_and_purged() {
    // The OS trash lives under the data home; keep it (and the store) on one filesystem.
    let home = std::path::PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/tmp".into()));
    let base = tempfile::tempdir_in(&home).unwrap();
    std::env::set_var("XDG_DATA_HOME", base.path().join("xdg"));
    let cfg = base.path().join("config.toml");
    std::fs::write(
        &cfg,
        "[sessions]\ndelete_mode = \"trash\"\nconfirm_delete = true\n",
    )
    .unwrap();
    let engine = Engine::new(EngineOpts {
        user_config: cfg.clone(),
        pack: None,
        os: "linux".into(),
        arch: "x86_64".into(),
        machine: "default".into(),
        sessions_dir: base.path().join("sessions"),
        secrets: None,
    })
    .unwrap();
    let rt = base.path().join("rt");
    let _h = server::serve_with_poll(engine, &rt, Duration::from_millis(40)).unwrap();
    let mut c = Client::connect(&rt).unwrap();
    c.call("session-import", p(&[("from", json!(fixture()))]))
        .unwrap();
    let (a, b) = ("session_2026-10-20_09-00-00", "session_2026-10-21_15-00-00");

    assert_eq!(
        c.call("trash-list", Map::new()).unwrap()["data"]["count"],
        0
    );
    for id in [a, b] {
        let r = c
            .call(
                "session-delete",
                p(&[("session", json!(id)), ("confirm", json!(true))]),
            )
            .unwrap();
        assert_eq!(r["data"]["mode"], "trash", "{r}");
    }
    let r = c.call("trash-list", Map::new()).unwrap();
    let mut got = ids(&r);
    got.sort();
    assert_eq!(got, [a, b], "{r}");
    assert!(r["data"]["trash"][0]["deleted_at"].is_string(), "{r}");

    // Restore puts the session back in the store and out of the trash.
    let r = c
        .call("trash-restore", p(&[("session", json!(a))]))
        .unwrap();
    assert_eq!(r["data"]["restored"], true, "{r}");
    assert!(base
        .path()
        .join("sessions")
        .join(a)
        .join("session.json")
        .is_file());
    assert_eq!(ids(&c.call("trash-list", Map::new()).unwrap()), [b]);
    let r = c
        .call("trash-restore", p(&[("session", json!("nope"))]))
        .unwrap();
    assert_eq!(r["error"]["code"], "not_found", "{r}");
    let r = c
        .call("trash-restore", p(&[("session", json!("../x"))]))
        .unwrap();
    assert!(r["error"]["code"].is_string(), "{r}");

    // A live session of the same id blocks the restore.
    c.call(
        "session-delete",
        p(&[("session", json!(a)), ("confirm", json!(true))]),
    )
    .unwrap();
    c.call("session-import", p(&[("from", json!(fixture()))]))
        .unwrap();
    let r = c
        .call("trash-restore", p(&[("session", json!(a))]))
        .unwrap();
    assert_eq!(r["error"]["code"], "conflict", "{r}");

    // Emptying needs confirm while sessions.confirm_delete is on, then purges only our sessions.
    let r = c.call("trash-empty", Map::new()).unwrap();
    assert_eq!(r["error"]["code"], "confirmation_required", "{r}");
    let r = c
        .call(
            "trash-empty",
            p(&[("session", json!(a)), ("confirm", json!(true))]),
        )
        .unwrap();
    assert_eq!(r["data"]["purged"], json!([a]), "{r}");
    assert_eq!(ids(&c.call("trash-list", Map::new()).unwrap()), [b]);
    std::fs::write(
        &cfg,
        "[sessions]\ndelete_mode = \"trash\"\nconfirm_delete = false\n",
    )
    .unwrap();
    c.call("config-reload", Map::new()).unwrap();
    let r = c.call("trash-empty", Map::new()).unwrap();
    assert_eq!(r["data"]["purged"], json!([b]), "{r}");
    assert_eq!(
        c.call("trash-list", Map::new()).unwrap()["data"]["count"],
        0
    );
    let r = c.call("trash-empty", Map::new()).unwrap();
    assert_eq!(r["data"]["count"], 0, "{r}");
}
