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

/// Where recordings go: `audio.dir`, else the per-user sessions dir (same default as the
/// session store, so recordings show up as sessions).
pub fn record_root(flat: &Flat, env: &dyn Fn(&str) -> Option<String>) -> PathBuf {
    let cfg = s(flat, "audio.dir");
    if !cfg.is_empty() {
        return PathBuf::from(cfg);
    }
    if let Some(d) = env("OATS_DATA_DIR") {
        return PathBuf::from(d).join("sessions");
    }
    let home = env("HOME").or_else(|| env("USERPROFILE")).unwrap_or_default();
    let base = if cfg!(target_os = "macos") {
        PathBuf::from(&home).join("Library/Application Support")
    } else if cfg!(windows) {
        env("APPDATA").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(&home))
    } else {
        env("XDG_DATA_HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(&home).join(".local/share"))
    };
    base.join("oats-meetings-rs").join("sessions")
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
        write_meta(&dir, &meta_for(&id, &title, started, Some(ended), summary.duration_ms, summary.channels))?;
        Ok(json!({"session": id, "path": summary.path, "duration_ms": summary.duration_ms,
            "frames": summary.frames, "rate": summary.rate, "channels": summary.channels}))
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

    /// `session-get`: the live recording, else `<root>/<id>/session.json`.
    pub fn get(&self, root: &Path, id: &str) -> Result<Value> {
        if !valid_id(id) {
            return Err(ConfigError::new("invalid_argument", format!("invalid session id {id:?}")));
        }
        if let Some(a) = self.active.lock().expect("active").as_ref().filter(|a| a.id == id) {
            return Ok(json!({
                "id": a.id, "title": a.title, "state": if a.rec.paused() { "paused" } else { "recording" },
                "started_at": oats_audio::timefmt::iso(a.started), "ended_at": Value::Null,
                "duration_ms": a.rec.duration_ms(), "audio_path": a.rec.path(), "path": a.dir,
            }));
        }
        let dir = root.join(id);
        let m = read_meta(&dir).ok_or_else(|| ConfigError::new("not_found", format!("no session '{id}'")))?;
        let audio = dir.join(m.get("audioFile").and_then(Value::as_str).unwrap_or(AUDIO_FILE));
        let g = |k: &str| m.get(k).cloned().unwrap_or(Value::Null);
        Ok(json!({
            "id": id, "title": g("title"), "state": "saved", "started_at": g("startedAt"), "ended_at": g("endedAt"),
            "duration_ms": g("durationMs"), "audio_path": if audio.exists() { json!(audio) } else { Value::Null },
            "path": dir,
        }))
    }
}
