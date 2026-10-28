//! Transcription engine (dot-4uzgol.8). One trait, `Asr`, two backends chosen by `stt.backend`:
//!
//! - `local`: whisper.cpp through whisper-rs (cargo feature `whisper`; model file downloaded and
//!   checksummed by `ci/fetch-models.sh`, never committed).
//! - `cloud`: an OpenAI-compatible HTTP adapter (`stt.cloud.adapter`), gated by `network.allow`,
//!   its key a `secret:` reference resolved by the secrets module.
//!
//! A recording is two channels: the microphone is "you", system audio is "them". `Live`
//! transcribes fixed windows as audio arrives (cut at the quietest point near the window end);
//! the final pass transcribes each whole channel again. Both write OpenOats-layout JSONL into the
//! session store (`transcript.live.jsonl`, `transcript.final.jsonl`), speakers `stt.speaker_you`
//! / `stt.speaker_them`.
//!
//! Stable error codes: `backend_unavailable`, `model_not_found`, `model_load_failed`,
//! `asr_failed`, `network_denied`, `invalid_combination`, `secret_*` (see secrets), `stt_timeout`,
//! `stt_failed`, `stt_http_status`, `stt_bad_response`, `bad_audio`, `invalid_value`, `exists`,
//! `nothing_enabled`.

pub mod cloud;
#[cfg(feature = "whisper")]
pub mod local;
pub mod wav;
pub mod wer;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Weak};

use chrono::{DateTime, Duration, SecondsFormat, Utc};
use serde_json::{json, Value};

use crate::action::Action;
use crate::config::typed::{Config, Stt, SttAdapter, SttBackend};
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

// ---- settings ------------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct Settings {
    pub backend: SttBackend,
    pub adapter: SttAdapter,
    pub language: String,
    pub speaker_you: String,
    pub speaker_them: String,
    pub live_enabled: bool,
    pub chunk_secs: f64,
    pub final_enabled: bool,
    pub model: String,
    pub model_file: PathBuf,
    pub threads: u32,
    pub cloud_url: String,
    pub cloud_model: String,
    pub cloud_key: String,
    pub cloud_timeout: u64,
}

fn invalid(key: &str, why: impl std::fmt::Display) -> ConfigError {
    ConfigError::new("invalid_value", format!("stt.{key}: {why}"))
}

fn num(key: &str, v: &str) -> Result<u64> {
    v.trim().parse::<u64>().map_err(|_| invalid(key, format!("'{v}' is not a non-negative integer")))
}

impl Settings {
    /// Parse and check every `stt.*` value. `sessions_dir` anchors the default models directory.
    pub fn from_config(c: &Stt, sessions_dir: &Path) -> Result<Settings> {
        let chunk = num("live.chunk_secs", &c.live.chunk_secs)?;
        if !(1..=60).contains(&chunk) {
            return Err(invalid("live.chunk_secs", "must be between 1 and 60"));
        }
        let threads = num("local.threads", &c.local.threads)?;
        if !(1..=256).contains(&threads) {
            return Err(invalid("local.threads", "must be between 1 and 256"));
        }
        let timeout = num("cloud.timeout_secs", &c.cloud.timeout_secs)?;
        if timeout == 0 {
            return Err(invalid("cloud.timeout_secs", "must be at least 1"));
        }
        if c.language.trim().is_empty() {
            return Err(invalid("language", "must not be empty"));
        }
        let (you, them) = (c.speaker_you.trim(), c.speaker_them.trim());
        if you.is_empty() || them.is_empty() || you == them {
            return Err(invalid("speaker_you", "speaker_you and speaker_them must be non-empty and different"));
        }
        let model = c.local.model.trim();
        if model.is_empty() || !model.chars().all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-')) {
            return Err(invalid("local.model", format!("'{model}' is not a model name (letters, digits, '.', '_', '-')")));
        }
        let models_dir = if c.local.models_dir.trim().is_empty() {
            sessions_dir.parent().unwrap_or(sessions_dir).join("models")
        } else {
            PathBuf::from(c.local.models_dir.trim())
        };
        let model_file = if c.local.model_path.trim().is_empty() {
            models_dir.join(format!("ggml-{model}.bin"))
        } else {
            PathBuf::from(c.local.model_path.trim())
        };
        Ok(Settings {
            backend: c.backend,
            adapter: c.cloud.adapter,
            language: c.language.trim().to_string(),
            speaker_you: you.to_string(),
            speaker_them: them.to_string(),
            live_enabled: c.live.enabled,
            chunk_secs: chunk as f64,
            final_enabled: c.final_.enabled,
            model: model.to_string(),
            model_file,
            threads: threads as u32,
            cloud_url: c.cloud.url.trim().to_string(),
            cloud_model: c.cloud.model.trim().to_string(),
            cloud_key: c.cloud.key.trim().to_string(),
            cloud_timeout: timeout,
        })
    }

    /// Extra checks that only matter when the cloud backend is selected.
    pub fn check_cloud(&self) -> Result<()> {
        if self.adapter == SttAdapter::None {
            return Err(ConfigError::new("invalid_combination", "stt.backend = \"cloud\" needs stt.cloud.adapter other than \"none\""));
        }
        let u = self.cloud_url.to_ascii_lowercase();
        if !(u.starts_with("http://") || u.starts_with("https://")) || u.len() < 9 {
            return Err(invalid("cloud.url", format!("'{}' is not an http(s) URL", self.cloud_url)));
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
            SttBackend::Local => format!("whisper.cpp:{}", self.model),
            SttBackend::Cloud => format!("{}:{}", "openai-compatible", self.cloud_model),
        }
    }
}

/// Build the recogniser `settings` selects. `network_allow` and the secrets config come from the
/// resolved config.
pub fn make_asr(s: &Settings, network_allow: bool, secrets: &crate::config::typed::Secrets) -> Result<Arc<dyn Asr>> {
    match s.backend {
        SttBackend::Local => local_asr(s),
        SttBackend::Cloud => {
            s.check_cloud()?;
            if !network_allow {
                return Err(ConfigError::new("network_denied", "the cloud STT backend needs network.allow = true"));
            }
            let key = if s.cloud_key.is_empty() {
                None
            } else {
                let store = crate::secrets::store_for(secrets);
                Some(crate::secrets::resolve(&*store, &s.cloud_key)?)
            };
            Ok(Arc::new(cloud::OpenAiAsr {
                url: s.cloud_url.clone(),
                model: s.cloud_model.clone(),
                key,
                timeout_secs: s.cloud_timeout,
            }))
        }
    }
}

#[cfg(feature = "whisper")]
fn local_asr(s: &Settings) -> Result<Arc<dyn Asr>> {
    if !s.model_file.is_file() {
        return Err(ConfigError::new(
            "model_not_found",
            format!("{} not found; fetch it with ci/fetch-models.sh or set stt.local.model_path", s.model_file.display()),
        ));
    }
    Ok(Arc::new(local::WhisperAsr::load(&s.model_file, s.threads)?))
}

#[cfg(not(feature = "whisper"))]
fn local_asr(_s: &Settings) -> Result<Arc<dyn Asr>> {
    Err(ConfigError::new(
        "backend_unavailable",
        "this build has no local ASR; rebuild with --features oats-core/whisper",
    ))
}

// ---- text and audio helpers -----------------------------------------------------------------

/// Whisper marks non-speech as `[BLANK_AUDIO]`, `(music)`, `*sighs*`: not words.
fn clean(text: &str) -> Option<String> {
    let t = text.trim();
    let tagged = (t.starts_with('[') && t.ends_with(']')) || (t.starts_with('(') && t.ends_with(')')) || (t.starts_with('*') && t.ends_with('*') && t.len() > 1);
    (!t.is_empty() && !tagged).then(|| t.to_string())
}

fn peak(x: &[f32]) -> f32 {
    x.iter().fold(0.0f32, |m, v| m.max(v.abs()))
}

/// Below this peak a window is treated as silence and not sent to the recogniser.
const SILENCE_PEAK: f32 = 0.004;

fn utts(segs: Vec<Segment>, offset: f64, speaker: &str) -> Vec<Utt> {
    segs.into_iter()
        .filter_map(|s| {
            clean(&s.text).map(|text| Utt { speaker: speaker.to_string(), text, start: offset + s.start, end: offset + s.end })
        })
        .collect()
}

fn run_asr(asr: &dyn Asr, pcm: &[f32], offset: f64, speaker: &str, language: &str) -> Result<Vec<Utt>> {
    if pcm.is_empty() || peak(pcm) < SILENCE_PEAK {
        return Ok(vec![]);
    }
    Ok(utts(asr.transcribe(pcm, language)?, offset, speaker))
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
/// words are not split mid-way.
pub struct Live {
    asr: Arc<dyn Asr>,
    language: String,
    window: usize,
    names: [String; 2],
    bufs: [ChanBuf; 2],
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
    pub fn new(asr: Arc<dyn Asr>, language: &str, chunk_secs: f64, you: &str, them: &str) -> Live {
        Live {
            asr,
            language: language.to_string(),
            window: ((chunk_secs.max(0.5)) * f64::from(SAMPLE_RATE)) as usize,
            names: [you.to_string(), them.to_string()],
            bufs: [ChanBuf::default(), ChanBuf::default()],
        }
    }

    /// `channel` 0 = you, 1 = them. Returns the utterances of every window this push completed.
    pub fn push(&mut self, channel: usize, samples: &[f32]) -> Result<Vec<Utt>> {
        let ch = channel.min(1);
        self.bufs[ch].pcm.extend_from_slice(samples);
        let mut out = Vec::new();
        while self.bufs[ch].pcm.len() >= self.window {
            let cut = quiet_cut(&self.bufs[ch].pcm, self.window);
            let chunk: Vec<f32> = self.bufs[ch].pcm.drain(..cut).collect();
            let offset = self.bufs[ch].base as f64 / f64::from(SAMPLE_RATE);
            self.bufs[ch].base += cut;
            out.extend(run_asr(&*self.asr, &chunk, offset, &self.names[ch], &self.language)?);
        }
        Ok(out)
    }

    /// Transcribe whatever is left in both channels.
    pub fn flush(&mut self) -> Result<Vec<Utt>> {
        let mut out = Vec::new();
        for ch in 0..2 {
            let chunk = std::mem::take(&mut self.bufs[ch].pcm);
            let offset = self.bufs[ch].base as f64 / f64::from(SAMPLE_RATE);
            self.bufs[ch].base += chunk.len();
            out.extend(run_asr(&*self.asr, &chunk, offset, &self.names[ch], &self.language)?);
        }
        Ok(out)
    }
}

// ---- final ---------------------------------------------------------------------------------

/// Longest stretch sent in one request in the final pass (cloud uploads are size-limited).
const FINAL_PIECE_SECS: usize = 600;

fn final_channel(asr: &dyn Asr, pcm: &[f32], speaker: &str, language: &str) -> Result<Vec<Utt>> {
    let piece = FINAL_PIECE_SECS * SAMPLE_RATE as usize;
    let mut out = Vec::new();
    for (i, chunk) in pcm.chunks(piece).enumerate() {
        out.extend(run_asr(asr, chunk, (i * piece) as f64 / f64::from(SAMPLE_RATE), speaker, language)?);
    }
    Ok(out)
}

fn by_time(mut v: Vec<Utt>) -> Vec<Utt> {
    // Stable: utterances that start together keep their input order (you before them).
    v.sort_by(|a, b| a.start.total_cmp(&b.start));
    v
}

fn stamp(started: DateTime<Utc>, secs: f64) -> String {
    (started + Duration::milliseconds((secs * 1000.0).round() as i64)).to_rfc3339_opts(SecondsFormat::Secs, true)
}

/// The OpenOats utterance object, plus our start/end offsets (ignored by the OpenOats reader).
pub fn utterance_json(u: &Utt, started: DateTime<Utc>) -> Value {
    json!({
        "speaker": u.speaker, "text": u.text, "timestamp": stamp(started, u.start),
        "startSeconds": (u.start * 100.0).round() / 100.0, "endSeconds": (u.end * 100.0).round() / 100.0,
    })
}

// ---- the pipeline --------------------------------------------------------------------------

/// Window size, in seconds, used to feed a file to `Live` as if it were arriving.
const FEED_SECS: f64 = 0.5;

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
        return Err(ConfigError::new("nothing_enabled", "stt.live.enabled and stt.final.enabled are both off"));
    }
    let duration = chans.seconds();
    let started = now - Duration::milliseconds((duration * 1000.0) as i64);
    let dir = store.create(id, &started.to_rfc3339_opts(SecondsFormat::Secs, true), "", &s.engine_label())?;
    let emit = |kind: &str, u: &Utt| {
        if let Some(b) = bus {
            b.publish(
                "segment",
                json!({"session": id, "kind": kind, "speaker": u.speaker, "text": u.text, "start": u.start, "end": u.end}),
            );
        }
    };
    let (mut live_n, mut final_n) = (0usize, 0usize);
    let result = (|| -> Result<()> {
        if s.live_enabled {
            let mut live = Live::new(Arc::clone(&asr), &s.language, s.chunk_secs, &s.speaker_you, &s.speaker_them);
            let step = (FEED_SECS * f64::from(SAMPLE_RATE)) as usize;
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
        }
        if s.final_enabled {
            let mut all = final_channel(&*asr, &chans.you, &s.speaker_you, &s.language)?;
            all.extend(final_channel(&*asr, &chans.them, &s.speaker_them, &s.language)?);
            let all = by_time(all);
            let lines: Vec<Value> = all.iter().map(|u| utterance_json(u, started)).collect();
            store.write_transcript(id, "final", &lines)?;
            all.iter().for_each(|u| emit("final", u));
            final_n = lines.len();
        }
        Ok(())
    })();
    if let Err(e) = result {
        // A failed run leaves no half-written session behind.
        let _ = std::fs::remove_dir_all(&dir);
        return Err(e);
    }
    let summary = store.finish(id, &now.to_rfc3339_opts(SecondsFormat::Secs, true))?;
    if let Some(b) = bus {
        b.publish("session-saved", json!({"id": id, "change": "transcribed"}));
    }
    Ok(json!({
        "session": id, "backend": asr.name(), "duration_secs": duration,
        "live_utterances": live_n, "final_utterances": final_n, "summary": summary,
    }))
}

// ---- engine glue ---------------------------------------------------------------------------

fn settings_of(engine: &Engine, cfg: &Config) -> Result<Settings> {
    Settings::from_config(&cfg.stt, engine.default_sessions_dir())
}

pub fn dispatch(engine: &Engine, a: &Action) -> Result<Value> {
    let cfg = engine.resolved().typed()?;
    let s = settings_of(engine, &cfg)?;
    match a {
        Action::SttTranscribe { path, session, speaker } => {
            let mut chans = wav::read(Path::new(path))?;
            match speaker.as_deref() {
                None | Some("you") => {}
                Some("them") => chans = Channels { you: vec![], them: std::mem::take(&mut chans.you) },
                Some(other) => {
                    return Err(ConfigError::new("invalid_argument", format!("speaker must be 'you' or 'them', got {other:?}")))
                }
            }
            let asr = make_asr(&s, cfg.network.allow, &cfg.secrets)?;
            let now = Utc::now();
            let id = session.clone().unwrap_or_else(|| format!("session_{}", now.format("%Y-%m-%d_%H-%M-%S")));
            run(&engine.store(), Some(&engine.bus), asr, &s, &chans, &id, now)
        }
        Action::SttStatus {} => {
            let key = if s.cloud_key.is_empty() {
                "none"
            } else {
                let store = crate::secrets::store_for(&cfg.secrets);
                match crate::secrets::resolve(&*store, &s.cloud_key) {
                    Ok(_) => "set",
                    Err(e) if e.code == "secret_not_found" => "missing",
                    Err(e) => e.code,
                }
            };
            Ok(json!({
                "backend": match s.backend { SttBackend::Local => "local", SttBackend::Cloud => "cloud" },
                "language": s.language, "live": s.live_enabled, "final": s.final_enabled,
                "local": {
                    "model": s.model, "path": s.model_file, "present": s.model_file.is_file(),
                    "compiled": cfg!(feature = "whisper"), "threads": s.threads,
                },
                "cloud": {
                    "adapter": match s.adapter { SttAdapter::None => "none", SttAdapter::OpenaiCompatible => "openai-compatible" },
                    "url": s.cloud_url, "model": s.cloud_model, "network_allow": cfg.network.allow, "key": key,
                },
            }))
        }
        _ => Err(ConfigError::new("bad_request", "not an stt action")),
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
        let s = Settings::from_config(&new.stt, &dir)?;
        if s.backend == SttBackend::Cloud {
            s.check_cloud()?;
        }
        Ok(())
    }

    fn apply(&self, old: &Config, new: &Config) {
        #[cfg(feature = "whisper")]
        if old.stt.local != new.stt.local {
            local::clear_cache();
        }
        #[cfg(not(feature = "whisper"))]
        let _ = (old, new);
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
                Segment { start: 0.0, end: secs, text: format!("heard {}", secs.round() as u64) },
                Segment { start: 0.0, end: 0.1, text: "[BLANK_AUDIO]".into() },
            ])
        }
    }

    fn tone(secs: f64) -> Vec<f32> {
        (0..(secs * f64::from(SAMPLE_RATE)) as usize).map(|i| (i as f32 * 0.05).sin() * 0.3).collect()
    }

    fn settings(live: bool, fin: bool) -> Settings {
        let mut c = crate::config::resolve::defaults_flat();
        c.insert("stt.live.enabled".into(), json!(live));
        c.insert("stt.final.enabled".into(), json!(fin));
        let nested = crate::config::resolve::nest(&c);
        let cfg: Config = serde_json::from_value(nested).unwrap();
        Settings::from_config(&cfg.stt, Path::new("/tmp/x/sessions")).unwrap()
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
        let asr = Arc::new(Fake { calls: Mutex::new(vec![]) });
        let mut live = Live::new(asr.clone(), "en", 4.0, "you", "remote_1");
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
        assert_eq!(rest.iter().find(|u| u.speaker == "remote_1").unwrap().start, 0.0);
    }

    #[test]
    fn silence_is_not_sent_to_the_recogniser() {
        let asr = Arc::new(Fake { calls: Mutex::new(vec![]) });
        let mut live = Live::new(asr.clone(), "en", 2.0, "you", "remote_1");
        assert!(live.push(0, &vec![0.0; 16_000 * 5]).unwrap().is_empty());
        assert!(live.flush().unwrap().is_empty());
        assert!(asr.calls.lock().unwrap().is_empty());
    }

    #[test]
    fn run_writes_openoats_layout_for_every_live_final_combination() {
        let chans = Channels { you: tone(5.0), them: tone(3.0) };
        for (live, fin) in [(true, true), (true, false), (false, true)] {
            let dir = tempfile::tempdir().unwrap();
            let store = Store::new(dir.path());
            let bus = Bus::default();
            let rx = bus.subscribe(vec!["segment".into()]);
            let now = DateTime::parse_from_rfc3339("2026-10-28T12:00:10Z").unwrap().with_timezone(&Utc);
            let r = run(&store, Some(&bus), Arc::new(Fake { calls: Mutex::new(vec![]) }), &settings(live, fin), &chans, "session_t", now).unwrap();
            assert_eq!(r["live_utterances"].as_u64().unwrap() > 0, live, "{live}/{fin}");
            assert_eq!(r["final_utterances"].as_u64().unwrap() > 0, fin, "{live}/{fin}");
            let t = store.transcript("session_t").unwrap();
            let us = t["utterances"].as_array().unwrap();
            assert!(!us.is_empty());
            assert_eq!(t["source"], if fin { "final" } else { "live" });
            let speakers: std::collections::BTreeSet<&str> = us.iter().map(|u| u["speaker"].as_str().unwrap()).collect();
            assert_eq!(speakers, ["remote_1", "you"].into_iter().collect::<std::collections::BTreeSet<&str>>());
            // Timestamps: recording started 5 s before `now`, utterances are offsets from it.
            assert!(us.iter().all(|u| u["timestamp"].as_str().unwrap().starts_with("2026-10-28T12:00:")));
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
        let chans = Channels { you: tone(1.0), them: vec![] };
        let now = Utc::now();
        let e = run(&store, None, Arc::new(Fake { calls: Mutex::new(vec![]) }), &settings(false, false), &chans, "s1", now).unwrap_err();
        assert_eq!(e.code, "nothing_enabled");
        run(&store, None, Arc::new(Fake { calls: Mutex::new(vec![]) }), &settings(true, true), &chans, "s1", now).unwrap();
        let e = run(&store, None, Arc::new(Fake { calls: Mutex::new(vec![]) }), &settings(true, true), &chans, "s1", now).unwrap_err();
        assert_eq!(e.code, "exists");
    }

    #[test]
    fn settings_validation() {
        let base = crate::config::resolve::defaults_flat();
        let check = |k: &str, v: Value| {
            let mut c = base.clone();
            c.insert(k.into(), v);
            let cfg: Config = serde_json::from_value(crate::config::resolve::nest(&c)).unwrap();
            Settings::from_config(&cfg.stt, Path::new("/tmp/x/sessions")).map(|_| ())
        };
        for (k, v) in [
            ("stt.live.chunk_secs", "0"), ("stt.live.chunk_secs", "61"), ("stt.live.chunk_secs", "x"),
            ("stt.local.threads", "0"), ("stt.local.model", "../evil"), ("stt.cloud.timeout_secs", "0"),
            ("stt.language", " "), ("stt.speaker_you", "remote_1"),
        ] {
            assert_eq!(check(k, json!(v)).unwrap_err().code, "invalid_value", "{k}={v}");
        }
        check("stt.live.chunk_secs", json!("10")).unwrap();
    }
}
