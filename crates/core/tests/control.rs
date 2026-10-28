//! Control API against a real in-process server over a real local socket.
//! Acceptance (dot-4uzgol.5): list-actions == manifest, ping, parallel ids, garbage-bytes
//! robustness, subscribe receives setting-changed after config-set; plus single-instance,
//! hot reload on/off and persistence.

use std::io::Write;
use std::path::PathBuf;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use interprocess::local_socket::{traits::Stream as _, GenericFilePath, Stream, ToFsName};
use oats_core::action::{self, SPECS};
use oats_core::control::client::Client;
use oats_core::control::engine::{Engine, EngineOpts};
use oats_core::control::{paths, server};
use serde_json::{json, Map, Value};

struct Rig {
    _dir: tempfile::TempDir,
    rt: PathBuf,
    cfg: PathBuf,
    sessions: PathBuf,
    handle: Option<server::ServerHandle>,
}

impl Rig {
    fn new() -> Rig {
        Self::with_config(None)
    }

    fn with_config(initial: Option<&str>) -> Rig {
        let dir = tempfile::tempdir().unwrap();
        let rt = dir.path().join("rt");
        let cfg = dir.path().join("cfg").join("config.toml");
        if let Some(t) = initial {
            std::fs::create_dir_all(cfg.parent().unwrap()).unwrap();
            std::fs::write(&cfg, t).unwrap();
        }
        let engine = Engine::new(EngineOpts {
            user_config: cfg.clone(),
            pack: None,
            os: "linux".into(),
            arch: "x86_64".into(),
            machine: "default".into(),
            sessions_dir: dir.path().join("sessions"),
            secrets: None,
        })
        .unwrap();
        let handle = server::serve_with_poll(engine, &rt, Duration::from_millis(40)).unwrap();
        let sessions = dir.path().join("sessions");
        Rig { _dir: dir, rt, cfg, sessions, handle: Some(handle) }
    }

    fn client(&self) -> Client {
        Client::connect(&self.rt).unwrap()
    }
}

fn p(pairs: &[(&str, Value)]) -> Map<String, Value> {
    pairs.iter().map(|(k, v)| (k.to_string(), v.clone())).collect()
}

#[test]
fn ping_and_hello_handshake() {
    let rig = Rig::new();
    let mut c = rig.client();
    assert_eq!(c.hello["protocol"], 1);
    assert_eq!(c.hello["app"], "oats-meetings-rs");
    let r = c.call("ping", Map::new()).unwrap();
    assert_eq!(r["ok"], true);
    assert_eq!(r["schema_version"], 1);
    assert_eq!(r["data"]["pong"], true);
}

#[test]
fn protocol_version_mismatch_is_refused() {
    let rig = Rig::new();
    let mut c = rig.client();
    c.send_raw(b"{\"id\":7,\"v\":99,\"action\":\"ping\"}\n").unwrap();
    let r = c.read_line().unwrap();
    assert_eq!(r["error"]["code"], "unsupported_version");
    assert_eq!(r["id"], 7);
}

#[test]
fn list_actions_equals_generated_manifest_and_committed_file() {
    let rig = Rig::new();
    let mut c = rig.client();
    let r = c.call("list-actions", Map::new()).unwrap();
    assert_eq!(r["data"], action::manifest_json());
    let committed: Value = serde_json::from_str(
        &std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../config/actions.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(r["data"], committed);
    assert_eq!(r["data"]["actions"].as_array().unwrap().len(), SPECS.len());
}

#[test]
fn implemented_actions_have_handlers_planned_ones_say_so() {
    let rig = Rig::new();
    let mut c = rig.client();
    for (name, _, status, _, params) in SPECS {
        if *name == "subscribe" || *name == "quit" {
            continue;
        }
        let mut m = Map::new();
        for (pn, _, req) in *params {
            if *req {
                m.insert((*pn).to_string(), json!("x"));
            }
        }
        let r = c.call(name, m).unwrap();
        let code = r["error"]["code"].as_str().unwrap_or("");
        if *status == "planned" {
            assert_eq!(code, "not_implemented", "{name}");
        } else {
            assert_ne!(code, "not_implemented", "{name}");
            assert_ne!(code, "unknown_action", "{name}");
        }
    }
}

#[test]
fn unknown_action_and_bad_params() {
    let rig = Rig::new();
    let mut c = rig.client();
    let r = c.call("frobnicate", Map::new()).unwrap();
    assert_eq!(r["error"]["code"], "unknown_action");
    let r = c.call("config-get", Map::new()).unwrap();
    assert_eq!(r["error"]["code"], "bad_params");
    let r = c.call("config-get", p(&[("key", json!("nope"))])).unwrap();
    assert_eq!(r["error"]["code"], "unknown_key");
}

#[test]
fn parallel_clients_get_their_own_ids_back_in_order() {
    let rig = Rig::new();
    let rt = rig.rt.clone();
    let threads: Vec<_> = (0..4)
        .map(|t| {
            let rt = rt.clone();
            thread::spawn(move || {
                let mut c = Client::connect(&rt).unwrap();
                for i in 0..60 {
                    let id = json!(format!("c{t}-{i}"));
                    let r = c.call_with_id(id.clone(), "ping", Map::new()).unwrap();
                    assert_eq!(r["id"], id);
                    assert_eq!(r["ok"], true);
                }
            })
        })
        .collect();
    for t in threads {
        t.join().unwrap();
    }
}

fn raw_connect(rt: &std::path::Path) -> Stream {
    let name = paths::socket_path(rt).to_fs_name::<GenericFilePath>().unwrap();
    Stream::connect(name).unwrap()
}

#[test]
fn garbage_bytes_do_not_kill_the_server() {
    let rig = Rig::new();
    // 1. binary junk that is not UTF-8, with and without a newline, then a hard close.
    for payload in [
        vec![0xff, 0xfe, 0x00, 0x80, b'\n'],
        (0..=255u8).collect::<Vec<u8>>(),
        b"{\"action\":".to_vec(),
        b"null\n[]\n42\n\"str\"\n{}\n{\"action\":5}\n{\"action\":\"ping\",\"id\":{\"a\":[1]}}\n".to_vec(),
        vec![b'\n'; 4096],
    ] {
        let mut s = raw_connect(&rig.rt);
        let _ = s.write_all(&payload);
        drop(s);
    }
    // 2. an oversize line with no newline: refused, not buffered forever.
    {
        let mut s = raw_connect(&rig.rt);
        let big = vec![b'a'; 2 * 1024 * 1024];
        let _ = s.write_all(&big);
        drop(s);
    }
    // 3. connect-and-vanish many times.
    for _ in 0..50 {
        drop(raw_connect(&rig.rt));
    }
    // 4. the server still answers, and bad lines on a live connection get errors, not a close.
    let mut c = rig.client();
    c.send_raw(b"\xff\xfe\n").unwrap();
    assert_eq!(c.read_line().unwrap()["error"]["code"], "bad_request");
    c.send_raw(b"not json\n").unwrap();
    assert_eq!(c.read_line().unwrap()["error"]["code"], "bad_request");
    c.send_raw(b"[1,2]\n").unwrap();
    assert_eq!(c.read_line().unwrap()["error"]["code"], "bad_request");
    c.send_raw(b"{\"id\":1}\n").unwrap();
    assert_eq!(c.read_line().unwrap()["error"]["code"], "bad_request");
    let r = c.call("ping", Map::new()).unwrap();
    assert_eq!(r["ok"], true);
    // 5. oversize on a live connection: error line, then close; a fresh connection works.
    let mut c2 = rig.client();
    let mut huge = vec![b'x'; 1_100_000];
    huge.push(b'\n');
    c2.send_raw(&huge).unwrap();
    assert_eq!(c2.read_line().unwrap()["error"]["code"], "request_too_large");
    assert_eq!(rig.client().call("ping", Map::new()).unwrap()["ok"], true);
}

/// control.max_line at its boundary: a line of exactly max_line bytes (newline not counted) is read,
/// one byte more is refused; and a reload changes the limit for a connection already open.
#[test]
fn control_max_line_boundary_and_live_change() {
    let rig = Rig::with_config(Some("[control]\nmax_line = 1024\n"));
    let mut c = rig.client();
    let mut ok = vec![b'x'; 1024];
    ok.push(b'\n');
    c.send_raw(&ok).unwrap();
    assert_eq!(c.read_line().unwrap()["error"]["code"], "bad_request");
    let mut over = vec![b'x'; 1025];
    over.push(b'\n');
    let mut c2 = rig.client();
    c2.send_raw(&over).unwrap();
    assert_eq!(c2.read_line().unwrap()["error"]["code"], "request_too_large");
    // Raise the limit through the engine; the open connection `c` picks it up on its next line.
    let r = c.call("config-set", p(&[("key", json!("control.max_line")), ("value", json!(4096))])).unwrap();
    assert_eq!(r["ok"], true, "{r}");
    let mut big = vec![b'x'; 3000];
    big.push(b'\n');
    c.send_raw(&big).unwrap();
    assert_eq!(c.read_line().unwrap()["error"]["code"], "bad_request");
}

#[test]
fn subscribe_receives_setting_changed_after_config_set() {
    let rig = Rig::new();
    let mut sub = rig.client();
    let ack = sub.call("subscribe", p(&[("topics", json!(["setting-changed"]))])).unwrap();
    assert_eq!(ack["ok"], true);
    let mut other = rig.client();
    // A different topic must not reach the filtered subscriber.
    other.call("window-show", Map::new()).unwrap();
    let r = other
        .call("config-set", p(&[("key", json!("app.menubar")), ("value", json!(true))]))
        .unwrap();
    assert_eq!(r["ok"], true, "{r}");
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        tx.send(sub.next_event()).ok();
    });
    let ev = rx.recv_timeout(Duration::from_secs(5)).expect("event").unwrap();
    assert_eq!(ev["event"], "setting-changed");
    assert_eq!(ev["data"]["key"], "app.menubar");
    assert_eq!(ev["data"]["old"], false);
    assert_eq!(ev["data"]["new"], true);
    assert_eq!(ev["data"]["source"], "config-set");
    assert!(ev["seq"].as_u64().unwrap() > 0);
}

#[test]
fn subscribe_rejects_unknown_topic() {
    let rig = Rig::new();
    let r = rig.client().call("subscribe", p(&[("topics", json!(["nope"]))])).unwrap();
    assert_eq!(r["error"]["code"], "bad_params");
}

#[test]
fn config_set_persists_validates_and_never_touches_unrelated_files() {
    let rig = Rig::new();
    let mut c = rig.client();
    let r = c.call("config-set", p(&[("key", json!("stt.local.model")), ("value", json!("small.en"))])).unwrap();
    assert_eq!(r["ok"], true);
    let text = std::fs::read_to_string(&rig.cfg).unwrap();
    assert!(text.contains("small.en"), "{text}");
    let g = c.call("config-get", p(&[("key", json!("stt.local.model"))])).unwrap();
    assert_eq!(g["data"]["value"], "small.en");
    // Rejected writes leave the file untouched.
    let before = std::fs::read_to_string(&rig.cfg).unwrap();
    for (k, v, code) in [
        ("nope.key", json!(1), "unknown_key"),
        ("stt.backend", json!("quantum"), "invalid_value"),
        ("app.menubar", json!("maybe"), "type_mismatch"),
        ("stt.backend", json!("cloud"), "invalid_combination"),
    ] {
        let r = c.call("config-set", p(&[("key", json!(k)), ("value", v)])).unwrap();
        assert_eq!(r["error"]["code"], code, "{k}");
    }
    assert_eq!(std::fs::read_to_string(&rig.cfg).unwrap(), before);
    // Bool keys accept the strings the CLI sends.
    let r = c.call("config-set", p(&[("key", json!("app.menubar")), ("value", json!("true"))])).unwrap();
    assert_eq!(r["ok"], true);
    let g = c.call("config-get", p(&[("key", json!("app.menubar"))])).unwrap();
    assert_eq!(g["data"]["value"], true);
    // And a fresh engine reading the same file sees both values.
    let rig2 = Rig::with_config(Some(&std::fs::read_to_string(&rig.cfg).unwrap()));
    let g = rig2.client().call("config-get", p(&[("key", json!("stt.local.model"))])).unwrap();
    assert_eq!(g["data"]["value"], "small.en");
}

#[test]
fn privacy_toggle_and_window_actions() {
    let rig = Rig::new();
    let mut c = rig.client();
    let r = c.call("privacy-toggle", Map::new()).unwrap();
    assert_eq!(r["data"]["value"], false); // default is true
    let r = c.call("privacy-toggle", Map::new()).unwrap();
    assert_eq!(r["data"]["value"], true);
    assert_eq!(c.call("window-toggle", Map::new()).unwrap()["data"]["visible"], true);
    assert_eq!(c.call("window-toggle", Map::new()).unwrap()["data"]["visible"], false);
    assert_eq!(c.call("window-show", Map::new()).unwrap()["data"]["visible"], true);
    assert_eq!(c.call("window-hide", Map::new()).unwrap()["data"]["visible"], false);
}

/// Pump a subscribed client's events into a channel so tests can wait with a deadline.
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

#[test]
fn hot_reload_on_picks_up_file_edits() {
    let rig = Rig::with_config(Some("[config]\nhot_reload = true\n"));
    let mut sub = rig.client();
    sub.call("subscribe", p(&[("topics", json!(["setting-changed"]))])).unwrap();
    std::fs::write(&rig.cfg, "[config]\nhot_reload = true\n[app]\nmenubar = true\n").unwrap();
    let ev = events(sub).recv_timeout(Duration::from_secs(5)).expect("hot reload event");
    assert_eq!(ev["data"]["key"], "app.menubar");
    assert_eq!(ev["data"]["source"], "hot-reload");
}

#[test]
fn hot_reload_off_ignores_file_edits_until_explicit_reload() {
    let rig = Rig::with_config(Some("[config]\nhot_reload = false\n"));
    let mut sub = rig.client();
    sub.call("subscribe", p(&[("topics", json!(["setting-changed"]))])).unwrap();
    let rx = events(sub);
    std::fs::write(&rig.cfg, "[config]\nhot_reload = false\n[app]\nmenubar = true\n").unwrap();
    thread::sleep(Duration::from_millis(500));
    let mut c = rig.client();
    let g = c.call("config-get", p(&[("key", json!("app.menubar"))])).unwrap();
    assert_eq!(g["data"]["value"], false, "file edit must not apply with hot_reload=false");
    assert!(rx.try_recv().is_err(), "no event may arrive from the ignored edit");
    // Explicit reload applies it and emits the event.
    let r = c.call("config-reload", Map::new()).unwrap();
    assert_eq!(r["data"]["changed"], json!(["app.menubar"]));
    let ev = rx.recv_timeout(Duration::from_secs(5)).expect("reload event");
    assert_eq!(ev["data"]["source"], "config-reload");
}

#[test]
fn bad_edit_on_disk_keeps_the_running_config() {
    let rig = Rig::with_config(Some("[config]\nhot_reload = true\n"));
    std::fs::write(&rig.cfg, "[bogus]\nx = 1\n").unwrap();
    thread::sleep(Duration::from_millis(300));
    let mut c = rig.client();
    assert_eq!(c.call("config-check", Map::new()).unwrap()["ok"], true);
    let r = c.call("config-reload", Map::new()).unwrap();
    assert_eq!(r["error"]["code"], "unknown_key");
}

#[test]
fn single_instance_lock() {
    let mut rig = Rig::new();
    let engine2 = Engine::new(EngineOpts {
        user_config: rig.cfg.clone(),
        pack: None,
        os: "linux".into(),
        arch: "x86_64".into(),
        machine: "default".into(),
        sessions_dir: rig.cfg.parent().unwrap().join("sessions"),
        secrets: None,
    })
    .unwrap();
    let e = server::serve(engine2.clone(), &rig.rt).err().expect("second engine refused");
    assert_eq!(e.code, "already_running");
    // After the first stops, the lock is free again.
    rig.handle.take().unwrap().shutdown();
    let h = server::serve(engine2, &rig.rt).unwrap();
    h.shutdown();
}

#[test]
fn quit_stops_the_engine_and_removes_the_socket() {
    let mut rig = Rig::new();
    let mut c = rig.client();
    assert_eq!(c.call("quit", Map::new()).unwrap()["data"]["stopping"], true);
    rig.handle.take().unwrap().wait();
    assert!(Client::connect(&rig.rt).is_err());
}

fn fixture() -> String {
    concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/fixtures/openoats").to_string()
}

fn session_ids(c: &mut Client) -> Vec<String> {
    let r = c.call("session-list", Map::new()).unwrap();
    assert_eq!(r["ok"], true, "{r}");
    r["data"]["sessions"].as_array().unwrap().iter().map(|s| s["id"].as_str().unwrap().to_string()).collect()
}

#[test]
fn session_import_then_list_equals_fixture_ids_over_the_api() {
    let rig = Rig::new();
    let mut c = rig.client();
    assert_eq!(session_ids(&mut c), Vec::<String>::new());
    let r = c.call("session-import", p(&[("from", json!(fixture()))])).unwrap();
    assert_eq!(r["ok"], true, "{r}");
    assert_eq!(session_ids(&mut c), vec!["session_2026-10-20_09-00-00", "session_2026-10-21_15-00-00"]);
    // The store on disk is the OpenOats layout.
    assert!(rig.sessions.join("session_2026-10-20_09-00-00/session.json").is_file());
    let r = c.call("session-get", p(&[("session", json!("session_2026-10-20_09-00-00"))])).unwrap();
    assert_eq!(r["data"]["title"], "Acme sync");
    let r = c.call("transcript-get", p(&[("session", json!("session_2026-10-21_15-00-00"))])).unwrap();
    assert_eq!(r["data"]["source"], "live");
}

#[test]
fn session_edit_actions_publish_events_and_delete_needs_confirm() {
    let rig = Rig::new();
    let mut c = rig.client();
    c.call("session-import", p(&[("from", json!(fixture()))])).unwrap();
    let mut sub = rig.client();
    sub.call("subscribe", p(&[("topics", json!(["session-saved", "session-deleted"]))])).unwrap();
    let rx = events(sub);
    let id = "session_2026-10-20_09-00-00";
    let r = c.call("session-rename", p(&[("session", json!(id)), ("name", json!("Renamed"))])).unwrap();
    assert_eq!(r["data"]["title"], "Renamed");
    let r = c.call("session-tag", p(&[("session", json!(id)), ("tag", json!("sales"))])).unwrap();
    assert_eq!(r["data"]["tags"], json!(["sales"]));
    let r = c.call("session-tag", p(&[("session", json!(id)), ("tag", json!("sales")), ("remove", json!(true))])).unwrap();
    assert_eq!(r["data"]["tags"], json!([]));
    for change in ["rename", "tag", "tag"] {
        let ev = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!((ev["event"].as_str(), ev["data"]["change"].as_str()), (Some("session-saved"), Some(change)));
    }
    // Default (sessions.confirm_delete = true): refused without confirm, nothing deleted.
    let r = c.call("session-delete", p(&[("session", json!(id))])).unwrap();
    assert_eq!(r["error"]["code"], "confirmation_required");
    let r = c.call("session-delete", p(&[("session", json!(id)), ("confirm", json!(false))])).unwrap();
    assert_eq!(r["error"]["code"], "confirmation_required");
    assert_eq!(session_ids(&mut c).len(), 2);
    let r = c.call("session-delete", p(&[("session", json!(id)), ("confirm", json!(true))])).unwrap();
    assert_eq!(r["ok"], true);
    assert_eq!(rx.recv_timeout(Duration::from_secs(5)).unwrap()["event"], "session-deleted");
    assert_eq!(session_ids(&mut c), vec!["session_2026-10-21_15-00-00"]);
    let r = c.call("session-get", p(&[("session", json!(id))])).unwrap();
    assert_eq!(r["error"]["code"], "not_found");
}

#[test]
fn sessions_confirm_delete_off_allows_delete_without_confirm() {
    let rig = Rig::with_config(Some("[sessions]\nconfirm_delete = false\n"));
    let mut c = rig.client();
    c.call("session-import", p(&[("from", json!(fixture()))])).unwrap();
    let r = c.call("session-delete", p(&[("session", json!("session_2026-10-20_09-00-00"))])).unwrap();
    assert_eq!(r["ok"], true, "{r}");
    assert_eq!(session_ids(&mut c).len(), 1);
}

#[test]
fn sessions_root_config_key_moves_the_store() {
    let other = tempfile::tempdir().unwrap();
    let toml = format!("[sessions]\nroot = {:?}\n", other.path().join("elsewhere").to_str().unwrap());
    let rig = Rig::with_config(Some(&toml));
    let mut c = rig.client();
    c.call("session-import", p(&[("from", json!(fixture()))])).unwrap();
    assert!(other.path().join("elsewhere/session_2026-10-20_09-00-00/session.json").is_file());
    assert!(!rig.sessions.exists());
}

#[test]
fn config_set_keeps_user_comments_and_layout() {
    let original = "# my settings\n[app]\nmenubar = false # tray later\n\n# models\n[stt.local]\nmodel = \"base.en\"\n";
    let rig = Rig::with_config(Some(original));
    let mut c = rig.client();
    let r = c.call("config-set", p(&[("key", json!("app.menubar")), ("value", json!(true))])).unwrap();
    assert_eq!(r["ok"], true, "{r}");
    let r = c.call("config-set", p(&[("key", json!("privacy.hide_method")), ("value", json!("none"))])).unwrap();
    assert_eq!(r["ok"], true, "{r}");
    let text = std::fs::read_to_string(&rig.cfg).unwrap();
    assert!(text.contains("# my settings"), "{text}");
    assert!(text.contains("menubar = true # tray later"), "{text}");
    assert!(text.contains("# models\n[stt.local]"), "{text}");
    assert!(text.contains("model = \"base.en\""), "{text}");
    assert!(text.contains("hide_method = \"none\""), "{text}");
    assert!(!text.contains("[privacy]\n[privacy]"), "{text}");
}
