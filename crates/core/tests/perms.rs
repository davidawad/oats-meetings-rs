//! perms-status / perms-ensure (dot-4uzgol.96) through the engine, with a fake PipeWire graph
//! behind the capture-backend factory and a fake `Host` for the commands perms-ensure runs.
#![cfg(target_os = "linux")]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use oats_audio::{AudioError, Capture, CaptureBackend, DeviceInfo, DeviceKind, OnSamples};
use oats_core::action::Action;
use oats_core::control::engine::{Engine, EngineOpts};
use oats_core::perms::{self, Host, NULL_SINK};
use serde_json::{json, Value};

/// What the fake audio server has: is it running, does it have an output sink to monitor.
#[derive(Default)]
struct Graph {
    up: AtomicBool,
    sink: AtomicBool,
    mic: AtomicBool,
}

struct GraphBackend(Arc<Graph>);

impl CaptureBackend for GraphBackend {
    fn name(&self) -> &'static str {
        "pulse"
    }

    fn list_devices(&self) -> oats_audio::Result<Vec<DeviceInfo>> {
        let g = &self.0;
        if !g.up.load(Ordering::SeqCst) {
            return Err(AudioError::new(
                "backend_unavailable",
                "pactl: Connection failure: Connection refused",
            ));
        }
        let mut v = vec![];
        if g.mic.load(Ordering::SeqCst) {
            v.push(DeviceInfo {
                id: "alsa_input.usb-mic".into(),
                name: "USB Mic".into(),
                kind: DeviceKind::Mic,
                backend: "pulse",
                default: true,
            });
        }
        if g.sink.load(Ordering::SeqCst) {
            v.push(DeviceInfo {
                id: format!("{NULL_SINK}.monitor"),
                name: "Monitor".into(),
                kind: DeviceKind::Loopback,
                backend: "pulse",
                default: true,
            });
        }
        Ok(v)
    }

    fn open(&self, _: DeviceKind, _: &str, _: OnSamples) -> oats_audio::Result<Box<dyn Capture>> {
        Err(AudioError::new("unsupported", "listing only"))
    }
}

/// Commands act on the graph like the real tools would; every call is recorded.
struct FakeHost {
    graph: Arc<Graph>,
    calls: Mutex<Vec<String>>,
    env: Vec<(&'static str, &'static str)>,
    systemd: bool,
}

impl Host for FakeHost {
    fn env(&self, key: &str) -> Option<String> {
        self.env
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(_, v)| v.to_string())
    }
    fn read(&self, _: &str) -> Option<String> {
        None
    }
    fn run(&self, prog: &str, args: &[&str]) -> Result<String, String> {
        let line = format!("{prog} {}", args.join(" "));
        self.calls.lock().unwrap().push(line.clone());
        let g = &self.graph;
        match (prog, args.first().copied()) {
            ("systemctl", _) if self.systemd => {
                g.up.store(true, Ordering::SeqCst);
                Ok(String::new())
            }
            ("pactl", Some("info")) if g.up.load(Ordering::SeqCst) => {
                Ok("Server Name: PulseAudio (on PipeWire 1.2.0)".into())
            }
            ("pactl", Some("load-module")) if g.up.load(Ordering::SeqCst) => {
                g.sink.store(true, Ordering::SeqCst);
                Ok("536870913".into())
            }
            _ => Err(format!("{line}: failed")),
        }
    }
    fn sleep(&self, _: std::time::Duration) {}
}

struct Rig {
    _dir: tempfile::TempDir,
    engine: Arc<Engine>,
    graph: Arc<Graph>,
}

fn rig(toml: &str) -> Rig {
    let dir = tempfile::tempdir().unwrap();
    let cfg = dir.path().join("cfg").join("config.toml");
    std::fs::create_dir_all(cfg.parent().unwrap()).unwrap();
    std::fs::write(&cfg, toml).unwrap();
    let engine = Engine::new(EngineOpts {
        user_config: cfg,
        pack: None,
        os: "linux".into(),
        arch: "x86_64".into(),
        machine: "default".into(),
        sessions_dir: dir.path().join("data").join("sessions"),
        secrets: None,
    })
    .unwrap();
    let graph = Arc::new(Graph::default());
    let g = Arc::clone(&graph);
    engine.set_audio_factory(Box::new(move |_| {
        Ok(Box::new(GraphBackend(Arc::clone(&g))) as Box<dyn CaptureBackend>)
    }));
    Rig {
        _dir: dir,
        engine,
        graph,
    }
}

impl Rig {
    fn host(&self, systemd: bool) -> FakeHost {
        FakeHost {
            graph: Arc::clone(&self.graph),
            calls: Mutex::new(vec![]),
            env: vec![("XDG_RUNTIME_DIR", "/run/user/1000")],
            systemd,
        }
    }
}

fn perm<'a>(status: &'a Value, name: &str) -> &'a Value {
    status["permissions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["name"] == name)
        .unwrap_or_else(|| panic!("no {name} in {status}"))
}

#[test]
fn a_full_graph_is_all_granted_with_no_portal_and_says_how() {
    let r = rig("");
    r.graph.up.store(true, Ordering::SeqCst);
    r.graph.mic.store(true, Ordering::SeqCst);
    r.graph.sink.store(true, Ordering::SeqCst);
    let s = perms::status_with(&r.engine, &r.host(true));
    assert_eq!(s["all_granted"], true, "{s}");
    assert_eq!(s["os"], "linux");
    assert_eq!(s["sandbox"], "none");
    assert_eq!(s["headless"], true, "no DISPLAY in the fake host");
    let mic = perm(&s, "microphone");
    assert_eq!(mic["state"], "granted");
    assert_eq!(mic["device"], "alsa_input.usb-mic");
    assert!(mic["how"].as_str().unwrap().contains("no portal"), "{mic}");
    let sys = perm(&s, "system_audio");
    assert_eq!(sys["device"], format!("{NULL_SINK}.monitor"));
    assert!(
        sys["how"].as_str().unwrap().contains("monitor source"),
        "{sys}"
    );
    assert_eq!(perm(&s, "screencast_portal")["state"], "not_needed");
    assert_eq!(perm(&s, "calendar")["state"], "not_needed");
    // The engine action answers the same shape (the real host: no commands run).
    let v = r.engine.dispatch(&Action::PermsStatus {}).unwrap();
    assert_eq!(v["all_granted"], true, "{v}");
}

#[test]
fn ensure_starts_the_server_and_loads_a_null_sink_without_a_human() {
    let r = rig("");
    r.graph.mic.store(true, Ordering::SeqCst);
    let host = r.host(true);
    let before = perms::status_with(&r.engine, &host);
    assert_eq!(before["all_granted"], false);
    assert_eq!(perm(&before, "microphone")["state"], "unavailable");
    assert!(host.calls.lock().unwrap().is_empty(), "status runs nothing");

    let v = perms::ensure_with(&r.engine, &host);
    let steps: Vec<&str> = v["steps"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["step"].as_str().unwrap())
        .collect();
    assert_eq!(steps, ["start_audio_server", "null_sink"], "{v}");
    assert!(
        v["steps"]
            .as_array()
            .unwrap()
            .iter()
            .all(|s| s["ok"] == true),
        "{v}"
    );
    assert_eq!(v["status"]["all_granted"], true, "{v}");
    let calls = host.calls.lock().unwrap().clone();
    assert!(
        calls[0].starts_with("systemctl --user start pipewire.socket"),
        "{calls:?}"
    );
    assert!(
        calls.iter().any(|c| c.starts_with(&format!(
            "pactl load-module module-null-sink sink_name={NULL_SINK}"
        ))),
        "{calls:?}"
    );

    // Granted already: a second ensure changes nothing.
    let host2 = r.host(true);
    let again = perms::ensure_with(&r.engine, &host2);
    assert_eq!(again["steps"], json!([]), "{again}");
    assert!(host2.calls.lock().unwrap().is_empty());
}

#[test]
fn without_systemd_ensure_falls_back_to_pulseaudio_and_reports_what_needs_a_person() {
    let r = rig("");
    let host = r.host(false);
    let v = perms::ensure_with(&r.engine, &host);
    let first = &v["steps"][0];
    assert_eq!(first["step"], "start_audio_server");
    assert_eq!(first["ok"], false);
    let calls = host.calls.lock().unwrap().clone();
    assert!(calls.iter().any(|c| c == "pulseaudio --start"), "{calls:?}");
    assert_eq!(v["status"]["all_granted"], false);
}

#[test]
fn null_sink_is_a_choice_and_a_missing_mic_is_for_a_person() {
    let r = rig("[audio]\nensure_null_sink = false\n");
    r.graph.up.store(true, Ordering::SeqCst);
    let v = perms::ensure_with(&r.engine, &r.host(true));
    let steps = v["steps"].as_array().unwrap();
    let sink = steps.iter().find(|s| s["step"] == "null_sink").unwrap();
    assert_eq!(sink["skipped"], true, "{v}");
    let mic = steps.iter().find(|s| s["step"] == "microphone").unwrap();
    assert_eq!(mic["needs_human"], true, "{v}");
    assert!(!r.graph.sink.load(Ordering::SeqCst));
}

#[test]
fn disabled_sources_count_as_fine() {
    let r = rig("[audio.mic]\nenabled = false\n");
    r.graph.up.store(true, Ordering::SeqCst);
    r.graph.sink.store(true, Ordering::SeqCst);
    let s = perms::status_with(&r.engine, &r.host(true));
    assert_eq!(perm(&s, "microphone")["state"], "disabled");
    assert_eq!(s["all_granted"], true, "{s}");
}

#[test]
fn calendar_feeds_need_policy_and_secrets_not_an_os_grant() {
    let r = rig("[secrets]\nbackend = \"env\"\n");
    r.graph.up.store(true, Ordering::SeqCst);
    r.graph.mic.store(true, Ordering::SeqCst);
    r.graph.sink.store(true, Ordering::SeqCst);
    let host = r.host(true);
    r.engine
        .dispatch(&Action::IcsAdd {
            name: "work".into(),
            url: "caldav://dav.example/cal/me/".into(),
            enabled: None,
            refresh_secs: None,
            header: Some("Authorization: secret:caldav/permswork".into()),
        })
        .unwrap();
    let s = perms::status_with(&r.engine, &host);
    let cal = perm(&s, "calendar");
    assert_eq!(cal["state"], "blocked", "{cal}");
    assert_eq!(cal["kinds"], json!(["caldav"]));
    let reasons = cal["problems"].to_string();
    assert!(reasons.contains("network.allow"), "{reasons}");
    assert!(reasons.contains("secret"), "{reasons}");
    assert_eq!(s["all_granted"], false);

    r.engine.set("network.allow", &json!(true), "test").unwrap();
    std::env::set_var("OATS_SECRET_CALDAV_PERMSWORK", "Basic dTpw");
    let s = perms::status_with(&r.engine, &host);
    let cal = perm(&s, "calendar");
    assert_eq!(cal["state"], "granted", "{cal}");
    assert_eq!(cal["feeds"], 1);
    assert_eq!(s["all_granted"], true, "{s}");
}
