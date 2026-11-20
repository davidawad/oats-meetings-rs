//! Idle means idle (dot-4uzgol.106): with the app launched and nothing recording, no model is
//! loaded however many `stt.local.unload_after` periods pass, whatever the control API is asked
//! (status, model list, reload, devices, pending transcriptions); a recording or a transcription
//! loads it exactly once, and the diarizer's model and status are released with it. The model is a
//! fake loader that counts its loads and joins the idle reaper the way the real caches do.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use oats_audio::fake::FakeBackend;
use oats_core::action::Action;
use oats_core::control::engine::{Engine, EngineOpts};
use oats_core::stt::{idle, Asr, Segment};
use serde_json::{json, Value};

/// The idle timeout of every test here (the reaper is process-wide, so they share it).
const AFTER: Duration = Duration::from_millis(300);
/// Idle periods watched for a stray load.
const PERIODS: u32 = 5;

/// One "model" in memory.
struct Model;

/// A model cache like the recogniser's: loaded on a miss, counted, emptied by the reaper.
#[derive(Default)]
struct FakeCache {
    slot: Mutex<Option<Arc<Model>>>,
    loads: AtomicUsize,
}

impl idle::Cache for FakeCache {
    fn in_use(&self) -> bool {
        self.slot
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|m| Arc::strong_count(m) > 1)
    }
    fn unload(&self) {
        *self.slot.lock().unwrap() = None;
    }
}

impl FakeCache {
    fn get(&self) -> Arc<Model> {
        let mut g = self.slot.lock().unwrap();
        if let Some(m) = g.as_ref() {
            return Arc::clone(m);
        }
        self.loads.fetch_add(1, Ordering::SeqCst);
        idle::loaded("fake model");
        let m = Arc::new(Model);
        *g = Some(Arc::clone(&m));
        m
    }
    fn loads(&self) -> usize {
        self.loads.load(Ordering::SeqCst)
    }
    fn loaded(&self) -> bool {
        self.slot.lock().unwrap().is_some()
    }
}

/// Says "tone" for any chunk with sound in it, holding the cached model while it exists.
struct ToneAsr {
    _model: Arc<Model>,
}

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

/// The tests share the reaper and its counters: one at a time.
static SERIAL: Mutex<()> = Mutex::new(());

struct Rig {
    _dir: tempfile::TempDir,
    engine: Arc<Engine>,
    cache: Arc<FakeCache>,
}

fn rig(diarize: &str) -> Rig {
    let dir = tempfile::tempdir().unwrap();
    let cfg = dir.path().join("config.toml");
    std::fs::write(
        &cfg,
        format!(
            "[stt.local]\nunload_after = \"{}ms\"\nauto_download = false\nmodels_dir = '{}'\n\n[stt.live]\nchunk = \"1s\"\n\n[stt.diarization]\n{diarize}\n\n[sessions]\nroot = '{}'\n",
            AFTER.as_millis(),
            dir.path().join("models").display(),
            dir.path().join("rec").display(),
        ),
    )
    .unwrap();
    let engine = Engine::new(EngineOpts {
        user_config: cfg,
        pack: None,
        os: "linux".into(),
        arch: "x86_64".into(),
        machine: "default".into(),
        sessions_dir: dir.path().join("sessions"),
        secrets: None,
    })
    .unwrap();
    engine.set_audio_factory(Box::new(|_| Ok(Box::new(FakeBackend::default()))));
    let cache = Arc::new(FakeCache::default());
    idle::watch(Arc::clone(&cache) as Arc<dyn idle::Cache>);
    let c = Arc::clone(&cache);
    // As `stt::make_asr` does: the model from its cache, and the idle clock started.
    engine.set_asr_factory(Box::new(move |s| {
        idle::touch(s.unload_after);
        Ok(Arc::new(ToneAsr { _model: c.get() }) as Arc<dyn Asr>)
    }));
    Rig {
        _dir: dir,
        engine,
        cache,
    }
}

fn status(r: &Rig) -> Value {
    r.engine.dispatch(&Action::SttStatus {}).unwrap()
}

/// `PERIODS` idle timeouts of an app nobody records with, asked everything a GUI, `ctl` or a
/// monitoring script asks while idle.
fn idle_periods(r: &Rig) {
    for _ in 0..PERIODS {
        let t = Instant::now();
        while t.elapsed() < AFTER {
            r.engine.dispatch(&Action::Ping {}).unwrap();
            status(r);
            r.engine.dispatch(&Action::SttModelList {}).unwrap();
            r.engine.dispatch(&Action::DevicesList {}).unwrap();
            r.engine.dispatch(&Action::ConfigReload {}).unwrap();
            r.engine.transcribe_pending();
            std::thread::sleep(AFTER / 6);
        }
    }
}

fn wait_until(what: &str, mut cond: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while !cond() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Record a few seconds of the fake backend's tones: one transcription (live and final pass).
fn record(r: &Rig, id: &str) -> Value {
    let v = r
        .engine
        .dispatch(&Action::RecordStart {
            session: Some(id.into()),
            title: Some("t".into()),
        })
        .unwrap();
    assert_eq!(v["transcription"]["state"], "on", "{v}");
    std::thread::sleep(Duration::from_secs(2));
    r.engine.dispatch(&Action::RecordStop { by: None }).unwrap()
}

#[test]
fn an_idle_app_loads_no_model_and_each_transcription_loads_it_once() {
    let _one = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let r = rig("enabled = false");
    let loads0 = status(&r)["models"]["loads"].as_u64().unwrap();
    // App launch, then idle: nothing loads.
    r.engine.launched();
    idle_periods(&r);
    assert_eq!(r.cache.loads(), 0);
    assert!(!r.cache.loaded());

    // A recording loads the model once for its live and final passes.
    let unloads = status(&r)["models"]["unloads"].as_u64().unwrap();
    let done = record(&r, "s1");
    assert_eq!(done["transcription"]["final"], "running", "{done}");
    wait_until("the final pass of s1", || {
        r.engine.bus.events_since(0, 0).iter().any(|e| {
            e["event"] == "transcribe-progress"
                && e["data"]["session"] == "s1"
                && e["data"]["state"] == "done"
        })
    });
    assert_eq!(r.cache.loads(), 1);
    assert_eq!(status(&r)["models"]["loads"].as_u64().unwrap(), loads0 + 1);
    // Released once unload_after passes, and idle again loads nothing.
    wait_until("the idle unload", || !r.cache.loaded());
    assert!(status(&r)["models"]["unloads"].as_u64().unwrap() > unloads);
    idle_periods(&r);
    assert_eq!(r.cache.loads(), 1);
    assert!(!r.cache.loaded());

    // A transcription of a saved session: one more load, released after it too.
    let v = r
        .engine
        .dispatch(&Action::SessionTranscribe {
            session: "s1".into(),
            overwrite: Some(true),
        })
        .unwrap();
    assert!(v.is_object(), "{v}");
    assert_eq!(r.cache.loads(), 2);
    wait_until("the idle unload", || !r.cache.loaded());
    idle_periods(&r);
    assert_eq!(r.cache.loads(), 2);
}

#[test]
fn the_diarizer_is_kept_while_it_runs_and_released_with_the_model() {
    let _one = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let r = rig("enabled = true\nalgorithm = \"spectral\"");
    r.engine.launched();
    let active = |r: &Rig| status(r)["diarization"]["active"].clone();
    assert_eq!(active(&r), Value::Null);

    // A recording longer than several idle timeouts: its diarizer and model stay loaded.
    r.engine
        .dispatch(&Action::RecordStart {
            session: Some("d1".into()),
            title: Some("t".into()),
        })
        .unwrap();
    wait_until("the live diarizer", || active(&r) == json!("spectral"));
    std::thread::sleep(AFTER * 4);
    assert!(r.cache.loaded());
    assert_eq!(active(&r), json!("spectral"));
    r.engine.dispatch(&Action::RecordStop { by: None }).unwrap();
    assert_eq!(r.cache.loads(), 1);

    // Then both go with unload_after, and stay gone while idle.
    wait_until("the diarizer unload", || {
        !r.cache.loaded() && active(&r) == Value::Null
    });
    idle_periods(&r);
    assert_eq!(active(&r), Value::Null);
    assert_eq!(r.cache.loads(), 1);
}
