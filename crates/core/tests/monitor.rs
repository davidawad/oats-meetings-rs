//! The input monitor (dot-4uzgol.92): `ui.monitor` meters the mic and system audio while not
//! recording, from a fake backend that counts its open devices and input callbacks. Idle
//! monitoring writes nothing, hiding the window (window_visible) or `off` releases every device
//! with no input callback after, and a recording takes the devices over and gives them back.

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use oats_audio::fake::FakeBackend;
use oats_audio::{Capture, CaptureBackend, DeviceInfo, DeviceKind, OnSamples};
use oats_core::action::Action;
use oats_core::control::engine::{Engine, EngineOpts};
use serde_json::json;

/// Counts what the fake devices do: opens, releases and input callbacks.
#[derive(Default)]
struct Counts {
    opened: AtomicUsize,
    released: AtomicUsize,
    callbacks: AtomicU64,
}

impl Counts {
    fn open_now(&self) -> usize {
        self.opened.load(Ordering::SeqCst) - self.released.load(Ordering::SeqCst)
    }
}

struct Counting(Arc<Counts>);

struct CountingCapture {
    inner: Box<dyn Capture>,
    counts: Arc<Counts>,
    done: bool,
}

impl Capture for CountingCapture {
    fn stop(&mut self) {
        self.inner.stop();
        if !std::mem::replace(&mut self.done, true) {
            self.counts.released.fetch_add(1, Ordering::SeqCst);
        }
    }
}

impl Drop for CountingCapture {
    fn drop(&mut self) {
        self.stop();
    }
}

impl CaptureBackend for Counting {
    fn name(&self) -> &'static str {
        "counting"
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
        let c = Arc::clone(&self.0);
        let inner = FakeBackend::default().open(
            kind,
            device,
            Box::new(move |s: &[f32]| {
                c.callbacks.fetch_add(1, Ordering::SeqCst);
                on(s)
            }),
        )?;
        self.0.opened.fetch_add(1, Ordering::SeqCst);
        Ok(Box::new(CountingCapture {
            inner,
            counts: Arc::clone(&self.0),
            done: false,
        }))
    }
}

struct Rig {
    dir: tempfile::TempDir,
    engine: Arc<Engine>,
    counts: Arc<Counts>,
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
    engine
        .set(
            "sessions.root",
            &json!(dir.path().join("rec").to_str().unwrap()),
            "test",
        )
        .unwrap();
    engine
        .set("audio.transcribe", &json!("off"), "test")
        .unwrap();
    let counts = Arc::new(Counts::default());
    let c = Arc::clone(&counts);
    engine.set_audio_factory(Box::new(move |_| Ok(Box::new(Counting(Arc::clone(&c))))));
    Rig {
        dir,
        engine,
        counts,
    }
}

fn wait_until(what: &str, mut cond: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while !cond() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn monitor_open(r: &Rig) -> bool {
    r.engine.monitor_state().is_some_and(|m| m.open)
}

fn meters_move(r: &Rig) {
    wait_until("idle meters", || {
        r.engine
            .monitor_state()
            .is_some_and(|m| m.open && m.levels.0 > 0.3 && m.levels.1 > 0.3)
    });
}

/// Every device released, and no input callback over a stretch of real time after.
fn released(r: &Rig) {
    wait_until("the devices released", || {
        !monitor_open(r) && r.counts.open_now() == 0
    });
    let before = r.counts.callbacks.load(Ordering::SeqCst);
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(
        r.counts.callbacks.load(Ordering::SeqCst),
        before,
        "input callbacks after release"
    );
}

fn show(r: &Rig, on: bool) {
    let a = if on {
        Action::WindowShow {}
    } else {
        Action::WindowHide {}
    };
    r.engine.dispatch(&a).unwrap();
}

#[test]
fn ui_monitor_is_a_settings_enum_defaulting_to_window_visible() {
    let r = rig();
    assert_eq!(
        r.engine.resolved().flat["ui.monitor"],
        json!("window_visible")
    );
    for v in ["always", "off", "window_visible"] {
        r.engine.set("ui.monitor", &json!(v), "test").unwrap();
    }
    assert!(r
        .engine
        .set("ui.monitor", &json!("sometimes"), "test")
        .is_err());
    let k = oats_core::config::keys::KEYS
        .iter()
        .find(|k| k.path == "ui.monitor")
        .unwrap();
    assert!(k.ui, "editable in Settings");
    assert!(
        k.doc.contains("orange mic indicator"),
        "the caveat: {}",
        k.doc
    );
}

#[test]
fn a_shown_window_meters_idle_audio_and_nothing_reaches_a_recording() {
    let r = rig();
    r.engine.enable_monitor();
    assert!(!monitor_open(&r), "the window is hidden");
    show(&r, true);
    meters_move(&r);
    assert_eq!(r.counts.open_now(), 2, "mic and system audio");
    let blocks = r.engine.monitor_state().unwrap().blocks;
    wait_until("more blocks", || {
        let b = r.engine.monitor_state().unwrap().blocks;
        b.0 > blocks.0 && b.1 > blocks.1
    });
    // Monitoring only: no recording, no session, no audio file.
    assert_eq!(r.engine.recording_levels(), None);
    assert_eq!(r.engine.recording(), Some(None));
    let rec = r.dir.path().join("rec");
    let files = walk(&rec);
    assert!(files.is_empty(), "idle monitoring wrote {files:?}");
}

fn walk(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return vec![];
    };
    rd.flatten()
        .flat_map(|e| {
            let p = e.path();
            if p.is_dir() {
                walk(&p)
            } else {
                vec![p]
            }
        })
        .collect()
}

#[test]
fn hiding_the_window_releases_the_mic_and_showing_it_reopens() {
    let r = rig();
    r.engine.enable_monitor();
    show(&r, true);
    meters_move(&r);
    show(&r, false);
    released(&r);
    show(&r, true);
    meters_move(&r);
    r.engine.dispatch(&Action::WindowToggle {}).unwrap();
    released(&r);
}

#[test]
fn off_never_opens_and_always_monitors_a_hidden_window() {
    let r = rig();
    r.engine.enable_monitor();
    r.engine.set("ui.monitor", &json!("off"), "test").unwrap();
    show(&r, true);
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(
        r.counts.opened.load(Ordering::SeqCst),
        0,
        "off opened a device"
    );
    assert!(!monitor_open(&r));
    show(&r, false);
    r.engine
        .set("ui.monitor", &json!("always"), "test")
        .unwrap();
    meters_move(&r);
    // Turned off while open: released at once.
    r.engine.set("ui.monitor", &json!("off"), "test").unwrap();
    released(&r);
}

#[test]
fn an_engine_that_was_not_launched_never_monitors() {
    let r = rig();
    r.engine
        .set("ui.monitor", &json!("always"), "test")
        .unwrap();
    show(&r, true);
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(r.counts.opened.load(Ordering::SeqCst), 0);
}

#[test]
fn a_recording_takes_the_devices_over_and_the_monitor_comes_back_after_stop() {
    let r = rig();
    r.engine.enable_monitor();
    show(&r, true);
    meters_move(&r);
    r.engine
        .dispatch(&Action::RecordStart {
            session: Some("m1".into()),
            title: None,
        })
        .unwrap();
    // Only the recording's two captures are open; its meters show the recorded streams.
    assert!(!monitor_open(&r));
    assert_eq!(r.counts.open_now(), 2);
    wait_until("recording meters", || {
        r.engine
            .recording_levels()
            .is_some_and(|(m, s)| m > 0.3 && s > 0.3)
    });
    // A window shown during the recording does not reopen the monitor beside it.
    show(&r, true);
    std::thread::sleep(Duration::from_millis(100));
    assert!(!monitor_open(&r) && r.counts.open_now() == 2);
    r.engine.dispatch(&Action::RecordStop { by: None }).unwrap();
    meters_move(&r);
    assert_eq!(r.counts.open_now(), 2, "monitor only");
}

#[test]
fn a_failed_record_start_gives_the_devices_back_to_the_monitor() {
    let r = rig();
    r.engine.enable_monitor();
    show(&r, true);
    meters_move(&r);
    let e = r
        .engine
        .dispatch(&Action::RecordStart {
            session: Some("../bad".into()),
            title: None,
        })
        .unwrap_err();
    assert_eq!(e.code, "invalid_argument");
    meters_move(&r);
    // A start that fails opening its devices also gives them back.
    r.engine.set_audio_factory(Box::new(|_| {
        Err(oats_audio::AudioError::new("tool_missing", "no backend"))
    }));
    assert!(r
        .engine
        .dispatch(&Action::RecordStart {
            session: Some("m2".into()),
            title: None,
        })
        .is_err());
    wait_until("the open error", || {
        r.engine
            .monitor_state()
            .is_some_and(|m| !m.open && m.error.as_deref() == Some("no backend"))
    });
}

#[test]
fn a_failed_open_waits_ui_monitor_retry_and_a_new_backend_retries_at_once() {
    let r = rig();
    let tries = Arc::new(AtomicUsize::new(0));
    let t = Arc::clone(&tries);
    r.engine.set_audio_factory(Box::new(move |_| {
        t.fetch_add(1, Ordering::SeqCst);
        Err(oats_audio::AudioError::new("tool_missing", "no backend"))
    }));
    r.engine.enable_monitor();
    show(&r, true);
    wait_until("the first try", || tries.load(Ordering::SeqCst) == 1);
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(
        tries.load(Ordering::SeqCst),
        1,
        "retried before ui.monitor_retry"
    );
    let c = Arc::clone(&r.counts);
    r.engine
        .set_audio_factory(Box::new(move |_| Ok(Box::new(Counting(Arc::clone(&c))))));
    meters_move(&r);
}
