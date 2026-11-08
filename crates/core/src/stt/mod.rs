//! Transcription engine (dot-4uzgol.8). One trait, `Asr`, two backends chosen by `stt.backend`:
//!
//! - `local`: the model `stt.local.model` names in the catalog (`catalog`), run by whisper.cpp
//!   (cargo feature `whisper`), sherpa-onnx (Parakeet, Moonshine; cargo feature `parakeet`) or, on
//!   macOS, CoreML (`coreml`: Parakeet v2 on the Neural Engine, the default; dot-4uzgol.69). Where
//!   CoreML is missing or cannot load the model, its catalog fallback (sherpa-onnx Parakeet v2 on
//!   the CPU) runs instead.
//!   Model files are downloaded and checksummed by `models` (or `ci/fetch-models.sh`), never
//!   committed.
//! - `cloud`: an OpenAI-compatible HTTP adapter (`stt.cloud.adapter`), gated by `network.allow`,
//!   its key a `secret:` reference resolved by the secrets module.
//!
//! A recording is two channels: the microphone is "you", system audio is "them". `Live`
//! transcribes fixed windows as audio arrives (cut at the quietest point near the window end);
//! the final pass transcribes each whole channel again. Both write OpenOats-layout JSONL into the
//! session store (`transcript.live.jsonl`, `transcript.final.jsonl`), speaker ids `you` and
//! `remote_1`. With `stt.diarization.enabled` the system-audio channel is split by voice
//! (`diarize`, the algorithm `stt.diarization.algorithm` picks): `remote_1`, `remote_2`, ... one
//! id per remote speaker; the mic stays "you", or with `speakers.mic.diarize` is split into
//! `room_1`, `room_2` ... The ids are stable; `crate::speakers` labels them for display.
//!
//! Stable error codes: `backend_unavailable`, `model_not_found`, `model_load_failed`,
//! `asr_failed`, `network_denied`, `invalid_combination`, `secret_*` (see secrets), `stt_timeout`,
//! `stt_failed`, `stt_http_status`, `stt_bad_response`, `bad_audio`, `invalid_value`, `exists`,
//! `nothing_enabled`.

pub mod capture;
pub mod catalog;
pub mod cloud;
#[cfg(target_os = "macos")]
pub mod coreml;
pub mod diarize;
pub mod idle;
#[cfg(feature = "whisper")]
pub mod local;
pub mod mel;
pub mod models;
#[cfg(feature = "parakeet")]
pub mod sherpa;
pub mod tdt;
pub mod wav;
pub mod wer;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Weak};

use chrono::{DateTime, Duration, SecondsFormat, Utc};
use serde_json::{json, Value};

use crate::action::Action;
use crate::config::typed::{Config, SttAdapter, SttBackend, SttProvider};
use crate::config::{ConfigError, Result};
use crate::control::bus::Bus;
use crate::control::engine::Engine;
use crate::control::subsystem::Subsystem;
use crate::sessions::Store;

use wav::{Channels, SAMPLE_RATE};

#[derive(Clone, Debug, PartialEq)]
pub struct Segment {
    pub start: f64,
    pub end: f64,
    pub text: String,
}

/// A speech recogniser over 16 kHz mono f32 audio. Segment times are seconds from the start of
/// the audio passed in.
pub trait Asr: Send + Sync {
    fn name(&self) -> &'static str;
    /// Where inference runs (`cpu`, `coreml`).
    fn provider(&self) -> &str {
        "cpu"
    }
    fn transcribe(&self, pcm: &[f32], language: &str) -> Result<Vec<Segment>>;
}

/// One recognised stretch of one speaker, in recording time.
#[derive(Clone, Debug, PartialEq)]
pub struct Utt {
    pub speaker: String,
    pub text: String,
    pub start: f64,
    pub end: f64,
}

/// The audio-shaping tunables (`stt.speech_peak`, `stt.pad`, `stt.final_piece`, `stt.feed`),
/// converted to samples at the recogniser's rate, and the echo dedupe.
#[derive(Clone, Copy, Debug)]
pub struct Tuning {
    /// A 10 ms frame whose peak is below this is silence.
    pub speech_peak: f32,
    pub pad_samples: usize,
    pub final_piece_samples: usize,
    pub feed_samples: usize,
    /// `None` when `audio.echo_cancel` is off.
    pub echo: Option<EchoDedupe>,
}

/// Speaker echo the canceller left behind (dot-4uzgol.65): a "you" utterance that says what
/// remote speech said at nearly the same time is the speakers heard by the mic, not the user.
/// The echo is often misheard ("Oaks meeting" for "Oats Meetings", dot-4uzgol.75), so words
/// match by spelling similarity, not only exactly.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EchoDedupe {
    /// `stt.echo_window` plus `audio.echo_delay`, seconds.
    pub window: f64,
    /// `stt.echo_overlap` as a fraction of the "you" utterance's words.
    pub overlap: f32,
    /// `stt.echo_similarity` as a fraction: the least letter edit-distance similarity at which
    /// two words count as the same.
    pub similarity: f32,
    /// `stt.echo_short`: a "you" utterance of at most this many words that repeats part of a
    /// remote utterance starting at the same moment is echo unless the mic is louder than bleed.
    pub short: usize,
    /// `stt.echo_bleed` as a fraction: the mic peak, against the remote channel's over the same
    /// moment, at or below which a short repeat is the speakers heard by the mic.
    pub bleed: f32,
    /// `stt.echo_sentence` as a fraction: the share of a "you" utterance's words, and of its
    /// content words, that must recur in order in the remote speech near it for it to be echo
    /// whatever the word timing (`says_sentence_of`).
    pub sentence: f32,
    /// `stt.echo_track` as a fraction: the least correlation of a "you" utterance's mic energy
    /// envelope with the remote channel's at the echo lag for it to be echo whatever its words
    /// (`Envelopes::tracks`); 0 turns this off.
    pub track: f32,
    /// `stt.echo_track_swing` as a fraction: the least swing (standard deviation over mean) of
    /// the remote envelope over the utterance for `track` to apply; a steady sound has no
    /// envelope to follow.
    pub swing: f32,
}

fn samples_of(d: crate::config::typed::Dur) -> usize {
    (d.0 as u128 * u128::from(SAMPLE_RATE) / 1000) as usize
}

impl Tuning {
    pub fn from_config(cfg: &Config) -> Tuning {
        let c = &cfg.stt;
        Tuning {
            speech_peak: c.speech_peak as f32 / 1000.0,
            pad_samples: samples_of(c.pad),
            final_piece_samples: samples_of(c.final_piece).max(1),
            feed_samples: samples_of(c.feed).max(1),
            echo: cfg.audio.echo_cancel.then(|| EchoDedupe {
                window: (c.echo_window.0 + cfg.audio.echo_delay.0) as f64 / 1000.0,
                overlap: c.echo_overlap as f32 / 100.0,
                similarity: c.echo_similarity as f32 / 100.0,
                short: c.echo_short as usize,
                bleed: c.echo_bleed as f32 / 100.0,
                sentence: c.echo_sentence as f32 / 100.0,
                track: c.echo_track as f32 / 100.0,
                swing: c.echo_track_swing as f32 / 100.0,
            }),
        }
    }
}

fn words(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_lowercase)
        .collect()
}

/// Letter edit-distance similarity of two words: 1 minus the Levenshtein distance over the
/// longer word's length ("oaks" / "oats" 0.75, "meeting" / "meetings" 0.875).
fn similarity(a: &str, b: &str) -> f32 {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let longest = a.len().max(b.len());
    if longest == 0 {
        return 1.0;
    }
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.iter().enumerate() {
        let mut diag = row[0];
        row[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let next = (diag + usize::from(ca != cb))
                .min(row[j] + 1)
                .min(row[j + 1] + 1);
            diag = row[j + 1];
            row[j + 1] = next;
        }
    }
    (longest - row[b.len()]) as f32 / longest as f32
}

/// The words of `u`, each at its estimated time: spread over the utterance by letters, as if
/// spoken at an even rate (the recognisers give times per utterance, not per word).
fn timed_words(u: &Utt) -> Vec<(String, f64)> {
    let w = words(&u.text);
    let letters = w.iter().map(|x| x.chars().count()).sum::<usize>().max(1) as f64;
    let mut before = 0.0;
    w.into_iter()
        .map(|x| {
            let n = x.chars().count() as f64;
            let at = u.start + (u.end - u.start) * (before + n / 2.0) / letters;
            before += n;
            (x, at)
        })
        .collect()
}

/// Words a conversation shares whatever is said: function words and short replies ("yeah",
/// "okay", "right"). Both sides say them all the time, so they never make an utterance echo on
/// their own (dot-4uzgol.94).
const COMMON: &[&str] = &[
    "a", "about", "after", "again", "ah", "all", "also", "am", "an", "and", "any", "are", "as",
    "at", "be", "been", "but", "by", "can", "could", "did", "do", "does", "doing", "done", "for",
    "from", "get", "go", "going", "got", "had", "has", "have", "he", "her", "here", "hi", "him",
    "his", "hmm", "how", "i", "if", "in", "is", "it", "its", "just", "know", "like", "ll", "m",
    "me", "mhm", "mm", "my", "no", "not", "now", "of", "oh", "ok", "okay", "on", "one", "or",
    "our", "out", "really", "right", "s", "she", "so", "some", "sure", "t", "that", "the", "their",
    "them", "then", "there", "they", "think", "this", "to", "uh", "um", "up", "us", "ve", "was",
    "we", "well", "were", "what", "when", "which", "who", "will", "with", "would", "yeah", "yep",
    "yes", "you", "your",
];

fn common(w: &str) -> bool {
    COMMON.binary_search(&w).is_ok()
}

/// The `you` words that `t` says, or nearly says (`similarity`), at the same moment: each word
/// matched to a distinct word of `t` estimated within the window of it: (content words, all).
fn matched(w: &[(String, f64)], t: &Utt, d: &EchoDedupe) -> (usize, usize) {
    let mut near: Vec<(String, f64, bool)> = timed_words(t)
        .into_iter()
        .map(|(x, at)| (x, at, false))
        .collect();
    let (mut content, mut all) = (0, 0);
    for (x, at) in w {
        let best = near
            .iter_mut()
            .filter(|(t, t_at, used)| {
                !used
                    && (t_at - at).abs() <= d.window
                    && (t == x || similarity(x, t) >= d.similarity)
            })
            .min_by(|a, b| (a.1 - at).abs().total_cmp(&(b.1 - at).abs()));
        if let Some(m) = best {
            m.2 = true;
            all += 1;
            content += usize::from(!common(x));
        }
    }
    (content, all)
}

/// `w` says, word for word or nearly, a run of `t`'s words (its first words, mostly) at the
/// same moment.
fn repeats_part_of(w: &[(String, f64)], t: &Utt, d: &EchoDedupe) -> bool {
    let tw = timed_words(t);
    let same = |a: &str, b: &str| a == b || similarity(a, b) >= d.similarity;
    !w.is_empty()
        && tw.windows(w.len()).any(|run| {
            (run[0].1 - w[0].1).abs() <= d.window
                && run.iter().zip(w).all(|((a, _), (b, _))| same(a, b))
        })
}

/// Most words of `w` the same, or nearly, and in the same order, in `them`: the words of the
/// remote utterances near it, in time order, as one stream (dot-4uzgol.98). An echo says one
/// remote sentence again whatever the capture latency; the recognisers' sentence and window cuts
/// differ between the channels, so it may span two remote utterances, and the words' estimated
/// times (spread evenly over an utterance) are too rough to match one by one. `sentence` of all
/// its words and of its content words must match within a run of remote words not much longer
/// than it, so double talk that shares words, even a phrase, with remote speech is not echo.
fn says_sentence_of(w: &[(String, f64)], them: &[&Utt], d: &EchoDedupe) -> bool {
    let n = w.len();
    let content = w.iter().filter(|(x, _)| !common(x)).count();
    if n <= d.short || content == 0 {
        return false;
    }
    let mut near: Vec<&&Utt> = them.iter().collect();
    near.sort_by(|a, b| a.start.total_cmp(&b.start));
    let stream: Vec<String> = near.iter().flat_map(|t| words(&t.text)).collect();
    let need = |of: usize| (d.sentence * of as f32).ceil() as usize;
    let (need_all, need_content) = (need(n), need(content));
    let span = (n + n - need_all).min(stream.len());
    let same = |a: &str, b: &str| a == b || similarity(a, b) >= d.similarity;
    if stream.len() < need_all {
        return false;
    }
    (0..=stream.len() - span).any(|at| {
        // Longest common subsequence of `w` and this run, counting all and content words.
        let run = &stream[at..at + span];
        let mut row = vec![(0usize, 0usize); span + 1];
        for (x, _) in w {
            let mut diag = (0, 0);
            for (j, y) in run.iter().enumerate() {
                let up = row[j + 1];
                let next = if same(x, y) {
                    (diag.0 + 1, diag.1 + usize::from(!common(x)))
                } else {
                    up.max(row[j])
                };
                diag = up;
                row[j + 1] = next;
            }
        }
        let (all, content) = row[span];
        all >= need_all && content >= need_content
    })
}

/// The loudest sample of `x` from `from` to `to` seconds.
fn peak_between(x: &[f32], from: f64, to: f64) -> f32 {
    let at = |secs: f64| ((secs.max(0.0) * f64::from(SAMPLE_RATE)) as usize).min(x.len());
    peak(&x[at(from)..at(to).max(at(from))])
}

/// The mic is louder over `you` than the speakers' bleed of the remote channel at that moment
/// (`bleed` of its peak, within the window before `you` for the echo's delay): someone near the
/// mic speaking.
fn near_end(you: &Utt, heard: &Channels, d: &EchoDedupe) -> bool {
    let mic = peak_between(&heard.you, you.start, you.end);
    let far = peak_between(&heard.them, you.start - d.window, you.end);
    mic > d.bleed * far
}

/// `you` is speaker echo when one remote utterance overlapping it says most of its words
/// (`overlap`, nearly the same spelling counting, `similarity`) at the same moment, and most of
/// its content words: words outside `COMMON`. Echo repeats one remote sentence where it was said.
/// A bag of every remote word near it also matched double talk that shares common words ("I
/// think we", "the review") with long remote sentences (dot-4uzgol.93); time overlap plus a few
/// common words or replies ("yeah", "okay, right") is not echo either (dot-4uzgol.94).
///
/// A short `you` utterance (`short` words or fewer) that repeats a run of words of a remote
/// utterance starting within the window of it ("This" as the speakers say "This is the
/// remote side...") is echo too, common words or not, unless the recorded channels (`heard`)
/// show the mic louder than bleed (`near_end`): a reply at the same moment (dot-4uzgol.95).
/// Without the audio, an utterance of common words alone is kept.
///
/// A longer one that says nearly all of the remote sentence near it, in order, is echo however
/// its word times line up (`says_sentence_of`, dot-4uzgol.98): a capture path's latency or the
/// recognisers' cuts move the words, not the sentence.
pub fn is_echo(you: &Utt, them: &[Utt], d: &EchoDedupe, heard: Option<&Channels>) -> bool {
    let w = timed_words(you);
    let content = w.iter().filter(|(x, _)| !common(x)).count();
    let most = |hits: usize, of: usize| hits as f32 >= d.overlap * of as f32;
    let near: Vec<&Utt> = them
        .iter()
        .filter(|t| t.start <= you.end + d.window && t.end >= you.start - d.window)
        .collect();
    if content > 0
        && near.iter().any(|t| {
            let (c, a) = matched(&w, t, d);
            most(c, content) && most(a, w.len())
        })
    {
        return true;
    }
    if says_sentence_of(&w, &near, d) {
        return true;
    }
    let Some(heard) = heard else { return false };
    w.len() <= d.short
        && near
            .iter()
            .any(|t| (t.start - you.start).abs() <= d.window && repeats_part_of(&w, t, d))
        && !near_end(you, heard, d)
}

/// The 10 ms RMS envelopes of the mic and remote channels from one moment on, and the lag at
/// which the mic's follows the remote's best (dot-4uzgol.101).
///
/// The speakers can distort the far end so that the echo the canceller leaves is misrecognised
/// ("Sons could the mean team" for "Sounds good to me"): no word matches, yet its loudness rises
/// and falls with the remote speech a fixed lag earlier. Local speech, even over the far end,
/// does not. The lag is measured once over all the audio (the echo is the one thing the two
/// channels share), within `EchoDedupe::window` either way.
pub struct Envelopes {
    /// Seconds of the first frame.
    base: f64,
    mic: Vec<f32>,
    far: Vec<f32>,
    /// Frames the mic lags the remote channel (negative: the remote channel is the late one).
    lag: isize,
}

fn envelope(x: &[f32]) -> Vec<f32> {
    x.chunks(SAMPLE_RATE as usize / 100)
        .map(|c| (c.iter().map(|v| v * v).sum::<f32>() / c.len() as f32).sqrt())
        .collect()
}

/// Pearson correlation of two equally long series; 0 when either is flat.
fn correlation(a: &[f32], b: &[f32]) -> f32 {
    let n = a.len().min(b.len());
    if n == 0 {
        return 0.0;
    }
    let mean = |x: &[f32]| x[..n].iter().map(|v| f64::from(*v)).sum::<f64>() / n as f64;
    let (ma, mb) = (mean(a), mean(b));
    let (mut sab, mut saa, mut sbb) = (0.0f64, 0.0f64, 0.0f64);
    for (x, y) in a[..n].iter().zip(&b[..n]) {
        let (x, y) = (f64::from(*x) - ma, f64::from(*y) - mb);
        sab += x * y;
        saa += x * x;
        sbb += y * y;
    }
    if saa <= 0.0 || sbb <= 0.0 {
        return 0.0;
    }
    (sab / (saa * sbb).sqrt()) as f32
}

impl Envelopes {
    /// `mic` and `far` both start at `base` seconds.
    pub fn new(mic: &[f32], far: &[f32], base: f64, d: &EchoDedupe) -> Envelopes {
        let (mic, far) = (envelope(mic), envelope(far));
        let reach = (d.window * 100.0).round() as isize;
        let n = mic.len().min(far.len()) as isize;
        let mut best = (f32::MIN, 0);
        for k in -reach..=reach {
            // mic[i] against far[i - k], over the frames both have.
            let (lo, hi) = (k.max(0), n.min(n + k));
            if hi - lo < 2 {
                continue;
            }
            let r = correlation(
                &mic[lo as usize..hi as usize],
                &far[(lo - k) as usize..(hi - k) as usize],
            );
            if r > best.0 {
                best = (r, k);
            }
        }
        Envelopes {
            base,
            mic,
            far,
            lag: best.1,
        }
    }

    /// The echo lag measured, seconds.
    pub fn lag_secs(&self) -> f64 {
        self.lag as f64 / 100.0
    }

    /// `you`'s mic envelope follows the remote channel's at the measured lag (a frame either way
    /// for the 10 ms grid) with at least `track` correlation, where the remote envelope swings
    /// by at least `swing`: the speakers heard by the mic, however it was recognised.
    pub fn tracks(&self, you: &Utt, d: &EchoDedupe) -> bool {
        if d.track <= 0.0 {
            return false;
        }
        let at = |secs: f64| ((secs - self.base) * 100.0).round().max(0.0) as isize;
        let (from, to) = (at(you.start), at(you.end).min(self.mic.len() as isize));
        if to - from < 2 {
            return false;
        }
        let m = &self.mic[from as usize..to as usize];
        (self.lag - 1..=self.lag + 1).any(|k| {
            let (lo, hi) = (from - k, to - k);
            if lo < 0 || hi > self.far.len() as isize {
                return false;
            }
            let f = &self.far[lo as usize..hi as usize];
            let mean = f.iter().sum::<f32>() / f.len() as f32;
            let sd =
                (f.iter().map(|v| (v - mean) * (v - mean)).sum::<f32>() / f.len() as f32).sqrt();
            mean > 0.0 && sd >= d.swing * mean && correlation(m, f) >= d.track
        })
    }
}

/// What became of the mic ("you") channel in one pass (dot-4uzgol.93), to find the stage that
/// loses near-end speech. The echo canceller (`aec`, `audio.echo_cancel`) runs while recording,
/// so the recorded mic and every count here are after it.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct MicStages {
    pub aec: bool,
    /// Speech stretches on the mic the level gate (`stt.speech_peak`) passes, and their seconds;
    /// `None` in the live pass, which sees the mic window by window.
    pub vad: Option<(usize, f64)>,
    /// "you" utterances the recogniser returned.
    pub recognised: usize,
    /// Of those, dropped as echo of remote speech.
    pub echo_dropped: usize,
    pub kept: usize,
}

impl MicStages {
    pub fn to_json(&self) -> Value {
        let mut v = json!({
            "aec": self.aec, "recognised": self.recognised,
            "echo_dropped": self.echo_dropped, "kept": self.kept,
        });
        if let Some((regions, secs)) = self.vad {
            v["vad_regions"] = json!(regions);
            v["vad_seconds"] = json!((secs * 100.0).round() / 100.0);
        }
        v
    }
}

/// The level gate over all of `x`: runs of non-silent 10 ms frames, joined across gaps the
/// `stt.pad` on both sides would bridge; (count, seconds of non-silent frames).
fn gate_stats(x: &[f32], t: &Tuning) -> (usize, f64) {
    let frame = SAMPLE_RATE as usize / 100;
    let bridge = (2 * t.pad_samples).div_ceil(frame);
    let (mut regions, mut loud, mut gap) = (0, 0, None::<usize>);
    for c in x.chunks(frame) {
        if peak(c) >= t.speech_peak {
            if gap.is_none_or(|g| g > bridge) {
                regions += 1;
            }
            loud += 1;
            gap = Some(0);
        } else if let Some(g) = gap.as_mut() {
            *g += 1;
        }
    }
    (regions, loud as f64 / 100.0)
}

/// Drop every `you` utterance that is echo of remote speech (no-op when `d` is `None`), with
/// the recorded channels (`heard`) when there are any: by its words (`is_echo`) or by its
/// loudness following the remote channel's (`Envelopes::tracks`).
fn drop_echoes(
    utts: Vec<Utt>,
    you: &str,
    d: Option<EchoDedupe>,
    heard: Option<&Channels>,
) -> Vec<Utt> {
    let Some(d) = d else { return utts };
    let them: Vec<Utt> = utts.iter().filter(|u| u.speaker != you).cloned().collect();
    let env = heard
        .filter(|_| d.track > 0.0)
        .map(|h| Envelopes::new(&h.you, &h.them, 0.0, &d));
    utts.into_iter()
        .filter(|u| {
            u.speaker != you
                || !(is_echo(u, &them, &d, heard) || env.as_ref().is_some_and(|e| e.tracks(u, &d)))
        })
        .collect()
}

// ---- settings ------------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct Settings {
    pub backend: SttBackend,
    pub adapter: SttAdapter,
    pub language: String,
    /// The mic channel's id (`speakers::YOU`).
    pub speaker_you: String,
    /// The system-audio channel's id before diarization (`speakers::REMOTE_1`).
    pub speaker_them: String,
    pub live_enabled: bool,
    pub chunk_secs: f64,
    pub final_enabled: bool,
    /// The catalog entry this build runs for `stt.local.model` (its fallback where the build has
    /// no runtime for it); its runtime runs it.
    pub entry: &'static catalog::Entry,
    /// `entry`'s id.
    pub model: String,
    /// `stt.local.model` as configured (differs from `model` when the fallback stands in).
    pub requested: String,
    /// The model file (whisper.cpp) or directory (sherpa-onnx).
    pub model_file: PathBuf,
    /// Where downloaded models live (`stt.local.models_dir`, else `<data>/models`).
    pub models_dir: PathBuf,
    /// `stt.local.model_path` is set: the file is the user's to provide.
    pub explicit_model_path: bool,
    /// Download mirror (`stt.local.model_url`); empty means the catalog URLs.
    pub model_url: String,
    pub auto_download: bool,
    pub threads: u32,
    /// `stt.local.unload_after`: release the loaded models after this long unused.
    pub unload_after: std::time::Duration,
    pub provider: SttProvider,
    /// `stt.local.piece` in samples: the longest stretch a sherpa-onnx model decodes at once.
    pub piece_samples: usize,
    /// `stt.local.tdt_*`: the CoreML TDT decoder's limits.
    pub tdt: TdtLimits,
    pub cloud_url: String,
    pub cloud_model: String,
    pub cloud_key: String,
    pub cloud_timeout: std::time::Duration,
    pub max_key_len: usize,
    pub error_snippet: usize,
    pub max_response: u64,
    pub tuning: Tuning,
    /// `stt.diarization.*`; `None` when it is off.
    pub diarize: Option<diarize::Diarization>,
    /// The same for the mic channel (`speakers.mic.diarize`); `None` when it is off.
    pub mic_diarize: Option<diarize::Diarization>,
}

fn invalid(key: &str, why: impl std::fmt::Display) -> ConfigError {
    ConfigError::new("invalid_value", format!("stt.{key}: {why}"))
}

impl Settings {
    /// Parse and check every `stt.*` value. `sessions_dir` anchors the default models directory.
    pub fn from_config(cfg: &Config, sessions_dir: &Path) -> Result<Settings> {
        let c = &cfg.stt;
        if c.language.trim().is_empty() {
            return Err(invalid("language", "must not be empty"));
        }
        let requested = c.local.model.trim();
        let entry = catalog::get(requested)
            .ok_or_else(|| {
                invalid(
                    "local.model",
                    format!("'{requested}' is not in the model catalog (see stt-model-list)"),
                )
            })?
            .resolved();
        let model = entry.id.as_str();
        let models_dir = if c.local.models_dir.trim().is_empty() {
            sessions_dir.parent().unwrap_or(sessions_dir).join("models")
        } else {
            PathBuf::from(c.local.models_dir.trim())
        };
        let model_file = if c.local.model_path.trim().is_empty() {
            entry.location(&models_dir)
        } else {
            PathBuf::from(c.local.model_path.trim())
        };
        let diarize = diarize::Diarization::from_config(cfg, &models_dir)?;
        let mic_diarize = diarize::Diarization::mic_from_config(cfg, &models_dir)?;
        Ok(Settings {
            backend: c.backend,
            adapter: c.cloud.adapter,
            language: c.language.trim().to_string(),
            speaker_you: crate::speakers::YOU.to_string(),
            speaker_them: crate::speakers::REMOTE_1.to_string(),
            live_enabled: c.live.enabled,
            chunk_secs: c.live.chunk.0 as f64 / 1000.0,
            final_enabled: c.final_.enabled,
            entry,
            model: model.to_string(),
            requested: requested.to_string(),
            model_file,
            models_dir,
            explicit_model_path: !c.local.model_path.trim().is_empty(),
            model_url: c.local.model_url.trim().to_string(),
            auto_download: c.local.auto_download,
            threads: c.local.threads,
            unload_after: c.local.unload_after.as_duration(),
            provider: c.local.provider,
            piece_samples: samples_of(c.local.piece).max(1),
            tdt: TdtLimits {
                max_symbols: c.local.tdt_max_symbols.max(1) as usize,
                max_tokens: c.local.tdt_max_tokens.max(1) as usize,
                tail_blanks: c.local.tdt_tail_blanks.max(1) as usize,
            },
            cloud_url: c.cloud.url.trim().to_string(),
            cloud_model: c.cloud.model.trim().to_string(),
            cloud_key: c.cloud.key.trim().to_string(),
            cloud_timeout: c.cloud.timeout.as_duration(),
            max_key_len: cfg.secrets.max_key_len as usize,
            error_snippet: cfg.network.error_snippet as usize,
            max_response: c.cloud.max_response,
            tuning: Tuning::from_config(cfg),
            diarize,
            mic_diarize,
        })
    }

    /// Extra checks that only matter when the cloud backend is selected.
    pub fn check_cloud(&self) -> Result<()> {
        if self.adapter == SttAdapter::None {
            return Err(ConfigError::new(
                "invalid_combination",
                "stt.backend = \"cloud\" needs stt.cloud.adapter other than \"none\"",
            ));
        }
        let u = self.cloud_url.to_ascii_lowercase();
        if !(u.starts_with("http://") || u.starts_with("https://")) || u.len() < 9 {
            return Err(invalid(
                "cloud.url",
                format!("'{}' is not an http(s) URL", self.cloud_url),
            ));
        }
        if self.cloud_model.is_empty() {
            return Err(invalid("cloud.model", "must not be empty"));
        }
        if !self.cloud_key.is_empty() {
            crate::secrets::require_ref(&self.cloud_key, "stt.cloud.key")?;
        }
        Ok(())
    }

    /// Engine label for `session.json`.
    pub fn engine_label(&self) -> String {
        match self.backend {
            SttBackend::Local => format!("{}:{}", self.entry.runtime.label(), self.model),
            SttBackend::Cloud => format!("{}:{}", "openai-compatible", self.cloud_model),
        }
    }
}

/// Build the recogniser `settings` selects. `network_allow` and the secrets config come from the
/// resolved config.
pub fn make_asr(
    s: &Settings,
    network_allow: bool,
    secrets: &dyn crate::secrets::SecretStore,
) -> Result<Arc<dyn Asr>> {
    idle::touch(s.unload_after);
    match s.backend {
        SttBackend::Local => local_asr(s),
        SttBackend::Cloud => {
            s.check_cloud()?;
            if !network_allow {
                return Err(ConfigError::new(
                    "network_denied",
                    "the cloud STT backend needs network.allow = true",
                ));
            }
            let key = if s.cloud_key.is_empty() {
                None
            } else {
                Some(crate::secrets::resolve(
                    secrets,
                    &s.cloud_key,
                    s.max_key_len,
                )?)
            };
            Ok(Arc::new(cloud::OpenAiAsr {
                url: s.cloud_url.clone(),
                model: s.cloud_model.clone(),
                key,
                timeout: s.cloud_timeout,
                snippet: s.error_snippet,
                max_response: s.max_response,
            }))
        }
    }
}

fn local_asr(s: &Settings) -> Result<Arc<dyn Asr>> {
    match s.entry.runtime {
        catalog::Runtime::WhisperCpp => whisper_asr(s),
        catalog::Runtime::SherpaOnnx => sherpa_asr(s),
        catalog::Runtime::CoreMl => coreml_asr(s),
    }
}

/// With `stt.local.auto_download`, start fetching the speaker-embedding model the diarizer wants
/// when it is missing (dot-4uzgol.83), in the background; until it lands the diarizer falls back
/// down its chain. True when a download is under way.
pub fn fetch_speaker_model(s: &Settings, bus: Option<Arc<Bus>>) -> bool {
    let wanted = s.diarize.as_ref().or(s.mic_diarize.as_ref());
    let Some(m) = wanted.and_then(|d| d.wanted_model()) else {
        return false;
    };
    if !s.auto_download || m.entry.present(&m.path) {
        return false;
    }
    match models::for_model(s, &m.entry.id) {
        Ok(t) => models::spawn(&t, bus, Box::new(|| {})).is_ok(),
        Err(_) => false,
    }
}

/// The decoder limits `stt.local.tdt_*` set (CoreML TDT models).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TdtLimits {
    pub max_symbols: usize,
    pub max_tokens: usize,
    pub tail_blanks: usize,
}

/// Why the CoreML model in a directory is not the one running: the fallback model id and the
/// CoreML error, set when loading it failed and the fallback took over (dot-4uzgol.69).
static FALLBACK: std::sync::Mutex<Option<(PathBuf, String, String)>> = std::sync::Mutex::new(None);

fn fallback_for(dir: &Path) -> Option<(String, String)> {
    FALLBACK
        .lock()
        .expect("stt fallback")
        .as_ref()
        .filter(|f| f.0 == dir)
        .map(|f| (f.1.clone(), f.2.clone()))
}

fn set_fallback(v: Option<(PathBuf, String, String)>) {
    *FALLBACK.lock().expect("stt fallback") = v;
}

#[cfg(any(target_os = "macos", feature = "parakeet"))]
fn not_downloaded(s: &Settings) -> ConfigError {
    ConfigError::new(
        "model_not_found",
        format!(
            "model {} not found at {}; download it with `ctl stt model fetch` (or the Download button in Models), or set stt.local.model_path",
            s.model,
            s.model_file.display()
        ),
    )
}

#[cfg(target_os = "macos")]
fn coreml_asr(s: &Settings) -> Result<Arc<dyn Asr>> {
    if !s.entry.present(&s.model_file) {
        return Err(not_downloaded(s));
    }
    match coreml::CoreMlAsr::load(s.entry, &s.model_file, s.provider, s.tdt) {
        Ok(a) => {
            set_fallback(None);
            Ok(Arc::new(a.with_piece(s.piece_samples)))
        }
        Err(e) => coreml_fallback(s, e),
    }
}

#[cfg(not(target_os = "macos"))]
fn coreml_asr(s: &Settings) -> Result<Arc<dyn Asr>> {
    coreml_fallback(
        s,
        ConfigError::new(
            "backend_unavailable",
            format!("this build has no CoreML runtime for {}", s.model),
        ),
    )
}

/// CoreML could not load `s`'s model (`why`): run the catalog fallback (sherpa-onnx Parakeet v2)
/// on the CPU instead and remember why, for stt-status.
fn coreml_fallback(s: &Settings, why: ConfigError) -> Result<Arc<dyn Asr>> {
    let Some(f) = s.entry.fallback() else {
        return Err(why);
    };
    let mut t = models::for_model(s, &f.id)?;
    t.model_file = f.location(&s.models_dir);
    t.provider = SttProvider::Cpu;
    set_fallback(Some((
        s.model_file.clone(),
        f.id.clone(),
        why.message.clone(),
    )));
    local_asr(&t).map_err(|e| {
        ConfigError::new(
            e.code,
            format!(
                "{} (CoreML could not run {}: {}; {} is its CPU fallback)",
                e.message, s.model, why.message, f.id
            ),
        )
    })
}

#[cfg(feature = "parakeet")]
fn sherpa_asr(s: &Settings) -> Result<Arc<dyn Asr>> {
    if !s.entry.present(&s.model_file) {
        return Err(not_downloaded(s));
    }
    Ok(Arc::new(
        sherpa::SherpaAsr::load(s.entry, &s.model_file, s.threads, s.provider)?
            .with_piece(s.piece_samples),
    ))
}

#[cfg(not(feature = "parakeet"))]
fn sherpa_asr(s: &Settings) -> Result<Arc<dyn Asr>> {
    Err(ConfigError::new(
        "backend_unavailable",
        format!(
            "this build has no sherpa-onnx runtime for {}; rebuild with --features oats-core/parakeet or pick a whisper model",
            s.model
        ),
    ))
}

#[cfg(feature = "parakeet")]
fn sherpa_loaded(dir: &Path) -> Option<String> {
    sherpa::loaded_provider(dir)
}

#[cfg(not(feature = "parakeet"))]
fn sherpa_loaded(_dir: &Path) -> Option<String> {
    None
}

#[cfg(target_os = "macos")]
fn coreml_loaded(dir: &Path) -> Option<String> {
    coreml::loaded_units(dir)
}

#[cfg(not(target_os = "macos"))]
fn coreml_loaded(_dir: &Path) -> Option<String> {
    None
}

/// Where inference runs, for `stt-status`: `runtime` and `active` (the onnxruntime provider, or
/// the CoreML compute units) of the loaded model, null until it is loaded; `planned`, what loading
/// would use; `fallback`, the model running in place of the configured one and why (no CoreML in
/// this build, or CoreML failed to load it).
fn provider_status(s: &Settings) -> Value {
    let mut fallback = Value::Null;
    let (runtime, active): (Option<&str>, Option<String>) = match s.entry.runtime {
        catalog::Runtime::WhisperCpp => (Some("whisper.cpp"), Some("cpu".into())),
        catalog::Runtime::SherpaOnnx => {
            let a = sherpa_loaded(&s.model_file);
            (a.as_ref().map(|_| "sherpa-onnx"), a)
        }
        catalog::Runtime::CoreMl => match fallback_for(&s.model_file) {
            Some((id, why)) => {
                let dir = catalog::get(&id).map(|e| e.location(&s.models_dir));
                let a = dir.as_deref().and_then(sherpa_loaded);
                fallback = json!({"model": id, "reason": why});
                (a.as_ref().map(|_| "sherpa-onnx"), a)
            }
            None => {
                let a = coreml_loaded(&s.model_file);
                (a.as_ref().map(|_| "coreml"), a)
            }
        },
    };
    if s.requested != s.model {
        fallback = json!({"model": s.model, "reason": format!("this build has no {} runtime for {}", catalog::get(&s.requested).map_or("", |e| e.runtime.label()), s.requested)});
    }
    let planned = match s.entry.runtime {
        catalog::Runtime::WhisperCpp => "cpu",
        catalog::Runtime::SherpaOnnx => provider_order(s.provider)[0],
        catalog::Runtime::CoreMl if s.provider == SttProvider::Cpu => "cpuOnly",
        catalog::Runtime::CoreMl => "cpuAndNeuralEngine",
    };
    json!({"setting": s.provider.as_str(), "runtime": runtime, "active": active, "planned": planned, "fallback": fallback})
}

/// The onnxruntime providers to try, in order, for `stt.local.provider` (cpu is always last).
pub fn provider_order(p: SttProvider) -> &'static [&'static str] {
    match p {
        SttProvider::Cpu => &["cpu"],
        SttProvider::Coreml => &["coreml", "cpu"],
        SttProvider::Auto if cfg!(target_os = "macos") => &["coreml", "cpu"],
        SttProvider::Auto => &["cpu"],
    }
}

#[cfg(feature = "whisper")]
fn whisper_asr(s: &Settings) -> Result<Arc<dyn Asr>> {
    if !s.model_file.is_file() {
        return Err(ConfigError::new(
            "model_not_found",
            format!(
                "whisper model {} not found at {}; download it with `ctl stt model fetch` (or the Download model button), or set stt.local.model_path",
                s.model,
                s.model_file.display()
            ),
        ));
    }
    Ok(Arc::new(local::WhisperAsr::load(&s.model_file, s.threads)?))
}

#[cfg(not(feature = "whisper"))]
fn whisper_asr(_s: &Settings) -> Result<Arc<dyn Asr>> {
    Err(ConfigError::new(
        "backend_unavailable",
        "this build has no local ASR; rebuild with --features oats-core/whisper (or use stt.backend = cloud)",
    ))
}

// ---- text and audio helpers -----------------------------------------------------------------

/// Whisper marks non-speech as `[BLANK_AUDIO]`, `(music)`, `*sighs*`: not words.
fn clean(text: &str) -> Option<String> {
    let t = text.trim();
    let tagged = (t.starts_with('[') && t.ends_with(']'))
        || (t.starts_with('(') && t.ends_with(')'))
        || (t.starts_with('*') && t.ends_with('*') && t.len() > 1);
    (!t.is_empty() && !tagged).then(|| t.to_string())
}

fn peak(x: &[f32]) -> f32 {
    x.iter().fold(0.0f32, |m, v| m.max(v.abs()))
}

/// First and last sample worth sending: the span from the first to the last non-silent 10 ms
/// frame, padded. `None` if the audio is all silence. Whisper's segment times drift inside long
/// silences, so leading silence is cut and its length added back as the offset.
fn speech_bounds(x: &[f32], t: &Tuning) -> Option<(usize, usize)> {
    let frame = SAMPLE_RATE as usize / 100;
    let loud = |c: &[f32]| peak(c) >= t.speech_peak;
    let first = x.chunks(frame).position(loud)?;
    let last = x.chunks(frame).rposition(loud)?;
    Some((
        (first * frame).saturating_sub(t.pad_samples),
        ((last + 1) * frame + t.pad_samples).min(x.len()),
    ))
}

fn utts(segs: Vec<Segment>, offset: f64, speaker: &str) -> Vec<Utt> {
    segs.into_iter()
        .filter_map(|s| {
            clean(&s.text).map(|text| Utt {
                speaker: speaker.to_string(),
                text,
                start: offset + s.start,
                end: offset + s.end,
            })
        })
        .collect()
}

fn run_asr(
    asr: &dyn Asr,
    pcm: &[f32],
    offset: f64,
    speaker: &str,
    language: &str,
    t: &Tuning,
) -> Result<Vec<Utt>> {
    let Some((from, to)) = speech_bounds(pcm, t) else {
        return Ok(vec![]);
    };
    let at = offset + from as f64 / f64::from(SAMPLE_RATE);
    Ok(utts(asr.transcribe(&pcm[from..to], language)?, at, speaker))
}

// ---- live ----------------------------------------------------------------------------------

#[derive(Default)]
struct ChanBuf {
    /// Absolute sample index of `pcm[0]`.
    base: usize,
    pcm: Vec<f32>,
}

/// Streaming transcription: push audio per channel as it arrives, get utterances back as each
/// window fills. Windows are cut at the quietest 10 ms frame in the last fifth of the window so
/// words are not split mid-way. With echo dedupe on, a "you" utterance is held until the remote
/// channel has been transcribed past it (or is far behind, or silent from the start), then
/// dropped if it repeats remote speech.
pub struct Live {
    asr: Arc<dyn Asr>,
    language: String,
    window: usize,
    tuning: Tuning,
    names: [String; 2],
    bufs: [ChanBuf; 2],
    /// "you" utterances waiting for the remote channel to catch up.
    held: Vec<Utt>,
    /// Recent remote utterances, to check the held ones against.
    heard: Vec<Utt>,
    /// Splits the remote channel by voice (`with_diarization`).
    diarizer: Option<diarize::Diarizer>,
    /// Splits the mic channel by voice (`with_mic_diarization`).
    mic_diarizer: Option<diarize::Diarizer>,
    stages: MicStages,
    /// Recent audio of both channels from sample `audio_base` on, for `Envelopes::tracks`.
    audio: [Vec<f32>; 2],
    audio_base: usize,
}

/// `pcm` cut into pieces of at most `max` samples, each cut at the quietest 10 ms frame in the
/// last fifth of the piece: `(start sample, piece)`.
pub fn pieces(pcm: &[f32], max: usize) -> Vec<(usize, &[f32])> {
    let max = max.max(SAMPLE_RATE as usize / 100);
    let (mut out, mut at) = (Vec::new(), 0);
    while pcm.len() - at > max {
        let cut = quiet_cut(&pcm[at..], max);
        out.push((at, &pcm[at..at + cut]));
        at += cut;
    }
    if at < pcm.len() || out.is_empty() {
        out.push((at, &pcm[at..]));
    }
    out
}

/// A recogniser's tokens with their start times (seconds) and, when it gives them, durations,
/// grouped into sentences: a segment ends after a token ending in `.`, `?` or `!`. `end` is where
/// the audio ends, for the last token when there are no durations. `▁` marks a word start.
pub fn sentences(tokens: &[String], times: &[f32], durs: Option<&[f32]>, end: f64) -> Vec<Segment> {
    let n = tokens.len().min(times.len());
    let mut out = Vec::new();
    let mut first = 0;
    for i in 0..n {
        let t = tokens[i].trim_end();
        let last = i + 1 == n;
        if !(last || t.ends_with(['.', '?', '!'])) {
            continue;
        }
        let stop = match durs.and_then(|d| d.get(i)) {
            Some(d) => f64::from(times[i] + d),
            None if last => end,
            None => f64::from(times[i + 1]),
        };
        let text: String = tokens[first..=i].concat().replace('\u{2581}', " ");
        let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
        if !text.is_empty() {
            out.push(Segment {
                start: f64::from(times[first]),
                end: stop.max(f64::from(times[first])),
                text,
            });
        }
        first = i + 1;
    }
    out
}

fn quiet_cut(x: &[f32], window: usize) -> usize {
    let frame = SAMPLE_RATE as usize / 100;
    let lo = window - window / 5;
    let mut best = (f32::MAX, window);
    let mut i = lo;
    while i + frame <= window {
        let e: f32 = x[i..i + frame].iter().map(|v| v * v).sum();
        if e < best.0 {
            best = (e, i + frame);
        }
        i += frame;
    }
    best.1
}

impl Live {
    pub fn new(
        asr: Arc<dyn Asr>,
        language: &str,
        chunk_secs: f64,
        tuning: Tuning,
        you: &str,
        them: &str,
    ) -> Live {
        Live {
            asr,
            language: language.to_string(),
            window: (chunk_secs * f64::from(SAMPLE_RATE)) as usize,
            tuning,
            names: [you.to_string(), them.to_string()],
            bufs: [ChanBuf::default(), ChanBuf::default()],
            held: Vec::new(),
            heard: Vec::new(),
            diarizer: None,
            mic_diarizer: None,
            stages: MicStages {
                aec: tuning.echo.is_some(),
                ..MicStages::default()
            },
            audio: [Vec::new(), Vec::new()],
            audio_base: 0,
        }
    }

    /// Label remote utterances by voice (`remote_1`, `remote_2`, ...) rather than all `them`.
    pub fn with_diarization(mut self, d: Option<diarize::Diarization>) -> Live {
        self.diarizer = d.map(diarize::Diarizer::new);
        self
    }

    /// Label mic utterances by voice (`room_1`, `room_2`, ...) rather than all `you`.
    pub fn with_mic_diarization(mut self, d: Option<diarize::Diarization>) -> Live {
        self.mic_diarizer = d.map(diarize::Diarizer::new);
        self
    }

    /// The live transcriber `s` configures.
    pub fn for_settings(asr: Arc<dyn Asr>, s: &Settings) -> Live {
        idle::touch(s.unload_after);
        Live::new(
            asr,
            &s.language,
            s.chunk_secs,
            s.tuning,
            &s.speaker_you,
            &s.speaker_them,
        )
        .with_diarization(s.diarize.clone())
        .with_mic_diarization(s.mic_diarize.clone())
    }

    /// Transcribe `chunk` of channel `ch`, which starts at `offset` seconds.
    fn recognise(&mut self, ch: usize, chunk: &[f32], offset: f64) -> Result<Vec<Utt>> {
        let mut us = run_asr(
            &*self.asr,
            chunk,
            offset,
            &self.names[ch],
            &self.language,
            &self.tuning,
        )?;
        match (ch, self.diarizer.as_mut(), self.mic_diarizer.as_mut()) {
            (1, Some(d), _) => d.label_live(&mut us, chunk, offset, &self.names[1]),
            (0, _, Some(d)) => d.label_live(&mut us, chunk, offset, crate::speakers::ROOM_1),
            _ => {}
        }
        Ok(us)
    }

    fn secs(samples: usize) -> f64 {
        samples as f64 / f64::from(SAMPLE_RATE)
    }

    /// What became of the mic channel so far (`echo_dropped` and `kept` count released ones).
    pub fn mic_stages(&self) -> MicStages {
        self.stages
    }

    /// Route fresh utterances of channel `ch`, then release the held ones that are decided.
    fn settle(&mut self, ch: usize, fresh: Vec<Utt>, all: bool) -> Vec<Utt> {
        if ch == 0 {
            self.stages.recognised += fresh.len();
        }
        let Some(d) = self.tuning.echo else {
            if ch == 0 {
                self.stages.kept += fresh.len();
            }
            return fresh;
        };
        let mut out = Vec::new();
        if ch == 0 {
            self.held.extend(fresh);
        } else {
            self.heard.extend(fresh.iter().cloned());
            out = fresh;
        }
        let them_at = Self::secs(self.bufs[1].base);
        let them_silent = self.bufs[1].base + self.bufs[1].pcm.len() == 0;
        let you_at = Self::secs(self.bufs[0].base);
        let max_lag = 2.0 * Self::secs(self.window) + d.window;
        let (ready, wait): (Vec<Utt>, Vec<Utt>) =
            std::mem::take(&mut self.held).into_iter().partition(|u| {
                all || them_silent || u.end + d.window <= them_at || you_at - u.end > max_lag
            });
        self.held = wait;
        let n = ready.len();
        let env = (n > 0 && d.track > 0.0).then(|| {
            Envelopes::new(
                &self.audio[0],
                &self.audio[1],
                Self::secs(self.audio_base),
                &d,
            )
        });
        let kept: Vec<Utt> = ready
            .into_iter()
            .filter(|u| {
                !(is_echo(u, &self.heard, &d, None)
                    || env.as_ref().is_some_and(|e| e.tracks(u, &d)))
            })
            .collect();
        self.stages.kept += kept.len();
        self.stages.echo_dropped += n - kept.len();
        out.extend(kept);
        // Later "you" utterances start at or after `you_at`.
        let horizon = self.held.iter().map(|u| u.start).fold(you_at, f64::min) - d.window;
        self.heard.retain(|t| t.end >= horizon);
        // Keep the audio of the lag search either side of the earliest utterance still to come.
        let keep = ((horizon - 2.0 * d.window).max(0.0) * f64::from(SAMPLE_RATE)) as usize;
        let cut = keep
            .saturating_sub(self.audio_base)
            .min(self.audio[0].len())
            .min(self.audio[1].len());
        for a in &mut self.audio {
            a.drain(..cut);
        }
        self.audio_base += cut;
        out
    }

    /// `channel` 0 = you, 1 = them. Returns the utterances of every window this push completed.
    pub fn push(&mut self, channel: usize, samples: &[f32]) -> Result<Vec<Utt>> {
        let ch = channel.min(1);
        self.bufs[ch].pcm.extend_from_slice(samples);
        if self.tuning.echo.is_some_and(|d| d.track > 0.0) {
            self.audio[ch].extend_from_slice(samples);
        }
        let mut fresh = Vec::new();
        while self.bufs[ch].pcm.len() >= self.window {
            let cut = quiet_cut(&self.bufs[ch].pcm, self.window);
            let chunk: Vec<f32> = self.bufs[ch].pcm.drain(..cut).collect();
            let offset = self.bufs[ch].base as f64 / f64::from(SAMPLE_RATE);
            self.bufs[ch].base += cut;
            fresh.extend(self.recognise(ch, &chunk, offset)?);
        }
        Ok(self.settle(ch, fresh, false))
    }

    /// Transcribe whatever is left in both channels.
    pub fn flush(&mut self) -> Result<Vec<Utt>> {
        let mut out = Vec::new();
        // Remote first, so the last "you" utterances are checked against all of it.
        for ch in [1, 0] {
            let chunk = std::mem::take(&mut self.bufs[ch].pcm);
            let offset = self.bufs[ch].base as f64 / f64::from(SAMPLE_RATE);
            self.bufs[ch].base += chunk.len();
            let fresh = self.recognise(ch, &chunk, offset)?;
            out.extend(self.settle(ch, fresh, ch == 0));
        }
        Ok(out)
    }
}

// ---- final ---------------------------------------------------------------------------------

/// The final pass sends `stt.final_piece` at a time (cloud uploads are size-limited).
fn final_channel(
    asr: &dyn Asr,
    pcm: &[f32],
    speaker: &str,
    language: &str,
    t: &Tuning,
) -> Result<Vec<Utt>> {
    let piece = t.final_piece_samples;
    let mut out = Vec::new();
    for (i, chunk) in pcm.chunks(piece).enumerate() {
        out.extend(run_asr(
            asr,
            chunk,
            (i * piece) as f64 / f64::from(SAMPLE_RATE),
            speaker,
            language,
            t,
        )?);
    }
    Ok(out)
}

/// The final pass over both channels: each transcribed whole, the remote one split by voice
/// (when diarization is on), echo of remote speech dropped from "you", in time order; and what
/// became of the mic channel on the way.
pub fn final_utts(asr: &dyn Asr, s: &Settings, chans: &Channels) -> Result<(Vec<Utt>, MicStages)> {
    let mut all = final_channel(asr, &chans.you, &s.speaker_you, &s.language, &s.tuning)?;
    let recognised = all.len();
    let mut them = final_channel(asr, &chans.them, &s.speaker_them, &s.language, &s.tuning)?;
    if let Some(d) = &s.diarize {
        diarize::label_final(d, &mut them, &chans.them, &s.speaker_them);
    }
    all.extend(them);
    let mut all = drop_echoes(all, &s.speaker_you, s.tuning.echo, Some(chans));
    let kept = all.iter().filter(|u| u.speaker == s.speaker_you).count();
    if let Some(d) = &s.mic_diarize {
        // After the echo is gone, so remote voices the mic picked up make no room speaker.
        let (mut mic, them): (Vec<Utt>, Vec<Utt>) =
            all.into_iter().partition(|u| u.speaker == s.speaker_you);
        diarize::label_final(d, &mut mic, &chans.you, crate::speakers::ROOM_1);
        all = mic;
        all.extend(them);
    }
    let stages = MicStages {
        aec: s.tuning.echo.is_some(),
        vad: Some(gate_stats(&chans.you, &s.tuning)),
        recognised,
        echo_dropped: recognised - kept,
        kept,
    };
    idle::touch(s.unload_after);
    Ok((by_time(all), stages))
}

/// `x_oats.micStages` in session.json: the live and final passes' `MicStages`.
fn record_stages(
    store: &Store,
    id: &str,
    live: Option<MicStages>,
    fin: Option<MicStages>,
) -> Result<Value> {
    let v = json!({
        "live": live.map(|m| m.to_json()),
        "final": fin.map(|m| m.to_json()),
    });
    store.set_ext_meta(id, MIC_STAGES, v.clone())?;
    Ok(v)
}

/// The session.json key (under `x_oats`) of the per-stage mic counts.
pub const MIC_STAGES: &str = "micStages";

fn by_time(mut v: Vec<Utt>) -> Vec<Utt> {
    // Stable: utterances that start together keep their input order (you before them).
    v.sort_by(|a, b| a.start.total_cmp(&b.start));
    v
}

fn stamp(started: DateTime<Utc>, secs: f64) -> String {
    (started + Duration::milliseconds((secs * 1000.0).round() as i64))
        .to_rfc3339_opts(SecondsFormat::Secs, true)
}

/// The OpenOats utterance object, plus our start/end offsets under `x_oats` (ignored by OpenOats).
pub fn utterance_json(u: &Utt, started: DateTime<Utc>) -> Value {
    json!({
        "speaker": u.speaker, "text": u.text, "timestamp": stamp(started, u.start),
        "x_oats": {"startSeconds": (u.start * 100.0).round() / 100.0, "endSeconds": (u.end * 100.0).round() / 100.0},
    })
}

// ---- the pipeline --------------------------------------------------------------------------

/// Transcribe `chans` into a new session `id`: live pass (if enabled) then final pass (if
/// enabled). `bus` gets a `segment` event per utterance and `session-saved` at the end.
pub fn run(
    store: &Store,
    bus: Option<&Bus>,
    asr: Arc<dyn Asr>,
    s: &Settings,
    chans: &Channels,
    id: &str,
    now: DateTime<Utc>,
) -> Result<Value> {
    if !s.live_enabled && !s.final_enabled {
        return Err(ConfigError::new(
            "nothing_enabled",
            "stt.live.enabled and stt.final.enabled are both off",
        ));
    }
    let duration = chans.seconds();
    let started = now - Duration::milliseconds((duration * 1000.0) as i64);
    let dir = store.create(
        id,
        &started.to_rfc3339_opts(SecondsFormat::Secs, true),
        "",
        &s.engine_label(),
    )?;
    store.set_meta(id, "language", json!(s.language))?;
    let (live_n, final_n, stages) = match passes(store, bus, &asr, s, chans, id, started) {
        Ok(n) => n,
        Err(e) => {
            // A failed run leaves no half-written session behind.
            let _ = std::fs::remove_dir_all(&dir);
            return Err(e);
        }
    };
    let summary = store.finish(id, &now.to_rfc3339_opts(SecondsFormat::Secs, true))?;
    if let Some(b) = bus {
        b.publish("session-saved", json!({"id": id, "change": "transcribed"}));
    }
    Ok(json!({
        "session": id, "backend": asr.name(), "duration_secs": duration,
        "live_utterances": live_n, "final_utterances": final_n, "mic_stages": stages, "summary": summary,
    }))
}

/// Transcribe `chans` (the session's own audio, which began at `started`) into the existing
/// session `id`, replacing any transcript it has (`session-transcribe`). A failed run leaves the
/// session with no transcript and its audio untouched.
pub fn into_session(
    store: &Store,
    bus: Option<&Bus>,
    asr: Arc<dyn Asr>,
    s: &Settings,
    chans: &Channels,
    id: &str,
    started: DateTime<Utc>,
) -> Result<Value> {
    if !s.live_enabled && !s.final_enabled {
        return Err(ConfigError::new(
            "nothing_enabled",
            "stt.live.enabled and stt.final.enabled are both off",
        ));
    }
    let clear = || {
        store
            .write_transcript(id, "live", &[])
            .and_then(|()| store.write_transcript(id, "final", &[]))
    };
    clear()?;
    let (live_n, final_n, stages) = match passes(store, bus, &asr, s, chans, id, started) {
        Ok(n) => n,
        Err(e) => {
            let _ = clear();
            return Err(e);
        }
    };
    store.set_meta(id, "language", json!(s.language))?;
    let summary = store.transcribed(id, &s.engine_label())?;
    if let Some(b) = bus {
        b.publish("session-saved", json!({"id": id, "change": "transcribed"}));
    }
    Ok(json!({
        "session": id, "backend": asr.name(), "duration_secs": chans.seconds(),
        "live_utterances": live_n, "final_utterances": final_n, "mic_stages": stages, "summary": summary,
    }))
}

/// The live pass (if enabled) then the final pass (if enabled) into session `id`; returns the
/// number of (live, final) utterances written and the mic stage counts (`record_stages`).
fn passes(
    store: &Store,
    bus: Option<&Bus>,
    asr: &Arc<dyn Asr>,
    s: &Settings,
    chans: &Channels,
    id: &str,
    started: DateTime<Utc>,
) -> Result<(usize, usize, Value)> {
    let emit = |kind: &str, u: &Utt| {
        if let Some(b) = bus {
            b.publish(
                "segment",
                json!({"session": id, "kind": kind, "speaker": u.speaker, "text": u.text, "start": u.start, "end": u.end}),
            );
        }
    };
    let (mut live_n, mut final_n) = (0usize, 0usize);
    let (mut live_stages, mut final_stages) = (None, None);
    if s.live_enabled {
        let mut live = Live::for_settings(Arc::clone(asr), s);
        let step = s.tuning.feed_samples;
        let total = chans.you.len().max(chans.them.len());
        let mut pos = 0;
        let mut write = |us: Vec<Utt>| -> Result<()> {
            for u in by_time(us) {
                store.append_utterance(id, "live", &utterance_json(&u, started))?;
                emit("live", &u);
                live_n += 1;
            }
            Ok(())
        };
        while pos < total {
            for (ch, pcm) in [(0usize, &chans.you), (1usize, &chans.them)] {
                let end = (pos + step).min(pcm.len());
                if pos < end {
                    write(live.push(ch, &pcm[pos..end])?)?;
                }
            }
            pos += step;
        }
        write(live.flush()?)?;
        live_stages = Some(live.mic_stages());
    }
    if s.final_enabled {
        let (all, stages) = final_utts(&**asr, s, chans)?;
        final_stages = Some(stages);
        let lines: Vec<Value> = all.iter().map(|u| utterance_json(u, started)).collect();
        store.write_transcript(id, "final", &lines)?;
        all.iter().for_each(|u| emit("final", u));
        final_n = lines.len();
    }
    let stages = record_stages(store, id, live_stages, final_stages)?;
    Ok((live_n, final_n, stages))
}

// ---- engine glue ---------------------------------------------------------------------------

fn settings_of(engine: &Engine, cfg: &Config) -> Result<Settings> {
    Settings::from_config(cfg, engine.default_sessions_dir())
}

pub fn dispatch(engine: &Engine, a: &Action) -> Result<Value> {
    let cfg = engine.resolved().typed()?;
    let s = settings_of(engine, &cfg)?;
    match a {
        Action::SttTranscribe {
            path,
            session,
            speaker,
        } => {
            let mut chans = wav::read(Path::new(path))?;
            match speaker.as_deref() {
                None | Some("you") => {}
                Some("them") => {
                    chans = Channels {
                        you: vec![],
                        them: std::mem::take(&mut chans.you),
                    }
                }
                Some(other) => {
                    return Err(ConfigError::new(
                        "invalid_argument",
                        format!("speaker must be 'you' or 'them', got {other:?}"),
                    ))
                }
            }
            let asr = make_asr(&s, cfg.network.allow, &*engine.secret_store(&cfg))?;
            let now = Utc::now();
            let id = session
                .clone()
                .unwrap_or_else(|| format!("session_{}", now.format("%Y-%m-%d_%H-%M-%S")));
            run(
                &engine.store(),
                Some(&*engine.bus),
                asr,
                &s,
                &chans,
                &id,
                now,
            )
        }
        Action::SttModelList {} => {
            let mut v = models::list(&s);
            v["network_allow"] = json!(cfg.network.allow);
            Ok(v)
        }
        Action::SttModelDelete { model } => models::delete(&s, model, Some(&*engine.bus)),
        Action::SttModelFetch { model, wait } => {
            let wait = wait.unwrap_or(false);
            let s = match model {
                Some(m) => models::for_model(&s, m)?,
                None => s,
            };
            if !s.entry.runtime.compiled() {
                return Err(ConfigError::new(
                    "backend_unavailable",
                    format!(
                        "this build has no {} runtime, so the {} model would not be used; {}",
                        s.entry.runtime.label(),
                        s.model,
                        s.entry.runtime.how_to_build()
                    ),
                ));
            }
            if wait {
                if s.entry.present(&s.model_file) {
                    return Ok(json!({"model": s.model, "path": s.model_file, "state": "present"}));
                }
                models::wait_idle(&s.model_file);
                if s.entry.present(&s.model_file) {
                    return Ok(json!({"model": s.model, "path": s.model_file, "state": "present"}));
                }
                let v = models::run(&s, Some(&*engine.bus))?;
                engine.model_arrived();
                return Ok(v);
            }
            models::spawn(&s, Some(Arc::clone(&engine.bus)), engine.on_model_arrived())
        }
        Action::SttStatus {} => {
            let key = if s.cloud_key.is_empty() {
                "none"
            } else {
                match crate::secrets::resolve(
                    &*engine.secret_store(&cfg),
                    &s.cloud_key,
                    cfg.secrets.max_key_len as usize,
                ) {
                    Ok(_) => "set",
                    Err(e) if e.code == "secret_not_found" => "missing",
                    Err(e) => e.code,
                }
            };
            Ok(json!({
                "backend": match s.backend { SttBackend::Local => "local", SttBackend::Cloud => "cloud" },
                "language": s.language, "live": s.live_enabled, "final": s.final_enabled,
                "local": {
                    "model": s.model, "requested": s.requested, "name": s.entry.name, "runtime": s.entry.runtime.label(),
                    "path": s.model_file, "present": s.entry.present(&s.model_file),
                    "compiled": s.entry.runtime.compiled(), "threads": s.threads,
                    "provider": provider_status(&s),
                    "auto_download": s.auto_download, "url": s.model_url,
                    "download": models::progress(&s.model_file).map(|p| p.to_json()),
                },
                "diarization": diarize::status(s.diarize.as_ref()),
                "problem": problem(&s),
                "cloud": {
                    "adapter": match s.adapter { SttAdapter::None => "none", SttAdapter::OpenaiCompatible => "openai-compatible" },
                    "url": s.cloud_url, "model": s.cloud_model, "network_allow": cfg.network.allow, "key": key,
                },
            }))
        }
        _ => Err(ConfigError::new("bad_request", "not an stt action")),
    }
}

/// What stops local transcription from working, for the GUI banner and `ctl`: `null` when nothing
/// does. `fix` is an action id that repairs it, when there is one.
fn problem(s: &Settings) -> Value {
    if s.backend != SttBackend::Local {
        return Value::Null;
    }
    if !s.entry.runtime.compiled() {
        return json!({"code": "backend_unavailable", "message": format!("This build has no {} runtime for the {} model, so recordings are saved as audio only. Pick a model this build can run in Models, use a build with local ASR, or set stt.backend to cloud.", s.entry.runtime.label(), s.entry.name), "fix": null});
    }
    if s.entry.present(&s.model_file) {
        return Value::Null;
    }
    let fetchable = models::check(s).is_ok();
    match models::progress(&s.model_file) {
        Some(p) if p.state == "downloading" => {
            let pct = p.to_json()["percent"]
                .as_u64()
                .map_or(String::new(), |x| format!(" {x}%"));
            json!({"code": "model_downloading", "message": format!("Downloading the {} model{pct}. Recordings made until it finishes are saved as audio only.", s.model), "fix": null})
        }
        Some(p) if p.state == "failed" => {
            let why = p.error.map(|e| e.1).unwrap_or_default();
            json!({"code": "model_download_failed", "message": format!("Downloading the {} model failed: {why}", s.model), "fix": "stt-model-fetch"})
        }
        _ => json!({
            "code": "model_not_found",
            "message": format!("The {} model is not downloaded, so recordings are saved as audio only.", s.model),
            "fix": if fetchable { json!("stt-model-fetch") } else { Value::Null },
        }),
    }
}

/// Takes part in config reload: a bad `stt.*` value vetoes the change; changed local settings
/// drop the loaded model.
pub struct SttSubsystem {
    engine: Weak<Engine>,
}

impl SttSubsystem {
    pub fn new(engine: Weak<Engine>) -> SttSubsystem {
        SttSubsystem { engine }
    }
}

impl Subsystem for SttSubsystem {
    fn name(&self) -> &'static str {
        "stt"
    }

    fn validate(&self, new: &Config) -> Result<()> {
        let dir = self
            .engine
            .upgrade()
            .map(|e| e.default_sessions_dir().to_path_buf())
            .unwrap_or_default();
        let s = Settings::from_config(new, &dir)?;
        if s.backend == SttBackend::Cloud {
            s.check_cloud()?;
        }
        Ok(())
    }

    fn apply(&self, old: &Config, new: &Config) {
        if old.stt.local != new.stt.local {
            #[cfg(feature = "whisper")]
            local::clear_cache();
            #[cfg(feature = "parakeet")]
            sherpa::clear_cache();
            #[cfg(target_os = "macos")]
            coreml::clear_cache();
            set_fallback(None);
        }
        if old.stt.diarization != new.stt.diarization
            || old.stt.local.models_dir != new.stt.local.models_dir
        {
            diarize::clear();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sessions::Store;
    use std::sync::Mutex;

    /// Says "heard N" where N is the window length in whole seconds: exercises windowing without
    /// a model. Silence (below the gate) never reaches it.
    struct Fake {
        calls: Mutex<Vec<usize>>,
    }

    impl Asr for Fake {
        fn name(&self) -> &'static str {
            "fake"
        }
        fn transcribe(&self, pcm: &[f32], _l: &str) -> Result<Vec<Segment>> {
            self.calls.lock().unwrap().push(pcm.len());
            let secs = pcm.len() as f64 / f64::from(SAMPLE_RATE);
            Ok(vec![
                Segment {
                    start: 0.0,
                    end: secs,
                    text: format!("heard {}", secs.round() as u64),
                },
                Segment {
                    start: 0.0,
                    end: 0.1,
                    text: "[BLANK_AUDIO]".into(),
                },
            ])
        }
    }

    fn tone(secs: f64) -> Vec<f32> {
        (0..(secs * f64::from(SAMPLE_RATE)) as usize)
            .map(|i| (i as f32 * 0.05).sin() * 0.3)
            .collect()
    }

    fn settings(live: bool, fin: bool) -> Settings {
        let mut c = crate::config::resolve::defaults_flat();
        c.insert("stt.live.enabled".into(), json!(live));
        c.insert("stt.final.enabled".into(), json!(fin));
        let nested = crate::config::resolve::nest(&c);
        let cfg: Config = serde_json::from_value(nested).unwrap();
        Settings::from_config(&cfg, Path::new("/tmp/x/sessions")).unwrap()
    }

    #[test]
    fn clean_drops_non_speech_tags() {
        assert_eq!(clean(" hello "), Some("hello".into()));
        for t in ["", "  ", "[BLANK_AUDIO]", "(music)", "*sighs*"] {
            assert_eq!(clean(t), None, "{t}");
        }
    }

    #[test]
    fn live_windows_offsets_and_flush() {
        let asr = Arc::new(Fake {
            calls: Mutex::new(vec![]),
        });
        let mut live = Live::new(
            asr.clone(),
            "en",
            4.0,
            settings(true, true).tuning,
            "you",
            "remote_1",
        );
        assert!(live.push(0, &tone(3.0)).unwrap().is_empty());
        let a = live.push(0, &tone(3.0)).unwrap(); // 6 s buffered: one 4 s window
        assert_eq!(a.len(), 1);
        assert_eq!((a[0].speaker.as_str(), a[0].start), ("you", 0.0));
        assert!(a[0].text.starts_with("heard "));
        live.push(1, &tone(1.0)).unwrap();
        let rest = live.flush().unwrap();
        assert_eq!(rest.len(), 2); // you: ~2 s left, them: 1 s
        let you = rest.iter().find(|u| u.speaker == "you").unwrap();
        assert!(you.start >= 3.2 && you.start <= 4.0, "{}", you.start); // second window starts at the cut
        assert_eq!(
            rest.iter().find(|u| u.speaker == "remote_1").unwrap().start,
            0.0
        );
    }

    #[test]
    fn leading_silence_is_cut_and_added_back_as_offset() {
        let asr = Arc::new(Fake {
            calls: Mutex::new(vec![]),
        });
        let mut pcm = vec![0.0f32; 16_000 * 2];
        pcm.extend(tone(1.0));
        pcm.extend(vec![0.0f32; 16_000 * 3]);
        let us = run_asr(&*asr, &pcm, 10.0, "you", "en", &settings(true, true).tuning).unwrap();
        assert_eq!(us.len(), 1);
        assert!((us[0].start - 11.75).abs() < 0.02, "{}", us[0].start); // 10 + 2 s - 0.25 s pad
        let sent = asr.calls.lock().unwrap()[0] as f64 / 16_000.0;
        assert!((sent - 1.5).abs() < 0.03, "{sent}"); // 1 s tone + 0.25 s pad each side
    }

    #[test]
    fn silence_is_not_sent_to_the_recogniser() {
        let asr = Arc::new(Fake {
            calls: Mutex::new(vec![]),
        });
        let mut live = Live::new(
            asr.clone(),
            "en",
            2.0,
            settings(true, true).tuning,
            "you",
            "remote_1",
        );
        assert!(live.push(0, &vec![0.0; 16_000 * 5]).unwrap().is_empty());
        assert!(live.flush().unwrap().is_empty());
        assert!(asr.calls.lock().unwrap().is_empty());
    }

    #[test]
    fn run_writes_openoats_layout_for_every_live_final_combination() {
        let chans = Channels {
            you: tone(5.0),
            them: tone(3.0),
        };
        for (live, fin) in [(true, true), (true, false), (false, true)] {
            let dir = tempfile::tempdir().unwrap();
            let store = Store::new(dir.path());
            let bus = Bus::default();
            let rx = bus.subscribe(vec!["segment".into()]);
            let now = DateTime::parse_from_rfc3339("2026-10-28T12:00:10Z")
                .unwrap()
                .with_timezone(&Utc);
            let r = run(
                &store,
                Some(&bus),
                Arc::new(Fake {
                    calls: Mutex::new(vec![]),
                }),
                &settings(live, fin),
                &chans,
                "session_t",
                now,
            )
            .unwrap();
            assert_eq!(
                r["live_utterances"].as_u64().unwrap() > 0,
                live,
                "{live}/{fin}"
            );
            assert_eq!(
                r["final_utterances"].as_u64().unwrap() > 0,
                fin,
                "{live}/{fin}"
            );
            let t = store.transcript("session_t").unwrap();
            let us = t["utterances"].as_array().unwrap();
            assert!(!us.is_empty());
            assert_eq!(t["source"], if fin { "final" } else { "live" });
            let speakers: std::collections::BTreeSet<&str> =
                us.iter().map(|u| u["speaker"].as_str().unwrap()).collect();
            assert_eq!(
                speakers,
                ["remote_1", "you"]
                    .into_iter()
                    .collect::<std::collections::BTreeSet<&str>>()
            );
            // Timestamps: recording started 5 s before `now`, utterances are offsets from it.
            assert!(us.iter().all(|u| u["timestamp"]
                .as_str()
                .unwrap()
                .starts_with("2026-10-28T12:00:")));
            assert_eq!(us[0]["timestamp"], "2026-10-28T12:00:05Z");
            let g = store.get("session_t").unwrap();
            assert_eq!(g["utterance_count"], us.len());
            assert_eq!(g["ended_at"], "2026-10-28T12:00:10Z");
            assert!(rx.try_recv().is_ok(), "segment events were published");
        }
    }

    #[test]
    fn nothing_enabled_and_duplicate_ids() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path());
        let chans = Channels {
            you: tone(1.0),
            them: vec![],
        };
        let now = Utc::now();
        let e = run(
            &store,
            None,
            Arc::new(Fake {
                calls: Mutex::new(vec![]),
            }),
            &settings(false, false),
            &chans,
            "s1",
            now,
        )
        .unwrap_err();
        assert_eq!(e.code, "nothing_enabled");
        run(
            &store,
            None,
            Arc::new(Fake {
                calls: Mutex::new(vec![]),
            }),
            &settings(true, true),
            &chans,
            "s1",
            now,
        )
        .unwrap();
        let e = run(
            &store,
            None,
            Arc::new(Fake {
                calls: Mutex::new(vec![]),
            }),
            &settings(true, true),
            &chans,
            "s1",
            now,
        )
        .unwrap_err();
        assert_eq!(e.code, "exists");
    }

    #[test]
    fn settings_validation() {
        let base = crate::config::resolve::defaults_flat();
        let check = |k: &str, v: Value| {
            let mut c = base.clone();
            c.insert(k.into(), v);
            let cfg: Config = serde_json::from_value(crate::config::resolve::nest(&c)).unwrap();
            Settings::from_config(&cfg, Path::new("/tmp/x/sessions")).map(|_| ())
        };
        for (k, v) in [("stt.local.model", "../evil"), ("stt.language", " ")] {
            assert_eq!(
                check(k, json!(v)).unwrap_err().code,
                "invalid_value",
                "{k}={v}"
            );
        }
        check("stt.live.chunk", json!("10s")).unwrap();
    }

    #[test]
    fn pieces_cover_the_audio_and_cut_in_the_quiet() {
        let sr = SAMPLE_RATE as usize;
        let mut pcm = tone(25.0);
        // A quiet stretch at 9.0-9.2 s: the 10 s piece is cut there, inside its last fifth.
        for v in &mut pcm[9 * sr..9 * sr + sr / 5] {
            *v = 0.0;
        }
        let p = pieces(&pcm, 10 * sr);
        assert_eq!(p.iter().map(|x| x.1.len()).sum::<usize>(), pcm.len());
        assert!(p.iter().all(|x| x.1.len() <= 10 * sr));
        assert!(
            p[0].1.len() > 9 * sr && p[0].1.len() <= 9 * sr + sr / 5,
            "{}",
            p[0].1.len()
        );
        assert_eq!(p[1].0, p[0].1.len());
        assert_eq!(pieces(&pcm[..sr], 10 * sr).len(), 1);
        assert_eq!(pieces(&[], 10 * sr), vec![(0, &[][..])]);
    }

    #[test]
    fn sentences_follow_the_token_times() {
        let toks: Vec<String> = [
            " Hello", " there", ".", " How", " are", " you", "?", " Fine",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let times = [0.1, 0.5, 0.9, 2.0, 2.3, 2.6, 2.9, 4.0];
        let s = sentences(&toks, &times, None, 5.0);
        let got: Vec<(&str, f64, f64)> = s
            .iter()
            .map(|x| (x.text.as_str(), x.start, x.end))
            .collect();
        assert_eq!(
            got,
            [
                ("Hello there.", 0.1f32 as f64, 2.0f32 as f64),
                ("How are you?", 2.0f32 as f64, 4.0f32 as f64),
                ("Fine", 4.0f32 as f64, 5.0)
            ]
        );
        let durs = [0.2; 8];
        let s = sentences(&toks, &times, Some(&durs), 5.0);
        assert!((s[0].end - 1.1).abs() < 1e-6, "{}", s[0].end);
        let bpe: Vec<String> = ["\u{2581}Let", "'s", "\u{2581}go", "."]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(
            sentences(&bpe, &[0.0, 0.1, 0.2, 0.3], None, 1.0)[0].text,
            "Let's go."
        );
        assert!(sentences(&[], &[], None, 1.0).is_empty());
    }

    #[test]
    fn the_catalog_entry_decides_runtime_location_and_label() {
        let base = crate::config::resolve::defaults_flat();
        let with = |id: &str| {
            let mut c = base.clone();
            c.insert("stt.local.model".into(), json!(id));
            c.insert("stt.local.models_dir".into(), json!("/m"));
            let cfg: Config = serde_json::from_value(crate::config::resolve::nest(&c)).unwrap();
            Settings::from_config(&cfg, Path::new("/tmp/x/sessions")).unwrap()
        };
        let p = with("parakeet-tdt-0.6b-v2");
        assert_eq!(p.entry.runtime, catalog::Runtime::SherpaOnnx);
        assert_eq!(
            p.model_file,
            Path::new("/m/sherpa-onnx-nemo-parakeet-tdt-0.6b-v2-int8")
        );
        assert_eq!(p.engine_label(), "sherpa-onnx:parakeet-tdt-0.6b-v2");
        let w = with("large-v3-turbo-q5_0");
        assert_eq!(w.entry.runtime, catalog::Runtime::WhisperCpp);
        assert_eq!(w.model_file, Path::new("/m/ggml-large-v3-turbo-q5_0.bin"));
        assert_eq!(w.engine_label(), "whisper.cpp:large-v3-turbo-q5_0");
        assert_eq!(provider_status(&w)["active"], "cpu");
        assert_eq!(provider_order(SttProvider::Cpu), ["cpu"]);
        assert_eq!(provider_order(SttProvider::Coreml), ["coreml", "cpu"]);
        assert_eq!(
            provider_order(SttProvider::Auto)[0],
            if cfg!(target_os = "macos") {
                "coreml"
            } else {
                "cpu"
            }
        );
    }

    /// dot-4uzgol.69: the default is Parakeet v2 on CoreML; a build without CoreML runs the
    /// sherpa-onnx export on the CPU instead and stt-status says so, as it does when CoreML fails
    /// to load the model.
    #[test]
    fn the_coreml_default_falls_back_to_sherpa_on_the_cpu_and_status_says_why() {
        let base = crate::config::resolve::defaults_flat();
        assert_eq!(base["stt.local.model"], "parakeet-tdt-0.6b-v2-coreml");
        let mut c = base.clone();
        c.insert("stt.local.models_dir".into(), json!("/m"));
        let cfg: Config = serde_json::from_value(crate::config::resolve::nest(&c)).unwrap();
        let s = Settings::from_config(&cfg, Path::new("/tmp/x/sessions")).unwrap();
        assert_eq!(s.requested, "parakeet-tdt-0.6b-v2-coreml");
        assert_eq!(
            s.tdt,
            TdtLimits {
                max_symbols: 10,
                max_tokens: 150,
                tail_blanks: 5
            }
        );
        let st = provider_status(&s);
        if cfg!(target_os = "macos") {
            assert_eq!(s.entry.runtime, catalog::Runtime::CoreMl);
            assert_eq!(s.model_file, Path::new("/m/parakeet-tdt-0.6b-v2-coreml"));
            assert_eq!(st["planned"], "cpuAndNeuralEngine");
            assert_eq!(st["fallback"], Value::Null);
        } else {
            assert_eq!(s.model, "parakeet-tdt-0.6b-v2");
            assert_eq!(s.entry.runtime, catalog::Runtime::SherpaOnnx);
            assert_eq!(s.engine_label(), "sherpa-onnx:parakeet-tdt-0.6b-v2");
            assert_eq!(st["fallback"]["model"], "parakeet-tdt-0.6b-v2");
            assert!(st["fallback"]["reason"]
                .as_str()
                .unwrap()
                .contains("no coreml runtime"));
        }
        // The CoreML model itself (stt-model-fetch model=, or a failed load on macOS): when
        // CoreML cannot run it, the sherpa-onnx fallback is tried on the CPU and the reason kept.
        let dir = tempfile::tempdir().unwrap();
        let mut c = base.clone();
        c.insert("stt.local.models_dir".into(), json!(dir.path()));
        let cfg: Config = serde_json::from_value(crate::config::resolve::nest(&c)).unwrap();
        let s = Settings::from_config(&cfg, Path::new("/tmp/x/sessions")).unwrap();
        let cm = models::for_model(&s, "parakeet-tdt-0.6b-v2-coreml").unwrap();
        let why = ConfigError::new("model_load_failed", "no Neural Engine here");
        let Err(e) = coreml_fallback(&cm, why) else {
            panic!("the fallback model is not downloaded")
        };
        assert!(
            e.code == "model_not_found" || e.code == "backend_unavailable",
            "{}",
            e.code
        );
        assert!(e.message.contains("no Neural Engine here"), "{}", e.message);
        assert!(e
            .message
            .contains("parakeet-tdt-0.6b-v2 is its CPU fallback"));
        let st = provider_status(&cm);
        assert_eq!(st["fallback"]["model"], "parakeet-tdt-0.6b-v2");
        assert_eq!(st["fallback"]["reason"], "no Neural Engine here");
        assert_eq!(st["active"], Value::Null, "nothing loaded");
        set_fallback(None);
        assert_eq!(provider_status(&cm)["fallback"], Value::Null);
    }

    fn tuning_with(over: &[(&str, Value)]) -> Tuning {
        let mut c = crate::config::resolve::defaults_flat();
        for (k, v) in over {
            c.insert((*k).into(), v.clone());
        }
        let cfg: Config = serde_json::from_value(crate::config::resolve::nest(&c)).unwrap();
        Tuning::from_config(&cfg)
    }

    /// stt.speech_peak at its bounds and either side of the threshold; stt.pad in samples.
    #[test]
    fn speech_peak_and_pad_boundaries() {
        let frame = SAMPLE_RATE as usize / 100;
        let mut x = vec![0.0f32; 20 * frame];
        x[10 * frame] = 0.05;
        // 50 permille is exactly the peak: loud. 51 is above it: silence.
        let t = tuning_with(&[("stt.speech_peak", json!(50)), ("stt.pad", json!("0ms"))]);
        assert_eq!(speech_bounds(&x, &t), Some((10 * frame, 11 * frame)));
        let t = tuning_with(&[("stt.speech_peak", json!(51)), ("stt.pad", json!("0ms"))]);
        assert_eq!(speech_bounds(&x, &t), None);
        let t = tuning_with(&[("stt.speech_peak", json!(1)), ("stt.pad", json!("0ms"))]);
        assert!(speech_bounds(&x, &t).is_some());
        let t = tuning_with(&[("stt.speech_peak", json!(1000))]);
        assert_eq!(speech_bounds(&x, &t), None);
        // Pad: 20 ms of it on each side, clamped to the audio.
        let t = tuning_with(&[("stt.speech_peak", json!(50)), ("stt.pad", json!("20ms"))]);
        assert_eq!(speech_bounds(&x, &t), Some((8 * frame, 13 * frame)));
        let t = tuning_with(&[("stt.speech_peak", json!(50)), ("stt.pad", json!("10s"))]);
        assert_eq!(speech_bounds(&x, &t), Some((0, 20 * frame)));
    }

    /// Says the same sentence for any audio: what the mic hears of the speakers.
    struct Parrot;

    impl Asr for Parrot {
        fn name(&self) -> &'static str {
            "parrot"
        }
        fn transcribe(&self, pcm: &[f32], _l: &str) -> Result<Vec<Segment>> {
            Ok(vec![Segment {
                start: 0.0,
                end: pcm.len() as f64 / f64::from(SAMPLE_RATE),
                text: "We ship on Friday.".into(),
            }])
        }
    }

    fn utt(speaker: &str, text: &str, start: f64) -> Utt {
        Utt {
            speaker: speaker.into(),
            text: text.into(),
            start,
            end: start + 2.0,
        }
    }

    #[test]
    fn final_drops_you_utterances_that_repeat_near_simultaneous_remote_speech() {
        let echo = tuning_with(&[]).echo;
        assert_eq!(
            echo,
            Some(EchoDedupe {
                window: 1.04,
                overlap: 0.6,
                similarity: 0.75,
                short: 2,
                bleed: 0.25,
                sentence: 0.85,
                track: 0.6,
                swing: 0.3,
            })
        );
        let all = vec![
            utt("remote_1", "We ship on Friday, right?", 10.0),
            // The speakers, heard by the mic a moment later (a fragment, other punctuation).
            utt("you", "we ship on friday", 10.3),
            // The same words long after: the user repeating them, kept.
            utt("you", "We ship on Friday.", 30.0),
            // Talking over the remote speaker with other words, kept.
            utt("you", "Not before the review.", 11.0),
        ];
        let kept = drop_echoes(all.clone(), "you", echo, None);
        let texts: Vec<&str> = kept.iter().map(|u| u.text.as_str()).collect();
        assert_eq!(
            texts,
            [
                "We ship on Friday, right?",
                "We ship on Friday.",
                "Not before the review."
            ]
        );
        assert_eq!(drop_echoes(all.clone(), "you", None, None), all);
        let off = tuning_with(&[("audio.echo_cancel", json!(false))]).echo;
        assert_eq!(off, None);
        // stt.echo_overlap 100: every word must be in the remote speech.
        let strict = tuning_with(&[("stt.echo_overlap", json!(100))]).echo;
        let partial = vec![
            utt("remote_1", "We ship the build on Friday", 0.0),
            utt("you", "we ship the build on monday", 0.0),
        ];
        assert_eq!(drop_echoes(partial.clone(), "you", strict, None).len(), 2);
        // 5 of 6 words and 2 of 3 content words (not "the", "on") >= 60%
        assert_eq!(drop_echoes(partial, "you", echo, None).len(), 1);
    }

    #[test]
    fn words_match_by_letter_edit_distance() {
        assert_eq!(similarity("oaks", "oats"), 0.75);
        assert_eq!(similarity("meeting", "meetings"), 0.875);
        assert_eq!(similarity("testing", "testing"), 1.0);
        assert_eq!(similarity("monday", "friday"), 0.5);
        assert_eq!(similarity("", "a"), 0.0);
        assert_eq!(similarity("a", "i"), 0.0);
    }

    #[test]
    fn a_misheard_echo_is_dropped_but_double_talk_is_kept() {
        // dot-4uzgol.75, carbon session_2026-11-04_20-19-01: the speakers' first sentence came
        // back on the mic misheard. Exact words matched 1 of 3, below stt.echo_overlap.
        let echo = tuning_with(&[]).echo;
        let leak = vec![
            utt("remote_1", "Testing Oats Meetings.", 5.0),
            utt("you", "Testing Oaks meeting.", 5.1),
        ];
        let kept = drop_echoes(leak.clone(), "you", echo, None);
        assert_eq!(kept, leak[..1]);
        // stt.echo_similarity 100 is the exact-word match that let it through.
        let exact = tuning_with(&[("stt.echo_similarity", json!(100))]).echo;
        assert_eq!(drop_echoes(leak, "you", exact, None).len(), 2);
        // The user talking over the same sentence with their own words, some of which are
        // near the remote ones ("meeting", "test"): true double talk, kept.
        let talk = vec![
            utt("remote_1", "Testing Oats Meetings.", 5.0),
            utt("you", "Yes, I can hear the test meeting fine.", 5.2),
        ];
        assert_eq!(drop_echoes(talk.clone(), "you", echo, None), talk);
        // The near-duplicate long after the remote speech: the user repeating it, kept.
        let later = vec![
            utt("remote_1", "Testing Oats Meetings.", 5.0),
            utt("you", "Testing Oaks meeting.", 20.0),
        ];
        assert_eq!(drop_echoes(later.clone(), "you", echo, None), later);
        // The window takes audio.echo_delay on top of stt.echo_window.
        let wide = tuning_with(&[
            ("stt.echo_window", json!("2s")),
            ("audio.echo_delay", json!("500ms")),
        ]);
        assert_eq!(wide.echo.unwrap().window, 2.5);
    }

    fn said(speaker: &str, text: &str, start: f64, end: f64) -> Utt {
        Utt {
            speaker: speaker.into(),
            text: text.into(),
            start,
            end,
        }
    }

    /// dot-4uzgol.93: the near-end side of a real meeting (David's mic) mostly vanished from the
    /// final transcript. Final-pass utterances are whole sentences, so a "you" sentence overlaps
    /// long remote sentences, and a bag of every remote word near it matched 60% of its words
    /// on common words alone. Synthetic double talk: near-end speech over far-end speech with
    /// different words, sharing only what conversation shares.
    #[test]
    fn double_talk_over_long_remote_sentences_keeps_every_near_end_utterance() {
        let echo = tuning_with(&[]).echo;
        let them = [
            said("remote_1", "So I think the main thing we need to figure out is whether the release can go out on Friday or if we have to push it to next week because of the review.", 100.0, 109.0),
            said("remote_1", "And I can take the review if that helps, I just need the notes by tomorrow so I can do it in the morning.", 109.5, 116.0),
            said("remote_2", "Is that something you can do, or do we need to ask the others to help us get it done?", 116.5, 121.0),
        ];
        let you = [
            said("you", "Yeah I think we can do that if the review is done by Thursday, I can push it out myself.", 104.0, 110.0),
            said("you", "Okay so I will send you the notes tonight and you can do the review in the morning.", 112.0, 117.5),
            said("you", "I can do it, we do not need to ask the others.", 119.0, 122.0),
            said("you", "Right.", 108.0, 108.4),
        ];
        let mut all: Vec<Utt> = them.to_vec();
        all.extend(you.iter().cloned());
        let kept = drop_echoes(all.clone(), "you", echo, None);
        assert_eq!(kept, all, "every near-end utterance survives");
        // The bag-of-words rule this replaced: 13 of the first sentence's 20 words are in the
        // remote speech around it, above stt.echo_overlap.
        let d = echo.unwrap();
        let near: std::collections::HashSet<String> =
            them.iter().flat_map(|t| words(&t.text)).collect();
        let w = words(&you[0].text);
        let bag = w.iter().filter(|x| near.contains(*x)).count();
        assert!(
            bag as f32 >= d.overlap * w.len() as f32,
            "{bag}/{}",
            w.len()
        );

        // Echo of the same remote speech still goes: the mic hearing the second half of the long
        // sentence (misheard in places), and the whole of the short one.
        let leak = [
            said("you", "if we have to push it to next weak because of the reviews", 105.6, 109.1),
            said("you", "is that something you can do or do we need to ask the others to help us get it done", 116.55, 121.05),
        ];
        let mut all: Vec<Utt> = them.to_vec();
        all.extend(leak.iter().cloned());
        assert_eq!(drop_echoes(all, "you", echo, None), them.to_vec());
    }

    /// dot-4uzgol.94: on David's meeting the dedupe still dropped 77 of 103 mic utterances. Near
    /// end and far end say different sentences at the same time, with the replies and common
    /// words a conversation shares: every near-end utterance survives, while the far end's own
    /// sentences heard by the mic still go.
    #[test]
    fn double_talk_with_shared_replies_and_common_words_is_never_echo() {
        let echo = tuning_with(&[]).echo;
        let them = [
            said("remote_1", "Yeah.", 10.0, 10.4),
            said(
                "remote_1",
                "Okay, right, so what do we do about it?",
                10.5,
                13.0,
            ),
            said("remote_2", "Right, yeah, okay.", 13.2, 14.4),
            said(
                "remote_1",
                "I think we can just ship it on Friday then.",
                15.0,
                17.5,
            ),
            said(
                "remote_2",
                "The dashboard numbers look wrong to me since the migration.",
                20.0,
                23.5,
            ),
            said(
                "remote_1",
                "Can you check the billing export before the call?",
                24.0,
                27.0,
            ),
        ];
        let you = [
            // Replies over the far end's replies: the same words at the same moment.
            said("you", "Yeah.", 10.1, 10.5),
            said("you", "Okay, right.", 10.6, 11.4),
            said("you", "Yeah, okay, right.", 13.3, 14.3),
            // Different sentences in common words over a far-end sentence.
            said("you", "I think we can do it then.", 15.1, 17.0),
            said("you", "So what do we do now?", 11.0, 12.6),
            // Content words of its own, a common word or two at the same time.
            said(
                "you",
                "The pipeline was rebuilt last week after the outage.",
                20.1,
                23.4,
            ),
            said(
                "you",
                "I can check the invoices tomorrow morning.",
                24.2,
                26.8,
            ),
        ];
        let mut all: Vec<Utt> = them.to_vec();
        all.extend(you.iter().cloned());
        assert_eq!(
            drop_echoes(all.clone(), "you", echo, None),
            all,
            "every near-end utterance survives"
        );
        // The far end heard by the mic: still dropped, misheard in places, a fragment.
        let leak = [
            said(
                "you",
                "The dashboard numbers look wrong to me since the migrations.",
                20.05,
                23.6,
            ),
            said("you", "can you check the billing exports", 24.05, 25.9),
            said("you", "ship it on friday", 16.0, 17.5),
        ];
        let mut all: Vec<Utt> = them.to_vec();
        all.extend(leak.iter().cloned());
        assert_eq!(
            drop_echoes(all, "you", echo, None),
            them.to_vec(),
            "no echo leaks"
        );
        // The list is searched by bisection.
        assert!(COMMON.windows(2).all(|p| p[0] < p[1]));
    }

    /// dot-4uzgol.98, PM say check at 77bee09 with the Core Audio tap: remote_1 said "Second
    /// check, this sentence should arrive through the core audio process tab." and the same
    /// sentence came back as a "you" line. The tap's latency puts the mic's copy later than
    /// ScreenCaptureKit's did, and the channels' recogniser cuts differ: the remote sentence in
    /// two pieces (a live window cut), or inside a longer utterance whose words' even-rate time
    /// estimate is off by more than the window. Either way the identical sentence is echo.
    #[test]
    fn an_identical_sentence_at_tap_latency_is_echo_however_the_remote_side_is_cut() {
        let echo = tuning_with(&[]).echo;
        let sentence =
            "Second check, this sentence should arrive through the core audio process tab.";
        let first = said("remote_1", "First check, through the speakers.", 4.0, 6.5);
        let cut = [
            first.clone(),
            said("remote_1", "Second check, this sentence should", 10.0, 11.6),
            said(
                "remote_1",
                "arrive through the core audio process tab.",
                11.6,
                14.0,
            ),
        ];
        // A long utterance: a pause inside it, so the even-rate word times run early.
        let long = [said(
            "remote_1",
            &format!("First check, through the speakers. {sentence}"),
            4.0,
            14.0,
        )];
        for (them, lag) in [
            (&cut[..], 0.4),
            (&cut[..], 1.5),
            (&long[..], 0.4),
            (&long[..], 1.5),
        ] {
            let you = said("you", sentence, 10.0 + lag, 14.0 + lag);
            let d = echo.unwrap();
            assert!(is_echo(&you, them, &d, None), "lag {lag}: {them:?}");
            let mut all = them.to_vec();
            all.push(you);
            assert_eq!(drop_echoes(all, "you", echo, None), them.to_vec());
            // Misheard on the way back, a word lost: still the remote sentence.
            let misheard = said(
                "you",
                "Second check this sentence should arrive through the core audio process tap",
                10.0 + lag,
                14.0 + lag,
            );
            assert!(is_echo(&misheard, them, &d, None), "lag {lag} misheard");
        }
        // The user answering over it, sharing a few of its words: kept.
        let d = echo.unwrap();
        for reply in [
            "Got it, the second one came through fine.",
            "Yes, I can hear this check now.",
            "The audio sounds fine from here.",
        ] {
            let you = said("you", reply, 10.5, 13.5);
            assert!(!is_echo(&you, &cut, &d, None), "{reply}");
            assert!(!is_echo(&you, &long, &d, None), "{reply}");
        }
        // The same sentence long after the remote one: the user reading it out, kept.
        assert!(!is_echo(&said("you", sentence, 30.0, 34.0), &cut, &d, None));
    }

    /// dot-4uzgol.95, PM say check at f312d0d: the speakers said "This is the remote side of a
    /// short test call about quarterly planning." and the final transcript kept a "you" line
    /// "This" at the same instant, a common word .94 never counts as echo. The say-voice fixture:
    /// remote sentences opening on common words, the mic hearing their first word(s) at bleed
    /// level: 0 leaks; the same words at near-end level (replies over the far end) are kept.
    #[test]
    fn a_short_repeat_of_a_remote_sentence_opening_is_echo_unless_the_mic_is_loud() {
        let far = vec![
            (
                0.0,
                4.0,
                "This is the remote side of a short test call about quarterly planning.",
            ),
            (5.0, 8.0, "So the numbers for the third quarter are in."),
            (9.0, 12.0, "Yeah, and we can go over them on Friday."),
        ];
        let near = vec![
            (0.1, 0.5, "This"),
            (5.1, 5.4, "So."),
            (9.05, 9.6, "Yeah, and"),
        ];
        let them: Vec<f32> = tone(12.0).iter().map(|v| v * 2.0).collect();
        let s = settings(false, true);
        for (gain, kept) in [(0.3, 0), (1.0, 3)] {
            let chans = Channels {
                you: tone(12.0).iter().map(|v| v * gain).collect(),
                them: them.clone(),
            };
            let asr = Scripted {
                near: near.clone(),
                far: far.clone(),
            };
            let (all, st) = final_utts(&asr, &s, &chans).unwrap();
            let you: Vec<&str> = all
                .iter()
                .filter(|u| u.speaker == "you")
                .map(|u| u.text.as_str())
                .collect();
            assert_eq!(you.len(), kept, "gain {gain}: {you:?}");
            assert_eq!((st.recognised, st.echo_dropped), (3, 3 - kept));
        }
        // Without the audio the short repeat is kept, as .94 keeps common words; stt.echo_short 0
        // turns the rule off.
        let d = s.tuning.echo.unwrap();
        let remote: Vec<Utt> = far
            .iter()
            .map(|&(a, b, t)| said("remote_1", t, a, b))
            .collect();
        let this = said("you", "This", 0.1, 0.5);
        let quiet = Channels {
            you: tone(12.0).iter().map(|v| v * 0.3).collect(),
            them,
        };
        assert!(!is_echo(&this, &remote, &d, None));
        assert!(is_echo(&this, &remote, &d, Some(&quiet)));
        let off = EchoDedupe { short: 0, ..d };
        assert!(!is_echo(&this, &remote, &off, Some(&quiet)));
        // Not a run of the remote words, or long after the remote sentence starts: kept.
        assert!(!is_echo(
            &said("you", "So", 0.1, 0.5),
            &remote,
            &d,
            Some(&quiet)
        ));
        assert!(!is_echo(
            &said("you", "This is", 3.0, 3.4),
            &remote,
            &d,
            Some(&quiet)
        ));
    }

    /// Says the near-end script for quiet audio (the mic), the far-end one for loud (the
    /// speakers), at times from the start of the audio it is sent.
    struct Scripted {
        near: Vec<(f64, f64, &'static str)>,
        far: Vec<(f64, f64, &'static str)>,
    }

    impl Asr for Scripted {
        fn name(&self) -> &'static str {
            "scripted"
        }
        fn transcribe(&self, pcm: &[f32], _l: &str) -> Result<Vec<Segment>> {
            let script = if peak(pcm) > 0.4 {
                &self.far
            } else {
                &self.near
            };
            Ok(script
                .iter()
                .map(|&(start, end, text)| Segment {
                    start,
                    end,
                    text: text.into(),
                })
                .collect())
        }
    }

    #[test]
    fn the_final_pass_records_mic_stage_counts_and_keeps_double_talk() {
        let silence = |secs: f64| vec![0.0f32; (secs * f64::from(SAMPLE_RATE)) as usize];
        // Mic: 4 s quiet, 10 s of the user talking, 6 s quiet. Speakers: 20 s of remote speech.
        let mut you = silence(4.0);
        you.extend(tone(10.0));
        you.extend(silence(6.0));
        let them: Vec<f32> = tone(20.0).iter().map(|v| v * 2.0).collect();
        let chans = Channels { you, them };
        let far = vec![
            (0.0, 9.0, "So I think the main thing we need to figure out is whether the release can go out on Friday or if we have to push it to next week because of the review."),
            (9.5, 16.0, "And I can take the review if that helps, I just need the notes by tomorrow so I can do it in the morning."),
        ];
        // The mic audio sent starts at 3.75 s (the 250 ms stt.pad before the speech).
        let cases = [
            // Double talk: both near-end sentences kept.
            (vec![
                (0.25, 6.25, "Yeah I think we can do that if the review is done by Thursday, I can push it out myself."),
                (6.25, 10.25, "Okay so I will send you the notes tonight."),
            ], 0),
            // The second one is the speakers on the mic: dropped as echo.
            (vec![
                (0.25, 6.25, "Yeah I think we can do that if the review is done by Thursday, I can push it out myself."),
                (6.0, 10.25, "and I can take the review if that helps I just need the notes"),
            ], 1),
        ];
        for (near, dropped) in cases {
            let dir = tempfile::tempdir().unwrap();
            let store = Store::new(dir.path());
            let asr = Arc::new(Scripted {
                near,
                far: far.clone(),
            });
            let r = run(
                &store,
                None,
                asr,
                &settings(false, true),
                &chans,
                "s",
                Utc::now(),
            )
            .unwrap();
            let t = store.transcript("s").unwrap();
            let n_you = t["utterances"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|u| u["speaker"] == "you")
                .count();
            assert_eq!(n_you, 2 - dropped);
            let fin = &r["mic_stages"]["final"];
            assert_eq!(fin["aec"], true);
            assert_eq!(fin["vad_regions"], 1);
            assert_eq!(fin["vad_seconds"], 10.0);
            assert_eq!(fin["recognised"], 2);
            assert_eq!(fin["echo_dropped"], dropped);
            assert_eq!(fin["kept"], 2 - dropped);
            assert_eq!(r["mic_stages"]["live"], Value::Null);
            let meta: Value = serde_json::from_str(
                &std::fs::read_to_string(dir.path().join("s/session.json")).unwrap(),
            )
            .unwrap();
            assert_eq!(meta["x_oats"][MIC_STAGES], r["mic_stages"]);
        }
    }

    #[test]
    fn live_counts_recognised_dropped_and_kept_mic_utterances() {
        let mut live = Live::new(
            Arc::new(Parrot),
            "en",
            2.0,
            tuning_with(&[]),
            "you",
            "remote_1",
        );
        // Both channels say the same at the same time, fed as they would arrive.
        let (x, step) = (tone(4.0), SAMPLE_RATE as usize / 2);
        for i in (0..x.len()).step_by(step) {
            live.push(1, &x[i..i + step]).unwrap();
            live.push(0, &x[i..i + step]).unwrap();
        }
        live.flush().unwrap();
        let st = live.mic_stages();
        assert!(st.recognised > 0 && st.aec && st.vad.is_none());
        assert_eq!((st.echo_dropped, st.kept), (st.recognised, 0));
        let mut live = Live::new(
            Arc::new(Parrot),
            "en",
            2.0,
            tuning_with(&[("audio.echo_cancel", json!(false))]),
            "you",
            "remote_1",
        );
        live.push(0, &tone(4.0)).unwrap();
        live.flush().unwrap();
        let st = live.mic_stages();
        assert_eq!(
            (st.aec, st.echo_dropped, st.kept),
            (false, 0, st.recognised)
        );
        assert!(st.recognised > 0);
    }

    #[test]
    fn the_gate_counts_speech_stretches_bridging_short_pauses() {
        let t = tuning_with(&[]); // stt.pad 250 ms: pauses up to 500 ms are one stretch
        let gap = |secs: f64| vec![0.0f32; (secs * f64::from(SAMPLE_RATE)) as usize];
        let mut x = gap(1.0);
        x.extend(tone(1.0));
        x.extend(gap(0.4));
        x.extend(tone(1.0));
        x.extend(gap(2.0));
        x.extend(tone(0.5));
        let (regions, secs) = gate_stats(&x, &t);
        assert_eq!(regions, 2);
        assert!((secs - 2.5).abs() < 0.03, "{secs}");
        assert_eq!(gate_stats(&gap(3.0), &t), (0, 0.0));
    }

    fn live_run(tuning: Tuning, them: &[f32]) -> Vec<Utt> {
        let mut live = Live::new(Arc::new(Parrot), "en", 2.0, tuning, "you", "remote_1");
        let you = tone(5.0);
        let step = SAMPLE_RATE as usize / 2;
        let mut out = Vec::new();
        for i in (0..you.len()).step_by(step) {
            out.extend(live.push(0, &you[i..(i + step).min(you.len())]).unwrap());
            if i < them.len() {
                out.extend(live.push(1, &them[i..(i + step).min(them.len())]).unwrap());
            }
        }
        out.extend(live.flush().unwrap());
        out
    }

    #[test]
    fn live_holds_you_until_remote_catches_up_then_drops_the_echo() {
        let on = tuning_with(&[]);
        let speakers = |us: &[Utt]| {
            let mut v: Vec<String> = us.iter().map(|u| u.speaker.clone()).collect();
            v.dedup();
            v
        };
        // Both channels say the same thing at the same time: only the remote speaker is kept.
        let us = live_run(on, &tone(5.0));
        assert!(!us.is_empty());
        assert_eq!(speakers(&us), ["remote_1"]);
        // Remote silent (but flowing): every "you" utterance comes out.
        let us = live_run(on, &vec![0.0; 5 * SAMPLE_RATE as usize]);
        assert!(!us.is_empty());
        assert_eq!(speakers(&us), ["you"]);
        // No remote channel at all: nothing is held back until flush.
        let mut live = Live::new(Arc::new(Parrot), "en", 2.0, on, "you", "remote_1");
        assert_eq!(live.push(0, &tone(3.0)).unwrap().len(), 1);
        // Echo cancel off: both.
        let off = tuning_with(&[("audio.echo_cancel", json!(false))]);
        let us = live_run(off, &tone(5.0));
        assert!(us.iter().any(|u| u.speaker == "you"));
        assert!(us.iter().any(|u| u.speaker == "remote_1"));
    }

    #[test]
    fn final_piece_and_feed_convert_to_samples() {
        let t = tuning_with(&[
            ("stt.final_piece", json!("10s")),
            ("stt.feed", json!("50ms")),
        ]);
        assert_eq!(t.final_piece_samples, 10 * SAMPLE_RATE as usize);
        assert_eq!(t.feed_samples, SAMPLE_RATE as usize / 20);
        let t = tuning_with(&[("stt.final_piece", json!("1h")), ("stt.feed", json!("10s"))]);
        assert_eq!(t.final_piece_samples, 3600 * SAMPLE_RATE as usize);
        assert_eq!(t.feed_samples, 10 * SAMPLE_RATE as usize);
    }

    /// Hears every stretch of speech (split at 300 ms of silence) by its loudness, as the
    /// dot-4uzgol.101 fixture is mixed: the remote voice at full level, the user at about half,
    /// and the speakers' echo far below both, misrecognised so that it shares no words with
    /// what the remote side said.
    struct ByLevel;

    const FAR: &str = "Sounds good to me. The design team needs two more weeks to finish the new onboarding flow.";
    const NEAR: &str = "Let's review the quarterly budget before the meeting starts.";
    const GARBLED: &str = "Sons could the mean team kneads to Moorwicks";

    impl Asr for ByLevel {
        fn name(&self) -> &'static str {
            "by-level"
        }
        fn transcribe(&self, pcm: &[f32], _l: &str) -> Result<Vec<Segment>> {
            let frame = SAMPLE_RATE as usize / 100;
            let loud: Vec<bool> = pcm.chunks(frame).map(|c| peak(c) >= 0.01).collect();
            let mut out = Vec::new();
            let mut i = 0;
            while i < loud.len() {
                if !loud[i] {
                    i += 1;
                    continue;
                }
                let (start, mut end, mut quiet) = (i, i, 0);
                while i < loud.len() && quiet < 30 {
                    if loud[i] {
                        end = i;
                        quiet = 0;
                    } else {
                        quiet += 1;
                    }
                    i += 1;
                }
                let p = peak(&pcm[start * frame..((end + 1) * frame).min(pcm.len())]);
                let text = match p {
                    p if p > 0.7 => FAR,
                    p if p > 0.3 => NEAR,
                    _ => GARBLED,
                };
                out.push(Segment {
                    start: start as f64 / 100.0,
                    end: (end + 1) as f64 / 100.0,
                    text: text.into(),
                });
            }
            Ok(out)
        }
    }

    fn fixture(name: &str) -> Vec<f32> {
        let p = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/stt")
            .join(name);
        wav::read(&p).unwrap().you
    }

    /// `x` scaled to a peak of `to`.
    fn at_peak(x: &[f32], to: f32) -> Vec<f32> {
        let p = peak(x);
        x.iter().map(|v| v / p * to).collect()
    }

    /// The far end as laptop speakers play it into the mic: `lag_ms` late, `db` down, through an
    /// overdriven cone, a resonant box and a coarse converter, which no linear filter models.
    fn garbled(far: &[f32], lag_ms: f64, db: f64) -> Vec<f32> {
        let g = 10f64.powf(db / 20.0) as f32;
        let p = peak(far);
        let lag = (lag_ms * f64::from(SAMPLE_RATE) / 1000.0) as usize;
        let (mut lp, mut out) = (0.0f32, vec![0.0f32; far.len()]);
        for i in lag..far.len() {
            let x = (3.0 * far[i - lag] / p).tanh();
            lp += 0.35 * (x - lp);
            out[i] = (lp * 16.0).round() / 16.0 * p * g;
        }
        out
    }

    /// dot-4uzgol.101's 2-channel fixture: the remote side says its two sentences, the user
    /// answers alone, then the remote side says them again while the user talks over it. The mic
    /// hears all remote speech 60 ms late at -14 dB, garbled.
    fn echo_fixture() -> (Channels, [(f64, f64); 2]) {
        let secs = |s: f64| vec![0.0f32; (s * f64::from(SAMPLE_RATE)) as usize];
        let them_says = at_peak(&fixture("them.wav"), 0.9);
        let you_say = at_peak(&fixture("you.wav"), 0.45);
        let mut them = secs(0.5);
        them.extend(&them_says);
        them.extend(secs(1.5));
        let alone = them.len();
        them.extend(secs(you_say.len() as f64 / f64::from(SAMPLE_RATE)));
        them.extend(secs(1.5));
        let over = them.len();
        them.extend(&them_says);
        them.extend(secs(1.0));
        let mut you = garbled(&them, 60.0, -14.0);
        you.resize(them.len().max(over + you_say.len()), 0.0);
        them.resize(you.len(), 0.0);
        for at in [alone, over] {
            for (o, v) in you[at..].iter_mut().zip(&you_say) {
                *o += v;
            }
        }
        let span = |at: usize| {
            let t = at as f64 / f64::from(SAMPLE_RATE);
            (t, t + you_say.len() as f64 / f64::from(SAMPLE_RATE))
        };
        (Channels { you, them }, [span(alone), span(over)])
    }

    fn you_lines(all: &[Utt]) -> Vec<&Utt> {
        all.iter().filter(|u| u.speaker == "you").collect()
    }

    #[test]
    fn a_garbled_echo_on_the_mic_makes_no_you_lines_and_local_speech_stays() {
        // dot-4uzgol.101, sodium live check at fe2b7d0: 2 of 6 final "you" lines were the
        // speakers' echo of remote_1, misrecognised so the word dedupe passed them.
        let (chans, local) = echo_fixture();
        let s = settings(true, true);
        let (all, st) = final_utts(&ByLevel, &s, &chans).unwrap();
        let you = you_lines(&all);
        assert!(
            you.iter().all(|u| u.text == NEAR),
            "echo kept as you: {you:?}"
        );
        for (from, to) in local {
            assert!(
                you.iter().any(|u| u.start < to && u.end > from),
                "local speech at {from:.1}..{to:.1} s dropped: {you:?}"
            );
        }
        assert_eq!(st.recognised - st.echo_dropped, you.len());
        assert!(all.iter().filter(|u| u.speaker == "remote_1").count() >= 2);
        // The words alone do not catch it: with the envelope check off the echo is "you".
        let mut off = s.clone();
        off.tuning.echo = off.tuning.echo.map(|d| EchoDedupe { track: 0.0, ..d });
        let (all, _) = final_utts(&ByLevel, &off, &chans).unwrap();
        assert!(you_lines(&all).iter().any(|u| u.text == GARBLED));
        // The lag the envelopes measured is the echo's.
        let env = Envelopes::new(&chans.you, &chans.them, 0.0, &s.tuning.echo.unwrap());
        assert!(
            (env.lag_secs() - 0.06).abs() <= 0.02,
            "lag {}",
            env.lag_secs()
        );
    }

    #[test]
    fn the_live_pass_drops_a_garbled_echo_too() {
        let (chans, local) = echo_fixture();
        let s = settings(true, true);
        let mut live = Live::new(
            Arc::new(ByLevel),
            "en",
            s.chunk_secs,
            s.tuning,
            "you",
            "remote_1",
        );
        let mut all = Vec::new();
        let feed = s.tuning.feed_samples;
        for at in (0..chans.you.len()).step_by(feed) {
            let end = (at + feed).min(chans.you.len());
            all.extend(live.push(1, &chans.them[at..end]).unwrap());
            all.extend(live.push(0, &chans.you[at..end]).unwrap());
        }
        all.extend(live.flush().unwrap());
        let you = you_lines(&all);
        assert!(
            you.iter().all(|u| u.text == NEAR),
            "echo kept as you: {you:?}"
        );
        for (from, to) in local {
            assert!(
                you.iter().any(|u| u.start < to && u.end > from),
                "local speech at {from:.1}..{to:.1} s dropped: {you:?}"
            );
        }
        // Bounded: the audio kept for the envelopes trails the transcript.
        assert!(live.audio[0].len() < chans.you.len());
    }
}
