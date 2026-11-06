//! Speaker diarization of the system-audio channel (dot-4uzgol.77, algorithms dot-4uzgol.83).
//! Everything the speakers play is the other side of the call, so only that channel is split: its
//! utterances are labelled `remote_1..remote_N` (from `stt.speaker_them`) by voice, the
//! microphone stays `you`.
//!
//! Each utterance gets a voice print, a unit vector, from the algorithm
//! `stt.diarization.algorithm` picks:
//!
//! - `neural`: a speaker embedding from the pyannote community-1 WeSpeaker ResNet34 networks on
//!   the Apple Neural Engine (`coreml::SpeakerNet`, macOS), the model FluidAudio diarizes with.
//! - `sherpa-onnx`: a speaker embedding from a sherpa-onnx model
//!   (`stt.diarization.sherpa_onnx.model`, `sherpa::SpeakerExtractor`).
//! - `spectral`: the shape of the utterance's long-term log-mel spectrum (the mean over its louder
//!   frames, less the mean over bands, so the level does not count). Pure Rust, no model; it
//!   follows the recording channel as much as the voice, so it is the fallback, not the default.
//!
//! `auto` is sherpa-onnx on every platform (the one tests/diarize_ami.rs measures on real
//! meetings; neural is opt-in until a DER run backs it). An algorithm whose runtime this build
//! lacks, or whose model is missing or will not load, falls back down the chain neural ->
//! sherpa-onnx -> spectral; `status` says which ran and why.
//!
//! Prints are clustered agglomeratively (average linkage): every utterance at least
//! `stt.diarization.min_new` long starts as its own speaker and the two most similar speakers
//! merge until none reach the algorithm's threshold and at most `stt.diarization.max_speakers`
//! remain; speakers with less speech than `stt.diarization.min_speaker` join their nearest, the
//! short utterances join theirs, and speakers are numbered by first appearance. The final pass
//! clusters the whole channel once; live, everything heard so far is clustered again each window
//! and each speaker keeps the label most of its earlier utterances got, so labels stay put.

use std::path::PathBuf;
use std::sync::Mutex;

use serde_json::{json, Value};

use super::catalog::{self, Entry};
use super::mel::{Mel, PARAKEET};
use super::wav::SAMPLE_RATE;
use super::Utt;
use crate::config::typed::{Config, SttDiarizationAlgorithm, SttProvider};
use crate::config::{ConfigError, Result};

/// A way to make voice prints.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Algorithm {
    Spectral,
    Neural,
    SherpaOnnx,
}

impl Algorithm {
    pub fn as_str(self) -> &'static str {
        match self {
            Algorithm::Spectral => "spectral",
            Algorithm::Neural => "neural",
            Algorithm::SherpaOnnx => "sherpa-onnx",
        }
    }

    /// Whether this build can run it (CoreML on macOS, sherpa-onnx with cargo feature
    /// `parakeet`).
    pub fn compiled(self) -> bool {
        match self {
            Algorithm::Spectral => true,
            Algorithm::Neural => catalog::Runtime::CoreMl.compiled(),
            Algorithm::SherpaOnnx => catalog::Runtime::SherpaOnnx.compiled(),
        }
    }
}

/// The model an embedding algorithm runs and its tunables.
#[derive(Clone, Debug)]
pub struct Model {
    pub entry: &'static Entry,
    /// The file (sherpa-onnx) or directory of bundles (CoreML) under the models directory.
    pub path: PathBuf,
    /// Cosine similarity, 0..1.
    pub threshold: f32,
    pub provider: SttProvider,
    pub threads: u32,
}

/// The `stt.diarization.*` settings; `None` in `Settings` when diarization is off.
#[derive(Clone, Debug)]
pub struct Diarization {
    /// `stt.diarization.algorithm` as configured (never `off` here).
    pub requested: SttDiarizationAlgorithm,
    /// What to try, in order; always ends with spectral.
    pub chain: Vec<Algorithm>,
    pub max_speakers: usize,
    /// Samples: a shorter utterance joins the nearest speaker rather than starting one.
    pub min_new_samples: usize,
    /// Samples: a speaker with less speech than this is merged into the nearest one.
    pub min_speaker_samples: usize,
    /// `stt.diarization.threshold`, cosine similarity 0..1.
    pub spectral_threshold: f32,
    pub neural: Model,
    pub sherpa: Model,
}

/// The CoreML embedding model `neural` runs.
pub const NEURAL_MODEL: &str = "pyannote-community-1-coreml";

fn pct(v: u32) -> f32 {
    v.min(100) as f32 / 100.0
}

impl Diarization {
    /// The settings `cfg` asks for, models under `models_dir`; `None` when diarization is off.
    pub fn from_config(cfg: &Config, models_dir: &std::path::Path) -> Result<Option<Diarization>> {
        let d = &cfg.stt.diarization;
        if !d.enabled || d.algorithm == SttDiarizationAlgorithm::Off {
            return Ok(None);
        }
        let speaker = |id: &str, key: &str| {
            catalog::speaker(id).ok_or_else(|| {
                ConfigError::new(
                    "invalid_value",
                    format!("stt.diarization.{key}: '{id}' is not a speaker model (crates/core/data/speaker-models.json)"),
                )
            })
        };
        let neural = speaker(NEURAL_MODEL, "neural")?;
        let sherpa = speaker(d.sherpa_onnx.model.trim(), "sherpa_onnx.model")?;
        if sherpa.runtime != catalog::Runtime::SherpaOnnx {
            return Err(ConfigError::new(
                "invalid_value",
                format!(
                    "stt.diarization.sherpa_onnx.model: '{}' is not a sherpa-onnx model",
                    sherpa.id
                ),
            ));
        }
        use Algorithm::*;
        let chain = match d.algorithm {
            SttDiarizationAlgorithm::Spectral => vec![Spectral],
            SttDiarizationAlgorithm::Neural => vec![Neural, SherpaOnnx, Spectral],
            SttDiarizationAlgorithm::SherpaOnnx => vec![SherpaOnnx, Spectral],
            SttDiarizationAlgorithm::Auto | SttDiarizationAlgorithm::Off => {
                vec![SherpaOnnx, Spectral]
            }
        };
        Ok(Some(Diarization {
            requested: d.algorithm,
            chain,
            max_speakers: d.max_speakers.max(1) as usize,
            min_new_samples: super::samples_of(d.min_new),
            min_speaker_samples: super::samples_of(d.min_speaker),
            spectral_threshold: pct(d.threshold),
            neural: Model {
                entry: neural,
                path: neural.location(models_dir),
                threshold: pct(d.neural.threshold),
                provider: d.neural.provider,
                threads: 1,
            },
            sherpa: Model {
                entry: sherpa,
                path: sherpa.location(models_dir),
                threshold: pct(d.sherpa_onnx.threshold),
                provider: d.sherpa_onnx.provider,
                threads: d.sherpa_onnx.threads.max(1),
            },
        }))
    }

    /// Spectral only, with these tunables (tests, and a fallback with nothing to load).
    pub fn spectral(max_speakers: usize, threshold: f32, min_new_samples: usize) -> Diarization {
        let e = catalog::speaker(NEURAL_MODEL).expect("the neural speaker model is in the catalog");
        let m = Model {
            entry: e,
            path: PathBuf::new(),
            threshold,
            provider: SttProvider::Auto,
            threads: 1,
        };
        Diarization {
            requested: SttDiarizationAlgorithm::Spectral,
            chain: vec![Algorithm::Spectral],
            max_speakers: max_speakers.max(1),
            min_new_samples,
            min_speaker_samples: 0,
            spectral_threshold: threshold,
            neural: m.clone(),
            sherpa: m,
        }
    }

    /// The model of an embedding algorithm.
    pub fn model(&self, a: Algorithm) -> Option<&Model> {
        match a {
            Algorithm::Spectral => None,
            Algorithm::Neural => Some(&self.neural),
            Algorithm::SherpaOnnx => Some(&self.sherpa),
        }
    }

    fn threshold(&self, a: Algorithm) -> f32 {
        self.model(a)
            .map_or(self.spectral_threshold, |m| m.threshold)
    }

    /// Why `a` cannot run now without loading anything (no runtime, model not downloaded), or
    /// `None` when it can be tried.
    fn unavailable(&self, a: Algorithm) -> Option<String> {
        let m = self.model(a)?;
        if !a.compiled() {
            return Some(format!(
                "this build has no {} runtime ({})",
                m.entry.runtime.label(),
                m.entry.runtime.how_to_build()
            ));
        }
        (!m.entry.present(&m.path)).then(|| {
            format!(
                "the {} model is not downloaded ({}); fetch it with `ctl stt model fetch model={}`",
                m.entry.id,
                m.path.display(),
                m.entry.id
            )
        })
    }

    /// The algorithm the chain would run now, and why each one before it would not.
    pub fn plan(&self) -> (Algorithm, Vec<String>) {
        let mut why = Vec::new();
        for &a in &self.chain {
            match self.unavailable(a) {
                None => return (a, why),
                Some(w) => why.push(format!("{}: {w}", a.as_str())),
            }
        }
        (Algorithm::Spectral, why)
    }

    /// The embedding model this build would use first and that is worth downloading: the first
    /// algorithm of the chain this build has the runtime for, when it needs a model.
    pub fn wanted_model(&self) -> Option<&Model> {
        let a = *self.chain.iter().find(|a| a.compiled())?;
        self.model(a)
    }
}

/// What a diarizer ran: (requested, algorithm, provider, fallback reasons).
type Ran = (String, Algorithm, Option<String>, Vec<String>);

/// What the last diarizer ran.
static ACTIVE: Mutex<Option<Ran>> = Mutex::new(None);

/// Forget what ran (config reload changed `stt.diarization.*`) and drop the loaded models.
pub fn clear() {
    *ACTIVE.lock().expect("diarizer status") = None;
    #[cfg(feature = "parakeet")]
    super::sherpa::clear_speaker_cache();
    #[cfg(target_os = "macos")]
    super::coreml::clear_speaker_cache();
}

/// The `diarization` object of `stt-status`: the setting, what would run now (`planned`) and why
/// the algorithms before it would not (`fallback`), what the last recording or transcription ran
/// (`active`, `provider`; null until one did), the embedding model and its threshold.
pub fn status(d: Option<&Diarization>) -> Value {
    let Some(d) = d else {
        return json!({"setting": "off", "planned": "off", "active": null, "provider": null, "fallback": []});
    };
    let (planned, why) = d.plan();
    let active = ACTIVE
        .lock()
        .expect("diarizer status")
        .clone()
        .filter(|a| a.0 == d.requested.as_str());
    let model = d.model(planned).or_else(|| d.wanted_model());
    json!({
        "setting": d.requested.as_str(),
        "chain": d.chain.iter().map(|a| a.as_str()).collect::<Vec<_>>(),
        "planned": planned.as_str(),
        "fallback": why,
        "active": active.as_ref().map(|a| a.1.as_str()),
        "provider": active.as_ref().and_then(|a| a.2.clone()),
        "active_fallback": active.as_ref().map(|a| a.3.clone()),
        "model": model.map(|m| json!({
            "id": m.entry.id, "name": m.entry.name, "runtime": m.entry.runtime.label(),
            "path": m.path, "present": m.entry.present(&m.path), "compiled": m.entry.runtime.compiled(),
        })),
        "threshold": (d.threshold(planned) * 100.0).round(),
        "max_speakers": d.max_speakers,
    })
}

/// Makes voice prints for one algorithm.
enum Embedder {
    Spectral(Mel),
    #[cfg(feature = "parakeet")]
    Sherpa(std::sync::Arc<super::sherpa::SpeakerExtractor>),
    #[cfg(target_os = "macos")]
    Neural(std::sync::Arc<Mutex<super::coreml::SpeakerNet>>),
}

impl Embedder {
    /// Load `a`'s model: the embedder and the provider it runs on.
    fn load(d: &Diarization, a: Algorithm) -> Result<(Embedder, Option<String>)> {
        if let Some(w) = d.unavailable(a) {
            return Err(ConfigError::new("model_not_found", w));
        }
        match a {
            Algorithm::Spectral => Ok((Embedder::Spectral(Mel::new(PARAKEET)), None)),
            #[cfg(feature = "parakeet")]
            Algorithm::SherpaOnnx => {
                let m = &d.sherpa;
                // onnxruntime aborts the process on a malformed file (a C++ exception through
                // FFI), so a file that is not the catalog's size is never handed to it.
                let want = m.entry.files[0].bytes;
                let got = std::fs::metadata(&m.path).map_or(0, |f| f.len());
                if got != want {
                    return Err(ConfigError::new(
                        "model_load_failed",
                        format!(
                            "{} is {got} bytes, not the {want} of {}; delete it and fetch it again with `ctl stt model fetch model={}`",
                            m.path.display(),
                            m.entry.id,
                            m.entry.id
                        ),
                    ));
                }
                let x = super::sherpa::SpeakerExtractor::load(&m.path, m.threads, m.provider)?;
                let p = x.provider().to_string();
                Ok((Embedder::Sherpa(x), Some(p)))
            }
            #[cfg(target_os = "macos")]
            Algorithm::Neural => {
                let m = &d.neural;
                let n = super::coreml::SpeakerNet::load(m.entry, &m.path, m.provider)?;
                let p = n
                    .lock()
                    .expect("coreml speaker net")
                    .units_label()
                    .to_string();
                Ok((Embedder::Neural(n), Some(p)))
            }
            #[allow(unreachable_patterns)]
            _ => Err(ConfigError::new(
                "backend_unavailable",
                format!("this build has no runtime for {}", a.as_str()),
            )),
        }
    }

    fn print(&self, pcm: &[f32]) -> Option<Vec<f32>> {
        match self {
            Embedder::Spectral(mel) => voice_print(mel, pcm),
            #[cfg(feature = "parakeet")]
            Embedder::Sherpa(x) => x.embed(pcm).and_then(unit),
            #[cfg(target_os = "macos")]
            Embedder::Neural(n) => n
                .lock()
                .expect("coreml speaker net")
                .embed(pcm)
                .ok()
                .flatten()
                .and_then(unit),
        }
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

/// Speaker clustering over voice prints. Live, every utterance heard so far is kept and each new
/// window's are labelled by clustering them all again the way the final pass does; each speaker
/// found keeps the label most of its earlier utterances got, so labels stay put.
pub struct Diarizer {
    cfg: Diarization,
    embedder: Embedder,
    algorithm: Algorithm,
    threshold: f32,
    /// Every utterance so far: its print, its length in samples and the label index it got.
    heard: Vec<(Option<Vec<f32>>, usize, usize)>,
    /// Label indices handed out so far.
    labels: usize,
}

impl Diarizer {
    /// The diarizer for `cfg`: the first algorithm of its chain that loads (spectral always
    /// does). What ran, and why the ones before it did not, go to `status`.
    pub fn new(cfg: Diarization) -> Diarizer {
        let mut why = Vec::new();
        let mut loaded = None;
        for &a in &cfg.chain {
            match Embedder::load(&cfg, a) {
                Ok((e, p)) => {
                    loaded = Some((a, e, p));
                    break;
                }
                Err(e) => why.push(format!("{}: {}", a.as_str(), e.message)),
            }
        }
        let (algorithm, embedder, provider) = loaded.unwrap_or_else(|| {
            (
                Algorithm::Spectral,
                Embedder::Spectral(Mel::new(PARAKEET)),
                None,
            )
        });
        *ACTIVE.lock().expect("diarizer status") =
            Some((cfg.requested.as_str().to_string(), algorithm, provider, why));
        Diarizer {
            threshold: cfg.threshold(algorithm),
            cfg,
            embedder,
            algorithm,
            heard: Vec::new(),
            labels: 0,
        }
    }

    /// The algorithm this diarizer runs.
    pub fn algorithm(&self) -> Algorithm {
        self.algorithm
    }

    pub fn print(&self, pcm: &[f32]) -> Option<Vec<f32>> {
        self.embedder.print(pcm)
    }

    /// The speaker index of the next utterance, whose audio is `pcm`.
    pub fn assign(&mut self, pcm: &[f32]) -> usize {
        let p = self.print(pcm);
        self.assign_prints(vec![(p, pcm.len())])[0]
    }

    /// The speaker indices of the next utterances (print, samples): everything heard clustered
    /// again; a speaker keeps the index most of its earlier utterances got, a new one takes the
    /// next index (or, at `max_speakers`, one no speaker holds any more).
    fn assign_prints(&mut self, new: Vec<(Option<Vec<f32>>, usize)>) -> Vec<usize> {
        let old = self.heard.len();
        let fresh = new.len();
        self.heard
            .extend(new.into_iter().map(|(p, n)| (p, n, usize::MAX)));
        let prints: Vec<Option<Vec<f32>>> = self.heard.iter().map(|h| h.0.clone()).collect();
        let lens: Vec<usize> = self.heard.iter().map(|h| h.1).collect();
        let ids = self.cluster(&prints, &lens);
        let k = ids.iter().max().map_or(0, |m| m + 1);
        let mut votes: Vec<(usize, usize, usize)> = Vec::new();
        for c in 0..k {
            for l in 0..self.labels {
                let n = (0..old)
                    .filter(|&i| ids[i] == c && self.heard[i].2 == l)
                    .count();
                if n > 0 {
                    votes.push((n, c, l));
                }
            }
        }
        votes.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        let mut map: Vec<Option<usize>> = vec![None; k];
        let mut used = vec![false; self.labels];
        for (_, c, l) in votes {
            if map[c].is_none() && !used[l] {
                map[c] = Some(l);
                used[l] = true;
            }
        }
        let mut out = Vec::with_capacity(fresh);
        for i in old..old + fresh {
            let c = ids[i];
            let l = match map[c] {
                Some(l) => l,
                None => {
                    let l = if self.labels < self.cfg.max_speakers {
                        self.labels += 1;
                        used.push(true);
                        self.labels - 1
                    } else {
                        let free = used.iter().position(|u| !u).unwrap_or(0);
                        used[free] = true;
                        free
                    };
                    map[c] = Some(l);
                    l
                }
            };
            self.heard[i].2 = l;
            out.push(l);
        }
        out
    }

    /// Label the `them` utterances of a live window whose audio is `pcm`, starting at `offset`
    /// seconds.
    pub fn label_live(&mut self, utts: &mut [Utt], pcm: &[f32], offset: f64, them: &str) {
        let new: Vec<(Option<Vec<f32>>, usize)> = utts
            .iter()
            .map(|u| {
                let a = span(pcm, offset, u);
                (self.print(a), a.len())
            })
            .collect();
        for (u, i) in utts.iter_mut().zip(self.assign_prints(new)) {
            u.speaker = label(them, i);
        }
    }

    /// The speaker (0..k) of each of a channel's utterances (`prints[i]` from utterance `i`,
    /// `lens[i]` its samples): agglomerative clustering of the long ones, a speaker with less
    /// speech than `stt.diarization.min_speaker` merged into its nearest; then each short one
    /// joins the speaker it is most similar to on average (one with no print the speaker before
    /// it). Speakers are numbered by first appearance.
    fn cluster(&self, prints: &[Option<Vec<f32>>], lens: &[usize]) -> Vec<usize> {
        let long: Vec<usize> = (0..prints.len())
            .filter(|&i| prints[i].is_some() && lens[i] >= self.cfg.min_new_samples)
            .collect();
        // Nothing long enough to found a speaker: the first print founds the only one.
        let seeds: Vec<usize> = if long.is_empty() {
            (0..prints.len())
                .find(|&i| prints[i].is_some())
                .into_iter()
                .collect()
        } else {
            long
        };
        if seeds.is_empty() {
            return vec![0; prints.len()];
        }
        let vecs: Vec<&[f32]> = seeds
            .iter()
            .map(|&i| prints[i].as_deref().expect("seeds have prints"))
            .collect();
        let seed_lens: Vec<usize> = seeds.iter().map(|&i| lens[i]).collect();
        let owner = agglomerate(&vecs, self.threshold, self.cfg.max_speakers);
        let owner = absorb_small(&vecs, owner, &seed_lens, self.cfg.min_speaker_samples);
        let mut of: Vec<Option<usize>> = vec![None; prints.len()];
        for (k, &i) in seeds.iter().enumerate() {
            of[i] = Some(owner[k]);
        }
        let mut out = vec![0usize; prints.len()];
        let (mut prev, mut order) = (None, Vec::<usize>::new());
        for (i, p) in prints.iter().enumerate() {
            let c = of[i]
                .or_else(|| {
                    let p = p.as_deref()?;
                    closest(&vecs, &owner, |x| cosine(p, x))
                })
                .or(prev)
                .unwrap_or(owner[0]);
            prev = Some(c);
            out[i] = order.iter().position(|&x| x == c).unwrap_or_else(|| {
                order.push(c);
                order.len() - 1
            });
        }
        out
    }
}

/// The cluster of `owner` whose members score highest on average by `score`.
fn closest(v: &[&[f32]], owner: &[usize], score: impl Fn(&[f32]) -> f32) -> Option<usize> {
    let mut sum: std::collections::BTreeMap<usize, (f32, usize)> = Default::default();
    for (x, &o) in v.iter().zip(owner) {
        let e = sum.entry(o).or_default();
        e.0 += score(x);
        e.1 += 1;
    }
    sum.into_iter()
        .map(|(c, (s, n))| (c, s / n as f32))
        .max_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(c, _)| c)
}

/// Clusters with less than `min` samples of speech merged, smallest first, into the cluster
/// they are most similar to on average, while more than one remains.
fn absorb_small(v: &[&[f32]], mut owner: Vec<usize>, lens: &[usize], min: usize) -> Vec<usize> {
    loop {
        let mut ids: Vec<usize> = owner.clone();
        ids.sort_unstable();
        ids.dedup();
        if ids.len() < 2 {
            return owner;
        }
        let size = |c: usize| -> usize {
            owner
                .iter()
                .zip(lens)
                .filter(|(o, _)| **o == c)
                .map(|(_, n)| n)
                .sum()
        };
        let Some(&small) = ids
            .iter()
            .filter(|&&c| size(c) < min)
            .min_by_key(|&&c| size(c))
        else {
            return owner;
        };
        let members: Vec<&[f32]> = v
            .iter()
            .zip(&owner)
            .filter(|(_, o)| **o == small)
            .map(|(x, _)| *x)
            .collect();
        let (rest, rest_owner): (Vec<&[f32]>, Vec<usize>) = v
            .iter()
            .zip(&owner)
            .filter(|(_, o)| **o != small)
            .map(|(x, o)| (*x, *o))
            .unzip();
        let score = |x: &[f32]| members.iter().map(|m| cosine(m, x)).sum::<f32>();
        match closest(&rest, &rest_owner, score) {
            Some(to) => owner
                .iter_mut()
                .filter(|o| **o == small)
                .for_each(|o| *o = to),
            None => return owner,
        }
    }
}

/// Agglomerative clustering of unit vectors with average linkage: every vector starts as its own
/// cluster, and the two clusters whose members are most similar on average (mean cosine over
/// every pair across them) merge while that reaches `threshold`, or while more than `max`
/// clusters remain. Average rather than centroid linkage: every print of a channel shares a part
/// that is not the voice (the room, the line), and summed into centroids it makes any two
/// speakers look alike. Returns each vector's cluster (the index of one of its members).
fn agglomerate(v: &[&[f32]], threshold: f32, max: usize) -> Vec<usize> {
    let n = v.len();
    // dot[a][b]: the sum of the cosines over every pair across clusters a and b, so their mean is
    // dot[a][b] / (size[a] size[b]) and a merge just adds rows.
    let mut dot: Vec<Vec<f32>> = (0..n)
        .map(|a| (0..n).map(|b| cosine(v[a], v[b])).collect())
        .collect();
    let mut size = vec![1usize; n];
    let mut alive = vec![true; n];
    let mut owner: Vec<usize> = (0..n).collect();
    let sim = |dot: &Vec<Vec<f32>>, size: &[usize], a: usize, b: usize| {
        dot[a][b] / (size[a] * size[b]) as f32
    };
    let best_of = |dot: &Vec<Vec<f32>>, size: &[usize], alive: &[bool], a: usize| {
        (0..n)
            .filter(|&b| b != a && alive[b])
            .map(|b| (b, sim(dot, size, a, b)))
            .max_by(|x, y| x.1.total_cmp(&y.1))
    };
    let mut best: Vec<Option<(usize, f32)>> =
        (0..n).map(|a| best_of(&dot, &size, &alive, a)).collect();
    let mut count = n;
    while count > 1 {
        let Some((a, (b, s))) = (0..n)
            .filter(|&a| alive[a])
            .filter_map(|a| best[a].map(|x| (a, x)))
            .max_by(|x, y| x.1 .1.total_cmp(&y.1 .1))
        else {
            break;
        };
        if s < threshold && count <= max {
            break;
        }
        // Merge b into a.
        for c in 0..n {
            let x = dot[a][c] + dot[b][c];
            dot[a][c] = x;
            dot[c][a] = x;
        }
        size[a] += size[b];
        alive[b] = false;
        best[b] = None;
        owner.iter_mut().filter(|o| **o == b).for_each(|o| *o = a);
        count -= 1;
        best[a] = best_of(&dot, &size, &alive, a);
        for c in (0..n).filter(|&c| alive[c] && c != a) {
            match best[c] {
                Some((x, _)) if x == a || x == b => best[c] = best_of(&dot, &size, &alive, c),
                Some((_, bs)) => {
                    let sc = sim(&dot, &size, c, a);
                    if sc > bs {
                        best[c] = Some((a, sc));
                    }
                }
                None => best[c] = best_of(&dot, &size, &alive, c),
            }
        }
    }
    owner
}

/// The audio of `u` within `pcm`, which starts at `offset` seconds.
fn span<'a>(pcm: &'a [f32], offset: f64, u: &Utt) -> &'a [f32] {
    let at = |t: f64| (((t - offset) * f64::from(SAMPLE_RATE)).max(0.0) as usize).min(pcm.len());
    let (a, b) = (at(u.start), at(u.end));
    &pcm[a..b.max(a)]
}

/// Label the final pass's `them` utterances (`pcm` is the whole channel): agglomerative
/// clustering over the whole channel, speakers numbered by first appearance.
pub fn label_final(cfg: &Diarization, utts: &mut [Utt], pcm: &[f32], them: &str) {
    let d = Diarizer::new(cfg.clone());
    let spans: Vec<&[f32]> = utts.iter().map(|u| span(pcm, 0.0, u)).collect();
    let prints: Vec<Option<Vec<f32>>> = spans.iter().map(|s| d.print(s)).collect();
    let lens: Vec<usize> = spans.iter().map(|s| s.len()).collect();
    for (u, id) in utts.iter_mut().zip(d.cluster(&prints, &lens)) {
        u.speaker = label(them, id);
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

    fn cfg(over: &[(&str, Value)]) -> Config {
        let mut c = crate::config::resolve::defaults_flat();
        for (k, v) in over {
            c.insert((*k).into(), v.clone());
        }
        serde_json::from_value(crate::config::resolve::nest(&c)).unwrap()
    }

    /// dot-4uzgol.83: stt.diarization.algorithm picks the chain; a missing model falls back to
    /// spectral and stt-status says why.
    #[test]
    fn the_algorithm_key_picks_a_chain_that_falls_back_to_spectral() {
        let dir = tempfile::tempdir().unwrap();
        let d = |over: &[(&str, Value)]| Diarization::from_config(&cfg(over), dir.path()).unwrap();
        assert!(d(&[("stt.diarization.algorithm", json!("off"))]).is_none());
        assert!(d(&[("stt.diarization.enabled", json!(false))]).is_none());
        assert_eq!(status(None)["setting"], "off");
        let auto = d(&[]).unwrap();
        assert_eq!(auto.chain, [Algorithm::SherpaOnnx, Algorithm::Spectral]);
        assert_eq!(auto.sherpa.entry.id, "titanet-small");
        assert!((auto.sherpa.threshold - 0.2).abs() < 1e-6);
        let neural = d(&[("stt.diarization.algorithm", json!("neural"))]).unwrap();
        assert_eq!(
            neural.chain,
            [
                Algorithm::Neural,
                Algorithm::SherpaOnnx,
                Algorithm::Spectral
            ]
        );
        let spectral = d(&[("stt.diarization.algorithm", json!("spectral"))]).unwrap();
        assert_eq!(spectral.plan(), (Algorithm::Spectral, vec![]));
        // No model downloaded: spectral runs, and each step says why it was skipped.
        let (planned, why) = neural.plan();
        assert_eq!(planned, Algorithm::Spectral);
        assert_eq!(why.len(), 2, "{why:?}");
        assert!(why[0].starts_with("neural: ") && why[1].starts_with("sherpa-onnx: "));
        let st = status(Some(&neural));
        assert_eq!(st["setting"], "neural");
        assert_eq!(st["planned"], "spectral");
        assert_eq!(st["chain"], json!(["neural", "sherpa-onnx", "spectral"]));
        assert_eq!(Diarizer::new(neural).algorithm(), Algorithm::Spectral);
        // A model file present is tried; one of the wrong size is not loaded: still spectral.
        let m = &auto.sherpa;
        std::fs::write(&m.path, b"not onnx").unwrap();
        if Algorithm::SherpaOnnx.compiled() {
            assert_eq!(auto.plan().0, Algorithm::SherpaOnnx);
        }
        assert_eq!(Diarizer::new(auto).algorithm(), Algorithm::Spectral);
        let bad = cfg(&[("stt.diarization.sherpa_onnx.model", json!("nope"))]);
        assert!(Diarization::from_config(&bad, dir.path()).is_err());
    }

    #[test]
    fn max_speakers_caps_the_labels() {
        let cfg = |max, min_new| Diarization::spectral(max, 1.0, min_new);
        let mut d = Diarizer::new(cfg(1, 0));
        let tone =
            |k: f32| -> Vec<f32> { (0..16_000).map(|i| (i as f32 * k).sin() * 0.3).collect() };
        assert_eq!(d.assign(&tone(0.05)), 0);
        assert_eq!(d.assign(&tone(0.9)), 0);
        let mut d = Diarizer::new(cfg(2, 0));
        assert_eq!(d.assign(&tone(0.05)), 0);
        assert_eq!(d.assign(&tone(0.9)), 1);
        // Too short to start a second: joins the nearest.
        let mut d = Diarizer::new(cfg(2, 20_000));
        assert_eq!(d.assign(&tone(0.05)), 0);
        assert_eq!(d.assign(&tone(0.9)), 0);
    }
}
