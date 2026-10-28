//! Second local engine: NVIDIA Parakeet TDT 0.6b v2 (int8) through sherpa-onnx (cargo feature
//! `parakeet`). The model directory (encoder, decoder, joiner, tokens) is fetched and
//! checksummed by `ci/fetch-models.sh`, never committed.

use std::path::Path;
use std::sync::Mutex;

use sherpa_onnx::{OfflineRecognizer, OfflineRecognizerConfig, OfflineTransducerModelConfig};

use super::{Asr, Segment};
use crate::config::{ConfigError, Result};

pub struct ParakeetAsr {
    recognizer: Mutex<OfflineRecognizer>,
}

// The recogniser is only ever used behind the mutex, one call at a time.
unsafe impl Send for ParakeetAsr {}
unsafe impl Sync for ParakeetAsr {}

impl ParakeetAsr {
    pub fn load(dir: &Path, threads: u32) -> Result<ParakeetAsr> {
        let file = |n: &str| -> Result<String> {
            let p = dir.join(n);
            if p.is_file() {
                Ok(p.display().to_string())
            } else {
                Err(ConfigError::new("model_not_found", format!("{} not found", p.display())))
            }
        };
        let mut cfg = OfflineRecognizerConfig::default();
        cfg.model_config.transducer = OfflineTransducerModelConfig {
            encoder: Some(file("encoder.int8.onnx")?),
            decoder: Some(file("decoder.int8.onnx")?),
            joiner: Some(file("joiner.int8.onnx")?),
        };
        cfg.model_config.tokens = Some(file("tokens.txt")?);
        cfg.model_config.provider = Some("cpu".into());
        cfg.model_config.num_threads = i32::try_from(threads.max(1)).unwrap_or(4);
        let recognizer = OfflineRecognizer::create(&cfg)
            .ok_or_else(|| ConfigError::new("model_load_failed", format!("{}: sherpa-onnx could not load the model", dir.display())))?;
        Ok(ParakeetAsr { recognizer: Mutex::new(recognizer) })
    }
}

impl Asr for ParakeetAsr {
    fn name(&self) -> &'static str {
        "parakeet"
    }

    /// Parakeet v2 is English-only: `language` is accepted and ignored. The result is one segment
    /// spanning the audio (no word-level times are used).
    fn transcribe(&self, pcm: &[f32], _language: &str) -> Result<Vec<Segment>> {
        let r = self.recognizer.lock().expect("parakeet recogniser");
        let stream = r.create_stream();
        stream.accept_waveform(16_000, pcm);
        r.decode(&stream);
        let text = stream.get_result().map(|x| x.text).unwrap_or_default();
        let text = text.trim().to_string();
        Ok(if text.is_empty() { vec![] } else { vec![Segment { start: 0.0, end: pcm.len() as f64 / 16_000.0, text }] })
    }
}
