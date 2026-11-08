//! Recording session state for the engine (dot-4uzgol.7): device listing, record start/stop/pause
//! and the recording facts in `session-get`. The audio itself is `oats-audio`; this module maps
//! config to `AudioSettings`, owns the one active `Recording`, creates the session folder in the
//! store (`<sessions root>/<id>/session.json` + `audio.wav`) and, when transcription is on, feeds
//! the captured audio to a `stt::capture::Transcriber`.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use chrono::{TimeZone, Utc};
use oats_audio::{
    AudioError, AudioSettings, BackendChoice, CaptureBackend, Layout, Recording, SystemBackend,
};
use serde_json::{json, Map, Value};

use crate::config::resolve::Flat;
use crate::config::typed::Config;
use crate::config::{ConfigError, Result};
use crate::sessions::{ext, set_ext, Store};
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
    /// The last finished enumeration: when, for which backend, and what it returned.
    done: Option<(Instant, Which, Result<Value>)>,
    /// The enumeration in flight: since when, for which backend. At most one runs at a time, so a
    /// wedged backend holds one thread, not one per call.
    running: Option<(Instant, Which)>,
    /// Bumped by `set_factory`: a worker of an older factory neither fills nor holds the cache.
    generation: u64,
}

/// What a listing depends on: `audio.backend`, and `audio.system_backend` (the macOS listing names
/// the system-audio source it plans, so a listing of the old plan is stale; dot-4uzgol.81).
type Which = (BackendChoice, SystemBackend);

fn which(st: &AudioSettings) -> Which {
    (st.backend, st.system_backend)
}

impl DevCache {
    /// The listing for `st` if younger than `ttl` or finished after `since`.
    fn fresh(&self, st: &AudioSettings, ttl: Duration, since: Instant) -> Option<Result<Value>> {
        self.done
            .as_ref()
            .filter(|(at, b, _)| *b == which(st) && (at.elapsed() < ttl || *at >= since))
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

/// The input monitor (dot-4uzgol.92, `ui.monitor`): meters while not recording. A worker thread
/// opens and closes it, so a slow device open never holds the GUI thread or the caller.
#[derive(Default)]
struct MonState {
    /// The settings to monitor with; None is off.
    want: Option<AudioSettings>,
    /// How long a failed open waits before the worker tries the same settings again.
    retry: Duration,
    /// A recording start or one in progress holds the devices: the monitor stays closed.
    suspended: bool,
    /// The open monitor and the settings it was opened with.
    open: Option<(AudioSettings, oats_audio::monitor::Monitor)>,
    /// An open in flight on the worker (the lock is not held over it).
    opening: bool,
    /// The last failed open: its settings, when, and why.
    failed: Option<(AudioSettings, Instant, String)>,
    worker: bool,
    quit: bool,
    /// Bumped by `set_factory`: a monitor opened from the old backend is not kept.
    epoch: u64,
}

impl MonState {
    fn target(&self) -> Option<&AudioSettings> {
        self.want.as_ref().filter(|_| !self.suspended && !self.quit)
    }
}

type MonShared = Arc<(Mutex<MonState>, Condvar)>;

/// What `Recorder::monitor_state` reports.
#[derive(Debug, Clone, PartialEq)]
pub struct MonitorState {
    pub open: bool,
    pub levels: (f32, f32),
    pub blocks: (u64, u64),
    pub error: Option<String>,
}

pub struct Recorder {
    active: Mutex<Option<Active>>,
    factory: Arc<Mutex<Factory>>,
    asr: Mutex<Option<AsrFactory>>,
    devices: Arc<(Mutex<DevCache>, Condvar)>,
    monitor: MonShared,
}

impl Default for Recorder {
    fn default() -> Self {
        Self {
            active: Mutex::new(None),
            factory: Arc::new(Mutex::new(Box::new(oats_audio::select))),
            asr: Mutex::new(None),
            devices: Arc::default(),
            monitor: Arc::default(),
        }
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        let (m, cv) = &*self.monitor;
        let mut g = m.lock().expect("monitor");
        g.quit = true;
        g.open = None;
        cv.notify_all();
    }
}

/// The monitor worker: make `open` match `target()` until the recorder goes.
fn monitor_worker(shared: MonShared, factory: Arc<Mutex<Factory>>) {
    let (m, cv) = &*shared;
    let mut g = m.lock().expect("monitor");
    loop {
        if g.quit {
            g.open = None;
            g.worker = false;
            cv.notify_all();
            return;
        }
        let target = g.target().cloned();
        if g.open.as_ref().map(|(st, _)| st) != target.as_ref() {
            // Released before anything else: hiding the window or a record start frees the mic.
            g.open = None;
            cv.notify_all();
        }
        let wait = match (&target, &g.open, &g.failed) {
            (Some(_), Some(_), _) | (None, _, _) => None,
            (Some(t), None, Some((st, at, _))) if st == t && at.elapsed() < g.retry => {
                Some(g.retry - at.elapsed())
            }
            (Some(t), None, _) => {
                let (t, epoch) = (t.clone(), g.epoch);
                g.opening = true;
                drop(g);
                let opened = (factory.lock().expect("factory"))(&t)
                    .and_then(|be| oats_audio::monitor::start(be.as_ref(), &t));
                g = m.lock().expect("monitor");
                g.opening = false;
                match opened {
                    // Still wanted: keep it; else it is dropped (released) here and the loop
                    // looks again.
                    Ok(mon) if g.target() == Some(&t) && g.epoch == epoch => {
                        g.failed = None;
                        g.open = Some((t, mon));
                    }
                    Err(e) if g.epoch == epoch => g.failed = Some((t, Instant::now(), e.message)),
                    _ => {}
                }
                cv.notify_all();
                continue;
            }
        };
        g = match wait {
            Some(d) => cv.wait_timeout(g, d).expect("monitor").0,
            None => cv.wait(g).expect("monitor"),
        };
    }
}

fn audio_err(e: AudioError) -> ConfigError {
    ConfigError::new(e.code, e.message)
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
            align: dur("audio.echo_align")?,
            align_probe: dur("audio.echo_align_probe")?,
            align_confidence_percent: int("audio.echo_align_confidence"),
            suppress_percent: int("audio.echo_suppress"),
            suppress_floor_percent: int("audio.echo_suppress_floor"),
        })
    } else {
        None
    };
    Ok(AudioSettings {
        backend: BackendChoice::parse(&s("audio.backend")).map_err(audio_err)?,
        system_backend: SystemBackend::parse(&s("audio.system_backend")).map_err(audio_err)?,
        layout: Layout::parse(&s("audio.layout")).map_err(audio_err)?,
        system_enabled: b("audio.system.enabled"),
        system_device: s("audio.system.device"),
        mic_enabled: b("audio.mic.enabled"),
        mic_device: s("audio.mic.device"),
        timing: oats_audio::Timing {
            open_timeout: dur("audio.open_timeout")?,
            poll: dur("audio.poll_interval")?,
            restart: dur("audio.restart_wait")?,
        },
        echo,
        silence_warning: dur("audio.silence_warning")?,
    })
}

/// The same settings from the typed config (what a reload candidate is validated as).
pub fn settings_of(c: &Config) -> AudioSettings {
    use crate::config::typed::{AudioBackend, AudioLayout, AudioSystemBackend};
    AudioSettings {
        backend: match c.audio.backend {
            AudioBackend::Auto => BackendChoice::Auto,
            AudioBackend::Pulse => BackendChoice::Pulse,
            AudioBackend::Pipewire => BackendChoice::Pipewire,
            AudioBackend::Cpal => BackendChoice::Cpal,
            AudioBackend::Screencapturekit => BackendChoice::ScreenCaptureKit,
        },
        system_backend: match c.audio.system_backend {
            AudioSystemBackend::Auto => SystemBackend::Auto,
            AudioSystemBackend::Tap => SystemBackend::Tap,
            AudioSystemBackend::Screencapturekit => SystemBackend::ScreenCaptureKit,
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
            restart: c.audio.restart_wait.as_duration(),
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
            align: c.audio.echo_align.as_duration(),
            align_probe: c.audio.echo_align_probe.as_duration(),
            align_confidence_percent: c.audio.echo_align_confidence,
            suppress_percent: c.audio.echo_suppress,
            suppress_floor_percent: c.audio.echo_suppress_floor,
        }),
        silence_warning: c.audio.silence_warning.as_duration(),
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

/// Merge the audio facts into `session.json` without touching what the store or the user wrote
/// (title, tags, utterance count).
fn merge_audio_meta(dir: &Path, duration_ms: u64, channels: u16) -> Result<()> {
    let mut m = read_meta(dir).unwrap_or_default();
    set_ext(&mut m, "durationMs", json!(duration_ms));
    set_ext(&mut m, "audioFile", json!(AUDIO_FILE));
    set_ext(&mut m, "audioChannels", json!(channels));
    set_ext(&mut m, "audioRate", json!(oats_audio::SAMPLE_RATE));
    crate::sessions::write_meta(dir, &m)
}

impl Recorder {
    pub fn set_factory(&self, f: Factory) {
        *self.factory.lock().expect("factory") = f;
        // An open monitor, or a failed open, was the old backend's: the worker reopens.
        {
            let (m, cv) = &*self.monitor;
            let mut g = m.lock().expect("monitor");
            g.open = None;
            g.failed = None;
            g.epoch += 1;
            cv.notify_all();
        }
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
        c.running = Some((Instant::now(), which(st)));
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
                    c.done = Some((Instant::now(), which(&want), out));
                    cv.notify_all();
                }
            });
        if let Err(e) = spawned {
            c.running = None;
            c.done = Some((
                Instant::now(),
                which(st),
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
        let stale = c.done.as_ref().filter(|(_, b, _)| *b == which(st));
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

    /// Monitor with `want` (None: off); `retry` is how long a failed open waits. Returns at once:
    /// the worker opens and closes the devices.
    pub fn monitor(&self, want: Option<AudioSettings>, retry: Duration) {
        let (m, cv) = &*self.monitor;
        let mut g = m.lock().expect("monitor");
        if g.want == want && g.retry == retry {
            return;
        }
        g.retry = retry;
        if let (Some(st), false) = (&want, g.worker) {
            let (shared, factory) = (Arc::clone(&self.monitor), Arc::clone(&self.factory));
            let spawned = std::thread::Builder::new()
                .name("oats-monitor".into())
                .spawn(move || monitor_worker(shared, factory));
            match spawned {
                Ok(_) => g.worker = true,
                Err(e) => {
                    g.failed = Some((st.clone(), Instant::now(), format!("spawn monitor: {e}")))
                }
            }
        }
        g.want = want;
        cv.notify_all();
    }

    /// Close the monitor and wait until its devices are released (true), or open it again once
    /// wanted (false).
    fn suspend_monitor(&self, on: bool) {
        let (m, cv) = &*self.monitor;
        let mut g = m.lock().expect("monitor");
        g.suspended = on;
        // The worker drops the open monitor on this wake-up.
        cv.notify_all();
        while on && g.worker && (g.open.is_some() || g.opening) {
            g = cv.wait(g).expect("monitor");
        }
    }

    /// The input monitor as `ui-state` and the meters see it: whether it is open, its
    /// `(mic, system)` levels and callback blocks, and the last open error. None while the worker
    /// holds the lock (never blocks).
    pub fn monitor_state(&self) -> Option<MonitorState> {
        let g = self.monitor.0.try_lock().ok()?;
        let open = g.open.as_ref().map(|(_, m)| m);
        Some(MonitorState {
            open: open.is_some(),
            levels: open.map_or((0., 0.), |m| m.levels()),
            blocks: open.map_or((0, 0), |m| m.blocks()),
            error: g
                .failed
                .as_ref()
                .filter(|(st, _, _)| g.target() == Some(st))
                .map(|(_, _, e)| e.clone()),
        })
    }

    /// The recording in progress's warning (`Recording::warning`); None when idle, while a start
    /// or stop holds the recorder, or when there is nothing to warn of.
    pub fn warning(&self) -> Option<String> {
        let g = self.active.try_lock().ok()?;
        g.as_ref().and_then(|a| a.rec.warning())
    }

    /// The recording in progress as `(session, why)` once it can no longer write audio
    /// (`Recording::fault`); None otherwise, and while a start or stop holds the recorder.
    pub fn fault(&self) -> Option<(String, String)> {
        let g = self.active.try_lock().ok()?;
        g.as_ref()
            .and_then(|a| a.rec.fault().map(|f| (a.id.clone(), f)))
    }

    /// The `(mic, system)` RMS levels of the recording in progress; None when idle or while a
    /// start or stop holds the recorder (never blocks).
    pub fn levels(&self) -> Option<(f32, f32)> {
        let g = self.active.try_lock().ok()?;
        g.as_ref().map(|a| a.rec.levels())
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
        // The recording opens the devices itself: the monitor lets go of them first, and has
        // them again if the start fails.
        self.suspend_monitor(true);
        let out = self.start_locked(&mut slot, st, store, id, started, title, plan, no_trans);
        if out.is_err() {
            self.suspend_monitor(false);
        }
        out
    }

    #[allow(clippy::too_many_arguments)]
    fn start_locked(
        &self,
        slot: &mut Option<Active>,
        st: &AudioSettings,
        store: &Store,
        id: String,
        started: i64,
        title: Option<&str>,
        plan: Option<Plan>,
        no_trans: Value,
    ) -> Result<Value> {
        let be: Arc<dyn CaptureBackend> = Arc::from(self.backend(st)?);
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
        if let Some(p) = plan.as_ref() {
            store.set_meta(&id, "language", json!(p.settings.language))?;
        }
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
            oats_audio::recorder::start(Arc::clone(&be), st, &wav, tap)
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
        // The source actually opened (the macOS tap or ScreenCaptureKit), not the backend's
        // name, which is `screencapturekit` for both (dot-4uzgol.98).
        let opened = rec.system_source();
        let out = json!({"session": id, "path": wav, "backend": opened.unwrap_or(be.name()),
            "system_backend": opened,
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
        let summary = rec.stop().map_err(audio_err);
        self.suspend_monitor(false);
        let summary = summary?;
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
            "system_backend": summary.system_source, "echo_lag_ms": summary.echo_lag_ms,
            "warning": summary.warning, "gaps": summary.gaps, "transcription": transcription}),
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
            v["system_backend"] = json!(a.rec.system_source());
            v["echo_lag_ms"] = json!(a.rec.echo_lag_ms());
            v["warning"] = json!(a.rec.warning());
            v["gaps"] = json!(a.rec.gaps());
            return;
        }
        v["state"] = json!("saved");
        let meta = v["metadata"].as_object().cloned().unwrap_or_default();
        v["duration_ms"] = ext(&meta, "durationMs").cloned().unwrap_or(Value::Null);
        let audio = v["path"].as_str().map(|d| {
            Path::new(d).join(
                ext(&meta, "audioFile")
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
