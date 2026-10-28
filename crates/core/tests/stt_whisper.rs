#![cfg(any(feature = "whisper", feature = "parakeet"))]
//! Local ASR acceptance: whisper.cpp transcribes the synthetic fixtures under the agreed word
//! error rate, alone and through the whole engine (live + final, you/them) into a session store.
//! The model comes from `ci/fetch-models.sh` (OATS_WHISPER_MODEL); it is never committed.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use oats_core::action::Action;
use oats_core::config::keys::STT_LOCAL_MODELS;
use oats_core::control::engine::{Engine, EngineOpts};
use oats_core::stt::wer::wer;
#[cfg(feature = "whisper")]
use oats_core::stt::local::WhisperAsr;
use oats_core::stt::{wav, Asr};
use serde_json::Value;

/// Agreed acceptance threshold: word error rate per channel with ggml-tiny.en on clean TTS.
const MAX_WER: f64 = 0.20;

/// Directory ci/fetch-models.sh filled (ggml-<name>.bin for every allowed stt.local.model).
fn models_dir() -> PathBuf {
    let p = std::env::var("OATS_WHISPER_MODELS_DIR").expect("OATS_WHISPER_MODELS_DIR is not set: run ci/fetch-models.sh first");
    PathBuf::from(p)
}

#[cfg(feature = "whisper")]
fn model(name: &str) -> PathBuf {
    let p = models_dir().join(format!("ggml-{name}.bin"));
    assert!(p.is_file(), "{} is not a file", p.display());
    p
}

#[cfg(feature = "whisper")]
fn only() -> Option<String> {
    std::env::var("OATS_ONLY_MODEL").ok().filter(|m| !m.is_empty())
}

#[cfg(feature = "whisper")]
fn models() -> Vec<&'static str> {
    STT_LOCAL_MODELS.iter().copied().filter(|m| only().as_deref().is_none_or(|o| o == *m)).collect()
}

#[test]
fn fetch_script_pins_exactly_the_allowed_models() {
    let sh = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../ci/fetch-models.sh")).unwrap();
    for m in STT_LOCAL_MODELS {
        assert!(sh.contains(&format!("[{m}]=")), "ci/fetch-models.sh has no sha pin for {m}");
    }
    assert_eq!(sh.matches("]=").count(), STT_LOCAL_MODELS.len(), "ci/fetch-models.sh pins a model stt.local.model does not allow");
}

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/stt").join(name)
}

fn expected(name: &str) -> String {
    let v: Value = serde_json::from_str(&std::fs::read_to_string(fixture("expected.json")).unwrap()).unwrap();
    v[name].as_str().unwrap().to_string()
}

#[cfg(feature = "whisper")]
#[test]
fn whisper_meets_the_wer_threshold_on_each_fixture() {
    for m in models() {
        let asr = WhisperAsr::load(&model(m), 4).unwrap();
        for name in ["you.wav", "them.wav"] {
            let pcm = wav::read(&fixture(name)).unwrap().you;
            let segs = asr.transcribe(&pcm, "en").unwrap();
            let text = segs.iter().map(|s| s.text.as_str()).collect::<Vec<_>>().join(" ");
            let w = wer(&expected(name), &text);
            eprintln!("WER_ROW model={m} stage=asr file={name} wer={w:.3} :: {text}");
            assert!(w <= MAX_WER, "{m} {name}: WER {w:.3} > {MAX_WER} :: {text}");
            assert!(segs.iter().all(|s| s.end >= s.start && s.start >= 0.0));
        }
    }
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap() {
        let e = e.unwrap();
        let t = to.join(e.file_name());
        if e.path().is_dir() {
            copy_dir(&e.path(), &t);
        } else {
            std::fs::copy(e.path(), t).unwrap();
        }
    }
}

#[cfg(feature = "whisper")]
#[test]
fn engine_local_backend_transcribes_a_two_channel_call_into_a_session() {
    for m in models() {
        engine_case(m, &format!("model = \"{m}\"\n"), "whisper.cpp");
    }
}

#[cfg(feature = "parakeet")]
#[test]
fn parakeet_meets_the_wer_threshold_on_each_fixture() {
    let dir = models_dir().join(oats_core::stt::PARAKEET_DIR);
    let asr = oats_core::stt::parakeet::ParakeetAsr::load(&dir, 4).unwrap();
    for name in ["you.wav", "them.wav"] {
        let pcm = wav::read(&fixture(name)).unwrap().you;
        let segs = asr.transcribe(&pcm, "en").unwrap();
        let text = segs.iter().map(|s| s.text.as_str()).collect::<Vec<_>>().join(" ");
        let w = wer(&expected(name), &text);
        eprintln!("WER_ROW model=parakeet-tdt-0.6b-v2 stage=asr file={name} wer={w:.3} :: {text}");
        assert!(w <= MAX_WER, "parakeet {name}: WER {w:.3} > {MAX_WER} :: {text}");
    }
}

#[cfg(feature = "parakeet")]
#[test]
fn engine_parakeet_transcribes_a_two_channel_call_into_a_session() {
    engine_case("parakeet-tdt-0.6b-v2", "engine = \"parakeet\"\n", "parakeet");
}

fn engine_case(m: &str, local_toml: &str, backend: &str) {
    let dir = tempfile::tempdir().unwrap();
    let you = wav::read(&fixture("you.wav")).unwrap().you;
    let them = wav::read(&fixture("them.wav")).unwrap().you;
    let gap = 16_000;
    let call = dir.path().join("call.wav");
    let spec = hound::WavSpec { channels: 2, sample_rate: 16_000, bits_per_sample: 16, sample_format: hound::SampleFormat::Int };
    let mut w = hound::WavWriter::create(&call, spec).unwrap();
    for i in 0..(you.len() + gap + them.len()) {
        w.write_sample((you.get(i).copied().unwrap_or(0.0) * 32767.0) as i16).unwrap();
        let r = if i >= you.len() + gap { them[i - you.len() - gap] } else { 0.0 };
        w.write_sample((r * 32767.0) as i16).unwrap();
    }
    w.finalize().unwrap();

    let cfg = dir.path().join("cfg").join("config.toml");
    std::fs::create_dir_all(cfg.parent().unwrap()).unwrap();
    std::fs::write(&cfg, format!("[stt.local]\n{local_toml}models_dir = '{}'\nthreads = \"4\"\n", models_dir().display())).unwrap();
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
    let id = "session_2026-10-28_12-00-00";
    let r = engine
        .dispatch(&Action::SttTranscribe { path: call.display().to_string(), session: Some(id.into()), speaker: None })
        .unwrap();
    assert_eq!(r["backend"], backend);
    assert!(r["live_utterances"].as_u64().unwrap() >= 2 && r["final_utterances"].as_u64().unwrap() >= 2, "{r}");

    for source in ["final", "live"] {
        let file = dir.path().join("data/sessions").join(id).join(format!("transcript.{source}.jsonl"));
        let lines: Vec<Value> = std::fs::read_to_string(&file).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect();
        for (speaker, name) in [("you", "you.wav"), ("remote_1", "them.wav")] {
            let text = lines.iter().filter(|u| u["speaker"] == speaker).map(|u| u["text"].as_str().unwrap()).collect::<Vec<_>>().join(" ");
            let w = wer(&expected(name), &text);
            eprintln!("WER_ROW model={m} stage={source} file={name} wer={w:.3} :: {text}");
            // Live windows can split a phrase mid-word; the live pass gets a looser bound.
            let limit = if source == "final" { MAX_WER } else { 0.35 };
            assert!(w <= limit, "{m} {source}/{speaker}: WER {w:.3} > {limit} :: {text}");
        }
        assert!(lines.iter().all(|u| u["timestamp"].as_str().unwrap().ends_with('Z') && u["speaker"].is_string()));
    }
    // Both speakers are in the transcript the app reads (whisper's own segment times inside a
    // long silence are not exact, so order across channels is not asserted here).
    let t = engine.dispatch(&Action::TranscriptGet { session: id.into() }).unwrap();
    assert_eq!(t["source"], "final");
    let speakers: std::collections::BTreeSet<&str> =
        t["utterances"].as_array().unwrap().iter().map(|u| u["speaker"].as_str().unwrap()).collect();
    assert_eq!(speakers, ["remote_1", "you"].into_iter().collect::<std::collections::BTreeSet<&str>>());

    // `bin/meeting sessions` compatibility: ci/stt-compat.sh dumps the store this wrote.
    if let (Ok(out), "tiny.en") = (std::env::var("OATS_STORE_DUMP"), m) {
        copy_dir(&dir.path().join("data/sessions"), Path::new(&out));
    }
}
