//! Recording through the Action registry (dot-4uzgol.7) with a deterministic fake capture
//! backend: devices-list, record-start/pause/stop, session-get duration, and every audio config
//! value. The real OS backends are proven by the live tests in the repo root `tests/audio_live.rs`.

use std::sync::Arc;
use std::time::Duration;

use oats_audio::fake::FakeBackend;
use oats_audio::wav;
use oats_core::action::Action;
use oats_core::config::keys;
use oats_core::control::engine::{Engine, EngineOpts};
use serde_json::{json, Value};

struct Rig {
    dir: tempfile::TempDir,
    engine: Arc<Engine>,
}

fn rig() -> Rig {
    let dir = tempfile::tempdir().unwrap();
    let engine = Engine::new(EngineOpts {
        user_config: dir.path().join("config.toml"),
        pack: None,
        os: "linux".into(),
        arch: "x86_64".into(),
        machine: "default".into(),
    })
    .unwrap();
    engine.set("audio.record_dir", &json!(dir.path().join("rec").to_str().unwrap()), "test").unwrap();
    engine.set_audio_factory(Box::new(|_| Ok(Box::new(FakeBackend::default()))));
    Rig { dir, engine }
}

fn start(r: &Rig, id: &str) -> Value {
    r.engine
        .dispatch(&Action::RecordStart { session: Some(id.into()), title: Some("t".into()) })
        .unwrap()
}

fn stop(r: &Rig) -> Value {
    r.engine.dispatch(&Action::RecordStop {}).unwrap()
}

#[test]
fn devices_list_reports_a_loopback_source() {
    let r = rig();
    let v = r.engine.dispatch(&Action::DevicesList {}).unwrap();
    assert_eq!(v["loopback_available"], true);
    let kinds: Vec<&str> = v["devices"].as_array().unwrap().iter().map(|d| d["kind"].as_str().unwrap()).collect();
    assert!(kinds.contains(&"loopback") && kinds.contains(&"mic"), "{kinds:?}");
}

#[test]
fn record_start_stop_session_get_roundtrip() {
    let r = rig();
    let s = start(&r, "s1");
    assert_eq!(s["session"], "s1");
    assert_eq!(s["channels"], 2);
    std::thread::sleep(Duration::from_millis(1500));
    let live = r.engine.dispatch(&Action::SessionGet { session: "s1".into() }).unwrap();
    assert_eq!(live["state"], "recording");
    assert!(live["duration_ms"].as_u64().unwrap() >= 800, "{live}");
    let done = stop(&r);
    assert!(done["duration_ms"].as_u64().unwrap() >= 1400, "{done}");
    let got = r.engine.dispatch(&Action::SessionGet { session: "s1".into() }).unwrap();
    assert_eq!(got["state"], "saved");
    assert_eq!(got["duration_ms"], done["duration_ms"]);
    assert_eq!(got["title"], "t");
    assert!(got["ended_at"].is_string());
    let d = wav::read(std::path::Path::new(got["audio_path"].as_str().unwrap())).unwrap();
    assert_eq!(d.channels, 2);
    assert!(d.rms(0) > 0.3 && d.rms(1) > 0.3);
    assert!((d.duration_secs() * 1000.0 - done["duration_ms"].as_f64().unwrap()).abs() < 5.0);
}

#[test]
fn state_errors_are_structured() {
    let r = rig();
    for a in [Action::RecordStop {}, Action::RecordPause {}] {
        assert_eq!(r.engine.dispatch(&a).unwrap_err().code, "not_recording");
    }
    start(&r, "s1");
    let e = r.engine.dispatch(&Action::RecordStart { session: None, title: None }).unwrap_err();
    assert_eq!(e.code, "already_recording");
    stop(&r);
    let e = r.engine.dispatch(&Action::RecordStart { session: Some("s1".into()), title: None }).unwrap_err();
    assert_eq!(e.code, "exists");
    let e = r.engine.dispatch(&Action::RecordStart { session: Some("../x".into()), title: None }).unwrap_err();
    assert_eq!(e.code, "invalid_argument");
    let e = r.engine.dispatch(&Action::SessionGet { session: "nope".into() }).unwrap_err();
    assert_eq!(e.code, "not_found");
}

#[test]
fn pause_toggles_and_reports() {
    let r = rig();
    start(&r, "p");
    assert_eq!(r.engine.dispatch(&Action::RecordPause {}).unwrap()["paused"], true);
    let g = r.engine.dispatch(&Action::SessionGet { session: "p".into() }).unwrap();
    assert_eq!(g["state"], "paused");
    assert_eq!(r.engine.dispatch(&Action::RecordPause {}).unwrap()["paused"], false);
    stop(&r);
}

#[test]
fn every_audio_choice_value_takes_effect() {
    // layout: split -> 2 channels, mix -> 1.
    for (layout, channels) in [("split", 2u64), ("mix", 1)] {
        let r = rig();
        r.engine.set("audio.layout", &json!(layout), "test").unwrap();
        let s = start(&r, "l");
        assert_eq!(s["channels"], channels, "{layout}");
        stop(&r);
        let d = wav::read(&r.dir.path().join("rec/l/audio.wav")).unwrap();
        assert_eq!(u64::from(d.channels), channels);
    }
    // system.enabled / mic.enabled, all four combinations: a disabled source is a silent channel,
    // both off is an error.
    for (sys, mic) in [(true, true), (true, false), (false, true), (false, false)] {
        let r = rig();
        r.engine.set("audio.system.enabled", &json!(sys), "test").unwrap();
        r.engine.set("audio.mic.enabled", &json!(mic), "test").unwrap();
        let res = r.engine.dispatch(&Action::RecordStart { session: Some("e".into()), title: None });
        if !sys && !mic {
            assert_eq!(res.unwrap_err().code, "invalid_config");
            assert!(!r.dir.path().join("rec/e").exists(), "failed start left a folder");
            continue;
        }
        res.unwrap();
        std::thread::sleep(Duration::from_millis(500));
        stop(&r);
        let d = wav::read(&r.dir.path().join("rec/e/audio.wav")).unwrap();
        assert_eq!(d.rms(0) > 0.3, sys, "system {sys} {mic}");
        assert_eq!(d.rms(1) > 0.3, mic, "mic {sys} {mic}");
    }
    // explicit device ids: known ones work, unknown ones are device_not_found.
    let r = rig();
    r.engine.set("audio.system.device", &json!("fake.monitor"), "test").unwrap();
    r.engine.set("audio.mic.device", &json!("fake.mic"), "test").unwrap();
    start(&r, "d");
    stop(&r);
    r.engine.set("audio.mic.device", &json!("missing"), "test").unwrap();
    let e = r.engine.dispatch(&Action::RecordStart { session: Some("d2".into()), title: None }).unwrap_err();
    assert_eq!(e.code, "device_not_found");
}

#[test]
fn every_backend_value_resolves_or_fails_with_a_structured_error() {
    // Real selection, no fake: whichever tools this machine has, no value may panic or be rejected.
    let dir = tempfile::tempdir().unwrap();
    let engine = Engine::new(EngineOpts {
        user_config: dir.path().join("config.toml"),
        pack: None,
        os: "linux".into(),
        arch: "x86_64".into(),
        machine: "default".into(),
    })
    .unwrap();
    let (_, values) = keys::choices().into_iter().find(|(p, _)| *p == "audio.backend").unwrap();
    assert_eq!(values, ["auto", "pulse", "pipewire", "cpal"]);
    for v in values {
        engine.set("audio.backend", &json!(v), "test").unwrap();
        match engine.dispatch(&Action::DevicesList {}) {
            Ok(d) => assert!(d["backend"].is_string(), "{v}"),
            Err(e) => assert!(
                ["tool_missing", "backend_unavailable", "unsupported_platform", "device_not_found", "capture_failed"]
                    .contains(&e.code),
                "{v}: {e}"
            ),
        }
    }
    assert_eq!(engine.set("audio.backend", &json!("alsa"), "test").unwrap_err().code, "invalid_value");
}

#[test]
fn recording_events_are_published() {
    let r = rig();
    let rx = r.engine.bus.subscribe(vec![]);
    start(&r, "ev");
    stop(&r);
    let topics: Vec<String> = std::iter::from_fn(|| rx.recv_timeout(Duration::from_millis(300)).ok())
        .map(|line| serde_json::from_str::<Value>(&line).unwrap()["event"].as_str().unwrap().to_string())
        .collect();
    for want in ["recording-started", "recording-stopped", "session-saved"] {
        assert!(topics.iter().any(|t| t == want), "{want} in {topics:?}");
    }
}
