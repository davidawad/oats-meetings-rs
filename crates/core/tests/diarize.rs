//! Speaker diarization of the system-audio channel (dot-4uzgol.77) on a two-voice fixture: the
//! two macOS `say` voices of tests/fixtures/stt (Karen in you.wav, Daniel in them.wav), cut into
//! short turns and interleaved on the system channel as a two-person remote side would be, some at
//! a lower level. The recogniser is a stand-in that returns one utterance per stretch of speech,
//! so what is measured is only who each utterance is attributed to: remote_1 is the voice heard
//! first, remote_2 the other, the microphone stays `you`.

use std::path::Path;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use oats_core::config::resolve::{defaults_flat, nest};
use oats_core::config::typed::Config;
use oats_core::config::Result;
use oats_core::sessions::Store;
use oats_core::stt::wav::{self, Channels, SAMPLE_RATE};
use oats_core::stt::{self, Asr, Segment, Settings};
use serde_json::{json, Value};

const SR: usize = SAMPLE_RATE as usize;

fn voice(name: &str) -> Vec<f32> {
    let p = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/stt")
        .join(name);
    wav::decode(&std::fs::read(p).unwrap()).unwrap().you
}

/// One utterance per stretch of speech (10 ms frames over the gate) with no gap of 300 ms or
/// more inside it; each gets a distinct word so echo dedupe never matches two.
struct Stretches;

impl Asr for Stretches {
    fn name(&self) -> &'static str {
        "stretches"
    }
    fn transcribe(&self, pcm: &[f32], _l: &str) -> Result<Vec<Segment>> {
        let frame = SR / 100;
        let loud: Vec<bool> = pcm
            .chunks(frame)
            .map(|c| c.iter().any(|v| v.abs() >= 0.01))
            .collect();
        let mut spans: Vec<(usize, usize)> = Vec::new();
        for (i, _) in loud.iter().enumerate().filter(|(_, l)| **l) {
            match spans.last_mut() {
                Some(s) if i - s.1 < 30 => s.1 = i,
                _ => spans.push((i, i)),
            }
        }
        Ok(spans
            .into_iter()
            .map(|(a, b)| {
                let (start, end) = (a as f64 / 100.0, (b + 1) as f64 / 100.0);
                Segment {
                    start,
                    end,
                    text: format!("w{}", (start * 1000.0) as u64 + end as u64),
                }
            })
            .collect())
    }
}

/// A turn on the system channel: which voice (0 = Karen, 1 = Daniel) and where.
struct Turn {
    voice: usize,
    start: f64,
    end: f64,
}

/// The system channel: each voice cut into pieces of about a second and a half (at quiet
/// points), played in an interleaved order with 0.8 s between turns, then everything once more
/// at half the level.
fn two_voice_channel() -> (Vec<f32>, Vec<Turn>) {
    let voices = [voice("you.wav"), voice("them.wav")];
    let pieces: Vec<Vec<&[f32]>> = voices
        .iter()
        .map(|v| {
            stt::pieces(v, SR * 3 / 2)
                .into_iter()
                .map(|(_, p)| p)
                .collect()
        })
        .collect();
    // Turn order: Karen opens, Daniel replies, then irregular back and forth.
    let order = [0, 1, 1, 0, 1, 0, 0, 1, 0, 1];
    let mut next = [0usize, 0];
    let mut seq: Vec<(usize, &[f32])> = Vec::new();
    for &v in order.iter().cycle() {
        if next.iter().zip(&pieces).all(|(n, p)| *n >= p.len()) {
            break;
        }
        if let Some(p) = pieces[v].get(next[v]) {
            seq.push((v, p));
        }
        next[v] += 1;
    }
    let gap = vec![0.0f32; SR * 4 / 5];
    let (mut pcm, mut turns) = (gap.clone(), Vec::new());
    for gain in [1.0f32, 0.5] {
        for (v, p) in &seq {
            let start = pcm.len() as f64 / SR as f64;
            pcm.extend(p.iter().map(|x| x * gain));
            turns.push(Turn {
                voice: *v,
                start,
                end: pcm.len() as f64 / SR as f64,
            });
            pcm.extend_from_slice(&gap);
        }
    }
    (pcm, turns)
}

fn settings(live: bool) -> Settings {
    let mut c = defaults_flat();
    c.insert("stt.live.enabled".into(), json!(live));
    c.insert("stt.final.enabled".into(), json!(!live));
    let cfg: Config = serde_json::from_value(nest(&c)).unwrap();
    Settings::from_config(&cfg, Path::new("/tmp/x/sessions")).unwrap()
}

fn transcribe(chans: &Channels, live: bool) -> Vec<Value> {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::new(dir.path());
    let now: DateTime<Utc> = "2026-11-05T12:00:00Z".parse().unwrap();
    stt::run(
        &store,
        None,
        Arc::new(Stretches),
        &settings(live),
        chans,
        "s",
        now,
    )
    .unwrap();
    let t = store.transcript("s").unwrap();
    assert_eq!(t["source"], if live { "live" } else { "final" });
    t["utterances"].as_array().unwrap().clone()
}

/// The voice of the turn an utterance falls in (by its midpoint).
fn truth(turns: &[Turn], u: &Value) -> Option<usize> {
    let mid = (u["x_oats"]["startSeconds"].as_f64().unwrap()
        + u["x_oats"]["endSeconds"].as_f64().unwrap())
        / 2.0;
    turns
        .iter()
        .find(|t| mid >= t.start && mid <= t.end)
        .map(|t| t.voice)
}

fn attributed(us: &[Value], turns: &[Turn]) -> (usize, usize) {
    let remote: Vec<&Value> = us.iter().filter(|u| u["speaker"] != "you").collect();
    let right = remote
        .iter()
        .filter(|u| {
            let want = ["remote_1", "remote_2"][truth(turns, u).expect("utterance inside a turn")];
            u["speaker"] == want
        })
        .count();
    (right, remote.len())
}

fn speakers(us: &[Value]) -> std::collections::BTreeSet<String> {
    us.iter()
        .map(|u| u["speaker"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn two_say_voices_on_system_audio_are_remote_1_and_remote_2() {
    let (them, turns) = two_voice_channel();
    assert!(turns.len() >= 16, "{} turns", turns.len());
    let chans = Channels { you: vec![], them };
    for live in [false, true] {
        let us = transcribe(&chans, live);
        let (right, n) = attributed(&us, &turns);
        eprintln!("live={live}: {right}/{n} attributed correctly");
        assert!(
            n >= turns.len(),
            "live={live}: {n} utterances for {} turns",
            turns.len()
        );
        assert!(
            right * 10 >= n * 9,
            "live={live}: only {right}/{n} attributed correctly"
        );
        let want: std::collections::BTreeSet<String> =
            ["remote_1", "remote_2"].map(String::from).into();
        assert_eq!(speakers(&us), want, "live={live}");
    }
}

#[test]
fn the_microphone_stays_you_whatever_the_voice() {
    // Daniel on the mic while Karen and Daniel talk on the system channel: the mic is never
    // diarized, so it is `you`, and no remote label lands on it.
    let (them, turns) = two_voice_channel();
    let daniel = voice("them.wav");
    let mut you = vec![0.0f32; them.len()];
    let at = (turns.last().unwrap().end * SR as f64) as usize;
    you.truncate(at);
    you.extend_from_slice(&daniel);
    let chans = Channels { you, them };
    let us = transcribe(&chans, false);
    let mic: Vec<&Value> = us
        .iter()
        .filter(|u| u["x_oats"]["startSeconds"].as_f64().unwrap() >= turns.last().unwrap().end)
        .collect();
    assert!(!mic.is_empty());
    assert!(mic.iter().all(|u| u["speaker"] == "you"), "{mic:?}");
    let (right, n) = attributed(&us, &turns);
    assert!(right * 10 >= n * 9, "{right}/{n}");
}

#[test]
fn diarization_off_labels_all_remote_speech_speaker_them() {
    let (them, _) = two_voice_channel();
    let mut c = defaults_flat();
    c.insert("stt.live.enabled".into(), json!(false));
    c.insert("stt.diarization.enabled".into(), json!(false));
    let cfg: Config = serde_json::from_value(nest(&c)).unwrap();
    let s = Settings::from_config(&cfg, Path::new("/tmp/x/sessions")).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let store = Store::new(dir.path());
    let chans = Channels { you: vec![], them };
    stt::run(
        &store,
        None,
        Arc::new(Stretches),
        &s,
        &chans,
        "s",
        Utc::now(),
    )
    .unwrap();
    let us = store.transcript("s").unwrap()["utterances"]
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(speakers(&us), ["remote_1".to_string()].into());
}

#[test]
fn speaker_echo_on_the_mic_is_dropped_not_labelled_you() {
    // The mic hears the speakers (both remote voices, quieter): every such "you" utterance says
    // what a remote one said at the same time, so it is dropped as echo, whichever remote
    // speaker it echoes.
    let (them, turns) = two_voice_channel();
    let you: Vec<f32> = them.iter().map(|x| x * 0.8).collect();
    for live in [false, true] {
        let us = transcribe(
            &Channels {
                you: you.clone(),
                them: them.clone(),
            },
            live,
        );
        assert!(us.iter().all(|u| u["speaker"] != "you"), "live={live}");
        let (right, n) = attributed(&us, &turns);
        assert!(right * 10 >= n * 9, "live={live}: {right}/{n}");
    }
}
