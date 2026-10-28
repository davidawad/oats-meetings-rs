//! Recording session state for the engine (dot-4uzgol.7): device listing, record start/stop/pause
//! and the recording facts in `session-get`. The audio itself is `oats-audio`; this module maps
//! config to `AudioSettings`, owns the one active `Recording`, creates the session folder in the
//! store (`<sessions root>/<id>/session.json` + `audio.wav`) and, when transcription is on, feeds
//! the captured audio to a `stt::capture::Transcriber`.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use chrono::{TimeZone, Utc};
use oats_audio::{AudioError, AudioSettings, BackendChoice, CaptureBackend, Layout, Recording};
use serde_json::{json, Map, Value};

use crate::config::resolve::Flat;
use crate::config::typed::Config;
use crate::config::{ConfigError, Result};
use crate::sessions::Store;
use crate::stt::capture::{Plan, Transcriber};
use crate::stt::Asr;

pub const AUDIO_FILE: &str = "audio.wav";

/// Builds the capture backend for the settings; the default is `oats_audio::select`.
pub type Factory = Box<dyn Fn(&AudioSettings) -> oats_audio::Result<Box<dyn CaptureBackend>> + Send + Sync>;

/// Builds the ASR for a recording; the default is `stt::make_asr`, tests inject a fake.
pub type AsrFactory = Box<dyn Fn(&crate::stt::Settings) -> Result<Arc<dyn Asr>> + Send + Sync>;

struct Active {
    id: String,
    dir: PathBuf,
    store: Store,
    layout: Layout,
    rec: Recording,
    trans: Option<Transcriber>,
    /// Why there is no transcription ("off", or the reason it was unavailable).
    no_trans: Value,
}

pub struct Recorder {
    active: Mutex<Option<Active>>,
    factory: Mutex<Factory>,
    asr: Mutex<Option<AsrFactory>>,
}

impl Default for Recorder {
    fn default() -> Self {
        Self { active: Mutex::new(None), factory: Mutex::new(Box::new(oats_audio::select)), asr: Mutex::new(None) }
    }
}

fn audio_err(e: AudioError) -> ConfigError {
    ConfigError::new(e.code, e.message)
}

fn io_err(p: &Path, e: impl std::fmt::Display) -> ConfigError {
    ConfigError::new("io_error", format!("{}: {e}", p.display()))
}

pub fn settings_from(flat: &Flat) -> Result<AudioSettings> {
    let s = |k: &str| flat.get(k).and_then(Value::as_str).unwrap_or_default().to_string();
    let b = |k: &str| flat.get(k).and_then(Value::as_bool).unwrap_or(true);
    Ok(AudioSettings {
        backend: BackendChoice::parse(&s("audio.backend")).map_err(audio_err)?,
        layout: Layout::parse(&s("audio.layout")).map_err(audio_err)?,
        system_enabled: b("audio.system.enabled"),
        system_device: s("audio.system.device"),
        mic_enabled: b("audio.mic.enabled"),
        mic_device: s("audio.mic.device"),
    })
}

/// The same settings from the typed config (what a reload candidate is validated as).
pub fn settings_of(c: &Config) -> AudioSettings {
    use crate::config::typed::{AudioBackend, AudioLayout};
    AudioSettings {
        backend: match c.audio.backend {
            AudioBackend::Auto => BackendChoice::Auto,
            AudioBackend::Pulse => BackendChoice::Pulse,
            AudioBackend::Pipewire => BackendChoice::Pipewire,
            AudioBackend::Cpal => BackendChoice::Cpal,
        },
        layout: match c.audio.layout {
            AudioLayout::Split => Layout::Split,
            AudioLayout::Mix => Layout::Mix,
        },
        system_enabled: c.audio.system.enabled,
        system_device: c.audio.system.device.clone(),
        mic_enabled: c.audio.mic.enabled,
        mic_device: c.audio.mic.device.clone(),
    }
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 200
        && !id.starts_with('.')
        && id.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
}

fn read_meta(dir: &Path) -> Option<Map<String, Value>> {
    match serde_json::from_slice::<Value>(&std::fs::read(dir.join("session.json")).ok()?).ok()? {
        Value::Object(m) => Some(m),
        _ => None,
    }
}

fn write_meta(dir: &Path, meta: &Map<String, Value>) -> Result<()> {
    let path = dir.join("session.json");
    let tmp = dir.join(format!(".session.json.tmp{}", std::process::id()));
    let text = serde_json::to_string_pretty(&Value::Object(meta.clone()))
        .map_err(|e| ConfigError::new("io_error", e.to_string()))?;
    std::fs::write(&tmp, text).and_then(|_| std::fs::rename(&tmp, &path)).map_err(|e| io_err(&path, e))
}

/// Merge the audio facts into `session.json` without touching what the store or the user wrote
/// (title, tags, utterance count).
fn merge_audio_meta(dir: &Path, duration_ms: u64, channels: u16) -> Result<()> {
    let mut m = read_meta(dir).unwrap_or_default();
    m.insert("durationMs".into(), json!(duration_ms));
    m.insert("audioFile".into(), json!(AUDIO_FILE));
    m.insert("audioChannels".into(), json!(channels));
    m.insert("audioRate".into(), json!(oats_audio::SAMPLE_RATE));
    write_meta(dir, &m)
}

impl Recorder {
    pub fn set_factory(&self, f: Factory) {
        *self.factory.lock().expect("factory") = f;
    }

    pub fn set_asr_factory(&self, f: AsrFactory) {
        *self.asr.lock().expect("asr") = Some(f);
    }

    /// ASR for a recording: the injected one, else `default`.
    pub fn make_asr(
        &self,
        s: &crate::stt::Settings,
        default: &dyn Fn(&crate::stt::Settings) -> Result<Arc<dyn Asr>>,
    ) -> Result<Arc<dyn Asr>> {
        match self.asr.lock().expect("asr").as_ref() {
            Some(f) => f(s),
            None => default(s),
        }
    }

    fn backend(&self, st: &AudioSettings) -> Result<Box<dyn CaptureBackend>> {
        (self.factory.lock().expect("factory"))(st).map_err(audio_err)
    }

    pub fn devices(&self, st: &AudioSettings) -> Result<Value> {
        let be = self.backend(st)?;
        let devices = be.list_devices().map_err(audio_err)?;
        let loopback = devices.iter().any(|d| d.kind == oats_audio::DeviceKind::Loopback);
        Ok(json!({"backend": be.name(), "os": std::env::consts::OS, "loopback_available": loopback, "devices": devices}))
    }

    /// Reload check: with an enumerable backend, a named device must exist; both sources off is
    /// never valid. A machine with no audio stack cannot be checked and is not vetoed (the error
    /// comes at `record start`).
    pub fn validate(&self, st: &AudioSettings) -> Result<()> {
        if !st.system_enabled && !st.mic_enabled {
            return Err(ConfigError::new(
                "invalid_config",
                "audio.system.enabled and audio.mic.enabled are both false: nothing to record",
            ));
        }
        let named = [
            (st.system_enabled, "audio.system.device", &st.system_device),
            (st.mic_enabled, "audio.mic.device", &st.mic_device),
        ];
        if named.iter().all(|(on, _, d)| !on || d.is_empty()) {
            return Ok(());
        }
        let Ok(be) = self.backend(st) else { return Ok(()) };
        let Ok(devices) = be.list_devices() else { return Ok(()) };
        for (on, key, d) in named {
            if on && !d.is_empty() && !devices.iter().any(|x| &x.id == d || &x.name == d) {
                return Err(ConfigError::new(
                    "device_not_found",
                    format!("{key} = '{d}' is not a device of the {} backend (see `ctl devices list`)", be.name()),
                ));
            }
        }
        Ok(())
    }

    /// The running recording keeps the devices, layout and transcription it started with; a config
    /// change applies to the next `record start`. This is the active session id, if any.
    pub fn active_id(&self) -> Option<String> {
        self.active.lock().expect("active").as_ref().map(|a| a.id.clone())
    }

    pub fn start(
        &self,
        st: &AudioSettings,
        store: &Store,
        session: Option<&str>,
        title: Option<&str>,
        plan: Option<Plan>,
        no_trans: Value,
    ) -> Result<Value> {
        let mut slot = self.active.lock().expect("active");
        if let Some(a) = slot.as_ref() {
            return Err(ConfigError::new("already_recording", format!("session {} is already recording", a.id)));
        }
        let started = oats_audio::timefmt::now_secs();
        let id = session.map_or_else(|| oats_audio::timefmt::session_id(started), str::to_string);
        if !valid_id(&id) {
            return Err(ConfigError::new(
                "invalid_argument",
                format!("invalid session id {id:?} (letters, digits, '_', '-', '.' only)"),
            ));
        }
        let be = self.backend(st)?;
        let plan = plan.filter(|p| p.settings.live_enabled || p.settings.final_enabled);
        let label = plan.as_ref().map_or_else(String::new, |p| p.settings.engine_label());
        let started_dt = Utc.timestamp_opt(started, 0).single().unwrap_or_else(Utc::now);
        let dir = store.create(&id, &oats_audio::timefmt::iso(started), title.unwrap_or_default(), &label)?;
        let wav = dir.join(AUDIO_FILE);
        let trans = plan.map(|p| Transcriber::start(p, Store::new(store.root.clone()), &id, started_dt));
        let rec = {
            let tapf = |src: oats_audio::Source| -> Box<dyn FnMut(&[f32]) + Send> {
                match trans.as_ref() {
                    Some(t) => t.tap(src),
                    None => Box::new(|_: &[f32]| {}),
                }
            };
            let tap: Option<oats_audio::recorder::TapFactory> = if trans.is_some() { Some(&tapf) } else { None };
            oats_audio::recorder::start(be.as_ref(), st, &wav, tap)
        };
        let rec = match rec {
            Ok(r) => r,
            Err(e) => {
                // Leave no session folder behind a failed start.
                if let Some(t) = trans {
                    t.finish(&wav, false, Utc::now());
                }
                let _ = std::fs::remove_dir_all(&dir);
                return Err(audio_err(e));
            }
        };
        merge_audio_meta(&dir, 0, st.layout.channels())?;
        let transcription = if trans.is_some() { json!({"state": "on"}) } else { no_trans.clone() };
        let out = json!({"session": id, "path": wav, "backend": be.name(),
            "channels": st.layout.channels(), "rate": oats_audio::SAMPLE_RATE,
            "started_at": oats_audio::timefmt::iso(started), "transcription": transcription});
        *slot = Some(Active {
            id,
            dir,
            store: Store::new(store.root.clone()),
            layout: st.layout,
            rec,
            trans,
            no_trans,
        });
        Ok(out)
    }

    pub fn stop(&self) -> Result<Value> {
        let a = self
            .active
            .lock()
            .expect("active")
            .take()
            .ok_or_else(|| ConfigError::new("not_recording", "no recording is active"))?;
        let ended = Utc::now();
        let Active { id, dir, store, layout, rec, trans, no_trans, .. } = a;
        let summary = rec.stop().map_err(audio_err)?;
        let transcription = match trans {
            Some(t) => t.finish(&summary.path, layout == Layout::Split, ended),
            None => {
                let _ = store.finish(&id, &ended.to_rfc3339_opts(chrono::SecondsFormat::Secs, true));
                no_trans
            }
        };
        merge_audio_meta(&dir, summary.duration_ms, summary.channels)?;
        Ok(json!({"session": id, "path": summary.path, "duration_ms": summary.duration_ms,
            "frames": summary.frames, "rate": summary.rate, "channels": summary.channels,
            "system_samples": summary.system_samples, "mic_samples": summary.mic_samples,
            "transcription": transcription}))
    }

    /// Toggle pause; returns the new state.
    pub fn pause(&self) -> Result<Value> {
        let slot = self.active.lock().expect("active");
        let a = slot.as_ref().ok_or_else(|| ConfigError::new("not_recording", "no recording is active"))?;
        let now = !a.rec.paused();
        a.rec.set_paused(now);
        Ok(json!({"session": a.id, "paused": now, "duration_ms": a.rec.duration_ms()}))
    }

    /// Add recording facts to a `session-get` result: `state`, `duration_ms` and `audio_path`.
    /// Live recordings report the audio written so far; saved ones the duration stored at stop.
    pub fn annotate(&self, v: &mut Value, id: &str) {
        let live = self.active.lock().expect("active");
        if let Some(a) = live.as_ref().filter(|a| a.id == id) {
            v["state"] = json!(if a.rec.paused() { "paused" } else { "recording" });
            v["duration_ms"] = json!(a.rec.duration_ms());
            v["audio_path"] = json!(a.rec.path());
            return;
        }
        v["state"] = json!("saved");
        let meta = v["metadata"].as_object().cloned().unwrap_or_default();
        v["duration_ms"] = meta.get("durationMs").cloned().unwrap_or(Value::Null);
        let audio = v["path"]
            .as_str()
            .map(|d| Path::new(d).join(meta.get("audioFile").and_then(Value::as_str).unwrap_or(AUDIO_FILE)));
        v["audio_path"] = match audio {
            Some(p) if p.exists() => json!(p),
            _ => Value::Null,
        };
    }
}
