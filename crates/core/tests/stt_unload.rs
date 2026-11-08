#![cfg(all(feature = "parakeet", target_os = "linux"))]
//! Idle model release (dot-4uzgol.102): after a transcription through the engine (live + final
//! pass, one shared Parakeet recogniser and speaker model), `stt.local.unload_after` of quiet
//! drops the models and the process's anonymous memory returns near where it started. Its own
//! test binary so no other test's models or threads skew this process's RSS. The model comes from
//! `ci/fetch-models.sh` (OATS_WHISPER_MODELS_DIR); it is never committed.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use oats_core::action::Action;
use oats_core::control::engine::{Engine, EngineOpts};

/// RssAnon of this process, in bytes.
fn rss_anon() -> u64 {
    let s = std::fs::read_to_string("/proc/self/status").unwrap();
    let kb: u64 = s
        .lines()
        .find_map(|l| l.strip_prefix("RssAnon:"))
        .and_then(|v| v.trim().trim_end_matches("kB").trim().parse().ok())
        .expect("RssAnon in /proc/self/status");
    kb * 1024
}

fn models_dir() -> PathBuf {
    PathBuf::from(
        std::env::var("OATS_WHISPER_MODELS_DIR")
            .expect("OATS_WHISPER_MODELS_DIR is not set: run ci/fetch-models.sh first"),
    )
}

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/stt")
        .join(name)
}

#[test]
fn the_models_are_released_after_the_idle_timeout() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = dir.path().join("cfg").join("config.toml");
    std::fs::create_dir_all(cfg.parent().unwrap()).unwrap();
    std::fs::write(
        &cfg,
        format!(
            "[stt.local]\nmodel = \"parakeet-tdt-0.6b-v2\"\nmodels_dir = '{}'\nthreads = 4\nauto_download = false\nunload_after = \"2s\"\n",
            models_dir().display()
        ),
    )
    .unwrap();
    let engine: Arc<Engine> = Engine::new(EngineOpts {
        user_config: cfg,
        pack: None,
        os: "linux".into(),
        arch: "x86_64".into(),
        machine: "default".into(),
        sessions_dir: dir.path().join("data").join("sessions"),
        secrets: None,
    })
    .unwrap();
    let active = |e: &Engine| {
        e.dispatch(&Action::SttStatus {}).unwrap()["local"]["provider"]["active"].clone()
    };
    assert!(active(&engine).is_null());
    let base = rss_anon();

    let r = engine
        .dispatch(&Action::SttTranscribe {
            path: fixture("you.wav").display().to_string(),
            session: Some("session_2026-11-08_12-00-00".into()),
            speaker: None,
        })
        .unwrap();
    assert_eq!(r["backend"], "sherpa-onnx", "{r}");
    assert_eq!(active(&engine), "cpu");
    let loaded = rss_anon();
    let grew = loaded.saturating_sub(base);
    eprintln!("RSS_ROW base={base} loaded={loaded}");
    assert!(grew > 200 << 20, "the model added only {grew} bytes");

    // Released within twice the idle time, and the memory with it.
    let t = Instant::now();
    while !active(&engine).is_null() {
        assert!(t.elapsed() < Duration::from_secs(30), "still loaded");
        std::thread::sleep(Duration::from_millis(100));
    }
    let after = rss_anon();
    eprintln!("RSS_ROW base={base} loaded={loaded} unloaded={after}");
    assert!(
        after <= base + grew / 4,
        "RssAnon {after} after unload; {base} before the model, {loaded} with it"
    );

    // The next transcription loads it again.
    engine
        .dispatch(&Action::SttTranscribe {
            path: fixture("you.wav").display().to_string(),
            session: Some("session_2026-11-08_12-01-00".into()),
            speaker: None,
        })
        .unwrap();
    assert_eq!(active(&engine), "cpu");
}
