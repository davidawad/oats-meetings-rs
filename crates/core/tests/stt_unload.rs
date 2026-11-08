//! Idle model release (dot-4uzgol.102): after a transcription through the engine (live + final
//! pass, one shared Parakeet recogniser and speaker model), `stt.local.unload_after` of quiet
//! drops the models and the process's anonymous memory returns near where it started (Linux,
//! `parakeet` feature). Its own test binary so no other test's models or threads skew this
//! process's RSS. The model comes from `ci/fetch-models.sh` (OATS_WHISPER_MODELS_DIR); it is never
//! committed.

use std::path::Path;
#[cfg(all(feature = "parakeet", target_os = "linux"))]
use std::path::PathBuf;
use std::sync::Arc;
#[cfg(all(feature = "parakeet", target_os = "linux"))]
use std::time::{Duration, Instant};

use oats_core::action::Action;
use oats_core::control::engine::{Engine, EngineOpts};

fn engine_with(dir: &Path, toml: &str) -> Arc<Engine> {
    let cfg = dir.join("cfg").join("config.toml");
    std::fs::create_dir_all(cfg.parent().unwrap()).unwrap();
    std::fs::write(&cfg, toml).unwrap();
    Engine::new(EngineOpts {
        user_config: cfg,
        pack: None,
        os: "linux".into(),
        arch: "x86_64".into(),
        machine: "default".into(),
        sessions_dir: dir.join("data").join("sessions"),
        secrets: None,
    })
    .unwrap()
}

fn unload_after(e: &Engine) -> serde_json::Value {
    e.dispatch(&Action::ConfigGet {
        key: "stt.local.unload_after".into(),
    })
    .unwrap()["value"]
        .clone()
}

/// The idle timeout is a declarative key: five minutes by default, settable in the config file
/// (0s keeps the models loaded).
#[test]
fn unload_after_is_a_config_key_defaulting_to_five_minutes() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(unload_after(&engine_with(dir.path(), "")), "5m");
    let dir = tempfile::tempdir().unwrap();
    let e = engine_with(dir.path(), "[stt.local]\nunload_after = \"2s\"\n");
    assert_eq!(unload_after(&e), "2s");
    let dir = tempfile::tempdir().unwrap();
    let e = engine_with(dir.path(), "[stt.local]\nunload_after = \"0s\"\n");
    assert_eq!(unload_after(&e), "0s");
}

#[cfg(all(feature = "parakeet", target_os = "linux"))]
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

#[cfg(all(feature = "parakeet", target_os = "linux"))]
fn models_dir() -> PathBuf {
    PathBuf::from(
        std::env::var("OATS_WHISPER_MODELS_DIR")
            .expect("OATS_WHISPER_MODELS_DIR is not set: run ci/fetch-models.sh first"),
    )
}

#[cfg(all(feature = "parakeet", target_os = "linux"))]
fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/stt")
        .join(name)
}

#[cfg(all(feature = "parakeet", target_os = "linux"))]
#[test]
fn the_models_are_released_after_the_idle_timeout() {
    let dir = tempfile::tempdir().unwrap();
    let engine = engine_with(
        dir.path(),
        &format!(
            "[stt.local]\nmodel = \"parakeet-tdt-0.6b-v2\"\nmodels_dir = '{}'\nthreads = 4\nauto_download = false\nunload_after = \"2s\"\n",
            models_dir().display()
        ),
    );
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
