//! sherpa-onnx runtime (cargo feature `parakeet`): the catalog's sherpa-onnx models, NVIDIA
//! Parakeet TDT (transducer) and Moonshine, behind one `Asr`. The catalog entry names the model
//! files by role; `stt.local.provider` picks the onnxruntime execution provider (CoreML on macOS,
//! falling back to the CPU when CoreML cannot load the model). The loaded recogniser is cached per
//! model directory; config reload clears it when `stt.local.*` changes.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use sherpa_onnx::{
    OfflineMoonshineModelConfig, OfflineRecognizer, OfflineRecognizerConfig,
    OfflineTransducerModelConfig, SpeakerEmbeddingExtractor, SpeakerEmbeddingExtractorConfig,
};

use super::catalog::Entry;
use super::wav::SAMPLE_RATE;
use super::{Asr, Segment};
use crate::config::typed::SttProvider;
use crate::config::{ConfigError, Result};

struct Loaded {
    recognizer: Mutex<OfflineRecognizer>,
    provider: String,
}

type Cached = (PathBuf, SttProvider, u32, Arc<Loaded>);

static CACHE: Mutex<Option<Cached>> = Mutex::new(None);

pub fn clear_cache() {
    *CACHE.lock().expect("sherpa cache") = None;
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

fn config(entry: &Entry, dir: &Path, threads: u32) -> Result<OfflineRecognizerConfig> {
    let file = |role: &str| -> Result<Option<String>> {
        let f = entry.file(role).ok_or_else(|| {
            ConfigError::new(
                "model_load_failed",
                format!("catalog entry {} has no {role} file", entry.id),
            )
        })?;
        let p = dir.join(&f.name);
        if p.is_file() {
            Ok(Some(p.display().to_string()))
        } else {
            Err(ConfigError::new(
                "model_not_found",
                format!("{} not found", p.display()),
            ))
        }
    };
    let mut cfg = OfflineRecognizerConfig::default();
    match entry.arch.as_str() {
        "transducer" => {
            cfg.model_config.transducer = OfflineTransducerModelConfig {
                encoder: file("encoder")?,
                decoder: file("decoder")?,
                joiner: file("joiner")?,
            }
        }
        "moonshine" => {
            cfg.model_config.moonshine = OfflineMoonshineModelConfig {
                preprocessor: file("preprocessor")?,
                encoder: file("encoder")?,
                uncached_decoder: file("uncached_decoder")?,
                cached_decoder: file("cached_decoder")?,
                merged_decoder: None,
            }
        }
        a => {
            return Err(ConfigError::new(
                "model_load_failed",
                format!("{}: sherpa-onnx arch '{a}' is not supported", entry.id),
            ))
        }
    }
    cfg.model_config.tokens = file("tokens")?;
    cfg.model_config.num_threads = i32::try_from(threads.max(1)).unwrap_or(i32::MAX);
    Ok(cfg)
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
        let mut cfg = config(entry, dir, threads)?;
        let mut tried = vec![];
        for name in super::provider_order(provider) {
            cfg.model_config.provider = Some((*name).to_string());
            if let Some(r) = OfflineRecognizer::create(&cfg) {
                let loaded = Arc::new(Loaded {
                    recognizer: Mutex::new(r),
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
        let r = self.loaded.recognizer.lock().expect("sherpa recogniser");
        let stream = r.create_stream();
        stream.accept_waveform(SAMPLE_RATE as i32, pcm);
        r.decode(&stream);
        let Some(res) = stream.get_result() else {
            return vec![];
        };
        let end = pcm.len() as f64 / f64::from(SAMPLE_RATE);
        match &res.timestamps {
            Some(t) if !t.is_empty() && t.len() == res.tokens.len() => {
                super::sentences(&res.tokens, t, res.durations.as_deref(), end)
            }
            // No token times (Moonshine): the piece is one segment.
            _ => {
                let text = res.text.trim().to_string();
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
    extractor: SpeakerEmbeddingExtractor,
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
            let cfg = SpeakerEmbeddingExtractorConfig {
                model: Some(path.display().to_string()),
                num_threads: i32::try_from(threads.max(1)).unwrap_or(i32::MAX),
                debug: false,
                provider: Some((*name).to_string()),
            };
            if let Some(extractor) = SpeakerEmbeddingExtractor::create(&cfg) {
                let loaded = Arc::new(SpeakerExtractor {
                    extractor,
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
        let stream = self.extractor.create_stream()?;
        stream.accept_waveform(SAMPLE_RATE as i32, pcm);
        stream.input_finished();
        if !self.extractor.is_ready(&stream) {
            return None;
        }
        self.extractor.compute(&stream)
    }
}
