//! `ui.keys.*` (dot-4uzgol.82): every window shortcut is a config key. A bad keystroke or one
//! keystroke on two commands is refused by `config check`, by `config-set` and by a file reload,
//! and a good change lands in the user file and the running config.

use std::path::{Path, PathBuf};

use oats_core::config::cli;
use oats_core::config::keys::{Kind, KEYS};
use oats_core::control::engine::{Engine, EngineOpts};
use serde_json::{json, Value};

fn engine(dir: &Path) -> (std::sync::Arc<Engine>, PathBuf) {
    let cfg = dir.join("config.toml");
    let e = Engine::new(EngineOpts {
        user_config: cfg.clone(),
        pack: None,
        os: "linux".into(),
        arch: "x86_64".into(),
        machine: "default".into(),
        sessions_dir: dir.join("sessions"),
        secrets: None,
    })
    .unwrap();
    (e, cfg)
}

fn check(file: &Path) -> Value {
    let args: Vec<String> = ["check", "--json", "--config", file.to_str().unwrap()]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let o = cli::run(&args, &|_| None);
    serde_json::from_str(&o.stdout).unwrap()
}

#[test]
fn defaults_bind_every_command_and_nothing_twice() {
    let d = tempfile::tempdir().unwrap();
    let (e, _) = engine(d.path());
    let keys = e.snapshot().config.ui.keys.clone();
    let declared: Vec<&str> = KEYS
        .iter()
        .filter(|k| k.kind == Kind::Keys)
        .map(|k| k.path.strip_prefix("ui.keys.").expect("ui.keys.*"))
        .collect();
    assert_eq!(keys.keys().map(String::as_str).collect::<Vec<_>>(), {
        let mut v = declared.clone();
        v.sort_unstable();
        v
    });
    assert_eq!(keys["record-toggle"], "space");
    assert_eq!(keys["record-stop"], "mod-shift-s, s");
    let a = serde_json::from_value(json!({"action": "config-check"})).unwrap();
    assert_eq!(e.dispatch(&a).unwrap()["valid"], true);
}

#[test]
fn config_check_refuses_a_bad_keystroke_and_a_clash() {
    let d = tempfile::tempdir().unwrap();
    let f = d.path().join("c.toml");
    std::fs::write(&f, "[ui.keys]\nrecord-toggle = \"ctrl-bogus\"\n").unwrap();
    let v = check(&f);
    assert_eq!(v["error"]["code"], "invalid_value", "{v}");
    assert!(v["error"]["message"]
        .as_str()
        .unwrap()
        .contains("ui.keys.record-toggle"));
    std::fs::write(&f, "[ui.keys]\ntab-live = \"space\"\n").unwrap();
    let v = check(&f);
    assert_eq!(v["error"]["code"], "invalid_combination", "{v}");
    let m = v["error"]["message"].as_str().unwrap();
    assert!(m.contains("record-toggle") && m.contains("tab-live"), "{m}");
    // mod is ctrl on Linux and cmd on macOS: it clashes with either.
    std::fs::write(&f, "[ui.keys]\nquit = \"cmd-r\"\n").unwrap();
    assert_eq!(check(&f)["error"]["code"], "invalid_combination");
    // Freed first, the same keystroke is fine; empty disables a command.
    std::fs::write(
        &f,
        "[ui.keys]\nrecord-toggle = \"\"\ntab-live = \"space, F1\"\n",
    )
    .unwrap();
    assert_eq!(check(&f)["ok"], true, "{}", check(&f));
}

#[test]
fn config_set_vetoes_a_clash_and_hot_reloads_a_rebind() {
    let d = tempfile::tempdir().unwrap();
    let (e, cfg) = engine(d.path());
    let err = e
        .set("ui.keys.tab-live", &json!("space"), "test")
        .unwrap_err();
    assert_eq!(err.code, "invalid_combination");
    assert!(!cfg.exists() || !std::fs::read_to_string(&cfg).unwrap().contains("tab-live"));
    let err = e
        .set("ui.keys.help", &json!("hyper-h"), "test")
        .unwrap_err();
    assert_eq!(err.code, "invalid_value");
    // A bare number from ctl is a keystroke string.
    e.set("ui.keys.tab-live", &json!(0), "test").unwrap();
    assert_eq!(e.snapshot().config.ui.keys["tab-live"], "0");
    e.set("ui.keys.record-toggle", &json!("mod-alt-space"), "test")
        .unwrap();
    let text = std::fs::read_to_string(&cfg).unwrap();
    assert!(text.contains("record-toggle = \"mod-alt-space\""), "{text}");
    // An edit to the file reloads; a clash in it is refused and the running map kept.
    std::fs::write(&cfg, "[ui.keys]\nhelp = \"h\"\n").unwrap();
    e.reload("test").unwrap();
    let keys = e.snapshot().config.ui.keys.clone();
    assert_eq!(keys["help"], "h");
    assert_eq!(keys["record-toggle"], "space");
    std::fs::write(&cfg, "[ui.keys]\nhelp = \"h\"\nsearch = \"/, h\"\n").unwrap();
    assert_eq!(e.reload("test").unwrap_err().code, "invalid_combination");
    assert_eq!(e.snapshot().config.ui.keys, keys);
}
