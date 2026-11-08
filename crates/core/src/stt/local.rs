//! Local ASR: whisper.cpp through `whisper-rs` (cargo feature `whisper`). The loaded model is
//! cached per path; config reload clears it when `stt.local.*` changes, and `idle` once
//! `stt.local.unload_after` passes unused.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

use super::{Asr, Segment};
use crate::config::{ConfigError, Result};

static CACHE: Mutex<Option<(PathBuf, Arc<WhisperContext>)>> = Mutex::new(None);

pub fn clear_cache() {
    *CACHE.lock().expect("whisper cache") = None;
}

/// Whether the loaded model is held outside the cache (`idle`).
pub fn in_use() -> bool {
    CACHE
        .lock()
        .expect("whisper cache")
        .as_ref()
        .is_some_and(|c| Arc::strong_count(&c.1) > 1)
}

pub struct WhisperAsr {
    ctx: Arc<WhisperContext>,
    threads: i32,
}

impl WhisperAsr {
    pub fn load(path: &Path, threads: u32) -> Result<WhisperAsr> {
        let mut cache = CACHE.lock().expect("whisper cache");
        let ctx = match cache.as_ref() {
            Some((p, c)) if p == path => Arc::clone(c),
            _ => {
                let c = Arc::new(
                    WhisperContext::new_with_params(path, WhisperContextParameters::default())
                        .map_err(|e| {
                            ConfigError::new(
                                "model_load_failed",
                                format!("{}: {e}", path.display()),
                            )
                        })?,
                );
                *cache = Some((path.to_path_buf(), Arc::clone(&c)));
                c
            }
        };
        Ok(WhisperAsr {
            ctx,
            threads: i32::try_from(threads.max(1)).unwrap_or(4),
        })
    }
}

impl Asr for WhisperAsr {
    fn name(&self) -> &'static str {
        "whisper.cpp"
    }

    fn transcribe(&self, pcm: &[f32], language: &str) -> Result<Vec<Segment>> {
        let fail = |e: &dyn std::fmt::Display| ConfigError::new("asr_failed", e.to_string());
        let mut state = self.ctx.create_state().map_err(|e| fail(&e))?;
        let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
        if !language.is_empty() {
            params.set_language(Some(language));
        }
        params.set_n_threads(self.threads);
        params.set_translate(false);
        params.set_print_special(false);
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_timestamps(false);
        state.full(params, pcm).map_err(|e| fail(&e))?;
        Ok(state
            .as_iter()
            .map(|seg| Segment {
                start: seg.start_timestamp() as f64 / 100.0,
                end: seg.end_timestamp() as f64 / 100.0,
                text: seg.to_string().trim().to_string(),
            })
            .collect())
    }
}
