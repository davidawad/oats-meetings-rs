//! sherpa-onnx runtime (cargo feature `parakeet`): the catalog's sherpa-onnx models, NVIDIA
//! Parakeet TDT (transducer) and Moonshine, behind one `Asr`. The catalog entry names the model
//! files by role; `stt.local.provider` picks the onnxruntime execution provider (CoreML on macOS,
//! falling back to the CPU when CoreML cannot load the model). The loaded recogniser is cached per
//! model directory; config reload clears it when `stt.local.*` changes, and `idle` once
//! `stt.local.unload_after` passes unused. Each loaded model runs in a worker process of its own
//! that ends when it is dropped (`worker`), so whatever onnxruntime and CoreML keep for it is
//! released with it; without a worker command, on a thread of its own that exits with it
//! (`confine`).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use sherpa_onnx::{
    OfflineMoonshineModelConfig, OfflineRecognizer, OfflineRecognizerConfig,
    OfflineTransducerModelConfig, SpeakerEmbeddingExtractor, SpeakerEmbeddingExtractorConfig,
};

use super::catalog::Entry;
use super::confine::Confined;
use super::wav::SAMPLE_RATE;
use super::worker::Worker;
use super::{Asr, Segment};
use crate::config::typed::SttProvider;
use crate::config::{ConfigError, Result};

/// A model to make, wherever it runs: the recogniser (its catalog arch and files by role) or the
/// speaker model, with its threads and execution provider.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Spec {
    Recognizer {
        arch: String,
        files: BTreeMap<String, String>,
        threads: i32,
        provider: String,
    },
    Speaker {
        model: String,
        threads: i32,
        provider: String,
    },
}

impl Spec {
    /// What it makes, for messages.
    pub fn what(&self) -> String {
        match self {
            Spec::Recognizer {
                arch,
                files,
                provider,
                ..
            } => format!(
                "the {arch} recogniser {} on {provider}",
                files.get("tokens").map_or("", String::as_str)
            ),
            Spec::Speaker {
                model, provider, ..
            } => format!("the speaker model {model} on {provider}"),
        }
    }
}

/// A made model.
pub enum Model {
    Recognizer(OfflineRecognizer),
    Speaker(SpeakerEmbeddingExtractor),
}

/// What a model gives for some audio.
#[derive(Debug, Serialize, Deserialize)]
pub enum Reply {
    /// The recogniser's text, tokens and their times.
    Decoded {
        text: String,
        tokens: Vec<String>,
        timestamps: Option<Vec<f32>>,
        durations: Option<Vec<f32>>,
    },
    /// The speaker model's embedding; `None` when it wants more audio.
    Embedding(Option<Vec<f32>>),
}

impl Model {
    pub fn make(spec: &Spec) -> Option<Model> {
        match spec {
            Spec::Recognizer {
                arch,
                files,
                threads,
                provider,
            } => {
                let f = |role: &str| files.get(role).cloned();
                let mut cfg = OfflineRecognizerConfig::default();
                match arch.as_str() {
                    "transducer" => {
                        cfg.model_config.transducer = OfflineTransducerModelConfig {
                            encoder: f("encoder"),
                            decoder: f("decoder"),
                            joiner: f("joiner"),
                        }
                    }
                    "moonshine" => {
                        cfg.model_config.moonshine = OfflineMoonshineModelConfig {
                            preprocessor: f("preprocessor"),
                            encoder: f("encoder"),
                            uncached_decoder: f("uncached_decoder"),
                            cached_decoder: f("cached_decoder"),
                            merged_decoder: None,
                        }
                    }
                    _ => return None,
                }
                cfg.model_config.tokens = f("tokens");
                cfg.model_config.num_threads = *threads;
                cfg.model_config.provider = Some(provider.clone());
                OfflineRecognizer::create(&cfg).map(Model::Recognizer)
            }
            Spec::Speaker {
                model,
                threads,
                provider,
            } => SpeakerEmbeddingExtractor::create(&SpeakerEmbeddingExtractorConfig {
                model: Some(model.clone()),
                num_threads: *threads,
                debug: false,
                provider: Some(provider.clone()),
            })
            .map(Model::Speaker),
        }
    }

    /// Run the model on `pcm` (16 kHz mono).
    pub fn run(&self, pcm: &[f32]) -> Reply {
        match self {
            Model::Recognizer(r) => {
                let stream = r.create_stream();
                stream.accept_waveform(SAMPLE_RATE as i32, pcm);
                r.decode(&stream);
                match stream.get_result() {
                    Some(res) => Reply::Decoded {
                        text: res.text,
                        tokens: res.tokens,
                        timestamps: res.timestamps,
                        durations: res.durations,
                    },
                    None => Reply::Decoded {
                        text: String::new(),
                        tokens: vec![],
                        timestamps: None,
                        durations: None,
                    },
                }
            }
            Model::Speaker(x) => Reply::Embedding((|| {
                let stream = x.create_stream()?;
                stream.accept_waveform(SAMPLE_RATE as i32, pcm);
                stream.input_finished();
                if !x.is_ready(&stream) {
                    return None;
                }
                x.compute(&stream)
            })()),
        }
    }
}

/// Where a loaded model runs: a worker process, or a thread of this one.
enum Home {
    Process(Worker),
    Thread(Confined<Model>),
}

impl Home {
    /// Make `spec`'s model, in a worker when the host can start one; `None` when it would not load.
    fn make(thread: &str, spec: Spec) -> Option<Home> {
        match Worker::spawn(&spec) {
            Some(Ok(w)) => Some(Home::Process(w)),
            Some(Err(e)) => {
                crate::elog!("oats-meetings-rs: {e}");
                None
            }
            None => Confined::spawn(thread, move || Model::make(&spec)).map(Home::Thread),
        }
    }

    /// Run the model on `pcm`; `None` when its worker is gone.
    fn run(&self, pcm: &[f32]) -> Option<Reply> {
        match self {
            Home::Process(w) => w
                .run(pcm)
                .map_err(|e| crate::elog!("oats-meetings-rs: {e}"))
                .ok(),
            Home::Thread(c) => {
                let samples = pcm.to_vec();
                Some(c.run(move |m| m.run(&samples)))
            }
        }
    }
}

struct Loaded {
    model: Home,
    provider: String,
}

type Cached = (PathBuf, SttProvider, u32, Arc<Loaded>);

static CACHE: Mutex<Option<Cached>> = Mutex::new(None);

pub fn clear_cache() {
    *CACHE.lock().expect("sherpa cache") = None;
}

/// Whether a loaded recogniser or speaker model is held outside its cache (`idle`).
pub fn in_use() -> bool {
    let asr = CACHE
        .lock()
        .expect("sherpa cache")
        .as_ref()
        .is_some_and(|c| Arc::strong_count(&c.3) > 1);
    let speaker = SPEAKER_CACHE
        .lock()
        .expect("sherpa speaker cache")
        .as_ref()
        .is_some_and(|c| Arc::strong_count(&c.3) > 1);
    asr || speaker
}

/// The provider the model loaded from `dir` runs on, if it is the one loaded.
pub fn loaded_provider(dir: &Path) -> Option<String> {
    CACHE
        .lock()
        .expect("sherpa cache")
        .as_ref()
        .filter(|c| c.0 == dir)
        .map(|c| c.3.provider.clone())
}

pub struct SherpaAsr {
    loaded: Arc<Loaded>,
    /// Longest stretch decoded at once, in samples (`stt.local.piece`).
    piece: usize,
}

/// The recogniser `entry` names, from the files in `dir`, on `provider`.
fn spec(entry: &Entry, dir: &Path, threads: u32, provider: &str) -> Result<Spec> {
    let roles: &[&str] = match entry.arch.as_str() {
        "transducer" => &["encoder", "decoder", "joiner", "tokens"],
        "moonshine" => &[
            "preprocessor",
            "encoder",
            "uncached_decoder",
            "cached_decoder",
            "tokens",
        ],
        a => {
            return Err(ConfigError::new(
                "model_load_failed",
                format!("{}: sherpa-onnx arch '{a}' is not supported", entry.id),
            ))
        }
    };
    let mut files = BTreeMap::new();
    for role in roles {
        let f = entry.file(role).ok_or_else(|| {
            ConfigError::new(
                "model_load_failed",
                format!("catalog entry {} has no {role} file", entry.id),
            )
        })?;
        let p = dir.join(&f.name);
        if !p.is_file() {
            return Err(ConfigError::new(
                "model_not_found",
                format!("{} not found", p.display()),
            ));
        }
        files.insert((*role).to_string(), p.display().to_string());
    }
    Ok(Spec::Recognizer {
        arch: entry.arch.clone(),
        files,
        threads: i32::try_from(threads.max(1)).unwrap_or(i32::MAX),
        provider: provider.to_string(),
    })
}

impl SherpaAsr {
    /// Load `entry`'s files from `dir`, trying the providers `provider` allows in order.
    pub fn load(
        entry: &Entry,
        dir: &Path,
        threads: u32,
        provider: SttProvider,
    ) -> Result<SherpaAsr> {
        let mut cache = CACHE.lock().expect("sherpa cache");
        if let Some((p, pv, t, l)) = cache.as_ref() {
            if p == dir && *pv == provider && *t == threads {
                return Ok(SherpaAsr {
                    loaded: Arc::clone(l),
                    piece: usize::MAX,
                });
            }
        }
        let mut tried = vec![];
        for name in super::provider_order(provider) {
            let spec = spec(entry, dir, threads, name)?;
            if let Some(model) = Home::make("oats-sherpa-asr", spec) {
                super::idle::loaded(&format!("{} on {name}", entry.id));
                let loaded = Arc::new(Loaded {
                    model,
                    provider: (*name).to_string(),
                });
                *cache = Some((dir.to_path_buf(), provider, threads, Arc::clone(&loaded)));
                return Ok(SherpaAsr {
                    loaded,
                    piece: usize::MAX,
                });
            }
            tried.push(*name);
        }
        Err(ConfigError::new(
            "model_load_failed",
            format!(
                "{}: sherpa-onnx could not load {} (providers tried: {})",
                dir.display(),
                entry.id,
                tried.join(", ")
            ),
        ))
    }
}

impl SherpaAsr {
    /// Decode at most `samples` at once (default: the whole input).
    pub fn with_piece(mut self, samples: usize) -> SherpaAsr {
        self.piece = samples.max(1);
        self
    }

    fn decode(&self, pcm: &[f32]) -> Vec<Segment> {
        let Some(Reply::Decoded {
            text,
            tokens,
            timestamps,
            durations,
        }) = self.loaded.model.run(pcm)
        else {
            return vec![];
        };
        let end = pcm.len() as f64 / f64::from(SAMPLE_RATE);
        match &timestamps {
            Some(t) if !t.is_empty() && t.len() == tokens.len() => {
                super::sentences(&tokens, t, durations.as_deref(), end)
            }
            // No token times (Moonshine): the piece is one segment.
            _ => {
                let text = text.trim().to_string();
                if text.is_empty() {
                    vec![]
                } else {
                    vec![Segment {
                        start: 0.0,
                        end,
                        text,
                    }]
                }
            }
        }
    }
}

impl Asr for SherpaAsr {
    fn name(&self) -> &'static str {
        "sherpa-onnx"
    }

    fn provider(&self) -> &str {
        &self.loaded.provider
    }

    /// Parakeet v2 and Moonshine are English-only and Parakeet v3 detects the language itself:
    /// `language` is accepted and ignored. Audio longer than `stt.local.piece` is decoded piece by
    /// piece (cut in the quiet); each piece becomes one segment per sentence when the model gives
    /// token times (Parakeet), else one segment.
    fn transcribe(&self, pcm: &[f32], _language: &str) -> Result<Vec<Segment>> {
        let mut out = Vec::new();
        for (at, piece) in super::pieces(pcm, self.piece) {
            let off = at as f64 / f64::from(SAMPLE_RATE);
            out.extend(self.decode(piece).into_iter().map(|s| Segment {
                start: s.start + off,
                end: s.end + off,
                text: s.text,
            }));
        }
        Ok(out)
    }
}

/// A sherpa-onnx speaker-embedding model (`stt.diarization.algorithm = sherpa-onnx`,
/// dot-4uzgol.83): one embedding per utterance, from the whole utterance.
pub struct SpeakerExtractor {
    model: Home,
    provider: String,
}

type SpeakerCached = (PathBuf, SttProvider, u32, Arc<SpeakerExtractor>);

static SPEAKER_CACHE: Mutex<Option<SpeakerCached>> = Mutex::new(None);

pub fn clear_speaker_cache() {
    *SPEAKER_CACHE.lock().expect("sherpa speaker cache") = None;
}

impl SpeakerExtractor {
    /// Load the model file `path`, trying the providers `provider` allows in order. Cached per
    /// file, provider and threads.
    pub fn load(path: &Path, threads: u32, provider: SttProvider) -> Result<Arc<SpeakerExtractor>> {
        let mut cache = SPEAKER_CACHE.lock().expect("sherpa speaker cache");
        if let Some((p, pv, t, l)) = cache.as_ref() {
            if p == path && *pv == provider && *t == threads {
                return Ok(Arc::clone(l));
            }
        }
        if !path.is_file() {
            return Err(ConfigError::new(
                "model_not_found",
                format!("{} not found", path.display()),
            ));
        }
        let mut tried = vec![];
        for name in super::provider_order(provider) {
            let spec = Spec::Speaker {
                model: path.display().to_string(),
                threads: i32::try_from(threads.max(1)).unwrap_or(i32::MAX),
                provider: (*name).to_string(),
            };
            if let Some(model) = Home::make("oats-sherpa-speaker", spec) {
                super::idle::loaded(&format!("speaker model {} on {name}", path.display()));
                let loaded = Arc::new(SpeakerExtractor {
                    model,
                    provider: (*name).to_string(),
                });
                *cache = Some((path.to_path_buf(), provider, threads, Arc::clone(&loaded)));
                return Ok(loaded);
            }
            tried.push(*name);
        }
        Err(ConfigError::new(
            "model_load_failed",
            format!(
                "sherpa-onnx could not load the speaker model {} (providers tried: {})",
                path.display(),
                tried.join(", ")
            ),
        ))
    }

    pub fn provider(&self) -> &str {
        &self.provider
    }

    /// The embedding of `pcm` (16 kHz mono); `None` when the model wants more audio.
    pub fn embed(&self, pcm: &[f32]) -> Option<Vec<f32>> {
        match self.model.run(pcm)? {
            Reply::Embedding(e) => e,
            Reply::Decoded { .. } => None,
        }
    }
}
