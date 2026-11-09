//! Recordings that stop and start by themselves (dot-4uzgol.110), end to end through the engine
//! with a fake capture backend whose loudness the test turns up and down: a recording fed speech
//! then silence auto-stops at `record.auto_stop.after` with the trailing silence cut from its
//! audio and the auto-stopped event published; `record-keep` cancels a pending stop; while idle,
//! sustained speaker output starts a recording and short blips do not.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::Arc;
use std::time::{Duration, Instant};

use oats_audio::fake::FakeBackend;
use oats_audio::{wav, Capture, CaptureBackend, DeviceInfo, DeviceKind, OnSamples};
use oats_core::action::Action;
use oats_core::control::engine::{Engine, EngineOpts};
use oats_core::stt::{Asr, Segment};
use serde_json::{json, Value};

/// The fake tones scaled by a gain per source the test sets: loopback (speaker) and mic.
#[derive(Clone, Default)]
struct Gains(Arc<[AtomicU32; 2]>);

impl Gains {
    fn set(&self, speaker: f32, mic: f32) {
        self.0[0].store(speaker.to_bits(), Ordering::SeqCst);
        self.0[1].store(mic.to_bits(), Ordering::SeqCst);
    }
}

struct Gated(Gains);

impl CaptureBackend for Gated {
    fn name(&self) -> &'static str {
        "gated"
    }

    fn list_devices(&self) -> oats_audio::Result<Vec<DeviceInfo>> {
        FakeBackend::default().list_devices()
    }

    fn open(
        &self,
        kind: DeviceKind,
        device: &str,
        mut on: OnSamples,
    ) -> oats_audio::Result<Box<dyn Capture>> {
        let g = Arc::clone(&self.0 .0);
        let i = usize::from(kind == DeviceKind::Mic);
        FakeBackend::default().open(
            kind,
            device,
            Box::new(move |s: &[f32]| {
                let gain = f32::from_bits(g[i].load(Ordering::SeqCst));
                let scaled: Vec<f32> = s.iter().map(|x| x * gain).collect();
                on(&scaled)
            }),
        )
    }
}

/// Says "tone" for any chunk with sound in it.
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
    _dir: tempfile::TempDir,
    engine: Arc<Engine>,
    gains: Gains,
}

fn rig(settings: &[(&str, Value)]) -> Rig {
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
    let set = |k: &str, v: Value| engine.set(k, &v, "test").unwrap();
    set(
        "sessions.root",
        json!(dir.path().join("rec").to_str().unwrap()),
    );
    set("stt.live.chunk", json!("1s"));
    set("record.auto_check", json!("50ms"));
    // The echo canceller is slow in a debug build: the writer would fall seconds behind the
    // capture, and what the test mutes now would reach the recording late.
    set("audio.echo_cancel", json!(false));
    for (k, v) in settings {
        set(k, v.clone());
    }
    let gains = Gains::default();
    gains.set(0.5, 0.5);
    let g = gains.clone();
    engine.set_audio_factory(Box::new(move |_| Ok(Box::new(Gated(g.clone())))));
    engine.set_asr_factory(Box::new(|_| Ok(Arc::new(ToneAsr))));
    Rig {
        _dir: dir,
        engine,
        gains,
    }
}

fn wait_until(what: &str, secs: u64, mut cond: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(secs);
    while !cond() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// The next event on `topic` within `secs`, skipping others (they are kept in `seen`).
fn next(rx: &Receiver<String>, topic: &str, secs: u64, seen: &mut Vec<Value>) -> Option<Value> {
    let deadline = Instant::now() + Duration::from_secs(secs);
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        let line = rx.recv_timeout(left).ok()?;
        let v: Value = serde_json::from_str(&line).unwrap();
        seen.push(v.clone());
        if v["event"] == topic {
            return Some(v["data"].clone());
        }
    }
}

fn duration_ms(r: &Rig) -> u64 {
    r.engine.recording().flatten().map_or(0, |(_, _, ms)| ms)
}

fn idle(r: &Rig) -> bool {
    r.engine.recording() == Some(None)
}

#[test]
fn speech_then_silence_auto_stops_at_the_configured_time_with_the_silence_cut() {
    let r = rig(&[
        ("record.auto_stop.after", json!("2s")),
        ("record.auto_stop.warn_before", json!("1s")),
        ("record.auto_stop.tail", json!("300ms")),
    ]);
    let rx = r.engine.bus.subscribe(vec![]);
    let v = r
        .engine
        .dispatch(&Action::RecordStart {
            session: Some("auto".into()),
            title: Some("t".into()),
        })
        .unwrap();
    let wav_path = std::path::PathBuf::from(v["path"].as_str().unwrap());
    // Speech on both channels, then silence on both.
    wait_until("1.5 s of speech", 30, || duration_ms(&r) >= 1_500);
    r.gains.set(0.0, 0.0);
    let muted_at = duration_ms(&r);
    let mut seen = vec![];
    let pending = next(&rx, "auto-stop-pending", 30, &mut seen).expect("a warning first");
    assert_eq!(pending["session"], "auto");
    assert!(
        pending["stop_in_ms"].as_u64().unwrap() <= 1_000,
        "{pending}"
    );
    assert!(pending["message"]
        .as_str()
        .unwrap()
        .contains("recording stops in"));
    let stopped = next(&rx, "auto-stopped", 30, &mut seen).expect("auto-stopped");
    assert!(idle(&r), "the recording is over");
    assert_eq!(stopped["session"], "auto");
    assert_eq!(
        stopped["message"], "Recording stopped after 2s of silence",
        "{stopped}"
    );
    // At the configured time: 2 s of silence, give or take the check interval.
    let silence = stopped["silence_ms"].as_u64().unwrap();
    assert!((2_000..2_500).contains(&silence), "{stopped}");
    let rec_stop = seen
        .iter()
        .find(|e| e["event"] == "recording-stopped")
        .expect("recording-stopped")["data"]
        .clone();
    assert_eq!(rec_stop["by"], "auto-stop");
    assert_eq!(rec_stop["forced"], false);

    // The trailing silence is cut: the audio ends the tail after the last sound.
    let cut = stopped["cut_ms"].as_u64().unwrap();
    let data = wav::read(&wav_path).unwrap();
    let kept_ms = (data.duration_secs() * 1000.0).round() as u64;
    assert_eq!(kept_ms, cut, "{stopped}");
    assert_eq!(stopped["duration_ms"], json!(kept_ms));
    // The last sound is where the speech stopped (the echo canceller's residue may run on a
    // little), plus the 300 ms tail.
    assert!(
        cut >= muted_at + 200 && cut <= muted_at + 900,
        "cut {cut} ms, muted at {muted_at} ms"
    );
    let trimmed = stopped["trimmed_ms"].as_u64().unwrap();
    assert!(trimmed >= 1_500, "{stopped}");
    let last_loud = (0..2)
        .filter_map(|c| data.samples[c].iter().rposition(|x| x.abs() > 0.01))
        .max()
        .unwrap() as f64
        / f64::from(data.rate);
    let end = data.duration_secs();
    assert!(
        end - last_loud > 0.2 && end - last_loud < 0.45,
        "last sound at {last_loud}s of {end}s"
    );

    // The session says where it was cut; no utterance starts after the cut.
    let got = r
        .engine
        .dispatch(&Action::SessionGet {
            session: "auto".into(),
        })
        .unwrap();
    let meta = &got["metadata"];
    let auto = meta
        .get("autoStop")
        .or_else(|| meta["x_oats"].get("autoStop"))
        .unwrap_or_else(|| panic!("autoStop in {meta}"));
    assert_eq!(auto["cutAtMs"], json!(cut));
    assert_eq!(got["duration_ms"], json!(kept_ms));
    let t = r
        .engine
        .dispatch(&Action::TranscriptGet {
            session: "auto".into(),
        })
        .unwrap();
    let us = t["utterances"].as_array().unwrap();
    assert!(!us.is_empty(), "the speech is transcribed: {t}");
    for u in us {
        let start = u["x_oats"]["startSeconds"].as_f64().unwrap();
        assert!(start * 1000.0 < cut as f64, "{u}");
    }
}

#[test]
fn record_keep_cancels_a_pending_auto_stop_and_sound_withdraws_one() {
    let r = rig(&[
        ("record.auto_stop.after", json!("2s")),
        ("record.auto_stop.warn_before", json!("1500ms")),
        ("audio.transcribe", json!("off")),
    ]);
    let rx = r.engine.bus.subscribe(vec![]);
    r.engine
        .dispatch(&Action::RecordStart {
            session: Some("keep".into()),
            title: None,
        })
        .unwrap();
    wait_until("speech", 30, || duration_ms(&r) >= 500);
    r.gains.set(0.0, 0.0);
    let mut seen = vec![];
    next(&rx, "auto-stop-pending", 30, &mut seen).expect("pending");
    let st = r.engine.dispatch(&Action::RecordAutoStatus {}).unwrap();
    assert_eq!(st["recording"]["pending"], true, "{st}");
    assert!(
        st["recording"]["silence_ms"].as_u64().unwrap() >= 500,
        "{st}"
    );
    let v = r
        .engine
        .dispatch(&Action::RecordKeep {
            by: Some("ctl".into()),
        })
        .unwrap();
    assert_eq!(v, json!({"session": "keep", "was_pending": true}));
    let c = next(&rx, "auto-stop-cancelled", 5, &mut seen).expect("cancelled");
    assert_eq!(c["by"], "ctl");
    // Kept: still recording well past where it would have stopped.
    let kept_at = duration_ms(&r);
    wait_until("more silence", 30, || duration_ms(&r) >= kept_at + 1_000);
    assert!(!idle(&r));
    // The next warning is withdrawn by sound coming back.
    next(&rx, "auto-stop-pending", 30, &mut seen).expect("pending again");
    r.gains.set(0.0, 0.5);
    let c = next(&rx, "auto-stop-cancelled", 30, &mut seen).expect("withdrawn");
    assert_eq!(c["by"], "sound");
    assert!(!idle(&r));
    // A pause stops the clock: a long pause does not stop the recording.
    r.gains.set(0.0, 0.0);
    r.engine.dispatch(&Action::RecordPause {}).unwrap();
    std::thread::sleep(Duration::from_millis(2_500));
    assert!(!idle(&r), "paused time is not silence");
    r.engine.dispatch(&Action::RecordPause {}).unwrap();
    let v = next(&rx, "auto-stopped", 30, &mut seen).expect("stops in the end");
    assert_eq!(v["session"], "keep");
    // Nothing is idle-only: keep without a recording says so.
    let e = r
        .engine
        .dispatch(&Action::RecordKeep { by: None })
        .unwrap_err();
    assert_eq!(e.code, "not_recording");
}

#[test]
fn auto_stop_off_never_stops() {
    let r = rig(&[
        ("record.auto_stop.enabled", json!(false)),
        ("record.auto_stop.after", json!("1s")),
        ("audio.transcribe", json!("off")),
    ]);
    r.gains.set(0.0, 0.0);
    r.engine
        .dispatch(&Action::RecordStart {
            session: Some("off".into()),
            title: None,
        })
        .unwrap();
    wait_until("silence", 30, || duration_ms(&r) >= 2_500);
    assert!(!idle(&r));
    let v = r.engine.dispatch(&Action::RecordStop { by: None }).unwrap();
    assert_eq!(v["trimmed_ms"], Value::Null);
    assert!(v["duration_ms"].as_u64().unwrap() >= 2_500);
}

#[test]
fn sustained_speaker_output_starts_a_recording_and_blips_do_not() {
    let r = rig(&[
        ("record.auto_start.enabled", json!(true)),
        ("record.auto_start.after", json!("1s")),
        ("record.auto_start.gap", json!("200ms")),
        ("record.auto_start.cooldown", json!("0s")),
        ("record.auto_stop.enabled", json!(false)),
        ("audio.transcribe", json!("off")),
        ("ui.monitor", json!("off")),
    ]);
    r.gains.set(0.0, 0.0);
    let rx = r.engine.bus.subscribe(vec![]);
    // App launch: auto-start listens through the monitor even with ui.monitor = off.
    r.engine.enable_monitor();
    wait_until("the monitor", 30, || {
        r.engine.monitor_state().is_some_and(|m| m.open)
    });
    // Notification-like blips on the speaker: 300 ms on, 500 ms off.
    for _ in 0..5 {
        r.gains.set(0.5, 0.0);
        std::thread::sleep(Duration::from_millis(300));
        r.gains.set(0.0, 0.0);
        std::thread::sleep(Duration::from_millis(500));
    }
    // The mic does not count when sources = speaker.
    r.gains.set(0.0, 0.5);
    std::thread::sleep(Duration::from_millis(1_500));
    assert!(idle(&r), "short blips and the mic start nothing");
    let st = r.engine.dispatch(&Action::RecordAutoStatus {}).unwrap();
    assert_eq!(st["listening"], true, "{st}");
    assert!(st["activity_ms"]["mic"].as_u64().unwrap() >= 1_000, "{st}");
    assert_eq!(st["due"], Value::Null);
    let mut seen = vec![];
    // Sustained speaker output starts a recording.
    r.gains.set(0.5, 0.0);
    let started = next(&rx, "auto-started", 30, &mut seen).expect("auto-started");
    assert_eq!(started["source"], "speaker");
    assert_eq!(
        started["message"],
        "Recording started: speaker audio for 1s"
    );
    assert!(
        !seen
            .iter()
            .any(|e| e["event"] == "auto-started" && e["data"] != started),
        "one start"
    );
    let rs = seen
        .iter()
        .find(|e| e["event"] == "recording-started")
        .expect("recording-started first")["data"]
        .clone();
    assert_eq!(rs["session"], started["session"]);
    assert_eq!(rs["auto_start"]["source"], "speaker");
    assert!(!idle(&r));
    // The cooldown: a recording stopped while the sound goes on is not restarted at once.
    r.engine
        .set("record.auto_start.cooldown", &json!("1h"), "test")
        .unwrap();
    r.engine.dispatch(&Action::RecordStop { by: None }).unwrap();
    std::thread::sleep(Duration::from_millis(2_000));
    assert!(idle(&r), "within the cooldown");
    // Turned off, sustained sound starts nothing either.
    r.engine
        .set("record.auto_start.enabled", &json!(false), "test")
        .unwrap();
    r.engine
        .set("record.auto_start.cooldown", &json!("0s"), "test")
        .unwrap();
    std::thread::sleep(Duration::from_millis(2_000));
    assert!(idle(&r), "off");
    r.engine.request_shutdown();
}

#[test]
fn trimming_a_transcript_drops_the_utterances_from_the_cut_on() {
    let d = tempfile::tempdir().unwrap();
    let store = oats_core::sessions::Store::new(d.path().to_path_buf());
    store.create("s", "2026-11-09T10:00:00Z", "", "").unwrap();
    let u = |start: f64| json!({"speaker": "you", "text": "x", "x_oats": {"startSeconds": start}});
    for line in [
        u(1.0),
        u(9.99),
        u(10.0),
        u(42.0),
        json!({"speaker": "you", "text": "untimed"}),
    ] {
        store.append_utterance("s", "live", &line).unwrap();
    }
    assert_eq!(store.trim_transcript("s", "live", 10.0).unwrap(), 2);
    let t = store.transcript("s").unwrap();
    let texts: Vec<_> = t["utterances"]
        .as_array()
        .unwrap()
        .iter()
        .map(|u| u["x_oats"]["startSeconds"].clone())
        .collect();
    assert_eq!(texts, vec![json!(1.0), json!(9.99), Value::Null]);
    assert_eq!(
        store.trim_transcript("s", "final", 1.0).unwrap(),
        0,
        "no file"
    );
}
