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
use oats_core::stt::{Asr, Segment};
use serde_json::{json, Value};

/// Says "tone" for any chunk that has sound in it, with the chunk's length as its time span.
struct ToneAsr;

impl Asr for ToneAsr {
    fn name(&self) -> &'static str {
        "tone-asr"
    }

    fn transcribe(&self, pcm: &[f32], _language: &str) -> oats_core::config::Result<Vec<Segment>> {
        let loud = pcm.iter().any(|s| s.abs() > 0.1);
        Ok(if loud {
            vec![Segment {
                start: 0.0,
                end: pcm.len() as f64 / 16000.0,
                text: "tone".into(),
            }]
        } else {
            vec![]
        })
    }
}

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
        sessions_dir: dir.path().join("sessions"),
        secrets: None,
    })
    .unwrap();
    // Recordings are sessions: they land under the session store root.
    engine
        .set(
            "sessions.root",
            &json!(dir.path().join("rec").to_str().unwrap()),
            "test",
        )
        .unwrap();
    engine.set_audio_factory(Box::new(|_| Ok(Box::new(FakeBackend::default()))));
    engine.set_asr_factory(Box::new(|_| Ok(Arc::new(ToneAsr))));
    // 1 s live windows so a few seconds of fake audio produce live utterances.
    engine.set("stt.live.chunk", &json!("1s"), "test").unwrap();
    Rig { dir, engine }
}

/// Poll `cond` until it holds (30 s deadline) instead of sleeping a fixed wall time.
fn wait_until(what: &str, mut cond: impl FnMut() -> bool) {
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    while !cond() {
        assert!(
            std::time::Instant::now() < deadline,
            "timed out waiting for {what}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn start(r: &Rig, id: &str) -> Value {
    r.engine
        .dispatch(&Action::RecordStart {
            session: Some(id.into()),
            title: Some("t".into()),
        })
        .unwrap()
}

fn stop(r: &Rig) -> Value {
    r.engine.dispatch(&Action::RecordStop {}).unwrap()
}

/// A backend that never answers its device probe (dot-4uzgol.57): devices-list fails with
/// devices_timeout after audio.devices_timeout instead of hanging, the GUI's cached read never
/// waits, and the listing lands in the cache once the backend answers.
#[test]
fn a_stuck_backend_times_devices_list_out_and_the_cached_read_never_blocks() {
    use oats_audio::fake::StuckBackend;
    let r = rig();
    r.engine
        .set("audio.devices_timeout", &json!("300ms"), "test")
        .unwrap();
    let (gate, release) = StuckBackend::gate();
    r.engine.set_audio_factory(Box::new(move |_| {
        Ok(Box::new(StuckBackend {
            gate: Arc::clone(&gate),
        }))
    }));
    let t = std::time::Instant::now();
    assert_eq!(r.engine.devices_cached().unwrap(), None, "first read waits");
    assert!(
        t.elapsed() < Duration::from_millis(250),
        "{:?}",
        t.elapsed()
    );
    let e = r.engine.dispatch(&Action::DevicesList {}).unwrap_err();
    assert_eq!(e.code, "devices_timeout", "{e}");
    assert!(e.message.contains("audio.devices_timeout"), "{e}");
    let took = t.elapsed();
    assert!(
        took >= Duration::from_millis(300) && took < Duration::from_secs(5),
        "{took:?}"
    );
    // Past the timeout the cached read says so, still without waiting.
    let t = std::time::Instant::now();
    assert_eq!(
        r.engine.devices_cached().unwrap_err().code,
        "devices_timeout"
    );
    assert!(t.elapsed() < Duration::from_millis(250));
    // A config reload with a named device is not vetoed by a backend that cannot answer.
    r.engine
        .set("audio.mic.device", &json!("fake.mic2"), "test")
        .unwrap();
    // The backend answers: the worker fills the cache and both reads return the listing.
    release.send(()).unwrap();
    wait_until("the cached listing", || {
        matches!(r.engine.devices_cached(), Ok(Some(_)))
    });
    let v = r.engine.dispatch(&Action::DevicesList {}).unwrap();
    assert_eq!(v["backend"], "stuck");
}

/// One listing serves audio.devices_ttl of devices-list calls; a new backend is asked anew.
#[test]
fn devices_list_is_cached_for_the_ttl() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let r = rig();
    let calls = Arc::new(AtomicUsize::new(0));
    let n = Arc::clone(&calls);
    r.engine.set_audio_factory(Box::new(move |_| {
        n.fetch_add(1, Ordering::SeqCst);
        Ok(Box::new(FakeBackend::default()))
    }));
    for _ in 0..3 {
        r.engine.dispatch(&Action::DevicesList {}).unwrap();
    }
    assert!(r.engine.devices_cached().unwrap().is_some());
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    r.engine
        .set("audio.devices_ttl", &json!("100ms"), "test")
        .unwrap();
    std::thread::sleep(Duration::from_millis(150));
    r.engine.dispatch(&Action::DevicesList {}).unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[test]
fn devices_list_reports_a_loopback_source() {
    let r = rig();
    let v = r.engine.dispatch(&Action::DevicesList {}).unwrap();
    assert_eq!(v["loopback_available"], true);
    let kinds: Vec<&str> = v["devices"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["kind"].as_str().unwrap())
        .collect();
    assert!(
        kinds.contains(&"loopback") && kinds.contains(&"mic"),
        "{kinds:?}"
    );
}

#[test]
fn record_start_stop_session_get_roundtrip() {
    let r = rig();
    let s = start(&r, "s1");
    assert_eq!(s["session"], "s1");
    assert_eq!(s["channels"], 2);
    wait_until("800 ms recorded", || {
        let v = r
            .engine
            .dispatch(&Action::SessionGet {
                session: "s1".into(),
            })
            .unwrap();
        v["duration_ms"].as_u64().unwrap_or(0) >= 800
    });
    let live = r
        .engine
        .dispatch(&Action::SessionGet {
            session: "s1".into(),
        })
        .unwrap();
    assert_eq!(live["state"], "recording");
    assert!(live["duration_ms"].as_u64().unwrap() >= 800, "{live}");
    let done = stop(&r);
    assert!(done["duration_ms"].as_u64().unwrap() >= 800, "{done}");
    let got = r
        .engine
        .dispatch(&Action::SessionGet {
            session: "s1".into(),
        })
        .unwrap();
    assert_eq!(got["state"], "saved");
    assert_eq!(got["duration_ms"], done["duration_ms"]);
    assert_eq!(got["title"], "t");
    assert!(got["ended_at"].is_string());
    let d = wav::read(std::path::Path::new(got["audio_path"].as_str().unwrap())).unwrap();
    assert_eq!(d.channels, 2);
    assert!(d.rms(0) > 0.3 && d.rms(1) > 0.3);
    // Left = mic (880 Hz), right = system (440 Hz): the layout `stt-transcribe` reads.
    assert!((d.tone_hz(0) - 880.0).abs() < 25.0 && (d.tone_hz(1) - 440.0).abs() < 15.0);
    assert!((d.duration_secs() * 1000.0 - done["duration_ms"].as_f64().unwrap()).abs() < 5.0);
}

#[test]
fn state_errors_are_structured() {
    let r = rig();
    for a in [Action::RecordStop {}, Action::RecordPause {}] {
        assert_eq!(r.engine.dispatch(&a).unwrap_err().code, "not_recording");
    }
    start(&r, "s1");
    let e = r
        .engine
        .dispatch(&Action::RecordStart {
            session: None,
            title: None,
        })
        .unwrap_err();
    assert_eq!(e.code, "already_recording");
    stop(&r);
    let e = r
        .engine
        .dispatch(&Action::RecordStart {
            session: Some("s1".into()),
            title: None,
        })
        .unwrap_err();
    assert_eq!(e.code, "exists");
    let e = r
        .engine
        .dispatch(&Action::RecordStart {
            session: Some("../x".into()),
            title: None,
        })
        .unwrap_err();
    assert_eq!(e.code, "invalid_argument");
    let e = r
        .engine
        .dispatch(&Action::SessionGet {
            session: "nope".into(),
        })
        .unwrap_err();
    assert_eq!(e.code, "not_found");
}

#[test]
fn pause_toggles_and_reports() {
    let r = rig();
    start(&r, "p");
    assert_eq!(
        r.engine.dispatch(&Action::RecordPause {}).unwrap()["paused"],
        true
    );
    let g = r
        .engine
        .dispatch(&Action::SessionGet {
            session: "p".into(),
        })
        .unwrap();
    assert_eq!(g["state"], "paused");
    assert_eq!(
        r.engine.dispatch(&Action::RecordPause {}).unwrap()["paused"],
        false
    );
    stop(&r);
}

#[test]
fn every_audio_choice_value_takes_effect() {
    // layout: split -> 2 channels, mix -> 1.
    for (layout, channels) in [("split", 2u64), ("mix", 1)] {
        let r = rig();
        r.engine
            .set("audio.layout", &json!(layout), "test")
            .unwrap();
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
        r.engine
            .set("audio.system.enabled", &json!(sys), "test")
            .unwrap();
        if !sys && !mic {
            // Both off never gets into the config: the audio subsystem vetoes it.
            assert_eq!(
                r.engine
                    .set("audio.mic.enabled", &json!(false), "test")
                    .unwrap_err()
                    .code,
                "invalid_config"
            );
            assert!(!r.dir.path().join("rec/e").exists());
            continue;
        }
        r.engine
            .set("audio.mic.enabled", &json!(mic), "test")
            .unwrap();
        r.engine
            .dispatch(&Action::RecordStart {
                session: Some("e".into()),
                title: None,
            })
            .unwrap();
        std::thread::sleep(Duration::from_millis(500));
        stop(&r);
        let d = wav::read(&r.dir.path().join("rec/e/audio.wav")).unwrap();
        assert_eq!(d.rms(1) > 0.3, sys, "system (right) {sys} {mic}");
        assert_eq!(d.rms(0) > 0.3, mic, "mic (left) {sys} {mic}");
    }
    // explicit device ids: known ones work, unknown ones are device_not_found.
    let r = rig();
    r.engine
        .set("audio.system.device", &json!("fake.monitor"), "test")
        .unwrap();
    r.engine
        .set("audio.mic.device", &json!("fake.mic"), "test")
        .unwrap();
    start(&r, "d");
    stop(&r);
    // An unknown device never gets into the config (the audio subsystem vetoes the reload); the
    // record-start error for a missing default device is covered live in tests/audio_live.rs.
    let e = r
        .engine
        .set("audio.mic.device", &json!("missing"), "test")
        .unwrap_err();
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
        sessions_dir: dir.path().join("sessions"),
        secrets: None,
    })
    .unwrap();
    let (_, values) = keys::choices()
        .into_iter()
        .find(|(p, _)| *p == "audio.backend")
        .unwrap();
    assert_eq!(
        values,
        ["auto", "pulse", "pipewire", "cpal", "screencapturekit"]
    );
    for v in values {
        engine.set("audio.backend", &json!(v), "test").unwrap();
        match engine.dispatch(&Action::DevicesList {}) {
            Ok(d) => assert!(d["backend"].is_string(), "{v}"),
            Err(e) => assert!(
                [
                    "tool_missing",
                    "backend_unavailable",
                    "unsupported_platform",
                    "device_not_found",
                    "capture_failed",
                    "devices_timeout"
                ]
                .contains(&e.code),
                "{v}: {e}"
            ),
        }
    }
    assert_eq!(
        engine
            .set("audio.backend", &json!("alsa"), "test")
            .unwrap_err()
            .code,
        "invalid_value"
    );
}

#[test]
fn recording_events_are_published() {
    let r = rig();
    let rx = r.engine.bus.subscribe(vec![]);
    start(&r, "ev");
    stop(&r);
    let topics: Vec<String> =
        std::iter::from_fn(|| rx.recv_timeout(Duration::from_millis(300)).ok())
            .map(|line| {
                serde_json::from_str::<Value>(&line).unwrap()["event"]
                    .as_str()
                    .unwrap()
                    .to_string()
            })
            .collect();
    for want in ["recording-started", "recording-stopped", "session-saved"] {
        assert!(topics.iter().any(|t| t == want), "{want} in {topics:?}");
    }
}

fn lines(r: &Rig, id: &str, kind: &str) -> Vec<Value> {
    let f = r
        .dir
        .path()
        .join(format!("rec/{id}/transcript.{kind}.jsonl"));
    std::fs::read_to_string(f)
        .unwrap_or_default()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

#[test]
fn recording_writes_live_and_final_transcripts_with_the_stt_speakers() {
    let r = rig();
    let s = start(&r, "t1");
    assert_eq!(s["transcription"]["state"], "on");
    // Live utterances arrive while still recording.
    // The file is appended while we read: only a newline-terminated line is a whole utterance.
    wait_until("a live transcript before stop", || {
        std::fs::read_to_string(r.dir.path().join("rec/t1/transcript.live.jsonl"))
            .is_ok_and(|t| t.contains('\n'))
    });
    let done = stop(&r);
    let t = &done["transcription"];
    assert_eq!(t["state"], "done", "{t}");
    assert_eq!(
        (t["live"].as_str(), t["final"].as_str()),
        (Some("ok"), Some("ok")),
        "{t}"
    );
    for kind in ["live", "final"] {
        let l = lines(&r, "t1", kind);
        let speakers: std::collections::BTreeSet<&str> =
            l.iter().map(|u| u["speaker"].as_str().unwrap()).collect();
        assert_eq!(
            speakers,
            ["remote_1", "you"].into_iter().collect(),
            "{kind}: {l:?}"
        );
        assert!(l.iter().all(|u| u["text"] == "tone"), "{kind}");
    }
    let got = r
        .engine
        .dispatch(&Action::SessionGet {
            session: "t1".into(),
        })
        .unwrap();
    assert!(got["utterance_count"].as_u64().unwrap() >= 2, "{got}");
    assert!(got["ended_at"].is_string());
    let tr = r
        .engine
        .dispatch(&Action::TranscriptGet {
            session: "t1".into(),
        })
        .unwrap();
    assert_eq!(tr["source"], "final");
}

#[test]
fn every_transcribe_value_and_the_stt_switches() {
    // off: audio only, never an ASR.
    let r = rig();
    r.engine
        .set("audio.transcribe", &json!("off"), "test")
        .unwrap();
    assert_eq!(start(&r, "o")["transcription"]["state"], "off");
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(stop(&r)["transcription"]["state"], "off");
    assert!(lines(&r, "o", "live").is_empty() && lines(&r, "o", "final").is_empty());
    // auto with an ASR that cannot be built: audio only, with the reason; on: record-start fails.
    let r = rig();
    r.engine.set_asr_factory(Box::new(|_| {
        Err(oats_core::config::ConfigError::new(
            "model_not_found",
            "no model",
        ))
    }));
    let s = start(&r, "a");
    assert_eq!(s["transcription"]["state"], "unavailable");
    assert_eq!(s["transcription"]["code"], "model_not_found");
    stop(&r);
    r.engine
        .set("audio.transcribe", &json!("on"), "test")
        .unwrap();
    let e = r
        .engine
        .dispatch(&Action::RecordStart {
            session: Some("b".into()),
            title: None,
        })
        .unwrap_err();
    assert_eq!(e.code, "model_not_found");
    assert!(!r.dir.path().join("rec/b").exists());
    // stt.live.enabled / stt.final.enabled: each alone, and neither.
    for (live, fin) in [(true, false), (false, true), (false, false)] {
        let r = rig();
        r.engine
            .set("stt.live.enabled", &json!(live), "test")
            .unwrap();
        r.engine
            .set("stt.final.enabled", &json!(fin), "test")
            .unwrap();
        let id = format!("sw{}{}", live as u8, fin as u8);
        let s = start(&r, &id);
        assert_eq!(
            s["transcription"]["state"],
            if live || fin { "on" } else { "unavailable" },
            "{live} {fin}"
        );
        std::thread::sleep(Duration::from_millis(2500));
        stop(&r);
        assert_eq!(
            !lines(&r, &id, "live").is_empty(),
            live,
            "live {live} {fin}"
        );
        assert_eq!(
            !lines(&r, &id, "final").is_empty(),
            fin,
            "final {live} {fin}"
        );
    }
}

#[test]
fn mix_layout_keeps_live_speakers_but_skips_the_final_pass() {
    let r = rig();
    r.engine.set("audio.layout", &json!("mix"), "test").unwrap();
    start(&r, "m");
    std::thread::sleep(Duration::from_millis(2500));
    let done = stop(&r);
    let t = &done["transcription"];
    assert_eq!(t["live"], "ok", "{t}");
    assert!(t["final"].as_str().unwrap().starts_with("skipped"), "{t}");
    assert!(!lines(&r, "m", "live").is_empty() && lines(&r, "m", "final").is_empty());
}

#[test]
fn bad_audio_settings_veto_a_reload_and_change_nothing() {
    let r = rig();
    // A device the backend does not have.
    let e = r
        .engine
        .set("audio.mic.device", &json!("nope"), "test")
        .unwrap_err();
    assert_eq!(e.code, "device_not_found");
    assert_eq!(
        r.engine
            .dispatch(&Action::ConfigGet {
                key: "audio.mic.device".into()
            })
            .unwrap()["value"],
        ""
    );
    // A device it has is fine.
    r.engine
        .set("audio.mic.device", &json!("fake.mic2"), "test")
        .unwrap();
    // Both sources off.
    r.engine
        .set("audio.system.enabled", &json!(false), "test")
        .unwrap();
    let e = r
        .engine
        .set("audio.mic.enabled", &json!(false), "test")
        .unwrap_err();
    assert_eq!(e.code, "invalid_config");
    // A disabled source's device is not checked.
    r.engine
        .set("audio.system.device", &json!("whatever"), "test")
        .unwrap();
}

#[test]
fn changing_devices_during_a_recording_defers_to_the_next_one() {
    let r = rig();
    let rx = r
        .engine
        .bus
        .subscribe(vec!["recording-config-deferred".into()]);
    start(&r, "d1");
    std::thread::sleep(Duration::from_millis(600));
    // Valid change mid-recording: accepted, announced, and the running recording is untouched.
    r.engine
        .set("audio.mic.device", &json!("fake.mic2"), "test")
        .unwrap();
    let ev: Value = serde_json::from_str(
        &rx.recv_timeout(Duration::from_secs(2))
            .expect("deferred event"),
    )
    .unwrap();
    assert_eq!(ev["data"]["session"], "d1");
    // An invalid change mid-recording is vetoed exactly as when idle, and the recording continues.
    assert_eq!(
        r.engine
            .set("audio.mic.device", &json!("nope"), "test")
            .unwrap_err()
            .code,
        "device_not_found"
    );
    std::thread::sleep(Duration::from_millis(600));
    assert_eq!(
        r.engine
            .dispatch(&Action::SessionGet {
                session: "d1".into()
            })
            .unwrap()["state"],
        "recording"
    );
    stop(&r);
    let d1 = wav::read(&r.dir.path().join("rec/d1/audio.wav")).unwrap();
    assert!(
        (d1.tone_hz(0) - 880.0).abs() < 25.0,
        "d1 mic stayed fake.mic: {}",
        d1.tone_hz(0)
    );
    // The next recording uses the new device.
    start(&r, "d2");
    std::thread::sleep(Duration::from_millis(800));
    stop(&r);
    let d2 = wav::read(&r.dir.path().join("rec/d2/audio.wav")).unwrap();
    assert!(
        (d2.tone_hz(0) - 1320.0).abs() < 40.0,
        "d2 mic is fake.mic2: {}",
        d2.tone_hz(0)
    );
}

/// An ASR that is missing (model_not_found) until `present` is set: the first-run model download.
fn missing_until(present: &Arc<std::sync::atomic::AtomicBool>) -> oats_core::record::AsrFactory {
    let present = Arc::clone(present);
    Box::new(move |_| {
        if present.load(std::sync::atomic::Ordering::SeqCst) {
            Ok(Arc::new(ToneAsr) as Arc<dyn Asr>)
        } else {
            Err(oats_core::config::ConfigError::new(
                "model_not_found",
                "no model",
            ))
        }
    })
}

fn meta(r: &Rig, id: &str) -> Value {
    r.engine
        .dispatch(&Action::SessionGet { session: id.into() })
        .unwrap()
}

#[test]
fn a_recording_made_while_the_model_is_missing_is_transcribed_once_it_arrives() {
    let r = rig();
    // No real download from a test; the flow is the same once the model file appears.
    r.engine
        .set("stt.local.auto_download", &json!(false), "test")
        .unwrap();
    let present = Arc::new(std::sync::atomic::AtomicBool::new(false));
    r.engine.set_asr_factory(missing_until(&present));
    let s = start(&r, "first");
    assert_eq!(s["transcription"]["code"], "model_not_found");
    std::thread::sleep(Duration::from_millis(2500));
    stop(&r);
    let m = meta(&r, "first");
    let ended = m["ended_at"].clone();
    assert!(ended.is_string());
    assert_eq!(m["utterance_count"], 0);
    assert_eq!(m["metadata"]["transcribePending"], true);
    // Still missing: the pass leaves it pending and says why.
    let v = r.engine.transcribe_pending();
    assert_eq!(v["failed"][0]["code"], "model_not_found");
    assert_eq!(meta(&r, "first")["metadata"]["transcribePending"], true);
    // The model arrives: the same session gets its transcript, no second recording needed.
    present.store(true, std::sync::atomic::Ordering::SeqCst);
    // (record-stop also queued a pass in the background; either one may do the work.)
    let v = r.engine.transcribe_pending();
    assert_eq!(v["failed"], json!([]), "{v}");
    let m = meta(&r, "first");
    assert!(m["utterance_count"].as_u64().unwrap() > 0, "{m}");
    assert!(m["metadata"].get("transcribePending").is_none(), "{m}");
    assert_eq!(m["metadata"]["engine"], "whisper.cpp:base.en");
    assert_eq!(m["ended_at"], ended);
    assert!(!lines(&r, "first", "final").is_empty());
    assert!(lines(&r, "first", "live")
        .iter()
        .all(|u| u["text"] == "tone"));
    // Nothing left to do.
    assert_eq!(r.engine.transcribe_pending()["transcribed"], json!([]));
}

#[test]
fn a_pending_recording_still_running_when_the_model_arrives_is_transcribed_at_stop() {
    let r = rig();
    r.engine
        .set("stt.local.auto_download", &json!(false), "test")
        .unwrap();
    let present = Arc::new(std::sync::atomic::AtomicBool::new(false));
    r.engine.set_asr_factory(missing_until(&present));
    start(&r, "long");
    std::thread::sleep(Duration::from_millis(1500));
    present.store(true, std::sync::atomic::Ordering::SeqCst);
    // The download-finished pass skips the session that is still recording.
    assert_eq!(r.engine.transcribe_pending()["transcribed"], json!([]));
    stop(&r);
    wait_until("the stopped session to be transcribed", || {
        meta(&r, "long")["utterance_count"].as_u64().unwrap_or(0) > 0
    });
    assert!(meta(&r, "long")["metadata"]
        .get("transcribePending")
        .is_none());
}

#[test]
fn session_transcribe_runs_on_a_saved_session_and_guards_its_transcript() {
    let r = rig();
    r.engine
        .set("audio.transcribe", &json!("off"), "test")
        .unwrap();
    start(&r, "s");
    let busy = r
        .engine
        .dispatch(&Action::SessionTranscribe {
            session: "s".into(),
            overwrite: None,
        })
        .unwrap_err();
    assert_eq!(busy.code, "recording");
    std::thread::sleep(Duration::from_millis(2500));
    stop(&r);
    // audio.transcribe = off is not a pending session.
    assert!(meta(&r, "s")["metadata"].get("transcribePending").is_none());
    let v = r
        .engine
        .dispatch(&Action::SessionTranscribe {
            session: "s".into(),
            overwrite: None,
        })
        .unwrap();
    assert_eq!(v["session"], "s");
    let n = v["summary"]["utterance_count"].as_u64().unwrap();
    assert!(n > 0, "{v}");
    let again = r
        .engine
        .dispatch(&Action::SessionTranscribe {
            session: "s".into(),
            overwrite: None,
        })
        .unwrap_err();
    assert_eq!(again.code, "exists");
    let v = r
        .engine
        .dispatch(&Action::SessionTranscribe {
            session: "s".into(),
            overwrite: Some(true),
        })
        .unwrap();
    assert_eq!(v["summary"]["utterance_count"].as_u64().unwrap(), n);
    // No audio, unknown session.
    let dir = r.dir.path().join("rec/noaudio");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("session.json"), r#"{"id":"noaudio"}"#).unwrap();
    let e = r
        .engine
        .dispatch(&Action::SessionTranscribe {
            session: "noaudio".into(),
            overwrite: None,
        })
        .unwrap_err();
    assert_eq!(e.code, "no_audio");
    let e = r
        .engine
        .dispatch(&Action::SessionTranscribe {
            session: "nope".into(),
            overwrite: None,
        })
        .unwrap_err();
    assert_eq!(e.code, "not_found");
}
