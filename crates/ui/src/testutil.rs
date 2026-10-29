//! Test helpers: a real `Engine` over a temp dir with the given user-config keys set, on a fake
//! audio backend: no lib test touches the OS audio stack (CoreAudio enumeration can hang,
//! dot-4uzgol.57).
use std::sync::mpsc::Sender;
use std::sync::Arc;

use oats_audio::fake::{FakeBackend, StuckBackend};

use oats_core::config::resolve::nest;
use oats_core::control::engine::{Engine, EngineOpts};
use serde_json::Value;

pub fn engine(pairs: &[(&str, Value)]) -> (Arc<Engine>, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let cfg = dir.path().join("config.toml");
    let flat = pairs
        .iter()
        .map(|(k, v)| ((*k).to_string(), v.clone()))
        .collect();
    let v: toml::Value = serde_json::from_value(nest(&flat)).expect("toml value");
    std::fs::write(&cfg, toml::to_string(&v).expect("toml")).expect("write config");
    let engine = Engine::new(EngineOpts {
        user_config: cfg,
        pack: None,
        os: "linux".into(),
        arch: "x86_64".into(),
        machine: "default".into(),
        sessions_dir: dir.path().join("sessions"),
        secrets: None,
    })
    .expect("engine");
    engine.set_audio_factory(fake_audio());
    (engine, dir)
}

/// The deterministic in-memory backend (`oats_audio::fake::FakeBackend`).
pub fn fake_audio() -> oats_core::record::Factory {
    Box::new(|_| Ok(Box::new(FakeBackend::default())))
}

/// A backend whose device listing hangs until the returned sender sends (or is dropped).
pub fn stuck_audio() -> (oats_core::record::Factory, Sender<()>) {
    let (gate, release) = StuckBackend::gate();
    let f: oats_core::record::Factory = Box::new(move |_| {
        Ok(Box::new(StuckBackend {
            gate: Arc::clone(&gate),
        }))
    });
    (f, release)
}
