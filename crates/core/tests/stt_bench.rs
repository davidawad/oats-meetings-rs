#![cfg(any(feature = "whisper", feature = "parakeet"))]
//! STT benchmark (dot-4uzgol.22): WER, real-time factor and peak memory of ONE engine x model over the
//! LibriSpeech clips in tests/fixtures/stt-bench/manifest.tsv (fetched by ci/fetch-bench-corpus.sh), clean and with
//! noise and crosstalk mixes, for the live and the final pass of the real pipeline (`stt::run`, default config).
//! One config per process so VmHWM is that config's peak. Runs only when OATS_BENCH_ENGINE is set (ci/stt-bench.sh);
//! prints `BENCH_ROW` lines. It asserts nothing about accuracy: it measures, David picks the defaults.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use oats_core::stt::wer::{wer, words};
use oats_core::stt::{wav, Asr};

const THREADS: u32 = 4;
const NOISE_SNR_DB: f32 = 10.0;
const CROSSTALK_SNR_DB: f32 = 5.0;

struct Clip {
    id: String,
    subset: String,
    reference: String,
    pcm: Vec<f32>,
}

fn clips() -> Vec<Clip> {
    let dir = PathBuf::from(
        std::env::var("OATS_BENCH_DIR").expect("OATS_BENCH_DIR: run ci/fetch-bench-corpus.sh"),
    );
    let m =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/stt-bench/manifest.tsv");
    std::fs::read_to_string(m)
        .unwrap()
        .lines()
        .filter(|l| !l.starts_with('#') && !l.is_empty())
        .map(|l| {
            let f: Vec<&str> = l.split('\t').collect();
            assert_eq!(
                f[2], "CC-BY-4.0",
                "{}: licence not recorded as CC-BY-4.0",
                f[0]
            );
            Clip {
                id: f[0].into(),
                subset: f[1].into(),
                reference: f[6].into(),
                pcm: wav::read(&dir.join(format!("{}.wav", f[0]))).unwrap().you,
            }
        })
        .collect()
}

fn rms(x: &[f32]) -> f32 {
    (x.iter().map(|v| v * v).sum::<f32>() / x.len().max(1) as f32).sqrt()
}

/// Deterministic xorshift noise in [-1, 1), smoothed once (pink-ish: office hum rather than hiss).
fn noise(n: usize, seed: u64) -> Vec<f32> {
    let mut s = seed;
    let mut prev = 0.0f32;
    (0..n)
        .map(|_| {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            let w = (s >> 11) as f32 / (1u64 << 53) as f32 * 2.0 - 1.0;
            prev = 0.8 * prev + 0.2 * w;
            prev
        })
        .collect()
}

/// `a` plus `b` scaled so rms(a)/rms(scaled b) = snr_db; `b` is looped/cut to `a`'s length, starting `lead` samples in.
fn mix(a: &[f32], b: &[f32], snr_db: f32, lead: usize) -> Vec<f32> {
    let g = rms(a) / (rms(b).max(1e-9) * 10f32.powf(snr_db / 20.0));
    (0..a.len())
        .map(|i| {
            a[i] + g * if i >= lead {
                b[(i - lead) % b.len()]
            } else {
                0.0
            }
        })
        .collect()
}

/// One pass of the real pipeline (`stt::run`: speech gate, padding, windowing, feed size from the default config) on a
/// mic-only clip; returns the transcript text of that pass.
fn pass(asr: &Arc<dyn Asr>, pcm: &[f32], live: bool) -> String {
    use oats_core::config::typed::Config;
    let mut c = oats_core::config::resolve::defaults_flat();
    c.insert("stt.live.enabled".into(), serde_json::json!(live));
    c.insert("stt.final.enabled".into(), serde_json::json!(!live));
    let cfg: Config = serde_json::from_value(oats_core::config::resolve::nest(&c)).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let settings = oats_core::stt::Settings::from_config(&cfg, dir.path()).unwrap();
    let store = oats_core::sessions::Store::new(dir.path());
    let chans = wav::Channels {
        you: pcm.to_vec(),
        them: vec![],
    };
    let id = "session_2026-10-28_12-00-00";
    oats_core::stt::run(
        &store,
        None,
        Arc::clone(asr),
        &settings,
        &chans,
        id,
        chrono::Utc::now(),
    )
    .unwrap();
    let f = dir.path().join(id).join(format!(
        "transcript.{}.jsonl",
        if live { "live" } else { "final" }
    ));
    std::fs::read_to_string(f)
        .unwrap_or_default()
        .lines()
        .map(|l| {
            serde_json::from_str::<serde_json::Value>(l).unwrap()["text"]
                .as_str()
                .unwrap_or("")
                .to_string()
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn status_kb(key: &str) -> u64 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with(key))
                .and_then(|l| l.split_whitespace().nth(1)?.parse().ok())
        })
        .unwrap_or(0)
}

fn load() -> (String, Arc<dyn Asr>) {
    let engine = std::env::var("OATS_BENCH_ENGINE").unwrap();
    let models =
        PathBuf::from(std::env::var("OATS_WHISPER_MODELS_DIR").expect("OATS_WHISPER_MODELS_DIR"));
    match engine.as_str() {
        #[cfg(feature = "whisper")]
        "whisper" => {
            let m = std::env::var("OATS_BENCH_MODEL").expect("OATS_BENCH_MODEL");
            let asr = oats_core::stt::local::WhisperAsr::load(
                &models.join(format!("ggml-{m}.bin")),
                THREADS,
            )
            .unwrap();
            (m, Arc::new(asr))
        }
        // Any sherpa-onnx catalog model (OATS_BENCH_MODEL = its id; `-` means Parakeet v2).
        #[cfg(feature = "parakeet")]
        "parakeet" | "sherpa" => {
            let m = std::env::var("OATS_BENCH_MODEL")
                .ok()
                .filter(|m| m != "-")
                .unwrap_or_else(|| "parakeet-tdt-0.6b-v2".into());
            let e = oats_core::stt::catalog::get(&m).expect("a catalog model id");
            let asr = oats_core::stt::sherpa::SherpaAsr::load(
                e,
                &e.location(&models),
                THREADS,
                oats_core::config::typed::SttProvider::Cpu,
            )
            .unwrap();
            (m, Arc::new(asr))
        }
        e => panic!("engine {e} not built in"),
    }
}

#[test]
fn bench() {
    let Ok(engine) = std::env::var("OATS_BENCH_ENGINE") else {
        return;
    };
    let clips = clips();
    let base_kb = status_kb("VmRSS:");
    let t0 = Instant::now();
    let (model, asr) = load();
    let load_s = t0.elapsed().as_secs_f64();
    // Warm-up: the first call pays one-off allocation and graph setup; it is not a steady-state cost.
    let _ = pass(&asr, &clips[0].pcm[..16_000 * 3], false);

    let n = clips.len();
    let mut conds: Vec<(&str, Vec<Vec<f32>>)> =
        vec![("clean", clips.iter().map(|c| c.pcm.clone()).collect())];
    conds.push((
        "noise10db",
        clips
            .iter()
            .enumerate()
            .map(|(i, c)| {
                mix(
                    &c.pcm,
                    &noise(c.pcm.len(), 0x9E37_79B9 + i as u64),
                    NOISE_SNR_DB,
                    0,
                )
            })
            .collect(),
    ));
    conds.push((
        "crosstalk5db",
        clips
            .iter()
            .enumerate()
            .map(|(i, c)| mix(&c.pcm, &clips[(i + 5) % n].pcm, CROSSTALK_SNR_DB, 8_000))
            .collect(),
    ));
    for (cond, audio) in &conds {
        for stage in ["live", "final"] {
            for subset in ["all", "dev-clean", "dev-other"] {
                let (mut edits, mut ref_words, mut secs, mut wall) =
                    (0.0f64, 0usize, 0.0f64, 0.0f64);
                for (c, pcm) in clips.iter().zip(audio) {
                    if subset != "all" && c.subset != subset {
                        continue;
                    }
                    let t = Instant::now();
                    let text = pass(&asr, pcm, stage == "live");
                    wall += t.elapsed().as_secs_f64();
                    let r = words(&c.reference).len();
                    edits += (wer(&c.reference, &text) * r as f64).round();
                    ref_words += r;
                    secs += pcm.len() as f64 / 16_000.0;
                }
                println!(
                    "BENCH_ROW engine={engine} model={model} cond={cond} stage={stage} subset={subset} wer={:.4} rtf={:.3} audio_s={secs:.1} words={ref_words}",
                    edits / ref_words as f64,
                    wall / secs
                );
            }
        }
    }
    println!(
        "BENCH_MEM engine={engine} model={model} load_s={load_s:.2} peak_rss_mb={:.0} base_rss_mb={:.0}",
        status_kb("VmHWM:") as f64 / 1024.0,
        base_kb as f64 / 1024.0
    );
}
