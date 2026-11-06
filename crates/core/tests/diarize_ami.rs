//! Diarization error rate on real meetings (dot-4uzgol.83): AMI single-distant-microphone
//! recordings (tests/fixtures/diarize-ami/manifest.tsv), the spectral and the sherpa-onnx
//! algorithm (default keys) against the corpus's word-aligned speaker turns.
//!
//! The meeting audio is the system channel: the far side of a call is one mixed stream of several
//! people in one room. The recogniser is the reference itself (oracle segmentation): each
//! utterance is one stretch where a single speaker talks, cut at 15 s like a recogniser's
//! sentences, so what is measured is who each utterance is attributed to. DER is over 10 ms frames,
//! no collar, overlapped speech scored (a stretch two people talk in counts the second as missed),
//! with the best one-to-one map of found to reference speakers.
//!
//! The audio (27-34 MB per meeting) and the sherpa-onnx speaker model are fetched and checksummed by
//! ci/fetch-models.sh, never committed: `OATS_DIARIZE_AMI_DIR` holds `<meeting>.Array1-01.wav`,
//! `OATS_DIARIZE_MODELS_DIR` the speaker models (as `stt.local.models_dir`). Without the audio the
//! test says so and passes. `OATS_DIARIZE_SWEEP=1` prints DER for a range of thresholds.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use oats_core::config::resolve::{defaults_flat, nest};
use oats_core::config::typed::Config;
use oats_core::stt::diarize::{self, Algorithm, Diarizer};
use oats_core::stt::wav::{self, SAMPLE_RATE};
use oats_core::stt::{Settings, Utt};
use serde_json::{json, Value};

const FRAME: f64 = 0.01;
/// Longest utterance the stand-in recogniser returns, seconds.
const MAX_UTT: f64 = 15.0;

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/diarize-ami")
}

struct Meeting {
    id: String,
    wav_sha: String,
    /// DER ceilings, percent: spectral, sherpa-onnx (final pass).
    ceiling: (f64, f64),
}

fn manifest() -> Vec<Meeting> {
    std::fs::read_to_string(fixtures().join("manifest.tsv"))
        .unwrap()
        .lines()
        .filter(|l| !l.starts_with('#') && !l.trim().is_empty())
        .map(|l| {
            let f: Vec<&str> = l.split('\t').collect();
            assert_eq!(f[3], "CC-BY-4.0", "{}: licence", f[0]);
            Meeting {
                id: f[0].into(),
                wav_sha: f[2].into(),
                ceiling: (f[5].parse().unwrap(), f[6].parse().unwrap()),
            }
        })
        .collect()
}

/// Reference turns `(start, end, speaker)` from the meeting's RTTM.
fn reference(id: &str) -> Vec<(f64, f64, String)> {
    std::fs::read_to_string(fixtures().join(format!("{id}.rttm")))
        .unwrap()
        .lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.split_whitespace().collect();
            (f.first() == Some(&"SPEAKER")).then(|| {
                let (s, d): (f64, f64) = (f[3].parse().unwrap(), f[4].parse().unwrap());
                (s, s + d, f[7].to_string())
            })
        })
        .collect()
}

fn frames_of(t: f64) -> usize {
    (t / FRAME).round().max(0.0) as usize
}

/// Per 10 ms frame, the reference speakers talking.
fn ref_frames(turns: &[(f64, f64, String)], n: usize) -> Vec<Vec<usize>> {
    let names: Vec<String> = {
        let mut v: Vec<String> = turns.iter().map(|t| t.2.clone()).collect();
        v.sort();
        v.dedup();
        v
    };
    let mut out = vec![Vec::new(); n];
    for (s, e, who) in turns {
        let k = names.iter().position(|x| x == who).unwrap();
        for f in out
            .iter_mut()
            .take(frames_of(*e).min(n))
            .skip(frames_of(*s))
        {
            if !f.contains(&k) {
                f.push(k);
            }
        }
    }
    out
}

/// The stand-in recogniser's utterances: stretches with exactly one reference speaker, a gap
/// under 0.5 s inside one speaker's stretch bridged, cut at `MAX_UTT`; shorter than 0.2 s dropped.
fn utterances(refs: &[Vec<usize>]) -> Vec<(f64, f64)> {
    let mut spans: Vec<(usize, usize, usize)> = Vec::new();
    for (i, f) in refs.iter().enumerate() {
        if f.len() != 1 {
            continue;
        }
        match spans.last_mut() {
            Some(s) if s.2 == f[0] && i - s.1 <= 50 => s.1 = i + 1,
            _ => spans.push((i, i + 1, f[0])),
        }
    }
    let max = (MAX_UTT / FRAME) as usize;
    let mut out = Vec::new();
    for (a, b, _) in spans {
        let mut at = a;
        while at < b {
            let end = (at + max).min(b);
            if end - at >= 20 {
                out.push((at as f64 * FRAME, end as f64 * FRAME));
            }
            at = end;
        }
    }
    out
}

/// Diarization error rate (percent) of `hyp` (one label per utterance) against `refs`, with the
/// best one-to-one map of hypothesis to reference speakers; and the number of speakers found.
fn der(refs: &[Vec<usize>], utts: &[(f64, f64)], hyp: &[String]) -> (f64, usize) {
    let labels: Vec<String> = {
        let mut v = hyp.to_vec();
        v.sort();
        v.dedup();
        v
    };
    let mut hf: Vec<Option<usize>> = vec![None; refs.len()];
    for ((s, e), h) in utts.iter().zip(hyp) {
        let k = labels.iter().position(|x| x == h).unwrap();
        for f in hf
            .iter_mut()
            .take(frames_of(*e).min(refs.len()))
            .skip(frames_of(*s))
        {
            *f = Some(k);
        }
    }
    let nref = refs.iter().flatten().max().map_or(0, |m| m + 1);
    // overlap[h][r]: frames hypothesis speaker h and reference speaker r share.
    let mut overlap = vec![vec![0usize; nref]; labels.len()];
    for (r, h) in refs.iter().zip(&hf) {
        if let Some(h) = h {
            for &k in r {
                overlap[*h][k] += 1;
            }
        }
    }
    fn best(overlap: &[Vec<usize>], h: usize, used: &mut Vec<bool>) -> usize {
        if h == overlap.len() {
            return 0;
        }
        let mut b = best(overlap, h + 1, used);
        for r in 0..used.len() {
            if !used[r] {
                used[r] = true;
                b = b.max(overlap[h][r] + best(overlap, h + 1, used));
                used[r] = false;
            }
        }
        b
    }
    let correct = best(&overlap, 0, &mut vec![false; nref]);
    let total: usize = refs.iter().map(Vec::len).sum();
    let errors: usize = refs
        .iter()
        .zip(&hf)
        .map(|(r, h)| r.len().max(usize::from(h.is_some())))
        .sum::<usize>()
        - correct;
    (100.0 * errors as f64 / total.max(1) as f64, labels.len())
}

fn settings(over: &[(&str, Value)]) -> Settings {
    let mut c = defaults_flat();
    if let Ok(d) = std::env::var("OATS_DIARIZE_MODELS_DIR") {
        c.insert("stt.local.models_dir".into(), json!(d));
    }
    for (k, v) in over {
        c.insert((*k).into(), v.clone());
    }
    let cfg: Config = serde_json::from_value(nest(&c)).unwrap();
    Settings::from_config(&cfg, Path::new("/tmp/oats-diarize-ami/sessions")).unwrap()
}

struct Run {
    algorithm: Algorithm,
    final_der: f64,
    final_speakers: usize,
    live_der: f64,
    live_speakers: usize,
    secs: f64,
}

/// Diarize `pcm`'s utterances with `s`'s settings: the final pass, and the live (online) one.
fn run(s: &Settings, pcm: &[f32], refs: &[Vec<usize>], spans: &[(f64, f64)]) -> Run {
    let d = s.diarize.as_ref().expect("diarization on");
    let mut utts: Vec<Utt> = spans
        .iter()
        .map(|&(start, end)| Utt {
            speaker: String::new(),
            text: String::new(),
            start,
            end,
        })
        .collect();
    let t = Instant::now();
    diarize::label_final(d, &mut utts, pcm, "remote_1");
    let secs = t.elapsed().as_secs_f64();
    let hyp: Vec<String> = utts.iter().map(|u| u.speaker.clone()).collect();
    let (final_der, final_speakers) = der(refs, spans, &hyp);
    let mut live = Diarizer::new(d.clone());
    let algorithm = live.algorithm();
    let sr = f64::from(SAMPLE_RATE);
    let hyp: Vec<String> = spans
        .iter()
        .map(|&(a, b)| {
            let pcm = &pcm[(a * sr) as usize..((b * sr) as usize).min(pcm.len())];
            live.assign(pcm).to_string()
        })
        .collect();
    let (live_der, live_speakers) = der(refs, spans, &hyp);
    Run {
        algorithm,
        final_der,
        final_speakers,
        live_der,
        live_speakers,
        secs,
    }
}

fn sha256(path: &Path) -> String {
    use sha2::{Digest, Sha256};
    let d = Sha256::digest(std::fs::read(path).unwrap());
    d.iter().map(|b| format!("{b:02x}")).collect()
}

#[test]
fn ami_meetings_der_stays_under_the_ceiling() {
    let Ok(dir) = std::env::var("OATS_DIARIZE_AMI_DIR") else {
        eprintln!("OATS_DIARIZE_AMI_DIR unset (ci/fetch-models.sh fetches the AMI audio): skipped");
        return;
    };
    let sweep = std::env::var("OATS_DIARIZE_SWEEP").is_ok_and(|v| v == "1");
    let only = std::env::var("OATS_DIARIZE_MEETINGS").unwrap_or_default();
    for m in manifest() {
        if !only.is_empty() && !only.split(',').any(|x| x == m.id) {
            continue;
        }
        let path = Path::new(&dir).join(format!("{}.Array1-01.wav", m.id));
        assert_eq!(sha256(&path), m.wav_sha, "{}", path.display());
        let pcm = wav::read(&path).unwrap().you;
        let turns = reference(&m.id);
        let n = frames_of(pcm.len() as f64 / f64::from(SAMPLE_RATE));
        let refs = ref_frames(&turns, n);
        let spans = utterances(&refs);
        let nspk = refs.iter().flatten().max().map_or(0, |x| x + 1);
        eprintln!(
            "{}: {:.0} s, {nspk} speakers, {} utterances",
            m.id,
            pcm.len() as f64 / f64::from(SAMPLE_RATE),
            spans.len()
        );
        let truth: Vec<String> = spans
            .iter()
            .map(|&(a, _)| refs[frames_of(a)][0].to_string())
            .collect();
        eprintln!(
            "DER_ROW {} oracle labels (the floor: overlapped speech): {:.1}%",
            m.id,
            der(&refs, &spans, &truth).0
        );
        let mut results: BTreeMap<&str, Run> = BTreeMap::new();
        for algo in ["spectral", "sherpa-onnx"] {
            let over = vec![("stt.diarization.algorithm", json!(algo))];
            let r = run(&settings(&over), &pcm, &refs, &spans);
            eprintln!(
                "DER_ROW {} {algo} ({} ran): final DER {:.1}% ({} speakers), live DER {:.1}% ({} speakers), final pass {:.1} s",
                m.id, r.algorithm.as_str(), r.final_der, r.final_speakers, r.live_der, r.live_speakers, r.secs
            );
            if sweep && (algo == "spectral" || r.algorithm != Algorithm::Spectral) {
                let key = if algo == "spectral" {
                    "stt.diarization.threshold"
                } else {
                    "stt.diarization.sherpa_onnx.threshold"
                };
                for thr in [10, 15, 20, 25, 30, 40, 50, 60, 70, 80] {
                    let mut o = over.clone();
                    o.push((key, json!(thr)));
                    let r = run(&settings(&o), &pcm, &refs, &spans);
                    eprintln!(
                        "SWEEP_ROW {} {algo} threshold {thr}: final DER {:.1}% ({} spk), live DER {:.1}% ({} spk)",
                        m.id, r.final_der, r.final_speakers, r.live_der, r.live_speakers
                    );
                }
            }
            results.insert(algo, r);
        }
        let spectral = &results["spectral"];
        assert_eq!(spectral.algorithm, Algorithm::Spectral);
        assert!(
            spectral.final_der <= m.ceiling.0,
            "{}: spectral DER {:.1}% over the {:.1}% ceiling",
            m.id,
            spectral.final_der,
            m.ceiling.0
        );
        let embedding = &results["sherpa-onnx"];
        if embedding.algorithm == Algorithm::SherpaOnnx {
            assert!(
                embedding.final_der <= m.ceiling.1,
                "{}: sherpa-onnx DER {:.1}% over the {:.1}% ceiling",
                m.id,
                embedding.final_der,
                m.ceiling.1
            );
            assert!(
                embedding.final_der < spectral.final_der,
                "{}: the speaker-embedding model ({:.1}%) should beat spectral ({:.1}%)",
                m.id,
                embedding.final_der,
                spectral.final_der
            );
        } else {
            eprintln!(
                "{}: no sherpa-onnx speaker model here (OATS_DIARIZE_MODELS_DIR, feature parakeet): its ceiling is not checked",
                m.id
            );
        }
    }
}
