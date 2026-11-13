//! The engine: executes `Action`s against live state. No I/O transport here; the server and (later)
//! the GUI both call `dispatch`.
//!
//! Config is one immutable `Snapshot` behind an `Arc`, swapped atomically: a reader (any subsystem)
//! sees the whole old config or the whole new one, never a mix. A reload is validate-then-swap:
//! resolve the candidate, let every registered `Subsystem` validate it, and only then swap and
//! tell the subsystems. Any failure leaves the previous config running and emits
//! `config-reload-failed`; the watcher does not retry a file whose content has not changed.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock, Weak};
use std::time::{Duration, SystemTime};

use serde_json::{json, Value};

use crate::action::{self, Action};
use crate::config::format::{self, Format};
use crate::config::keys::{self, Kind};
use crate::config::resolve::{self, Inputs, Resolved};
use crate::config::typed::Config;
use crate::config::{cli, ConfigError, Result, SCHEMA_VERSION};
use crate::secrets::{self, SecretStore};
use crate::sessions::Store;
use crate::{kb, notes, templates};
use std::path::Path;

use crate::record::{self, Recorder};

use super::bus::Bus;
use super::subsystem::Subsystem;

#[derive(Clone)]
pub struct EngineOpts {
    pub user_config: PathBuf,
    pub pack: Option<String>,
    pub os: String,
    pub arch: String,
    pub machine: String,
    /// Used when `sessions.root` is empty.
    pub sessions_dir: PathBuf,
    /// Secret store override (tests, embedders); None builds the store from `secrets.backend`.
    pub secrets: Option<Arc<dyn SecretStore>>,
}

type Stamp = Option<(SystemTime, u64)>;

/// The GUI's side of the `ui-*` actions (dot-4uzgol.53). GUI mode registers one with
/// `Engine::set_ui`; it runs a request on the window's thread and answers with the window's state
/// dump. Without one (headless, or the window is closed) the actions answer `no_window`.
pub trait UiPort: Send + Sync {
    /// `req` is `{"op": "select", "tab": ..}`, `{"op": "state"}` or `{"op": "invoke", "name": ..,
    /// "args": {..}}`. `wait` (the `ui.drive_wait` key) bounds how long it waits for the window.
    fn call(&self, req: Value, wait: Duration) -> Result<Value>;
}

/// What `privacy-toggle` warns, and the UI shows, where capture hiding cannot take effect.
pub const CAPTURE_UNSUPPORTED: &str =
    "hiding from screen capture is not supported on this platform; the window is still capturable";

/// Whether this build's OS implements `privacy.hide_method` = `method`.
pub fn hide_method_supported(method: &str) -> bool {
    match method {
        "sharing_type" => cfg!(target_os = "macos"),
        "display_affinity" => cfg!(target_os = "windows"),
        _ => false,
    }
}

/// Takes part in config reload for audio: both sources off, or a named device that the backend can
/// see is not there, vetoes the change. A running recording keeps the devices and layout it started
/// with; `apply` says so on the bus (`recording-config-deferred`) and the new values take effect at
/// the next `record start`.
struct AudioSubsystem {
    recorder: Arc<Recorder>,
    bus: Arc<Bus>,
}

impl Subsystem for AudioSubsystem {
    fn name(&self) -> &'static str {
        "audio"
    }

    fn validate(&self, new: &Config) -> Result<()> {
        self.recorder
            .validate(&record::settings_of(new), record::device_wait_of(new))
    }

    fn apply(&self, old: &Config, new: &Config) {
        if old.audio != new.audio {
            if let Some(session) = self.recorder.active_id() {
                self.bus.publish(
                    "recording-config-deferred",
                    json!({"session": session, "note": "the running recording keeps its devices and layout; the new audio settings apply to the next record start"}),
                );
            }
        }
    }
}

/// One complete, validated config. Never mutated; replaced wholesale.
#[derive(Debug)]
pub struct Snapshot {
    pub resolved: Resolved,
    pub config: Config,
}

struct State {
    snap: Arc<Snapshot>,
    stamp: Stamp,
    /// Hash of the file content last applied successfully / last rejected (None = no file).
    good: Option<u64>,
    bad: Option<u64>,
}

/// Who a `record-stop` or `quit` without `by` came from: a caller of the engine API itself
/// (ctl fills in `ctl`, the GUI its surface).
const DEFAULT_BY: &str = "api";

pub struct Engine {
    opts: EngineOpts,
    /// The user file: `opts.user_config` until `config-convert` moves it to another format.
    user_path: RwLock<PathBuf>,
    state: RwLock<State>,
    subsystems: RwLock<Vec<Arc<dyn Subsystem>>>,
    // Serializes read-modify-write of the user file (config-set vs reload vs watcher).
    write_lock: Mutex<()>,
    pub bus: Arc<Bus>,
    shutdown: AtomicBool,
    window_visible: AtomicBool,
    /// The app launched (`launched`, `enable_monitor`): only then does `ui.monitor` open devices.
    monitoring: AtomicBool,
    recorder: Arc<Recorder>,
    me: Weak<Engine>,
    // One session-transcribe at a time (ctl, the GUI and the model-arrived pass).
    transcribe_lock: Mutex<()>,
    /// Sessions whose deferred final pass (`audio.transcribe = deferred`) is running; notes for
    /// one wait until it is done (`wait_final_pass`).
    final_passes: (Mutex<std::collections::HashSet<String>>, std::sync::Condvar),
    ui: RwLock<Option<Arc<dyn UiPort>>>,
    event_store: crate::calendar::StoreSlot,
    /// The calendar providers, one per source kind (tests inject fakes).
    calendar_providers: crate::calendar::provider::ProviderSlot,
    /// The hook runs so far (`hook-list`).
    pub(crate) hooks: crate::hooks::Runner,
    /// When the last recording stopped: `record.auto_start.cooldown` counts from it.
    last_stop: Mutex<Option<std::time::Instant>>,
    /// The auto-start watcher thread is running (`watch_auto_start`).
    auto_start: AtomicBool,
}

fn stamp_of(path: &std::path::Path) -> Stamp {
    std::fs::metadata(path)
        .ok()
        .map(|m| (m.modified().unwrap_or(SystemTime::UNIX_EPOCH), m.len()))
}

fn read_user(path: &std::path::Path) -> Result<Option<String>> {
    match std::fs::read_to_string(path) {
        Ok(s) => Ok(Some(s)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(ConfigError::new(
            "io_error",
            format!("{}: {e}", path.display()),
        )),
    }
}

/// Write the user file atomically (a temp file beside it, then rename).
fn write_user(path: &Path, text: &str) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)
            .map_err(|e| ConfigError::new("io_error", format!("{}: {e}", dir.display())))?;
    }
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    let tmp = path.with_extension(format!("{ext}.tmp{}", std::process::id()));
    std::fs::write(&tmp, text)
        .and_then(|_| std::fs::rename(&tmp, path))
        .map_err(|e| ConfigError::new("io_error", format!("{}: {e}", path.display())))
}

fn hash_of(text: &Option<String>) -> Option<u64> {
    text.as_ref().map(|t| {
        let mut h = DefaultHasher::new();
        t.hash(&mut h);
        h.finish()
    })
}

/// Built in: `sessions.root` must not be an existing non-directory.
struct SessionsSubsystem;

impl Subsystem for SessionsSubsystem {
    fn name(&self) -> &'static str {
        "sessions"
    }

    fn validate(&self, new: &Config) -> Result<()> {
        let root = &new.sessions.root;
        if !root.is_empty() && std::path::Path::new(root).is_file() {
            return Err(ConfigError::new(
                "invalid_value",
                format!("sessions.root {root:?} is an existing file, not a directory"),
            ));
        }
        Ok(())
    }
}

impl Engine {
    pub(crate) fn store(&self) -> Store {
        let snap = self.snapshot();
        let root = if snap.config.sessions.root.is_empty() {
            self.opts.sessions_dir.clone()
        } else {
            PathBuf::from(&snap.config.sessions.root)
        };
        Store::new(root)
            .with_max_id(snap.config.sessions.max_id_len as usize)
            .with_snippet_chars(snap.config.sessions.search_snippet_chars as usize)
    }

    fn templates_dir(&self, cfg: &Config) -> PathBuf {
        if cfg.templates.dir.is_empty() {
            self.opts
                .user_config
                .parent()
                .unwrap_or_else(|| Path::new("."))
                .join("templates")
        } else {
            PathBuf::from(&cfg.templates.dir)
        }
    }

    fn export_dir(&self, cfg: &Config) -> PathBuf {
        if cfg.sessions.export_dir.is_empty() {
            crate::openoats::default_export_dir(&|k| std::env::var(k).ok())
        } else {
            PathBuf::from(&cfg.sessions.export_dir)
        }
    }

    fn openoats_dir(&self, cfg: &Config) -> PathBuf {
        if cfg.sessions.openoats_dir.is_empty() {
            crate::openoats::default_openoats_dir(&|k| std::env::var(k).ok())
        } else {
            PathBuf::from(&cfg.sessions.openoats_dir)
        }
    }

    fn kb_dir(&self, cfg: &Config) -> PathBuf {
        if cfg.kb.dir.is_empty() {
            self.opts
                .sessions_dir
                .parent()
                .unwrap_or_else(|| Path::new("."))
                .join("kb")
        } else {
            PathBuf::from(&cfg.kb.dir)
        }
    }

    pub(crate) fn secret_store(&self, cfg: &Config) -> Arc<dyn SecretStore> {
        match &self.opts.secrets {
            Some(s) => Arc::clone(s),
            None => Arc::from(secrets::store_for(&cfg.secrets)),
        }
    }

    fn session_event(&self, id: &str, change: &str) {
        self.bus
            .publish("session-saved", json!({"id": id, "change": change}));
    }

    /// Where the sessions live by default; the ICS cache defaults to a sibling directory.
    pub fn default_sessions_dir(&self) -> &std::path::Path {
        &self.opts.sessions_dir
    }

    pub fn new(opts: EngineOpts) -> Result<Arc<Engine>> {
        let text = read_user(&opts.user_config)?;
        let resolved = Self::resolve_with(&opts, text.as_deref(), &opts.user_config)?;
        let config = resolved.typed()?;
        let stamp = stamp_of(&opts.user_config);
        let engine = Arc::new_cyclic(|me| Engine {
            user_path: RwLock::new(opts.user_config.clone()),
            opts,
            state: RwLock::new(State {
                snap: Arc::new(Snapshot { resolved, config }),
                stamp,
                good: hash_of(&text),
                bad: None,
            }),
            subsystems: RwLock::new(vec![]),
            write_lock: Mutex::new(()),
            bus: Arc::new(Bus::default()),
            shutdown: AtomicBool::new(false),
            window_visible: AtomicBool::new(false),
            monitoring: AtomicBool::new(false),
            recorder: Arc::new(Recorder::default()),
            me: me.clone(),
            transcribe_lock: Mutex::new(()),
            final_passes: Default::default(),
            ui: RwLock::new(None),
            event_store: crate::calendar::StoreSlot::default(),
            calendar_providers: Default::default(),
            hooks: crate::hooks::Runner::default(),
            last_stop: Mutex::new(None),
            auto_start: AtomicBool::new(false),
        });
        // Hooks follow the published events; the tap only starts threads, so it never blocks.
        let me = Arc::downgrade(&engine);
        engine.bus.set_tap(Some(Box::new(move |topic, data| {
            if let Some(e) = me.upgrade() {
                crate::hooks::on_event(&e, topic, data);
            }
        })));
        engine.register(Arc::new(SessionsSubsystem));
        engine.register(Arc::new(notes::NotesSubsystem));
        engine.register(Arc::new(crate::ics::IcsSubsystem::new(Arc::downgrade(
            &engine,
        ))));
        engine.register(Arc::new(crate::calendar::CalendarSubsystem));
        engine.register(Arc::new(crate::stt::SttSubsystem::new(Arc::downgrade(
            &engine,
        ))));
        engine.register(Arc::new(AudioSubsystem {
            recorder: Arc::clone(&engine.recorder),
            bus: Arc::clone(&engine.bus),
        }));
        Ok(engine)
    }

    pub fn from_env(env: &dyn Fn(&str) -> Option<String>) -> Result<Arc<Engine>> {
        let (path, _) = cli::default_user_config_path(None, env)?.ok_or_else(|| {
            ConfigError::new("io_error", "no user config path (set OATS_CONFIG or HOME)")
        })?;
        let (os, arch, machine) = cli::host(env);
        Self::new(EngineOpts {
            user_config: path,
            pack: env("OATS_PACK").filter(|p| !p.is_empty()),
            os,
            arch,
            machine,
            sessions_dir: crate::sessions::default_sessions_dir(env),
            secrets: None,
        })
    }

    /// Resolve with `user` as the text of the user file at `path` (its extension is its format).
    fn resolve_with(opts: &EngineOpts, user: Option<&str>, path: &Path) -> Result<Resolved> {
        resolve::resolve(&Inputs {
            pack: opts.pack.as_deref(),
            os: &opts.os,
            arch: &opts.arch,
            machine: &opts.machine,
            user: user.map(|t| (t, Format::of(path))),
        })
    }

    /// The user config file (`config.toml`, `.json` or `.yaml`).
    pub fn user_config(&self) -> PathBuf {
        self.user_path.read().expect("user path").clone()
    }

    /// Replace the capture backend factory (tests inject a fake; the default is the OS backend).
    pub fn set_audio_factory(&self, f: record::Factory) {
        self.recorder.set_factory(f);
    }

    /// Replace the ASR used by recordings (tests inject a fake; the default is `stt::make_asr`).
    pub fn set_asr_factory(&self, f: record::AsrFactory) {
        self.recorder.set_asr_factory(f);
    }

    fn audio_settings(&self) -> Result<oats_audio::AudioSettings> {
        record::settings_from(&self.snapshot().resolved.flat)
    }

    /// The capture devices as `devices-list` reports them, without waiting on the backend (for
    /// the GUI thread): `Ok(None)` while the first listing is still running, `devices_timeout`
    /// once it has run past `audio.devices_timeout`.
    pub fn devices_cached(&self) -> Result<Option<Value>> {
        let wait = record::device_wait_of(&self.snapshot().config);
        self.recorder.devices_cached(&self.audio_settings()?, wait)
    }

    /// The capture devices as `devices-list` answers them (bounded by `audio.devices_timeout`);
    /// `fresh` asks the backend again instead of reusing a listing younger than `audio.devices_ttl`.
    pub fn devices(&self, fresh: bool) -> Result<Value> {
        let mut wait = record::device_wait_of(&self.snapshot().config);
        if fresh {
            wait.ttl = Duration::ZERO;
        }
        self.recorder.devices(&self.audio_settings()?, wait)
    }

    /// Transcription plan for a new recording per `audio.transcribe`: the plan (if any) and, when
    /// there is none, why not.
    fn transcribe_plan(&self, cfg: &Config) -> Result<(Option<crate::stt::capture::Plan>, Value)> {
        use crate::config::typed::AudioTranscribe;
        if cfg.audio.transcribe == AudioTranscribe::Off {
            return Ok((
                None,
                json!({"state": "off", "reason": "audio.transcribe = off"}),
            ));
        }
        if cfg.audio.transcribe == AudioTranscribe::Deferred {
            // No ASR is built (or loaded) while recording: the final pass builds it at stop.
            return Ok((
                None,
                json!({"state": "deferred",
                       "reason": "audio.transcribe = deferred: transcribed when the recording stops"}),
            ));
        }
        let mut note = None;
        let built = (|| -> Result<crate::stt::capture::Plan> {
            let settings = crate::stt::Settings::from_config(&cfg, self.default_sessions_dir())?;
            crate::stt::fetch_speaker_model(&settings, Some(Arc::clone(&self.bus)));
            if !settings.live_enabled && !settings.final_enabled {
                return Err(ConfigError::new(
                    "nothing_enabled",
                    "stt.live.enabled and stt.final.enabled are both off",
                ));
            }
            let secrets = self.secret_store(cfg);
            let asr = match self.recorder.make_asr(&settings, &|s| {
                crate::stt::make_asr(s, cfg.network.allow, &*secrets)
            }) {
                Ok(a) => a,
                Err(e) => {
                    if self.first_run_model(&settings, &e) {
                        note = Some(
                            "Downloading it now (progress in stt-status); this recording is saved as audio only and transcribed when the download finishes",
                        );
                    }
                    return Err(e);
                }
            };
            Ok(crate::stt::capture::Plan {
                asr,
                settings,
                bus: Arc::clone(&self.bus),
            })
        })();
        match built {
            Ok(p) => Ok((Some(p), Value::Null)),
            Err(e) if cfg.audio.transcribe == AudioTranscribe::Auto => {
                let mut why = json!({"state": "unavailable", "code": e.code, "reason": e.message});
                if let Some(n) = note {
                    why["reason"] = json!(format!("{}. {n}", e.message));
                    why["download"] = json!(true);
                }
                Ok((None, why))
            }
            Err(e) => Err(e),
        }
    }

    /// The local model is missing: with `stt.local.auto_download` start fetching it (or join the
    /// download already running). True when a download is under way.
    fn first_run_model(&self, s: &crate::stt::Settings, e: &ConfigError) -> bool {
        e.code == "model_not_found"
            && s.auto_download
            && crate::stt::models::spawn(s, Some(Arc::clone(&self.bus)), self.on_model_arrived())
                .is_ok()
    }

    /// App launch (GUI or `--headless`; not `Engine::new`, which tests and embedders use): with
    /// `stt.local.auto_download` fetch a missing local model now, so the first recording does not
    /// find it missing, and transcribe the sessions an earlier run left pending.
    pub fn launched(&self) {
        use crate::config::typed::{AudioTranscribe, SttBackend};
        self.enable_monitor();
        let cfg = self.snapshot().config.clone();
        if let Ok(s) = crate::stt::Settings::from_config(&cfg, self.default_sessions_dir()) {
            if cfg.audio.transcribe != AudioTranscribe::Off {
                crate::stt::fetch_speaker_model(&s, Some(Arc::clone(&self.bus)));
            }
            if s.entry.runtime.compiled()
                && cfg.audio.transcribe != AudioTranscribe::Off
                && s.backend == SttBackend::Local
                && s.auto_download
                && !s.entry.present(&s.model_file)
                && crate::stt::models::check(&s).is_ok()
            {
                let _ = crate::stt::models::spawn(
                    &s,
                    Some(Arc::clone(&self.bus)),
                    self.on_model_arrived(),
                );
                return;
            }
        }
        if !self.store().pending().is_empty() {
            self.model_arrived();
        }
    }

    /// What a model download runs once the file is in place: `transcribe_pending`.
    pub(crate) fn on_model_arrived(&self) -> Box<dyn FnOnce() + Send> {
        let me = self.me.clone();
        Box::new(move || {
            if let Some(e) = me.upgrade() {
                e.transcribe_pending();
            }
        })
    }

    /// `transcribe_pending` on its own thread (it runs the ASR over whole recordings).
    pub(crate) fn model_arrived(&self) {
        if let Some(e) = self.me.upgrade() {
            let _ = std::thread::Builder::new()
                .name("oats-transcribe-pending".into())
                .spawn(move || e.transcribe_pending());
        }
    }

    /// Transcribe every session saved as audio only because the model was missing (marked
    /// `transcribePending`), except the one still recording; it is picked up when it stops.
    pub fn transcribe_pending(&self) -> Value {
        let store = self.store();
        let active = self.recorder.active_id();
        let (mut done, mut failed) = (vec![], vec![]);
        for id in store.pending() {
            if active.as_deref() == Some(id.as_str()) {
                continue;
            }
            match self.session_transcribe(&id, false, false) {
                Ok(_) => done.push(id),
                Err(e) if matches!(e.code, "exists" | "no_audio" | "not_found") => {
                    let _ = store.set_pending(&id, false);
                }
                Err(e) => {
                    crate::elog!("oats-meetings-rs: transcribing {id}: {e}");
                    failed.push(json!({"session": id, "code": e.code, "message": e.message}));
                }
            }
        }
        json!({"transcribed": done, "failed": failed})
    }

    /// `session-transcribe`: run the ASR over a saved session's own audio and write the transcript
    /// into it. `fetch` starts the model download when it is missing (the session stays pending).
    fn session_transcribe(&self, id: &str, overwrite: bool, fetch: bool) -> Result<Value> {
        let _one = self
            .transcribe_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if self.recorder.active_id().as_deref() == Some(id) {
            return Err(ConfigError::new(
                "recording",
                format!("session {id} is still recording; stop it first"),
            ));
        }
        let store = self.store();
        let v = store.get(id)?;
        if !overwrite && v["utterance_count"].as_u64().unwrap_or(0) > 0 {
            return Err(ConfigError::new(
                "exists",
                format!("session {id} already has a transcript; overwrite=true replaces it"),
            ));
        }
        let meta = v["metadata"].as_object().cloned().unwrap_or_default();
        let audio = Path::new(v["path"].as_str().unwrap_or_default()).join(
            crate::sessions::ext(&meta, "audioFile")
                .and_then(Value::as_str)
                .unwrap_or(crate::record::AUDIO_FILE),
        );
        if !audio.is_file() {
            return Err(ConfigError::new(
                "no_audio",
                format!("session {id} has no audio to transcribe"),
            ));
        }
        let chans = crate::stt::wav::read(&audio)?;
        let cfg = self.snapshot().config.clone();
        let mut settings = crate::stt::Settings::from_config(&cfg, self.default_sessions_dir())?;
        if cfg.audio.transcribe == crate::config::typed::AudioTranscribe::Deferred {
            // Deferred mode is the final pass alone: no live pass replayed over the recording.
            settings.live_enabled = false;
            settings.final_enabled = true;
        }
        let secrets = self.secret_store(&cfg);
        let asr = self
            .recorder
            .make_asr(&settings, &|s| {
                crate::stt::make_asr(s, cfg.network.allow, &*secrets)
            })
            .map_err(|e| {
                if e.code == "model_not_found" {
                    let _ = store.set_pending(id, true);
                    if fetch && self.first_run_model(&settings, &e) {
                        return ConfigError::new(
                            e.code,
                            format!("{}. Downloading it now (progress in stt-status); this session is transcribed when the download finishes", e.message),
                        );
                    }
                }
                e
            })?;
        let started = meta["startedAt"]
            .as_str()
            .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
            .map_or_else(
                || {
                    chrono::Utc::now()
                        - chrono::Duration::milliseconds((chans.seconds() * 1000.0) as i64)
                },
                |t| t.with_timezone(&chrono::Utc),
            );
        crate::stt::into_session(&store, Some(&self.bus), asr, &settings, &chans, id, started)
    }

    /// Register a subsystem (audio, hotkeys, ...). It sees every later reload.
    pub fn register(&self, s: Arc<dyn Subsystem>) {
        self.subsystems.write().expect("subsystems").push(s);
    }

    /// Route the `ui-*` actions to a GUI (or stop routing them, with `None`).
    /// The macOS Calendar store the eventkit sources read (tests inject a fake).
    pub fn event_store(&self) -> Arc<dyn crate::calendar::eventkit::EventStore> {
        Arc::clone(&self.event_store.0.lock().expect("event store"))
    }

    pub fn set_event_store(&self, store: Arc<dyn crate::calendar::eventkit::EventStore>) {
        *self.event_store.0.lock().expect("event store") = store;
    }

    /// The provider calendar sources of `kind` read through.
    pub fn calendar_provider(
        &self,
        kind: crate::calendar::SourceKind,
    ) -> Arc<dyn crate::calendar::provider::Provider> {
        self.calendar_providers.get(kind)
    }

    /// The command calendar sources' last runs and last good output.
    pub fn calendar_command_runs(&self) -> &crate::calendar::command::Runs {
        self.calendar_providers.command_runs()
    }

    pub fn set_calendar_provider(
        &self,
        kind: crate::calendar::SourceKind,
        p: Arc<dyn crate::calendar::provider::Provider>,
    ) {
        self.calendar_providers.set(kind, p);
    }

    /// The app's own secret store for the tokens it mints (calendar OAuth): `calendar.token_file`,
    /// else `calendar-tokens.json` beside the user config; the `secrets` override in tests.
    pub fn token_store(&self) -> Arc<dyn SecretStore> {
        if let Some(s) = &self.opts.secrets {
            return Arc::clone(s);
        }
        let file = self
            .snapshot()
            .config
            .calendar
            .token_file
            .trim()
            .to_string();
        let path = if file.is_empty() {
            self.user_config()
                .parent()
                .unwrap_or_else(|| Path::new("."))
                .join("calendar-tokens.json")
        } else {
            PathBuf::from(file)
        };
        Arc::new(secrets::FileStore::new(path))
    }

    /// This engine as an `Arc`, for work that outlives the call (a background sign-in).
    pub fn arc(&self) -> Arc<Engine> {
        self.me
            .upgrade()
            .expect("the engine is alive while it runs a call")
    }

    pub fn set_ui(&self, port: Option<Arc<dyn UiPort>>) {
        *self.ui.write().expect("ui") = port;
    }

    fn ui_call(&self, req: Value) -> Result<Value> {
        let port = self.ui.read().expect("ui").clone();
        match port {
            Some(p) => p.call(req, self.snapshot().config.ui.drive_wait.as_duration()),
            None => Err(ConfigError::new(
                "no_window",
                "no GUI window to drive: ui-* actions need GUI mode (not --headless)",
            )),
        }
    }

    pub fn unregister(&self, name: &str) {
        self.subsystems
            .write()
            .expect("subsystems")
            .retain(|s| s.name() != name);
    }

    pub fn is_shutdown(&self) -> bool {
        self.shutdown.load(Ordering::SeqCst)
    }

    /// Logs the caller the first time: an app that quits must say what asked (dot-4uzgol.99).
    #[track_caller]
    pub fn request_shutdown(&self) {
        if !self.shutdown.swap(true, Ordering::SeqCst) {
            let at = std::panic::Location::caller();
            crate::elog!("oats-meetings-rs: shutdown requested at {at}");
        }
    }

    /// The recording in progress as `(session, paused, audio duration ms)`, `Some(None)` when idle;
    /// the outer None means a start or stop is under way and holds the recorder (never blocks).
    pub fn recording(&self) -> Option<Option<(String, bool, u64)>> {
        self.recorder.active_state()
    }

    /// Input levels of the recording in progress as `(mic, system)` RMS in 0..1, for meters.
    pub fn recording_levels(&self) -> Option<(f32, f32)> {
        self.recorder.levels()
    }

    /// Why the recording in progress may be missing its system audio (the macOS tap delivering
    /// only silence before System Audio Recording is allowed), for the window banner.
    pub fn recording_warning(&self) -> Option<String> {
        self.recorder.warning()
    }

    /// The active config, as one consistent unit.
    pub fn snapshot(&self) -> Arc<Snapshot> {
        Arc::clone(&self.state.read().expect("state").snap)
    }

    pub fn resolved(&self) -> Resolved {
        self.snapshot().resolved.clone()
    }

    pub fn hot_reload_enabled(&self) -> bool {
        self.snapshot().config.config.hot_reload
    }

    /// Watcher check: has the user file's *content* changed since it was last applied or
    /// rejected? A rejected file is not retried until its content changes.
    pub fn file_needs_reload(&self) -> bool {
        let path = self.user_config();
        let stamp = stamp_of(&path);
        {
            let st = self.state.read().expect("state");
            if stamp == st.stamp {
                return false;
            }
        }
        let h = hash_of(&read_user(&path).ok().flatten());
        let mut st = self.state.write().expect("state");
        st.stamp = stamp;
        h != st.good && h != st.bad
    }

    /// Validate `candidate` against every subsystem, run `persist` (e.g. write the file), swap
    /// the snapshot, then tell the subsystems. Returns the changed keys.
    fn swap(
        &self,
        candidate: Resolved,
        source: &str,
        persist: &mut dyn FnMut() -> Result<()>,
    ) -> Result<Vec<String>> {
        let config = candidate.typed()?;
        let subs: Vec<Arc<dyn Subsystem>> = self.subsystems.read().expect("subsystems").clone();
        for s in &subs {
            if let Err(e) = s.validate(&config) {
                subs.iter().for_each(|t| t.abort());
                return Err(ConfigError::new(
                    e.code,
                    format!("{}: {}", s.name(), e.message),
                ));
            }
        }
        if let Err(e) = persist() {
            subs.iter().for_each(|t| t.abort());
            return Err(e);
        }
        let new = Arc::new(Snapshot {
            resolved: candidate,
            config,
        });
        let old = {
            let mut st = self.state.write().expect("state");
            std::mem::replace(&mut st.snap, Arc::clone(&new))
        };
        for s in &subs {
            s.apply(&old.config, &new.config);
        }
        self.sync_monitor();
        let changed: Vec<String> = new
            .resolved
            .flat
            .iter()
            .filter(|(k, v)| old.resolved.flat.get(*k) != Some(*v))
            .map(|(k, _)| k.clone())
            .collect();
        for k in &changed {
            self.bus.publish(
                "setting-changed",
                json!({"key": k, "old": old.resolved.flat.get(k), "new": new.resolved.flat.get(k), "source": source}),
            );
        }
        Ok(changed)
    }

    fn record(&self, good: bool, text: &Option<String>) {
        let mut st = self.state.write().expect("state");
        st.stamp = stamp_of(&self.user_config());
        if good {
            st.good = hash_of(text);
            st.bad = None;
        } else {
            st.bad = hash_of(text);
        }
    }

    /// Re-read the user file and swap the whole config. On any failure the previous config stays
    /// active and `config-reload-failed` is published.
    pub fn reload(&self, source: &str) -> Result<Vec<String>> {
        let _g = self.write_lock.lock().expect("write lock");
        let path = self.user_config();
        let text = match read_user(&path) {
            Ok(t) => t,
            Err(e) => {
                self.reload_failed(source, &e);
                return Err(e);
            }
        };
        let result = Self::resolve_with(&self.opts, text.as_deref(), &path)
            .and_then(|r| self.swap(r, source, &mut || Ok(())));
        match result {
            Ok(changed) => {
                self.record(true, &text);
                self.bus.publish(
                    "config-reloaded",
                    json!({"source": source, "changed": changed}),
                );
                Ok(changed)
            }
            Err(e) => {
                self.record(false, &text);
                self.reload_failed(source, &e);
                Err(e)
            }
        }
    }

    fn reload_failed(&self, source: &str, e: &ConfigError) {
        self.bus.publish(
            "config-reload-failed",
            json!({"source": source, "code": e.code, "message": e.message, "kept": "previous config"}),
        );
    }

    fn coerce(key: &str, value: &Value) -> Result<Value> {
        let k = keys::find(key)
            .ok_or_else(|| crate::config::resolve::unknown_key(&format!("'{key}'"), key))?;
        Ok(match (k.kind, value) {
            (Kind::Bool, Value::String(s)) => match s.as_str() {
                "true" => Value::Bool(true),
                "false" => Value::Bool(false),
                _ => value.clone(),
            },
            (Kind::Int { .. }, Value::String(s)) => match s.trim().parse::<i64>() {
                Ok(n) => Value::from(n),
                Err(_) => value.clone(),
            },
            (Kind::Str | Kind::Enum(_), Value::Bool(b)) => Value::String(b.to_string()),
            (Kind::Str | Kind::Enum(_) | Kind::Keys, Value::Number(n)) => {
                Value::String(n.to_string())
            }
            // `ctl config-set hooks '[{...}]'` passes the list as JSON text.
            (Kind::Hooks, Value::String(s)) => serde_json::from_str(s).unwrap_or(value.clone()),
            _ => value.clone(),
        })
    }

    /// Persist `key = value` in the user file (never a generated file), validated end to end
    /// (typed config and every subsystem) before the file is touched; emits `setting-changed`.
    pub fn set(&self, key: &str, value: &Value, source: &str) -> Result<Value> {
        let value = Self::coerce(key, value)?;
        let _g = self.write_lock.lock().expect("write lock");
        let path = self.user_config();
        let text = read_user(&path)?.unwrap_or_default();
        let new_text = format::set(&text, Format::of(&path), key, &value)?;
        let resolved = Self::resolve_with(&self.opts, Some(&new_text), &path)?;
        let changed = self.swap(resolved, source, &mut || write_user(&path, &new_text))?;
        self.record(true, &Some(new_text));
        Ok(json!({"key": key, "value": value, "changed": !changed.is_empty(), "path": path}))
    }

    /// Rewrite the user file in format `to` beside it (`config.toml` -> `config.json`) and remove
    /// the old one. The new file must resolve to exactly the same config first; comments are not
    /// carried over. The engine reads and writes the new file from then on.
    pub fn convert(&self, to: &str) -> Result<Value> {
        let to = Format::named(to)?;
        let _g = self.write_lock.lock().expect("write lock");
        let from = self.user_config();
        let target = from.with_extension(to.ext());
        if Format::of(&from) == to {
            return Ok(json!({"from": from, "path": from, "format": to.ext(), "changed": false}));
        }
        if target.exists() {
            return Err(ConfigError::new(
                "conflicting_config",
                format!("{} already exists; remove it first", target.display()),
            ));
        }
        let text = read_user(&from)?.unwrap_or_default();
        let new_text = format::convert(&text, Format::of(&from), to)?;
        let before = Self::resolve_with(&self.opts, Some(&text), &from)?;
        let after = Self::resolve_with(&self.opts, Some(&new_text), &target)?;
        if before.flat != after.flat {
            return Err(ConfigError::new(
                "invalid_value",
                format!(
                    "{} would not read back the same in {}",
                    from.display(),
                    to.ext()
                ),
            ));
        }
        write_user(&target, &new_text)?;
        if from.exists() {
            std::fs::remove_file(&from)
                .map_err(|e| ConfigError::new("io_error", format!("{}: {e}", from.display())))?;
        }
        *self.user_path.write().expect("user path") = target.clone();
        self.record(true, &Some(new_text));
        Ok(json!({"from": from, "path": target, "format": to.ext(), "changed": true}))
    }

    /// Whether `privacy.hide_from_capture` can take effect: `privacy.hide_method` names a method
    /// this build's OS implements (`none` never does).
    pub fn capture_hiding_supported(&self) -> bool {
        let m = self
            .snapshot()
            .resolved
            .flat
            .get("privacy.hide_method")
            .cloned();
        hide_method_supported(m.as_ref().and_then(Value::as_str).unwrap_or("none"))
    }

    fn get(&self, key: &str) -> Result<Value> {
        self.snapshot()
            .resolved
            .flat
            .get(key)
            .cloned()
            .ok_or_else(|| crate::config::resolve::unknown_key(&format!("'{key}'"), key))
    }

    /// Stop the recording and save it. `by` is who asked (a `record-stop` caller, or `engine`);
    /// `forced` is `(session, reason)` only on a forced stop, the one stop nobody asked for: the
    /// recording could no longer write its audio. Both are logged and in the `recording-stopped` event, whose
    /// `forced` the window shows as a banner (dot-4uzgol.99).
    ///
    /// `cut_ms` is an auto-stop's: the audio from there on is dropped (`Recorder::stop`).
    fn stop_recording(
        &self,
        by: &str,
        forced: Option<(&str, &str)>,
        cut_ms: Option<u64>,
    ) -> Result<Value> {
        let reason = forced.map(|(_, why)| why);
        let stopped = self.recorder.stop(cut_ms);
        *self.last_stop.lock().expect("last stop") = Some(std::time::Instant::now());
        let mut v = match stopped {
            Ok(v) => v,
            Err(e) => {
                // A forced stop is announced even when its audio could not be saved: never silent.
                if let Some((id, why)) = forced {
                    crate::elog!("oats-meetings-rs: recording {id} stopped by {by}: {why} ({e})");
                    self.bus.publish(
                        "recording-stopped",
                        json!({"session": id, "by": by, "forced": true, "reason": why,
                               "error": e.to_string()}),
                    );
                    self.session_saved(&json!(id));
                }
                return Err(e);
            }
        };
        let id = v["session"].as_str().unwrap_or_default().to_string();
        match reason {
            Some(why) => crate::elog!("oats-meetings-rs: recording {id} stopped by {by}: {why}"),
            None => crate::elog!("oats-meetings-rs: recording {id} stopped by {by}"),
        }
        v["by"] = json!(by);
        v["forced"] = json!(reason.is_some());
        v["reason"] = json!(reason);
        if let Some(c) = crate::calendar::on_saved(self, &id) {
            v["calendar"] = c;
        }
        let deferred = v["transcription"]["state"] == "deferred";
        if deferred {
            v["transcription"]["final"] = json!("running");
            self.final_passes
                .0
                .lock()
                .expect("final passes")
                .insert(id.clone());
        }
        self.bus.publish("recording-stopped", v.clone());
        self.session_saved(&v["session"]);
        if deferred {
            v["transcription"] = self.deferred_pass(&id);
        } else if self.store().pending().contains(&id) {
            // The model may have arrived while this pending recording was running.
            self.model_arrived();
        }
        Ok(v)
    }

    /// The final pass of a deferred recording (`audio.transcribe = deferred`), once it stopped:
    /// `session-transcribe` over its audio, final pass only. Its `session-saved` (`transcribed`)
    /// is the `transcript.final` hook event. `transcribe-progress` says `running`, then `done` or
    /// `failed`; a failed one stays pending (a missing model is downloaded per
    /// `stt.local.auto_download` and the session transcribed when it arrives; `session-transcribe`
    /// runs it any time). Returns record-stop's `transcription`.
    fn deferred_pass(&self, id: &str) -> Value {
        let progress = |state: &str, extra: Value| {
            let mut d = json!({"session": id, "mode": "deferred", "state": state});
            if let (Some(d), Value::Object(x)) = (d.as_object_mut(), extra) {
                d.extend(x);
            }
            self.bus.publish("transcribe-progress", d);
        };
        progress("running", json!({}));
        let out = self.session_transcribe(id, false, true);
        {
            let (set, cv) = &self.final_passes;
            set.lock().expect("final passes").remove(id);
            cv.notify_all();
        }
        match out {
            Ok(r) => {
                progress(
                    "done",
                    json!({"final_utterances": r["final_utterances"], "backend": r["backend"]}),
                );
                json!({"state": "done", "mode": "deferred", "backend": r["backend"],
                       "live": "off", "live_utterances": 0, "final": "ok",
                       "final_utterances": r["final_utterances"], "mic_stages": r["mic_stages"]})
            }
            Err(e) => {
                crate::elog!("oats-meetings-rs: deferred transcription of {id}: {e}");
                let _ = self.store().set_pending(id, true);
                let message = format!(
                    "Transcription after stop failed: {}. The audio is saved; session-transcribe runs it again",
                    e.message
                );
                let error = json!({"code": e.code, "message": message});
                progress("failed", json!({"error": error.clone()}));
                json!({"state": "deferred", "mode": "deferred", "live": "off",
                       "final": {"error": error}, "final_utterances": 0})
            }
        }
    }

    /// Block until session `id`'s deferred final pass (if one is running) is done, so notes read
    /// the final transcript as they do after an `auto` stop.
    fn wait_final_pass(&self, id: &str) {
        let (set, cv) = &self.final_passes;
        let mut g = set.lock().expect("final passes");
        while g.contains(id) {
            g = cv.wait(g).expect("final passes");
        }
    }

    /// Watch the recording `id` until it ends: a lost capture is the recorder's to reopen (the
    /// recording goes on), but one that can no longer write its audio is stopped here, with its
    /// reason, rather than left running without a sound (dot-4uzgol.99).
    fn guard_recording(&self, id: &str) {
        let me = Weak::clone(&self.me);
        let watched = id.to_string();
        let spawned = std::thread::Builder::new()
            .name("oats-rec-guard".into())
            .spawn(move || loop {
                let id = &watched;
                let Some(e) = me.upgrade() else { return };
                if e.is_shutdown() {
                    return;
                }
                match e.recorder.active_state() {
                    // A start or stop holds the recorder: look again next round.
                    None => {}
                    Some(Some((cur, _, _))) if cur == *id => {
                        if let Some((cur, why)) = e.recorder.fault() {
                            if cur == *id {
                                let _ = e.stop_recording("engine", Some((id, &why)), None);
                                return;
                            }
                        }
                        if e.auto_stop_tick(id) {
                            return;
                        }
                    }
                    Some(_) => return,
                }
                let cfg = &e.snapshot().config;
                let every = cfg
                    .audio
                    .restart_wait
                    .as_duration()
                    .min(cfg.record.auto_check.as_duration());
                drop(e);
                std::thread::sleep(every);
            });
        if let Err(err) = spawned {
            crate::elog!("oats-meetings-rs: cannot watch recording {id}: {err}");
        }
    }

    /// One auto-stop look at recording `id` (dot-4uzgol.110): warn, withdraw the warning, or stop
    /// it with its trailing silence cut. True once it is stopped.
    fn auto_stop_tick(&self, id: &str) -> bool {
        let rule = crate::autorec::StopRule::of(&self.snapshot().config);
        let after = keys::format_duration_ms(rule.after_ms);
        match self.recorder.auto_stop_check(id, rule) {
            Some(crate::autorec::StopCheck::Warn {
                silence_ms,
                stop_in_ms,
                first: true,
            }) => {
                self.bus.publish(
                    "auto-stop-pending",
                    json!({"session": id, "silence_ms": silence_ms, "stop_in_ms": stop_in_ms,
                           "after": after,
                           "message": format!("No sound for a while: recording stops in {} unless you keep it",
                               keys::format_duration_ms(stop_in_ms.div_ceil(1000) * 1000))}),
                );
                false
            }
            Some(crate::autorec::StopCheck::Resumed) => {
                self.bus
                    .publish("auto-stop-cancelled", json!({"session": id, "by": "sound"}));
                false
            }
            Some(crate::autorec::StopCheck::Stop { silence_ms, cut_ms }) => {
                let message = format!("Recording stopped after {after} of silence");
                crate::elog!("oats-meetings-rs: recording {id}: {message}");
                match self.stop_recording("auto-stop", None, Some(cut_ms)) {
                    Ok(v) => self.bus.publish(
                        "auto-stopped",
                        json!({"session": id, "silence_ms": silence_ms, "after": after,
                               "cut_ms": cut_ms, "trimmed_ms": v["trimmed_ms"],
                               "duration_ms": v["duration_ms"], "message": message}),
                    ),
                    Err(e) => crate::elog!("oats-meetings-rs: auto-stop of {id} failed: {e}"),
                }
                true
            }
            _ => false,
        }
    }

    /// `record-start` (a caller's, or auto-start's with `auto` the reason it started).
    fn start_recording(
        &self,
        session: Option<&str>,
        title: Option<&str>,
        auto: Option<Value>,
    ) -> Result<Value> {
        let cfg = self.snapshot().config.clone();
        let (plan, why) = self.transcribe_plan(&cfg)?;
        // Saved as audio only for want of the model: transcribed once it is present. A deferred
        // recording is pending from its start, so one the app quit during is transcribed later.
        let pending = why["code"] == "model_not_found" || why["state"] == "deferred";
        let store = self.store();
        // The event overlapping now names the session unless a title was given.
        let cal = crate::calendar::for_start(self, chrono::Utc::now(), title);
        let mut v = self.recorder.start(
            &self.audio_settings()?,
            &store,
            session,
            cal.title.as_deref(),
            plan,
            why,
            crate::autorec::StopRule::of(&cfg),
        )?;
        if let Some(id) = v["session"].as_str() {
            if pending {
                store.set_pending(id, true)?;
            }
            if !cal.fields.is_empty() {
                store.merge_meta(id, &cal.fields)?;
            }
        }
        v["title"] = json!(cal.title.unwrap_or_default());
        v["calendar"] = cal.report;
        if let Some(a) = auto {
            v["auto_start"] = a;
        }
        if let Some(id) = v["session"].as_str() {
            self.guard_recording(id);
        }
        self.bus.publish("recording-started", v.clone());
        Ok(v)
    }

    /// The auto-start watcher (dot-4uzgol.110), from app launch until shutdown: while idle, a
    /// source the monitor has heard active for `record.auto_start.after` starts a recording.
    fn watch_auto_start(&self) {
        if self.auto_start.swap(true, Ordering::SeqCst) {
            return;
        }
        let me = Weak::clone(&self.me);
        let spawned = std::thread::Builder::new()
            .name("oats-auto-start".into())
            .spawn(move || loop {
                let Some(e) = me.upgrade() else { return };
                if e.is_shutdown() {
                    return;
                }
                e.auto_start_tick();
                let every = e.snapshot().config.record.auto_check.as_duration();
                drop(e);
                std::thread::sleep(every);
            });
        if let Err(err) = spawned {
            self.auto_start.store(false, Ordering::SeqCst);
            crate::elog!("oats-meetings-rs: cannot watch for auto-start: {err}");
        }
    }

    /// One auto-start look: start a recording when a counted source is due, unless one is
    /// running, the last stopped within the cooldown, or (`only_in_event`) no event is on.
    fn auto_start_tick(&self) {
        let cfg = self.snapshot().config.clone();
        let rule = crate::autorec::StartRule::of(&cfg);
        let Some(src) = self.recorder.activity_due(rule) else {
            return;
        };
        if !matches!(self.recorder.active_state(), Some(None)) {
            return;
        }
        let a = &cfg.record.auto_start;
        let cooling = self
            .last_stop
            .lock()
            .expect("last stop")
            .is_some_and(|t| t.elapsed() < a.cooldown.as_duration());
        if cooling || (a.only_in_event && !crate::calendar::event_now(self, chrono::Utc::now())) {
            self.recorder.reset_activity();
            return;
        }
        let name = crate::autorec::source_name(src);
        let after = keys::format_duration_ms(a.after.0);
        let message = format!("Recording started: {name} audio for {after}");
        let why = json!({"source": name, "after": after});
        match self.start_recording(None, None, Some(why)) {
            Ok(v) => {
                crate::elog!("oats-meetings-rs: {message}");
                self.bus.publish(
                    "auto-started",
                    json!({"session": v["session"], "source": name, "after": after,
                           "message": message}),
                );
            }
            Err(e) => {
                crate::elog!("oats-meetings-rs: auto-start failed: {e}");
                self.recorder.reset_activity();
            }
        }
    }

    fn session_saved(&self, id: &Value) {
        self.bus
            .publish("session-saved", json!({"id": id, "change": "record"}));
    }

    /// Open or close the input monitor (dot-4uzgol.92) for `ui.monitor` and the window: the
    /// worker opens it off this thread, and a recording in progress keeps it closed.
    pub fn sync_monitor(&self) {
        use crate::config::typed::Monitor;
        let cfg = &self.snapshot().config;
        // Auto-start listens through the monitor: it keeps it open while the app runs.
        let on = self.monitoring.load(Ordering::SeqCst)
            && (cfg.record.auto_start.enabled
                || match cfg.ui.monitor {
                    Monitor::Off => false,
                    Monitor::Always => true,
                    Monitor::WindowVisible => self.window_visible.load(Ordering::SeqCst),
                });
        let want = on.then(|| record::settings_of(cfg));
        self.recorder
            .monitor(want, cfg.ui.monitor_retry.as_duration());
    }

    /// Let `ui.monitor` open the devices from now on (app launch; `Engine::new` alone never
    /// monitors, so tests and embedders do not hold a mic).
    pub fn enable_monitor(&self) {
        self.monitoring.store(true, Ordering::SeqCst);
        self.sync_monitor();
        self.watch_auto_start();
    }

    /// The input monitor: open or not, `(mic, system)` levels and callback blocks, the last open
    /// error; None while its worker holds it (never blocks).
    pub fn monitor_state(&self) -> Option<record::MonitorState> {
        self.recorder.monitor_state()
    }

    fn set_window(&self, visible: bool) -> Value {
        let was = self.window_visible.swap(visible, Ordering::SeqCst);
        self.sync_monitor();
        // A show of a visible window still publishes: it asks the GUI to raise the window.
        if was != visible || visible {
            self.bus
                .publish("window-changed", json!({"visible": visible}));
        }
        json!({"visible": visible})
    }

    /// Execute one action. `Subscribe` is handled by the transport (it needs the connection).
    pub fn dispatch(&self, a: &Action) -> Result<Value> {
        match a {
            Action::Ping {} => Ok(json!({"pong": true})),
            Action::Version {} => Ok(json!({
                "app": crate::APP_NAME,
                "version": env!("CARGO_PKG_VERSION"),
                "protocol": super::PROTOCOL,
                "schema_version": SCHEMA_VERSION,
            })),
            Action::ListActions {} => Ok(action::manifest_json()),
            Action::Subscribe { .. } => Err(ConfigError::new(
                "bad_request",
                "subscribe is only valid on a live connection",
            )),
            Action::Quit { by } => {
                // Who quit is logged: an app that quit on its own must say who asked (dot-4uzgol.99).
                let by = by.as_deref().unwrap_or(DEFAULT_BY);
                crate::elog!("oats-meetings-rs: quit requested by {by}");
                self.bus.publish("quit", json!({"by": by}));
                self.request_shutdown();
                Ok(json!({"stopping": true}))
            }
            Action::ConfigGet { key } => Ok(json!({"key": key, "value": self.get(key)?})),
            Action::ConfigSet { key, value } => self.set(key, value, "config-set"),
            Action::ConfigReload {} => {
                let changed = self.reload("config-reload")?;
                Ok(json!({"changed": changed}))
            }
            Action::ConfigConvert { to } => self.convert(to),
            Action::ConfigCheck {} => {
                format::sibling_conflict(&self.user_config())?;
                let typed = self.resolved().typed()?;
                crate::ics::validate_settings(&typed.ics)?;
                crate::calendar::validate(&typed.calendar)?;
                Ok(json!({"valid": true}))
            }
            Action::ConfigResolved {} => {
                let r = self.resolved();
                Ok(json!({"pack": r.pack, "config": r.nested(), "provenance": r.provenance}))
            }
            Action::WindowShow {} => Ok(self.set_window(true)),
            Action::WindowHide {} => Ok(self.set_window(false)),
            Action::WindowToggle {} => {
                let now = !self.window_visible.load(Ordering::SeqCst);
                Ok(self.set_window(now))
            }
            Action::PrivacyToggle {} => {
                let cur = matches!(self.get("privacy.hide_from_capture")?, Value::Bool(true));
                let mut v = self.set(
                    "privacy.hide_from_capture",
                    &Value::Bool(!cur),
                    "privacy-toggle",
                )?;
                // The preference is stored either way, but say plainly when it does nothing here
                // (dot-4uzgol.49): a user sharing the screen must not think the window is hidden.
                let supported = self.capture_hiding_supported();
                v["supported"] = json!(supported);
                if !supported {
                    v["warning"] = json!(CAPTURE_UNSUPPORTED);
                }
                Ok(v)
            }
            Action::DevicesList {} => self.recorder.devices(
                &self.audio_settings()?,
                record::device_wait_of(&self.snapshot().config),
            ),
            Action::RecordStart { session, title } => {
                self.start_recording(session.as_deref(), title.as_deref(), None)
            }
            Action::RecordStop { by } => {
                self.stop_recording(by.as_deref().unwrap_or(DEFAULT_BY), None, None)
            }
            Action::RecordPause {} => {
                let v = self.recorder.pause()?;
                self.bus.publish("recording-paused", v.clone());
                Ok(v)
            }
            Action::RecordKeep { by } => {
                let (session, pending) = self.recorder.keep()?;
                let by = by.as_deref().unwrap_or(DEFAULT_BY);
                if pending {
                    self.bus
                        .publish("auto-stop-cancelled", json!({"session": session, "by": by}));
                }
                Ok(json!({"session": session, "was_pending": pending}))
            }
            Action::RecordAutoStatus {} => {
                let cfg = &self.snapshot().config;
                let mut v = self.recorder.auto_status();
                v["auto_stop"] = json!(cfg.record.auto_stop);
                v["auto_start"] = json!(cfg.record.auto_start);
                v["listening"] =
                    json!(cfg.record.auto_start.enabled && self.auto_start.load(Ordering::SeqCst));
                Ok(v)
            }
            Action::SessionList { from, to } => self.store().list(from.as_deref(), to.as_deref()),
            Action::SessionGet { session } => {
                let mut v = self.store().get(session)?;
                self.recorder.annotate(&mut v, session);
                Ok(v)
            }
            Action::SessionEvents {
                session,
                topic,
                from,
                to,
            } => {
                let session = match session.clone().filter(|s| !s.is_empty()) {
                    Some(s) => s,
                    None => crate::hooks::newest_session(self)
                        .ok_or_else(|| ConfigError::new("not_found", "no sessions"))?,
                };
                let filter = crate::sessions::events::Filter {
                    topic: topic.clone().filter(|t| !t.is_empty()),
                    from: *from,
                    to: *to,
                };
                self.store().events(&session, &filter)
            }
            Action::TranscriptGet { session } => {
                let mut t = self.store().transcript(session)?;
                t["speakers"] = json!(crate::speakers::labels_of(
                    &self.snapshot().config.speakers,
                    &t
                ));
                Ok(t)
            }
            Action::SessionTranscribe { session, overwrite } => {
                self.session_transcribe(session, overwrite.unwrap_or(false), true)
            }
            Action::TranscriptSearch { query, limit } => self.store().search(
                query,
                usize::try_from(limit.unwrap_or(50)).unwrap_or(usize::MAX),
            ),
            Action::SessionRename { session, name } => {
                let v = self.store().rename(session, name)?;
                self.session_event(session, "rename");
                Ok(v)
            }
            Action::SessionSpeakerName {
                session,
                speaker,
                name,
            } => {
                let v = self.store().name_speaker(session, speaker, name)?;
                self.session_event(session, "speaker-name");
                Ok(v)
            }
            Action::SessionTag {
                session,
                tag,
                remove,
            } => {
                let v = self.store().tag(session, tag, remove.unwrap_or(false))?;
                self.session_event(session, "tag");
                Ok(v)
            }
            Action::SessionDelete { session, confirm } => {
                if self.snapshot().config.sessions.confirm_delete && *confirm != Some(true) {
                    return Err(ConfigError::new(
                        "confirmation_required",
                        "deleting a session needs confirm=true (--confirm); sessions.confirm_delete is on",
                    ));
                }
                let v = self
                    .store()
                    .delete(session, self.snapshot().config.sessions.delete_mode)?;
                self.bus.publish("session-deleted", json!({"id": session}));
                Ok(v)
            }
            Action::TrashList {} => self.store().trash_list(),
            Action::TrashRestore { session } => {
                let v = self.store().trash_restore(session)?;
                self.session_event(session, "restore");
                Ok(v)
            }
            Action::TrashEmpty { session, confirm } => {
                if self.snapshot().config.sessions.confirm_delete && *confirm != Some(true) {
                    return Err(ConfigError::new(
                        "confirmation_required",
                        "emptying the trash needs confirm=true (--confirm); sessions.confirm_delete is on",
                    ));
                }
                let v = self.store().trash_empty(session.as_deref())?;
                for id in v["purged"].as_array().into_iter().flatten() {
                    self.bus
                        .publish("session-deleted", json!({"id": id, "change": "purge"}));
                }
                Ok(v)
            }
            Action::NotesGenerate {
                session,
                template,
                dry_run,
            } => {
                self.wait_final_pass(session);
                let snap = self.snapshot();
                let (store, secrets, tdir) = (
                    self.store(),
                    self.secret_store(&snap.config),
                    self.templates_dir(&snap.config),
                );
                let dry = dry_run.unwrap_or(false);
                let v = notes::generate(
                    &notes::Ctx {
                        cfg: &snap.config,
                        store: &store,
                        templates_dir: &tdir,
                        secrets: &*secrets,
                    },
                    session,
                    template,
                    dry,
                )?;
                if !dry {
                    self.session_event(session, "notes");
                    if let Some(me) = self.me.upgrade() {
                        crate::hooks::fire(
                            &me,
                            "notes.generated",
                            json!({"session": session, "template": template, "notes": v.clone()}),
                        );
                    }
                }
                Ok(v)
            }
            Action::NotesGet { session } => self.store().notes(session),
            Action::NotesSet { session, notes } => {
                let v = self.store().edit_notes(session, notes)?;
                self.session_event(session, "notes");
                Ok(v)
            }
            Action::TemplateList {} => {
                let tdir = self.templates_dir(&self.snapshot().config);
                let all = templates::load_all(&tdir)?;
                let items: Vec<Value> = all
                    .iter()
                    .map(|t| json!({"name": t.name, "icon": t.icon, "headings": templates::required_headings(&t.system_prompt)}))
                    .collect();
                Ok(json!({"count": items.len(), "dir": tdir, "templates": items}))
            }
            Action::TemplateGet { name } => {
                let t = templates::find(&self.templates_dir(&self.snapshot().config), name)?;
                let mut v = serde_json::to_value(&t)
                    .map_err(|e| ConfigError::new("parse_error", e.to_string()))?;
                v["headings"] = json!(templates::required_headings(&t.system_prompt));
                Ok(v)
            }
            Action::TemplateSave {
                name,
                system_prompt,
                icon,
                previous,
                overwrite,
            } => {
                let tdir = self.templates_dir(&self.snapshot().config);
                templates::save(
                    &tdir,
                    name,
                    system_prompt,
                    icon.as_deref(),
                    previous.as_deref(),
                    overwrite.unwrap_or(false),
                )
            }
            Action::TemplateDelete { name } => {
                templates::delete(&self.templates_dir(&self.snapshot().config), name)
            }
            Action::TemplateImport { from, overwrite } => {
                let tdir = self.templates_dir(&self.snapshot().config);
                templates::import_openoats(Path::new(from), &tdir, overwrite.unwrap_or(false))
            }
            Action::KbReindex {} => {
                let snap = self.snapshot();
                let secrets = self.secret_store(&snap.config);
                kb::reindex(&kb::Ctx {
                    cfg: &snap.config,
                    dir: self.kb_dir(&snap.config),
                    secrets: &*secrets,
                })
            }
            Action::UiSelect { tab } => self.ui_call(json!({"op": "select", "tab": tab})),
            Action::UiState {} => self.ui_call(json!({"op": "state"})),
            Action::UiInvoke { op, args } => self.ui_call(json!({
                "op": "invoke",
                "name": op,
                "args": args.clone().unwrap_or_else(|| json!({})),
            })),
            Action::KbSearch { query, limit } => {
                let snap = self.snapshot();
                let secrets = self.secret_store(&snap.config);
                let n = usize::try_from(limit.unwrap_or(5)).unwrap_or(5);
                kb::search(
                    &kb::Ctx {
                        cfg: &snap.config,
                        dir: self.kb_dir(&snap.config),
                        secrets: &*secrets,
                    },
                    query,
                    n,
                )
            }
            Action::SessionExport { session, dir } => {
                let cfg = &self.snapshot().config;
                let dir = dir
                    .as_deref()
                    .map(PathBuf::from)
                    .unwrap_or_else(|| self.export_dir(cfg));
                self.store().export_markdown(
                    session,
                    &dir,
                    &cfg.sessions.export_recorder,
                    crate::openoats::zone_named(&cfg.ui.tz),
                    &cfg.speakers,
                )
            }
            Action::SessionExportOpenoats {
                session,
                to,
                overwrite,
            } => {
                let root = to
                    .as_deref()
                    .map(PathBuf::from)
                    .unwrap_or_else(|| self.openoats_dir(&self.snapshot().config).join("sessions"));
                self.store()
                    .export_openoats(session, &root, overwrite.unwrap_or(false))
            }
            Action::TemplateExport { to } => {
                let cfg = &self.snapshot().config;
                let to = to
                    .as_deref()
                    .map(PathBuf::from)
                    .unwrap_or_else(|| self.openoats_dir(cfg).join("templates.json"));
                templates::export_openoats(&self.templates_dir(cfg), &to)
            }
            Action::SessionImport { from, overwrite } => {
                let v = self
                    .store()
                    .import(std::path::Path::new(from), overwrite.unwrap_or(false))?;
                for id in v["imported"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                {
                    self.session_event(id, "import");
                }
                Ok(v)
            }
            Action::SttTranscribe { .. }
            | Action::SttModelFetch { .. }
            | Action::SttModelList {}
            | Action::SttModelDelete { .. }
            | Action::SttStatus {} => crate::stt::dispatch(self, a),
            Action::PermsStatus {} => Ok(crate::perms::status(self)),
            Action::PermsEnsure {} => Ok(crate::perms::ensure(self)),
            Action::SecretSet { .. } | Action::SecretDelete { .. } | Action::SecretHas { .. } => {
                crate::secrets::dispatch(self, a)
            }
            Action::IcsAdd { .. }
            | Action::IcsRemove { .. }
            | Action::IcsList {}
            | Action::IcsRefresh { .. }
            | Action::IcsEvents { .. } => crate::ics::dispatch(self, a),
            Action::HookList {}
            | Action::HookTest { .. }
            | Action::HookEvents { .. }
            | Action::HookAdd { .. }
            | Action::HookEdit { .. }
            | Action::HookRemove { .. }
            | Action::HookEnable { .. } => crate::hooks::dispatch(self, a),
            Action::CalendarSourcesList {}
            | Action::CalendarSourceAdd { .. }
            | Action::CalendarSourceEdit { .. }
            | Action::CalendarSourceRemove { .. }
            | Action::CalendarSourceEnable { .. }
            | Action::CalendarSelect { .. }
            | Action::CalendarAccess { .. }
            | Action::CalendarEvents { .. }
            | Action::CalendarMatch { .. }
            | Action::CalendarConnect { .. }
            | Action::CalendarDisconnect { .. } => crate::calendar::dispatch(self, a),
            // Every action is implemented today; a new "planned" one lands here until its handler.
            #[allow(unreachable_patterns)]
            other => {
                let name = serde_json::to_value(other)
                    .ok()
                    .and_then(|v| v["action"].as_str().map(str::to_string))
                    .unwrap_or_default();
                Err(ConfigError::new(
                    "not_implemented",
                    format!("action '{name}' is planned but not implemented yet"),
                ))
            }
        }
    }
}
