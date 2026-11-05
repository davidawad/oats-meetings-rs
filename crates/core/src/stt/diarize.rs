//! Speaker diarization of the system-audio channel (dot-4uzgol.77). Everything the speakers play
//! is the other side of the call, so only that channel is split: its utterances are labelled
//! `remote_1..remote_N` (from `stt.speaker_them`) by voice, the microphone stays `you`.
//!
//! Each utterance gets a voice print: the shape of its long-term log-mel spectrum (the mean over
//! its louder frames, less the mean over bands, so the level does not count), as a unit vector.
//! Prints are clustered online: an utterance joins the most similar speaker heard so far when the
//! cosine similarity reaches `stt.diarization.threshold`, else it starts a new one (at most
//! `stt.diarization.max_speakers`; one shorter than `stt.diarization.min_new` never does). The
//! final pass then moves each utterance to its nearest speaker once more and numbers speakers in
//! order of first appearance. Pure Rust, no model to download, the same on every platform.

use super::mel::{Mel, PARAKEET};
use super::wav::SAMPLE_RATE;
use super::Utt;
use crate::config::typed::Config;

/// The `stt.diarization.*` tunables; `None` in `Settings` when diarization is off.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Diarization {
    pub max_speakers: usize,
    /// Cosine similarity, 0..1.
    pub threshold: f32,
    /// Samples: a shorter utterance joins the nearest speaker rather than starting one.
    pub min_new_samples: usize,
}

impl Diarization {
    pub fn from_config(cfg: &Config) -> Option<Diarization> {
        let d = &cfg.stt.diarization;
        d.enabled.then(|| Diarization {
            max_speakers: d.max_speakers.max(1) as usize,
            threshold: d.threshold as f32 / 100.0,
            min_new_samples: super::samples_of(d.min_new),
        })
    }
}

/// The label of remote speaker `i` (0-based) given `stt.speaker_them`: `remote_1` numbers on
/// (`remote_2`, ...); a name without a trailing number gets one (`caller`, `caller_2`).
pub fn label(them: &str, i: usize) -> String {
    if i == 0 {
        return them.to_string();
    }
    let stem = them.trim_end_matches(|c: char| c.is_ascii_digit());
    if stem.len() < them.len() && !stem.is_empty() {
        format!("{stem}{}", i + 1)
    } else {
        format!("{them}_{}", i + 1)
    }
}

/// The voice print of `pcm` (16 kHz mono), a unit vector; `None` when it holds no audio.
pub fn voice_print(mel: &Mel, pcm: &[f32]) -> Option<Vec<f32>> {
    let hop = mel.spec().hop;
    let bands = mel.spec().mels;
    if pcm.len() < hop {
        return None;
    }
    let frames = pcm.len() / hop + 1;
    let (lm, valid) = mel.log_mel(pcm, frames);
    // The louder frames: speech, not the pauses between words.
    let energy: Vec<f32> = (0..valid)
        .map(|f| (0..bands).map(|b| lm[b * frames + f]).sum::<f32>() / bands as f32)
        .collect();
    let mean_e = energy.iter().sum::<f32>() / valid as f32;
    let loud: Vec<usize> = (0..valid).filter(|&f| energy[f] >= mean_e).collect();
    if loud.is_empty() {
        return None;
    }
    let n = loud.len() as f32;
    let v: Vec<f32> = (0..bands)
        .map(|b| loud.iter().map(|&f| lm[b * frames + f]).sum::<f32>() / n)
        .collect();
    let c = v.iter().sum::<f32>() / bands as f32;
    unit(v.into_iter().map(|x| x - c).collect())
}

fn unit(mut v: Vec<f32>) -> Option<Vec<f32>> {
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if !norm.is_finite() || norm <= f32::EPSILON {
        return None;
    }
    v.iter_mut().for_each(|x| *x /= norm);
    Some(v)
}

fn cosine(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

/// Online speaker clustering over voice prints.
pub struct Diarizer {
    cfg: Diarization,
    mel: Mel,
    /// Per speaker: the sum of its members' prints and the centroid (the sum, normalised).
    voices: Vec<(Vec<f32>, Vec<f32>)>,
    last: usize,
}

impl Diarizer {
    pub fn new(cfg: Diarization) -> Diarizer {
        Diarizer {
            cfg,
            mel: Mel::new(PARAKEET),
            voices: Vec::new(),
            last: 0,
        }
    }

    pub fn print(&self, pcm: &[f32]) -> Option<Vec<f32>> {
        voice_print(&self.mel, pcm)
    }

    fn nearest(&self, p: &[f32]) -> Option<(usize, f32)> {
        self.voices
            .iter()
            .enumerate()
            .map(|(i, v)| (i, cosine(p, &v.1)))
            .max_by(|a, b| a.1.total_cmp(&b.1))
    }

    /// The speaker index of an utterance whose audio is `pcm`. An utterance with no usable audio
    /// goes to the speaker heard last; one that joins a speaker only because it may not start one
    /// (too short, or `max_speakers` reached) does not move that speaker's centroid.
    pub fn assign(&mut self, pcm: &[f32]) -> usize {
        let Some(p) = self.print(pcm) else {
            return self.last;
        };
        let may_start = pcm.len() >= self.cfg.min_new_samples;
        let (i, learn) = match self.nearest(&p) {
            Some((i, sim)) if sim >= self.cfg.threshold => (i, true),
            Some((i, _)) if !may_start || self.voices.len() >= self.cfg.max_speakers => (i, false),
            _ => {
                self.voices.push((vec![0.0; p.len()], Vec::new()));
                (self.voices.len() - 1, true)
            }
        };
        if learn {
            let sum = &mut self.voices[i].0;
            sum.iter_mut().zip(&p).for_each(|(s, x)| *s += x);
            self.voices[i].1 = unit(sum.clone()).unwrap_or(p);
        }
        self.last = i;
        i
    }

    /// Label the `them` utterances of a live window whose audio is `pcm`, starting at `offset`
    /// seconds.
    pub fn label_live(&mut self, utts: &mut [Utt], pcm: &[f32], offset: f64, them: &str) {
        for u in utts.iter_mut() {
            let i = self.assign(span(pcm, offset, u));
            u.speaker = label(them, i);
        }
    }
}

/// The audio of `u` within `pcm`, which starts at `offset` seconds.
fn span<'a>(pcm: &'a [f32], offset: f64, u: &Utt) -> &'a [f32] {
    let at = |t: f64| (((t - offset) * f64::from(SAMPLE_RATE)).max(0.0) as usize).min(pcm.len());
    let (a, b) = (at(u.start), at(u.end));
    &pcm[a..b.max(a)]
}

/// Label the final pass's `them` utterances (`pcm` is the whole channel): online clustering, then
/// each utterance moves to its nearest speaker and speakers are numbered by first appearance.
pub fn label_final(cfg: Diarization, utts: &mut [Utt], pcm: &[f32], them: &str) {
    let mut d = Diarizer::new(cfg);
    let mut ids: Vec<usize> = utts.iter().map(|u| d.assign(span(pcm, 0.0, u))).collect();
    for (u, id) in utts.iter().zip(ids.iter_mut()) {
        if let Some((i, _)) = d.print(span(pcm, 0.0, u)).and_then(|p| d.nearest(&p)) {
            *id = i;
        }
    }
    let mut order: Vec<usize> = Vec::new();
    for (u, id) in utts.iter_mut().zip(ids) {
        let n = order.iter().position(|&x| x == id).unwrap_or_else(|| {
            order.push(id);
            order.len() - 1
        });
        u.speaker = label(them, n);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_number_on_from_speaker_them() {
        assert_eq!(label("remote_1", 0), "remote_1");
        assert_eq!(label("remote_1", 1), "remote_2");
        assert_eq!(label("remote_1", 9), "remote_10");
        assert_eq!(label("caller", 0), "caller");
        assert_eq!(label("caller", 2), "caller_3");
        assert_eq!(label("42", 1), "42_2");
    }

    #[test]
    fn silence_has_no_voice_print_and_level_does_not_change_it() {
        let mel = Mel::new(PARAKEET);
        assert_eq!(voice_print(&mel, &[0.0; 8000]), None);
        assert_eq!(voice_print(&mel, &[0.1; 10]), None);
        let x: Vec<f32> = (0..16_000)
            .map(|i| ((i as f32 * 0.07).sin() + (i as f32 * 0.31).sin()) * 0.2)
            .collect();
        let half: Vec<f32> = x.iter().map(|v| v * 0.5).collect();
        let (a, b) = (
            voice_print(&mel, &x).unwrap(),
            voice_print(&mel, &half).unwrap(),
        );
        assert!(cosine(&a, &b) > 0.99, "{}", cosine(&a, &b));
    }

    #[test]
    fn max_speakers_caps_the_labels() {
        let cfg = Diarization {
            max_speakers: 1,
            threshold: 1.0,
            min_new_samples: 0,
        };
        let mut d = Diarizer::new(cfg);
        let tone =
            |k: f32| -> Vec<f32> { (0..16_000).map(|i| (i as f32 * k).sin() * 0.3).collect() };
        assert_eq!(d.assign(&tone(0.05)), 0);
        assert_eq!(d.assign(&tone(0.9)), 0);
        let mut d = Diarizer::new(Diarization {
            max_speakers: 2,
            ..cfg
        });
        assert_eq!(d.assign(&tone(0.05)), 0);
        assert_eq!(d.assign(&tone(0.9)), 1);
        // Too short to start a second: joins the nearest.
        let mut d = Diarizer::new(Diarization {
            min_new_samples: 20_000,
            ..cfg
        });
        assert_eq!(d.assign(&tone(0.05)), 0);
        assert_eq!(d.assign(&tone(0.9)), 0);
    }
}
