//! Recording session state for the engine (dot-4uzgol.7): device listing, record start/stop/pause
//! and the recording facts in `session-get`. The audio itself is `oats-audio`; this module maps
//! config to `AudioSettings`, owns the one active `Recording`, creates the session folder in the
//! store (`<sessions root>/<id>/session.json` + `audio.wav`) and, when transcription is on, feeds
//! the captured audio to a `stt::capture::Transcriber`.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

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
pub type Factory =
    Box<dyn Fn(&AudioSettings) -> oats_audio::Result<Box<dyn CaptureBackend>> + Send + Sync>;

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

/// How long a device listing may take and how long its result is reused (`audio.devices_timeout`,
/// `audio.devices_ttl`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeviceWait {
    pub timeout: Duration,
    pub ttl: Duration,
}

/// Device listings, enumerated off the caller's thread (dot-4uzgol.57): one backend that never
/// answers (a CoreAudio AudioUnit probe) must not hang the GUI main thread or `ctl devices-list`.
#[derive(Default)]
struct DevCache {
    /// The last finished enumeration: when, for which `audio.backend`, and what it returned.
    done: Option<(Instant, BackendChoice, Result<Value>)>,
    /// The enumeration in flight: since when, for which backend. At most one runs at a time, so a
    /// wedged backend holds one thread, not one per call.
    running: Option<(Instant, BackendChoice)>,
    /// Bumped by `set_factory`: a worker of an older factory neither fills nor holds the cache.
    generation: u64,
}

impl DevCache {
    /// The listing for `st` if younger than `ttl` or finished after `since`.
    fn fresh(&self, st: &AudioSettings, ttl: Duration, since: Instant) -> Option<Result<Value>> {
        self.done
            .as_ref()
            .filter(|(at, b, _)| *b == st.backend && (at.elapsed() < ttl || *at >= since))
            .map(|(_, _, v)| v.clone())
    }

    fn timed_out(&self, wait: DeviceWait) -> bool {
        self.running
            .as_ref()
            .is_some_and(|(since, _)| since.elapsed() >= wait.timeout)
    }
}

fn devices_timeout(wait: DeviceWait) -> ConfigError {
    ConfigError::new(
        "devices_timeout",
        format!(
            "the audio backend did not list its devices within {:?} (audio.devices_timeout)",
            wait.timeout
        ),
    )
}

pub struct Recorder {
    active: Mutex<Option<Active>>,
    factory: Arc<Mutex<Factory>>,
    asr: Mutex<Option<AsrFactory>>,
    devices: Arc<(Mutex<DevCache>, Condvar)>,
}

impl Default for Recorder {
    fn default() -> Self {
        Self {
            active: Mutex::new(None),
            factory: Arc::new(Mutex::new(Box::new(oats_audio::select))),
            asr: Mutex::new(None),
            devices: Arc::default(),
        }
    }
}

fn audio_err(e: AudioError) -> ConfigError {
    ConfigError::new(e.code, e.message)
}

fn io_err(p: &Path, e: impl std::fmt::Display) -> ConfigError {
    ConfigError::new("io_error", format!("{}: {e}", p.display()))
}

pub fn settings_from(flat: &Flat) -> Result<AudioSettings> {
    let s = |k: &str| {
        flat.get(k)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    let b = |k: &str| flat.get(k).and_then(Value::as_bool).unwrap_or(true);
    let dur = |k: &str| {
        let text = s(k);
        crate::config::keys::parse_duration_ms(&text)
            .map(std::time::Duration::from_millis)
            .ok_or_else(|| {
                ConfigError::new(
                    "invalid_value",
                    format!("{k} '{text}' is not a duration like \"15s\""),
                )
            })
    };
    let int = |k: &str| flat.get(k).and_then(Value::as_u64).unwrap_or_default() as u32;
    let echo = if b("audio.echo_cancel") {
        Some(oats_audio::aec::EchoSettings {
            tail: dur("audio.echo_tail")?,
            delay: dur("audio.echo_delay")?,
            block: int("audio.echo_block") as usize,
            step_permille: int("audio.echo_step"),
            divergence_percent: int("audio.echo_divergence"),
            warmup: dur("audio.echo_warmup")?,
            warmup_step_permille: int("audio.echo_warmup_step"),
            dominance_percent: int("audio.echo_dominance"),
        })
    } else {
        None
    };
    Ok(AudioSettings {
        backend: BackendChoice::parse(&s("audio.backend")).map_err(audio_err)?,
        layout: Layout::parse(&s("audio.layout")).map_err(audio_err)?,
        system_enabled: b("audio.system.enabled"),
        system_device: s("audio.system.device"),
        mic_enabled: b("audio.mic.enabled"),
        mic_device: s("audio.mic.device"),
        timing: oats_audio::Timing {
            open_timeout: dur("audio.open_timeout")?,
            poll: dur("audio.poll_interval")?,
        },
        echo,
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
            AudioBackend::Screencapturekit => BackendChoice::ScreenCaptureKit,
        },
        layout: match c.audio.layout {
            AudioLayout::Split => Layout::Split,
            AudioLayout::Mix => Layout::Mix,
        },
        system_enabled: c.audio.system.enabled,
        system_device: c.audio.system.device.clone(),
        mic_enabled: c.audio.mic.enabled,
        mic_device: c.audio.mic.device.clone(),
        timing: oats_audio::Timing {
            open_timeout: c.audio.open_timeout.as_duration(),
            poll: c.audio.poll_interval.as_duration(),
        },
        echo: c.audio.echo_cancel.then(|| oats_audio::aec::EchoSettings {
            tail: c.audio.echo_tail.as_duration(),
            delay: c.audio.echo_delay.as_duration(),
            block: c.audio.echo_block as usize,
            step_permille: c.audio.echo_step,
            divergence_percent: c.audio.echo_divergence,
            warmup: c.audio.echo_warmup.as_duration(),
            warmup_step_permille: c.audio.echo_warmup_step,
            dominance_percent: c.audio.echo_dominance,
        }),
    }
}

/// One backend listing, as `devices-list` answers it.
fn enumerate(factory: &Mutex<Factory>, st: &AudioSettings) -> Result<Value> {
    let be = (factory.lock().expect("factory"))(st).map_err(audio_err)?;
    let devices = be.list_devices().map_err(audio_err)?;
    let loopback = devices
        .iter()
        .any(|d| d.kind == oats_audio::DeviceKind::Loopback);
    Ok(
        json!({"backend": be.name(), "os": std::env::consts::OS, "loopback_available": loopback, "devices": devices}),
    )
}

/// The device-listing waits from the typed config.
pub fn device_wait_of(c: &Config) -> DeviceWait {
    DeviceWait {
        timeout: c.audio.devices_timeout.as_duration(),
        ttl: c.audio.devices_ttl.as_duration(),
    }
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
    std::fs::write(&tmp, text)
        .and_then(|_| std::fs::rename(&tmp, &path))
        .map_err(|e| io_err(&path, e))
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
        // A listing from the old backend is not this backend's.
        let mut c = self.devices.0.lock().expect("devices");
        c.generation += 1;
        c.done = None;
        c.running = None;
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

    /// Start an enumeration for `st` on a worker thread unless one is already running.
    fn kick(&self, c: &mut DevCache, st: &AudioSettings) {
        if c.running.is_some() {
            return;
        }
        c.running = Some((Instant::now(), st.backend));
        let generation = c.generation;
        let (factory, cache, want) = (
            Arc::clone(&self.factory),
            Arc::clone(&self.devices),
            st.clone(),
        );
        let spawned = std::thread::Builder::new()
            .name("oats-devices".into())
            .spawn(move || {
                let out = enumerate(&factory, &want);
                let (m, cv) = &*cache;
                let mut c = m.lock().expect("devices");
                if c.generation == generation {
                    c.running = None;
                    c.done = Some((Instant::now(), want.backend, out));
                    cv.notify_all();
                }
            });
        if let Err(e) = spawned {
            c.running = None;
            c.done = Some((
                Instant::now(),
                st.backend,
                Err(ConfigError::new("io_error", format!("spawn devices: {e}"))),
            ));
        }
    }

    /// `devices-list`: the cached listing if younger than `wait.ttl`, else a fresh one from a worker
    /// thread, waited for at most `wait.timeout` (then `devices_timeout`; the worker keeps going
    /// and fills the cache when the backend answers).
    pub fn devices(&self, st: &AudioSettings, wait: DeviceWait) -> Result<Value> {
        let (m, cv) = &*self.devices;
        let since = Instant::now();
        let deadline = since + wait.timeout;
        let mut c = m.lock().expect("devices");
        loop {
            if let Some(v) = c.fresh(st, wait.ttl, since) {
                return v;
            }
            self.kick(&mut c, st);
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(devices_timeout(wait));
            }
            c = cv.wait_timeout(c, left).expect("devices").0;
        }
    }

    /// The listing without waiting, for the GUI thread (it never calls the backend): the cached
    /// one (a stale one too, while a fresh one is fetched), `Ok(None)` while the first is still
    /// within `wait.timeout`, `devices_timeout` past it.
    pub fn devices_cached(&self, st: &AudioSettings, wait: DeviceWait) -> Result<Option<Value>> {
        let mut c = self.devices.0.lock().expect("devices");
        if let Some(v) = c.fresh(st, wait.ttl, Instant::now()) {
            return v.map(Some);
        }
        self.kick(&mut c, st);
        let stale = c.done.as_ref().filter(|(_, b, _)| *b == st.backend);
        match stale {
            Some((_, _, Ok(v))) => Ok(Some(v.clone())),
            _ if c.timed_out(wait) => Err(devices_timeout(wait)),
            Some((_, _, Err(e))) => Err(e.clone()),
            None => Ok(None),
        }
    }

    /// Reload check: with an enumerable backend, a named device must exist; both sources off is
    /// never valid. A machine with no audio stack cannot be checked and is not vetoed (the error
    /// comes at `record start`).
    pub fn validate(&self, st: &AudioSettings, wait: DeviceWait) -> Result<()> {
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
        // A fresh listing (a device plugged in a moment ago counts), bounded like devices-list: a
        // backend that does not answer cannot be checked either.
        let wait = DeviceWait {
            ttl: Duration::ZERO,
            ..wait
        };
        let Ok(v) = self.devices(st, wait) else {
            return Ok(());
        };
        let devices = v["devices"].as_array().cloned().unwrap_or_default();
        let has = |d: &str| devices.iter().any(|x| x["id"] == d || x["name"] == d);
        for (on, key, d) in named {
            if on && !d.is_empty() && !has(d) {
                return Err(ConfigError::new(
                    "device_not_found",
                    format!(
                        "{key} = '{d}' is not a device of the {} backend (see `ctl devices list`)",
                        v["backend"].as_str().unwrap_or_default()
                    ),
                ));
            }
        }
        Ok(())
    }

    /// The running recording keeps the devices, layout and transcription it started with; a config
    /// change applies to the next `record start`. This is the active session id, if any.
    pub fn active_id(&self) -> Option<String> {
        self.active
            .lock()
            .expect("active")
            .as_ref()
            .map(|a| a.id.clone())
    }

    /// The recording in progress as `(session, paused, audio duration ms)`; the outer None means a
    /// start or stop holds the recorder right now (a GUI thread must not wait for it).
    pub fn active_state(&self) -> Option<Option<(String, bool, u64)>> {
        let g = self.active.try_lock().ok()?;
        Some(
            g.as_ref()
                .map(|a| (a.id.clone(), a.rec.paused(), a.rec.duration_ms())),
        )
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
            return Err(ConfigError::new(
                "already_recording",
                format!("session {} is already recording", a.id),
            ));
        }
        let started = oats_audio::timefmt::now_secs();
        let id = session.map_or_else(|| oats_audio::timefmt::session_id(started), str::to_string);
        if !store.valid_id(&id) {
            return Err(ConfigError::new(
                "invalid_argument",
                format!("invalid session id {id:?} (letters, digits, '_', '-', '.' only)"),
            ));
        }
        let be = self.backend(st)?;
        let plan = plan.filter(|p| p.settings.live_enabled || p.settings.final_enabled);
        let label = plan
            .as_ref()
            .map_or_else(String::new, |p| p.settings.engine_label());
        let started_dt = Utc
            .timestamp_opt(started, 0)
            .single()
            .unwrap_or_else(Utc::now);
        let dir = store.create(
            &id,
            &oats_audio::timefmt::iso(started),
            title.unwrap_or_default(),
            &label,
        )?;
        let wav = dir.join(AUDIO_FILE);
        let trans = plan.map(|p| Transcriber::start(p, store.sibling(), &id, started_dt));
        let rec = {
            let tapf = |src: oats_audio::Source| -> Box<dyn FnMut(&[f32]) + Send> {
                match trans.as_ref() {
                    Some(t) => t.tap(src),
                    None => Box::new(|_: &[f32]| {}),
                }
            };
            let tap: Option<oats_audio::recorder::TapFactory> =
                if trans.is_some() { Some(&tapf) } else { None };
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
        let transcription = if trans.is_some() {
            json!({"state": "on"})
        } else {
            no_trans.clone()
        };
        let out = json!({"session": id, "path": wav, "backend": be.name(),
            "channels": st.layout.channels(), "rate": oats_audio::SAMPLE_RATE,
            "started_at": oats_audio::timefmt::iso(started), "transcription": transcription});
        *slot = Some(Active {
            id,
            dir,
            store: store.sibling(),
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
        let Active {
            id,
            dir,
            store,
            layout,
            rec,
            trans,
            no_trans,
            ..
        } = a;
        let summary = rec.stop().map_err(audio_err)?;
        let transcription = match trans {
            Some(t) => t.finish(&summary.path, layout == Layout::Split, ended),
            None => {
                let _ = store.finish(
                    &id,
                    &ended.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                );
                no_trans
            }
        };
        merge_audio_meta(&dir, summary.duration_ms, summary.channels)?;
        Ok(
            json!({"session": id, "path": summary.path, "duration_ms": summary.duration_ms,
            "frames": summary.frames, "rate": summary.rate, "channels": summary.channels,
            "system_samples": summary.system_samples, "mic_samples": summary.mic_samples,
            "transcription": transcription}),
        )
    }

    /// Toggle pause; returns the new state.
    pub fn pause(&self) -> Result<Value> {
        let slot = self.active.lock().expect("active");
        let a = slot
            .as_ref()
            .ok_or_else(|| ConfigError::new("not_recording", "no recording is active"))?;
        let now = !a.rec.paused();
        a.rec.set_paused(now);
        Ok(json!({"session": a.id, "paused": now, "duration_ms": a.rec.duration_ms()}))
    }

    /// Add recording facts to a `session-get` result: `state`, `duration_ms` and `audio_path`.
    /// Live recordings report the audio written so far; saved ones the duration stored at stop.
    pub fn annotate(&self, v: &mut Value, id: &str) {
        let live = self.active.lock().expect("active");
        if let Some(a) = live.as_ref().filter(|a| a.id == id) {
            v["state"] = json!(if a.rec.paused() {
                "paused"
            } else {
                "recording"
            });
            v["duration_ms"] = json!(a.rec.duration_ms());
            v["audio_path"] = json!(a.rec.path());
            return;
        }
        v["state"] = json!("saved");
        let meta = v["metadata"].as_object().cloned().unwrap_or_default();
        v["duration_ms"] = meta.get("durationMs").cloned().unwrap_or(Value::Null);
        let audio = v["path"].as_str().map(|d| {
            Path::new(d).join(
                meta.get("audioFile")
                    .and_then(Value::as_str)
                    .unwrap_or(AUDIO_FILE),
            )
        });
        v["audio_path"] = match audio {
            Some(p) if p.exists() => json!(p),
            _ => Value::Null,
        };
    }
}
