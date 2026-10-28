//! Recording session state for the engine (dot-4uzgol.7): device listing, record start/stop/pause
//! and `session-get` for recordings. The audio itself is `oats-audio`; this module maps config
//! to `AudioSettings`, owns the one active `Recording` and writes the session folder in the
//! OpenOats layout (`<root>/<id>/session.json` + `audio.wav`).

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use oats_audio::{AudioError, AudioSettings, BackendChoice, CaptureBackend, Layout, Recording};
use serde_json::{json, Map, Value};

use crate::config::resolve::Flat;
use crate::config::{ConfigError, Result};

pub const AUDIO_FILE: &str = "audio.wav";

/// Builds the capture backend for the settings; the default is `oats_audio::select`.
pub type Factory = Box<dyn Fn(&AudioSettings) -> oats_audio::Result<Box<dyn CaptureBackend>> + Send + Sync>;

struct Active {
    id: String,
    title: String,
    started: i64,
    dir: PathBuf,
    rec: Recording,
}

pub struct Recorder {
    active: Mutex<Option<Active>>,
    factory: Mutex<Factory>,
}

impl Default for Recorder {
    fn default() -> Self {
        Self { active: Mutex::new(None), factory: Mutex::new(Box::new(oats_audio::select)) }
    }
}

fn audio_err(e: AudioError) -> ConfigError {
    ConfigError::new(e.code, e.message)
}

fn io_err(p: &Path, e: impl std::fmt::Display) -> ConfigError {
    ConfigError::new("io_error", format!("{}: {e}", p.display()))
}

fn s(flat: &Flat, k: &str) -> String {
    flat.get(k).and_then(Value::as_str).unwrap_or_default().to_string()
}

fn b(flat: &Flat, k: &str, default: bool) -> bool {
    flat.get(k).and_then(Value::as_bool).unwrap_or(default)
}

pub fn settings_from(flat: &Flat) -> Result<AudioSettings> {
    Ok(AudioSettings {
        backend: BackendChoice::parse(&s(flat, "audio.backend")).map_err(audio_err)?,
        layout: Layout::parse(&s(flat, "audio.layout")).map_err(audio_err)?,
        system_enabled: b(flat, "audio.system.enabled", true),
        system_device: s(flat, "audio.system.device"),
        mic_enabled: b(flat, "audio.mic.enabled", true),
        mic_device: s(flat, "audio.mic.device"),
    })
}

/// Where recordings go: `audio.dir`, else `default` (the session store root, so recordings are sessions).
pub fn record_root(flat: &Flat, default: PathBuf) -> PathBuf {
    let cfg = s(flat, "audio.dir");
    if cfg.is_empty() {
        default
    } else {
        PathBuf::from(cfg)
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

fn meta_for(id: &str, title: &str, started: i64, ended: Option<i64>, duration_ms: u64, channels: u16) -> Map<String, Value> {
    let mut m = Map::new();
    m.insert("id".into(), json!(id));
    m.insert("startedAt".into(), json!(oats_audio::timefmt::iso(started)));
    m.insert("endedAt".into(), ended.map_or(Value::Null, |e| json!(oats_audio::timefmt::iso(e))));
    m.insert("title".into(), json!(title));
    m.insert("utteranceCount".into(), json!(0));
    m.insert("hasNotes".into(), json!(false));
    m.insert("schemaVersion".into(), json!(1));
    m.insert("durationMs".into(), json!(duration_ms));
    m.insert("audioFile".into(), json!(AUDIO_FILE));
    m.insert("audioChannels".into(), json!(channels));
    m.insert("audioRate".into(), json!(oats_audio::SAMPLE_RATE));
    m
}

impl Recorder {
    pub fn set_factory(&self, f: Factory) {
        *self.factory.lock().expect("factory") = f;
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

    pub fn is_active(&self) -> bool {
        self.active.lock().expect("active").is_some()
    }

    pub fn start(&self, st: &AudioSettings, root: &Path, session: Option<&str>, title: Option<&str>) -> Result<Value> {
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
        let dir = root.join(&id);
        if dir.join(AUDIO_FILE).exists() {
            return Err(ConfigError::new("exists", format!("session {id} already has a recording")));
        }
        let be = self.backend(st)?;
        std::fs::create_dir_all(&dir).map_err(|e| io_err(&dir, e))?;
        let rec = match oats_audio::recorder::start(be.as_ref(), st, &dir.join(AUDIO_FILE)) {
            Ok(r) => r,
            Err(e) => {
                // Leave no empty session folder behind a failed start.
                let _ = std::fs::remove_dir(&dir);
                return Err(audio_err(e));
            }
        };
        let a = Active { id: id.clone(), title: title.unwrap_or_default().to_string(), started, dir: dir.clone(), rec };
        write_meta(&dir, &meta_for(&a.id, &a.title, a.started, None, 0, st.layout.channels()))?;
        let out = json!({"session": id, "path": dir.join(AUDIO_FILE), "backend": be.name(),
            "channels": st.layout.channels(), "rate": oats_audio::SAMPLE_RATE, "started_at": oats_audio::timefmt::iso(started)});
        *slot = Some(a);
        Ok(out)
    }

    pub fn stop(&self) -> Result<Value> {
        let a = self
            .active
            .lock()
            .expect("active")
            .take()
            .ok_or_else(|| ConfigError::new("not_recording", "no recording is active"))?;
        let ended = oats_audio::timefmt::now_secs();
        let Active { id, title, started, dir, rec } = a;
        let summary = rec.stop().map_err(audio_err)?;
        // Merge into what is on disk: tags or a rename made during the recording survive the stop.
        let mut m = read_meta(&dir).unwrap_or_default();
        for (k, v) in meta_for(&id, &title, started, Some(ended), summary.duration_ms, summary.channels) {
            if k == "title" && m.get("title").and_then(Value::as_str).is_some_and(|t| !t.is_empty()) {
                continue;
            }
            m.insert(k, v);
        }
        write_meta(&dir, &m)?;
        Ok(json!({"session": id, "path": summary.path, "duration_ms": summary.duration_ms,
            "frames": summary.frames, "rate": summary.rate, "channels": summary.channels,
            "system_samples": summary.system_samples, "mic_samples": summary.mic_samples}))
    }

    /// Toggle pause; returns the new state.
    pub fn pause(&self) -> Result<Value> {
        let slot = self.active.lock().expect("active");
        let a = slot.as_ref().ok_or_else(|| ConfigError::new("not_recording", "no recording is active"))?;
        let now = !a.rec.paused();
        a.rec.set_paused(now);
        Ok(json!({"session": a.id, "paused": now, "duration_ms": a.rec.duration_ms()}))
    }

    pub fn paused(&self) -> Option<bool> {
        self.active.lock().expect("active").as_ref().map(|a| a.rec.paused())
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
        let audio = v["path"].as_str().map(|d| Path::new(d).join(meta.get("audioFile").and_then(Value::as_str).unwrap_or(AUDIO_FILE)));
        v["audio_path"] = match audio {
            Some(p) if p.exists() => json!(p),
            _ => Value::Null,
        };
    }
}
