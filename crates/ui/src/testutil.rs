//! Test helpers: a real `Engine` over a temp dir with the given user-config keys set.
use std::sync::Arc;

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
    (engine, dir)
}
