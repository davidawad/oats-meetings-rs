//! Idle model release (dot-4uzgol.102, .107, .108): after a transcription through the engine
//! (live + final pass, one shared Parakeet recogniser and the sherpa-onnx speaker model),
//! `stt.local.unload_after` of quiet drops both models and the footprint (macOS
//! `phys_footprint`, Linux `RssAnon`) of this process and its model workers together returns
//! within 100 MB of where it was before the load, and stays there; the next transcription loads
//! them again (`parakeet` feature). The models run in worker processes, as in the app: this test
//! binary is the worker (`footprint::worker`), and each unload ends its worker. Its own test binary
//! so no other test's models or threads skew this process's footprint. The models come from
//! `ci/fetch-models.sh` (OATS_WHISPER_MODELS_DIR, OATS_DIARIZE_MODELS_DIR); never committed.

use std::path::Path;
use std::sync::Arc;

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

#[cfg(all(feature = "parakeet", any(target_os = "linux", target_os = "macos")))]
mod footprint {
    use std::path::{Path, PathBuf};
    use std::time::{Duration, Instant};

    use oats_core::action::Action;
    use oats_core::control::engine::Engine;
    use oats_core::stt::{catalog, wav, worker};

    /// What may stay of the models after they unload.
    const SLACK: u64 = 100 << 20;

    /// RssAnon from a /proc/<pid>/status, in bytes.
    #[cfg(target_os = "linux")]
    fn rss_anon(status: &str) -> Option<u64> {
        let kb: u64 = status
            .lines()
            .find_map(|l| l.strip_prefix("RssAnon:"))
            .and_then(|v| v.trim().trim_end_matches("kB").trim().parse().ok())?;
        Some(kb * 1024)
    }

    /// RssAnon of this process and its children (the model workers), in bytes. The children are
    /// found by their parent in /proc, not by the ids `Command` gives, which name other
    /// processes when /proc belongs to another pid namespace (a sandbox).
    #[cfg(target_os = "linux")]
    fn footprint() -> u64 {
        let stat = std::fs::read_to_string("/proc/self/stat").unwrap();
        let me = stat.split(' ').next().unwrap().to_string();
        let mut total = rss_anon(&std::fs::read_to_string("/proc/self/status").unwrap())
            .expect("RssAnon in /proc/self/status");
        for d in std::fs::read_dir("/proc").unwrap().flatten() {
            let Ok(stat) = std::fs::read_to_string(d.path().join("stat")) else {
                continue;
            };
            // pid (comm) state ppid ...; comm may hold spaces.
            let ppid = stat
                .rsplit_once(") ")
                .and_then(|(_, rest)| rest.split(' ').nth(1));
            if ppid == Some(me.as_str()) {
                if let Ok(s) = std::fs::read_to_string(d.path().join("status")) {
                    total += rss_anon(&s).unwrap_or(0);
                }
            }
        }
        total
    }

    /// phys_footprint of process `pid` (what Activity Monitor's Memory column shows), in bytes;
    /// `None` once it has exited.
    #[cfg(target_os = "macos")]
    fn footprint_of(pid: u32) -> Option<u64> {
        let mut info = std::mem::MaybeUninit::<libc::rusage_info_v2>::zeroed();
        // SAFETY: RUSAGE_INFO_V2 fills exactly a rusage_info_v2.
        let rc = unsafe {
            libc::proc_pid_rusage(
                pid as libc::pid_t,
                libc::RUSAGE_INFO_V2,
                info.as_mut_ptr().cast::<libc::rusage_info_t>(),
            )
        };
        // SAFETY: filled when rc is 0.
        (rc == 0).then(|| unsafe { info.assume_init() }.ri_phys_footprint)
    }

    /// phys_footprint of this process and its live model workers, in bytes.
    #[cfg(target_os = "macos")]
    fn footprint() -> u64 {
        let me = footprint_of(std::process::id()).expect("own footprint");
        me + worker::live()
            .into_iter()
            .filter_map(footprint_of)
            .sum::<u64>()
    }

    fn workers(e: &Engine) -> u64 {
        status(e)["models"]["workers"].as_u64().unwrap()
    }

    /// This test binary as a model worker, when the footprint test starts it as one (the app
    /// starts itself as `oats-meetings-rs stt-worker`); otherwise nothing.
    #[test]
    fn worker() {
        if std::env::args().any(|a| a == worker::ARG) {
            std::process::exit(worker::serve());
        }
    }

    fn fixture(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/stt")
            .join(name)
    }

    /// A models directory holding Parakeet v2 and the default sherpa-onnx speaker model, linked
    /// from where ci/fetch-models.sh put them; `None` (the test is skipped) when one is missing.
    fn models(into: &Path) -> Option<PathBuf> {
        let from = PathBuf::from(
            std::env::var("OATS_WHISPER_MODELS_DIR")
                .expect("OATS_WHISPER_MODELS_DIR is not set: run ci/fetch-models.sh first"),
        );
        let speakers = std::env::var("OATS_DIARIZE_MODELS_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| from.clone());
        let asr = catalog::get("parakeet-tdt-0.6b-v2").unwrap();
        let spk = catalog::speaker("titanet-small").unwrap();
        let dir = into.join("models");
        std::fs::create_dir_all(&dir).unwrap();
        for (e, src) in [
            (asr.location(&from), asr.location(&dir)),
            (spk.location(&speakers), spk.location(&dir)),
        ] {
            if !e.exists() {
                eprintln!("SKIP: {} is not there (ci/fetch-models.sh)", e.display());
                return None;
            }
            std::os::unix::fs::symlink(&e, &src).unwrap();
        }
        Some(dir)
    }

    /// A two-channel call: you, a second of quiet, then the other side (diarized).
    fn call(dir: &Path) -> PathBuf {
        let you = wav::read(&fixture("you.wav")).unwrap().you;
        let them = wav::read(&fixture("them.wav")).unwrap().you;
        let path = dir.join("call.wav");
        let spec = hound::WavSpec {
            channels: 2,
            sample_rate: 16_000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut w = hound::WavWriter::create(&path, spec).unwrap();
        let gap = 16_000;
        for i in 0..(you.len() + gap + them.len()) {
            let l = you.get(i).copied().unwrap_or(0.0);
            let r = i.checked_sub(you.len() + gap).map_or(0.0, |j| them[j]);
            w.write_sample((l * 32767.0) as i16).unwrap();
            w.write_sample((r * 32767.0) as i16).unwrap();
        }
        w.finalize().unwrap();
        path
    }

    fn status(e: &Engine) -> serde_json::Value {
        e.dispatch(&Action::SttStatus {}).unwrap()
    }

    fn loaded(e: &Engine) -> bool {
        let s = status(e);
        !s["local"]["provider"]["active"].is_null() || !s["diarization"]["active"].is_null()
    }

    fn loads(e: &Engine) -> u64 {
        status(e)["models"]["loads"].as_u64().unwrap()
    }

    /// Transcribe the call into a session; returns the footprint with both models loaded.
    fn transcribe(e: &Engine, call: &Path, session: &str) -> u64 {
        let before = loads(e);
        let r = e
            .dispatch(&Action::SttTranscribe {
                path: call.display().to_string(),
                session: Some(session.into()),
                speaker: None,
            })
            .unwrap();
        assert_eq!(r["backend"], "sherpa-onnx", "{r}");
        assert!(r["final_utterances"].as_u64().unwrap() >= 2, "{r}");
        let s = status(e);
        assert!(!s["local"]["provider"]["active"].is_null(), "{s}");
        assert_eq!(s["diarization"]["active"], "sherpa-onnx", "{s}");
        // The recogniser and the speaker model, each once (live and final share them), each in
        // a worker of its own.
        assert_eq!(loads(e) - before, 2, "{s}");
        assert_eq!(workers(e), 2, "{s}");
        footprint()
    }

    /// Wait for both models to unload, then watch the footprint stay down for `watch`.
    fn unloaded(e: &Engine, base: u64, with: u64, watch: Duration) {
        let t = Instant::now();
        while loaded(e) {
            assert!(t.elapsed() < Duration::from_secs(60), "still loaded");
            std::thread::sleep(Duration::from_millis(100));
        }
        // Each unload ended its worker.
        assert_eq!(workers(e), 0);
        assert!(worker::live().is_empty());
        let t = Instant::now();
        let first = footprint();
        let mut high = first;
        while t.elapsed() < watch {
            std::thread::sleep(Duration::from_millis(250));
            high = high.max(footprint());
        }
        eprintln!("FOOTPRINT_ROW base={base} loaded={with} unloaded={first} high={high}");
        assert!(
            high <= base + SLACK,
            "footprint {high} after unload; {base} before the models, {with} with them"
        );
        assert!(
            high <= first + (SLACK / 4),
            "footprint grew from {first} to {high} while idle"
        );
    }

    #[test]
    fn both_models_unload_back_to_the_pre_load_footprint() {
        let dir = tempfile::tempdir().unwrap();
        let Some(models) = models(dir.path()) else {
            return;
        };
        worker::set_command(
            std::env::current_exe().unwrap(),
            vec![
                "--exact".into(),
                "footprint::worker".into(),
                "--nocapture".into(),
            ],
        );
        let engine = super::engine_with(
            dir.path(),
            &format!(
                "[stt.local]\nmodel = \"parakeet-tdt-0.6b-v2\"\nmodels_dir = '{}'\nthreads = 4\nauto_download = false\nunload_after = \"2s\"\n\n[stt.diarization]\nalgorithm = \"sherpa-onnx\"\n",
                models.display()
            ),
        );
        let call = call(dir.path());
        assert!(!loaded(&engine));
        let base = footprint();

        let with = transcribe(&engine, &call, "session_2026-11-08_12-00-00");
        assert!(
            with > base + 2 * SLACK,
            "the models added only {} bytes",
            with.saturating_sub(base)
        );
        unloaded(&engine, base, with, Duration::from_secs(5));

        // Loaded again and released again: no climb from one cycle to the next.
        let again = transcribe(&engine, &call, "session_2026-11-08_12-01-00");
        unloaded(&engine, base, again, Duration::from_secs(3));
    }
}
