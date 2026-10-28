//! Whole-config reload at runtime (dot-4uzgol.18): atomic swap across subsystems, rollback on an
//! invalid file with no retry spam, a CLI verb, and a reload test generated from the key schema for
//! EVERY key (a new key is covered automatically, and `Key.reload` is a required field).

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{mpsc, Arc};
use std::thread;
use std::time::Duration;

use oats_core::config::keys::{self, Kind, Reload, KEYS};
use oats_core::config::resolve::{defaults_flat, nest};
use oats_core::config::typed::Config;
use oats_core::control::client::Client;
use oats_core::control::engine::{Engine, EngineOpts};
use oats_core::control::server;
use oats_core::control::subsystem::Subsystem;
use serde_json::{json, Map, Value};

fn opts(dir: &Path, cfg: &Path) -> EngineOpts {
    EngineOpts {
        user_config: cfg.to_path_buf(),
        pack: None,
        os: "linux".into(),
        arch: "x86_64".into(),
        machine: "default".into(),
        sessions_dir: dir.join("sessions"),
        secrets: None,
    }
}

fn toml_for(pairs: &[(&str, Value)]) -> String {
    let flat = pairs.iter().map(|(k, v)| ((*k).to_string(), v.clone())).collect();
    let v: toml::Value = serde_json::from_value(nest(&flat)).unwrap();
    toml::to_string(&v).unwrap()
}

fn p(pairs: &[(&str, Value)]) -> Map<String, Value> {
    pairs.iter().map(|(k, v)| (k.to_string(), v.clone())).collect()
}

fn events(mut sub: Client) -> mpsc::Receiver<Value> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        while let Ok(ev) = sub.next_event() {
            if tx.send(ev).is_err() {
                break;
            }
        }
    });
    rx
}

fn next(rx: &mpsc::Receiver<Value>, topic: &str, secs: u64) -> Option<Value> {
    let end = std::time::Instant::now() + Duration::from_secs(secs);
    while let Some(left) = end.checked_duration_since(std::time::Instant::now()) {
        match rx.recv_timeout(left) {
            Ok(ev) if ev["event"] == topic => return Some(ev),
            Ok(_) => {}
            Err(_) => return None,
        }
    }
    None
}

/// Values to try for a key: every allowed value (bool and enum), or a sample for strings.
fn samples(k: &oats_core::config::keys::Key, dir: &Path) -> Vec<Value> {
    match k.kind {
        Kind::Bool => vec![json!(true), json!(false)],
        Kind::Enum(vs) => vs.iter().map(|v| json!(v)).collect(),
        // Both ends of the allowed range: the boundaries are the interesting values.
        Kind::Int { min, max } => vec![json!(min), json!(max)],
        Kind::Duration { min_ms, max_ms } => {
            vec![json!(keys::format_duration_ms(min_ms)), json!(keys::format_duration_ms(max_ms))]
        }
        Kind::Str if k.path == "control.socket" => vec![json!(dir.join("alt.sock").to_str().unwrap()), json!("")],
        Kind::Str if k.path == "llm.api_key_ref" => vec![json!("secret:llm/other"), json!("secret:ns/x")],
        Kind::Str if k.path.ends_with("base_url") => vec![json!("http://127.0.0.1:9"), json!("")],
        Kind::Str if k.path == "sessions.root" => vec![json!(dir.join("elsewhere").to_str().unwrap()), json!("")],
        Kind::Str if k.path == "ics.feeds" => vec![
            json!(r#"[{"name":"a","url":"https://example.invalid/a.ics"}]"#),
            json!(r#"[{"name":"b","url":"file:///tmp/b.ics","enabled":false}]"#),
        ],
        Kind::Str if matches!(k.path, "stt.live.chunk_secs" | "stt.local.threads" | "stt.cloud.timeout_secs") => vec![json!("11"), json!("22")],
        Kind::Str if k.path == "ics.default_tz" => vec![json!("Europe/Paris"), json!("Asia/Tokyo")],
        Kind::Str => vec![json!("sample-x"), json!("sample-y")],
    }
}

/// Keys that only make sense together are written together.
fn companions(path: &str, v: &Value) -> Vec<(&'static str, Value)> {
    if path == "stt.backend" && v == "cloud" {
        vec![("stt.cloud.adapter", json!("openai-compatible"))]
    } else {
        vec![]
    }
}

#[test]
fn every_key_reloads_to_every_value_and_the_swap_is_whole() {
    let mut visited = BTreeSet::new();
    for k in KEYS {
        let d = tempfile::tempdir().unwrap();
        let cfg = d.path().join("config.toml");
        let engine = Engine::new(opts(d.path(), &cfg)).unwrap();
        // Live and Lock/Rebind keys are all checked here for the swap itself; the classes with
        // extra moving parts are exercised against a running server in the tests below.
        match k.reload {
            Reload::Live | Reload::Rebind | Reload::Lock => {}
        }
        for v in samples(k, d.path()) {
            let mut pairs = vec![(k.path, v.clone())];
            pairs.extend(companions(k.path, &v));
            std::fs::write(&cfg, toml_for(&pairs)).unwrap();
            let before = engine.snapshot();
            let changed = engine.reload("test").unwrap_or_else(|e| panic!("{} = {v}: {e}", k.path));
            let after = engine.snapshot();
            assert_eq!(after.resolved.flat[k.path], v, "{} = {v}", k.path);
            // Whole-config swap: every key not in the file is back at its default, and the typed
            // view is consistent with the flat view.
            for (other, dv) in defaults_flat() {
                if pairs.iter().all(|(pk, _)| *pk != other) {
                    assert_eq!(after.resolved.flat[&other], dv, "leak of {other} after {} = {v}", k.path);
                }
            }
            let typed: Config = serde_json::from_value(after.resolved.nested()).unwrap();
            assert_eq!(typed, after.config);
            let _ = (before, changed);
        }
        visited.insert(k.path);
    }
    assert_eq!(visited.len(), KEYS.len());
}

#[test]
fn reload_publishes_setting_changed_per_changed_key_only() {
    let d = tempfile::tempdir().unwrap();
    let cfg = d.path().join("config.toml");
    let engine = Engine::new(opts(d.path(), &cfg)).unwrap();
    let rx = engine.bus.subscribe(vec!["setting-changed".into(), "config-reloaded".into()]);
    std::fs::write(&cfg, toml_for(&[("app.menubar", json!(true)), ("llm.backend", json!("openrouter"))])).unwrap();
    let changed = engine.reload("test").unwrap();
    assert_eq!(changed, vec!["app.menubar".to_string(), "llm.backend".to_string()]);
    let evs: Vec<Value> = rx.try_iter().map(|l| serde_json::from_str(&l).unwrap()).collect();
    let keys: Vec<&str> = evs.iter().filter(|e| e["event"] == "setting-changed").map(|e| e["data"]["key"].as_str().unwrap()).collect();
    assert_eq!(keys, ["app.menubar", "llm.backend"]);
    assert!(evs.iter().any(|e| e["event"] == "config-reloaded"));
    // Reloading identical content changes nothing and says so.
    assert!(engine.reload("test").unwrap().is_empty());
}

struct Probe {
    name: &'static str,
    fail: bool,
    validated: AtomicUsize,
    applied: AtomicUsize,
    aborted: AtomicUsize,
    last_menubar: std::sync::Mutex<Option<(bool, bool)>>,
}

impl Probe {
    fn new(name: &'static str, fail: bool) -> Arc<Probe> {
        Arc::new(Probe {
            name,
            fail,
            validated: 0.into(),
            applied: 0.into(),
            aborted: 0.into(),
            last_menubar: Default::default(),
        })
    }
}

impl Subsystem for Probe {
    fn name(&self) -> &'static str {
        self.name
    }

    fn validate(&self, new: &Config) -> oats_core::config::Result<()> {
        self.validated.fetch_add(1, Ordering::SeqCst);
        if self.fail && new.app.menubar {
            return Err(oats_core::config::ConfigError::new("invalid_value", "probe says no"));
        }
        Ok(())
    }

    fn apply(&self, old: &Config, new: &Config) {
        self.applied.fetch_add(1, Ordering::SeqCst);
        *self.last_menubar.lock().unwrap() = Some((old.app.menubar, new.app.menubar));
    }

    fn abort(&self) {
        self.aborted.fetch_add(1, Ordering::SeqCst);
    }
}

#[test]
fn subsystems_see_old_and_new_and_one_veto_rolls_everything_back() {
    let d = tempfile::tempdir().unwrap();
    let cfg = d.path().join("config.toml");
    let engine = Engine::new(opts(d.path(), &cfg)).unwrap();
    let good = Probe::new("good", false);
    let veto = Probe::new("veto", true);
    engine.register(good.clone());
    engine.register(veto.clone());
    let rx = engine.bus.subscribe(vec!["config-reload-failed".into(), "setting-changed".into()]);

    std::fs::write(&cfg, toml_for(&[("app.menubar", json!(true))])).unwrap();
    let e = engine.reload("test").unwrap_err();
    assert_eq!(e.code, "invalid_value");
    assert!(e.message.starts_with("veto: "), "{}", e.message);
    // Nothing was applied anywhere, the running config is unchanged, the good one was told to abort.
    assert_eq!(good.applied.load(Ordering::SeqCst), 0);
    assert_eq!(veto.applied.load(Ordering::SeqCst), 0);
    assert!(good.aborted.load(Ordering::SeqCst) >= 1);
    assert_eq!(engine.snapshot().resolved.flat["app.menubar"], json!(false));
    let evs: Vec<Value> = rx.try_iter().map(|l| serde_json::from_str(&l).unwrap()).collect();
    assert_eq!(evs.len(), 1, "{evs:?}");
    assert_eq!(evs[0]["event"], "config-reload-failed");
    assert_eq!(evs[0]["data"]["code"], "invalid_value");

    // A config everyone accepts: both are applied, with old and new.
    std::fs::write(&cfg, toml_for(&[("app.menubar", json!(false)), ("llm.backend", json!("openrouter"))])).unwrap();
    engine.reload("test").unwrap();
    assert_eq!(good.applied.load(Ordering::SeqCst), 1);
    assert_eq!(veto.applied.load(Ordering::SeqCst), 1);
    assert_eq!(*good.last_menubar.lock().unwrap(), Some((false, false)));
}

#[test]
fn config_set_is_vetoed_before_the_file_is_written() {
    let d = tempfile::tempdir().unwrap();
    let cfg = d.path().join("config.toml");
    let engine = Engine::new(opts(d.path(), &cfg)).unwrap();
    engine.register(Probe::new("veto", true));
    let e = engine.set("app.menubar", &json!(true), "test").unwrap_err();
    assert_eq!(e.code, "invalid_value");
    assert!(!cfg.exists(), "a vetoed set must not create or change the file");
}

#[test]
fn sessions_root_that_is_a_file_is_rejected_by_the_sessions_subsystem() {
    let d = tempfile::tempdir().unwrap();
    let cfg = d.path().join("config.toml");
    let afile = d.path().join("afile");
    std::fs::write(&afile, "x").unwrap();
    let engine = Engine::new(opts(d.path(), &cfg)).unwrap();
    std::fs::write(&cfg, toml_for(&[("sessions.root", json!(afile.to_str().unwrap()))])).unwrap();
    let e = engine.reload("test").unwrap_err();
    assert_eq!(e.code, "invalid_value");
    assert!(e.message.starts_with("sessions: "), "{}", e.message);
    assert_eq!(engine.snapshot().resolved.flat["sessions.root"], json!(""));
}

struct Rig {
    _dir: tempfile::TempDir,
    dir: PathBuf,
    rt: PathBuf,
    cfg: PathBuf,
    handle: Option<server::ServerHandle>,
}

impl Rig {
    fn new(initial: &str) -> Rig {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join("config.toml");
        std::fs::write(&cfg, initial).unwrap();
        let engine = Engine::new(opts(dir.path(), &cfg)).unwrap();
        let rt = dir.path().join("rt");
        let handle = server::serve_with_poll(engine, &rt, Duration::from_millis(40)).unwrap();
        Rig { dir: dir.path().to_path_buf(), _dir: dir, rt, cfg, handle: Some(handle) }
    }

    fn client(&self) -> Client {
        Client::connect(&self.rt).unwrap()
    }
}

#[test]
fn invalid_file_rolls_back_once_and_is_not_retried_until_it_changes() {
    let rig = Rig::new("[app]\nmenubar = false\n");
    let mut sub = rig.client();
    sub.call("subscribe", p(&[("topics", json!(["config-reload-failed", "config-reloaded", "setting-changed"]))])).unwrap();
    let rx = events(sub);

    // 1. Broken edit (unknown key): rolled back, exactly one failure event.
    std::fs::write(&rig.cfg, "[app]\nmenubar = true\n[bogus]\nx = 1\n").unwrap();
    let ev = next(&rx, "config-reload-failed", 5).expect("failure event");
    assert_eq!(ev["data"]["code"], "unknown_key");
    assert_eq!(ev["data"]["source"], "hot-reload");
    let mut c = rig.client();
    assert_eq!(c.call("config-get", p(&[("key", json!("app.menubar"))])).unwrap()["data"]["value"], false);

    // 2. Same broken content, several polls later: still one failure, no retry spam.
    thread::sleep(Duration::from_millis(600));
    assert!(next(&rx, "config-reload-failed", 1).is_none(), "retried an unchanged broken file");

    // 3. Rewriting the file with the same bytes (new mtime, same content) is still not a retry.
    std::fs::write(&rig.cfg, "[app]\nmenubar = true\n[bogus]\nx = 1\n").unwrap();
    thread::sleep(Duration::from_millis(400));
    assert!(next(&rx, "config-reload-failed", 1).is_none());

    // 4. A different broken content is a new attempt (and a new failure).
    std::fs::write(&rig.cfg, "[app]\nmenubar = \"maybe\"\n").unwrap();
    let ev = next(&rx, "config-reload-failed", 5).expect("second failure");
    assert_eq!(ev["data"]["code"], "type_mismatch");

    // 5. An explicit reload always retries and reports the error to the caller.
    let r = c.call("config-reload", Map::new()).unwrap();
    assert_eq!(r["error"]["code"], "type_mismatch");
    assert!(next(&rx, "config-reload-failed", 5).is_some());

    // 6. Fixing the file applies it.
    std::fs::write(&rig.cfg, "[app]\nmenubar = true\n").unwrap();
    let ev = next(&rx, "setting-changed", 5).expect("recovered");
    assert_eq!(ev["data"]["key"], "app.menubar");
    assert_eq!(c.call("config-get", p(&[("key", json!("app.menubar"))])).unwrap()["data"]["value"], true);
}

#[test]
fn rejected_cross_key_config_is_rolled_back_as_a_whole() {
    let rig = Rig::new("");
    let mut c = rig.client();
    // Valid keys plus an invalid combination: none of it applies.
    std::fs::write(&rig.cfg, "[app]\nmenubar = true\n[stt]\nbackend = \"cloud\"\n").unwrap();
    let r = c.call("config-reload", Map::new()).unwrap();
    assert_eq!(r["error"]["code"], "invalid_combination");
    assert_eq!(c.call("config-get", p(&[("key", json!("app.menubar"))])).unwrap()["data"]["value"], false);
    assert_eq!(c.call("config-get", p(&[("key", json!("stt.backend"))])).unwrap()["data"]["value"], "local");
}

#[test]
fn control_socket_key_rebinds_the_running_server() {
    let rig = Rig::new("");
    let alt = rig.dir.join("alt.sock");
    let mut c = rig.client();
    let r = c.call("config-set", p(&[("key", json!("control.socket")), ("value", json!(alt.to_str().unwrap()))])).unwrap();
    assert_eq!(r["ok"], true, "{r}");
    // The new address answers; the old one no longer does.
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let mut moved = None;
    while std::time::Instant::now() < deadline {
        if let Ok(c2) = Client::connect_socket(&alt) {
            moved = Some(c2);
            break;
        }
        thread::sleep(Duration::from_millis(25));
    }
    let mut moved = moved.expect("server did not move to the new socket");
    assert_eq!(moved.call("ping", Map::new()).unwrap()["ok"], true);
    assert!(Client::connect(&rig.rt).is_err(), "old socket still answers");
    // Existing connections survive the move.
    assert_eq!(c.call("ping", Map::new()).unwrap()["ok"], true);
    // And back to the default.
    moved.call("config-set", p(&[("key", json!("control.socket")), ("value", json!(""))])).unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while Client::connect(&rig.rt).is_err() {
        assert!(std::time::Instant::now() < deadline, "server did not return to the default socket");
        thread::sleep(Duration::from_millis(25));
    }
    // A socket path that cannot be bound is rejected and the server stays where it is.
    // (The parent of this path is a regular file, so it can never be bound.)
    let unbindable = rig.cfg.join("x.sock");
    let mut c = rig.client();
    let r = c.call("config-set", p(&[("key", json!("control.socket")), ("value", json!(unbindable.to_str().unwrap()))])).unwrap();
    assert_eq!(r["ok"], false, "{r}");
    assert_eq!(c.call("ping", Map::new()).unwrap()["ok"], true);
}

#[test]
fn single_instance_key_releases_and_retakes_the_lock() {
    let rig = Rig::new("");
    // A second engine in the same runtime dir (its own socket) is refused while the lock is held.
    let d2 = tempfile::tempdir().unwrap();
    let cfg2 = d2.path().join("config.toml");
    std::fs::write(&cfg2, toml_for(&[("control.socket", json!(d2.path().join("e2.sock").to_str().unwrap()))])).unwrap();
    let e2 = Engine::new(opts(d2.path(), &cfg2)).unwrap();
    assert_eq!(server::serve(e2.clone(), &rig.rt).err().unwrap().code, "already_running");

    // Turn the key off at runtime: the lock is released, the second engine can start.
    let mut c = rig.client();
    assert_eq!(c.call("config-set", p(&[("key", json!("app.single_instance")), ("value", json!(false))])).unwrap()["ok"], true);
    let h2 = server::serve(e2, &rig.rt).expect("lock should be free after single_instance=false");

    // Turning it back on while the other engine holds the lock is refused and rolled back.
    let rx = {
        let mut sub = rig.client();
        sub.call("subscribe", p(&[("topics", json!(["config-reload-failed"]))])).unwrap();
        events(sub)
    };
    let r = c.call("config-set", p(&[("key", json!("app.single_instance")), ("value", json!(true))])).unwrap();
    assert_eq!(r["error"]["code"], "already_running", "{r}");
    assert_eq!(c.call("config-get", p(&[("key", json!("app.single_instance"))])).unwrap()["data"]["value"], false);
    let _ = rx;
    // Once the other engine stops, it succeeds.
    h2.shutdown();
    assert_eq!(c.call("config-set", p(&[("key", json!("app.single_instance")), ("value", json!(true))])).unwrap()["ok"], true);
    drop(rig);
}

#[test]
fn cli_config_reload_verb_talks_to_the_running_engine() {
    let rig = Rig::new("[config]\nhot_reload = false\n");
    std::fs::write(&rig.cfg, "[config]\nhot_reload = false\n[app]\nmenubar = true\n").unwrap();
    let rt = rig.rt.to_str().unwrap().to_string();
    let env = move |k: &str| (k == "OATS_RUNTIME_DIR").then(|| rt.clone());
    let out = oats_core::config::cli::run(&["reload".to_string(), "--json".to_string()], &env);
    assert_eq!(out.code, 0, "{out:?}");
    let v: Value = serde_json::from_str(&out.stdout).unwrap();
    assert_eq!(v["data"]["changed"], json!(["app.menubar"]));
    // Bad file: the verb fails with the engine's code and the old config stays.
    std::fs::write(&rig.cfg, "[nope]\nx = 1\n").unwrap();
    let out = oats_core::config::cli::run(&["reload".to_string()], &env);
    assert_eq!(out.code, 1);
    assert!(out.stderr.contains("unknown_key"), "{out:?}");
    // No engine: a clear error.
    let none = tempfile::tempdir().unwrap();
    let np = none.path().join("none").to_str().unwrap().to_string();
    let env2 = move |k: &str| (k == "OATS_RUNTIME_DIR").then(|| np.clone());
    let out = oats_core::config::cli::run(&["reload".to_string()], &env2);
    assert_eq!(out.code, 1);
    assert!(out.stderr.contains("not_running"), "{out:?}");
}

fn fixture() -> String {
    concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/fixtures/openoats").to_string()
}

#[test]
fn delete_mode_remove_deletes_for_good() {
    let rig = Rig::new("[sessions]\ndelete_mode = \"remove\"\n");
    let mut c = rig.client();
    c.call("session-import", p(&[("from", json!(fixture()))])).unwrap();
    let id = "session_2026-10-20_09-00-00";
    let r = c.call("session-delete", p(&[("session", json!(id)), ("confirm", json!(true))])).unwrap();
    assert_eq!(r["data"]["mode"], "remove", "{r}");
    assert!(!rig.dir.join("sessions").join(id).exists());
}

#[test]
fn delete_mode_trash_moves_to_the_os_trash_and_is_reloadable() {
    // The OS trash lives under the data home; keep it (and the store) on one filesystem.
    let home = PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/tmp".into()));
    let base = tempfile::tempdir_in(&home).unwrap();
    std::env::set_var("XDG_DATA_HOME", base.path().join("xdg"));
    let cfg = base.path().join("config.toml");
    std::fs::write(&cfg, "[sessions]\ndelete_mode = \"remove\"\n").unwrap();
    let engine = Engine::new(opts(base.path(), &cfg)).unwrap();
    let rt = base.path().join("rt");
    let _h = server::serve_with_poll(engine, &rt, Duration::from_millis(40)).unwrap();
    let mut c = Client::connect(&rt).unwrap();
    c.call("session-import", p(&[("from", json!(fixture()))])).unwrap();
    // Switch mode at runtime (whole-config reload), then delete.
    std::fs::write(&cfg, "[sessions]\ndelete_mode = \"trash\"\n").unwrap();
    assert_eq!(c.call("config-reload", Map::new()).unwrap()["data"]["changed"], json!(["sessions.delete_mode"]));
    let id = "session_2026-10-20_09-00-00";
    let r = c.call("session-delete", p(&[("session", json!(id)), ("confirm", json!(true))])).unwrap();
    assert_eq!(r["data"]["mode"], "trash", "{r}");
    assert!(!base.path().join("sessions").join(id).exists(), "still in the store");
    #[cfg(target_os = "linux")]
    {
        let found = std::fs::read_dir(base.path().join("xdg/Trash/files"))
            .map(|d| d.flatten().any(|e| e.file_name().to_string_lossy() == id))
            .unwrap_or(false);
        assert!(found, "session not found in the XDG trash");
    }
    // The other session is untouched.
    assert!(base.path().join("sessions/session_2026-10-21_15-00-00/session.json").is_file());
}
