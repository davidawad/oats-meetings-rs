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
use std::sync::{Arc, Mutex, RwLock};
use std::time::SystemTime;

use serde_json::{json, Value};

use crate::action::{self, Action};
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
        self.recorder.validate(&record::settings_of(new))
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

pub struct Engine {
    opts: EngineOpts,
    state: RwLock<State>,
    subsystems: RwLock<Vec<Arc<dyn Subsystem>>>,
    // Serializes read-modify-write of the user file (config-set vs reload vs watcher).
    write_lock: Mutex<()>,
    pub bus: Arc<Bus>,
    shutdown: AtomicBool,
    window_visible: AtomicBool,
    recorder: Arc<Recorder>,
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

/// Set `key` (dotted) in a TOML document, keeping every comment and the layout around it.
fn set_in_doc(doc: &mut toml_edit::DocumentMut, key: &str, value: &Value) -> Result<()> {
    use toml_edit::{Item, Table, TableLike};
    let new_val: toml_edit::Value = match value {
        Value::Bool(b) => (*b).into(),
        Value::String(s) => s.as_str().into(),
        Value::Number(n) if n.is_i64() => n.as_i64().unwrap_or_default().into(),
        other => {
            return Err(ConfigError::new(
                "type_mismatch",
                format!("cannot store {other} in TOML"),
            ))
        }
    };
    let parts: Vec<&str> = key.split('.').collect();
    let (leaf, parents) = parts.split_last().expect("non-empty key");
    let mut cur: &mut dyn TableLike = doc.as_table_mut();
    for part in parents {
        if !cur.contains_key(part) {
            let mut t = Table::new();
            t.set_implicit(true);
            cur.insert(part, Item::Table(t));
        }
        cur = cur
            .get_mut(part)
            .and_then(Item::as_table_like_mut)
            .ok_or_else(|| {
                ConfigError::new(
                    "type_mismatch",
                    format!("'{part}' in user config is not a table"),
                )
            })?;
    }
    match cur.get_mut(leaf) {
        Some(Item::Value(old)) => {
            let mut nv = new_val;
            *nv.decor_mut() = old.decor().clone();
            *old = nv;
        }
        _ => {
            cur.insert(leaf, Item::Value(new_val));
        }
    }
    Ok(())
}

impl Engine {
    pub(crate) fn store(&self) -> Store {
        let snap = self.snapshot();
        let root = if snap.config.sessions.root.is_empty() {
            self.opts.sessions_dir.clone()
        } else {
            PathBuf::from(&snap.config.sessions.root)
        };
        Store::new(root).with_max_id(snap.config.sessions.max_id_len as usize)
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
        let resolved = Self::resolve_with(&opts, text.as_deref())?;
        let config = resolved.typed()?;
        let stamp = stamp_of(&opts.user_config);
        let engine = Arc::new(Engine {
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
            recorder: Arc::new(Recorder::default()),
        });
        engine.register(Arc::new(SessionsSubsystem));
        engine.register(Arc::new(notes::NotesSubsystem));
        engine.register(Arc::new(crate::ics::IcsSubsystem::new(Arc::downgrade(
            &engine,
        ))));
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
        let (path, _) = cli::default_user_config_path(None, env).ok_or_else(|| {
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

    fn resolve_with(opts: &EngineOpts, user: Option<&str>) -> Result<Resolved> {
        resolve::resolve(&Inputs {
            pack: opts.pack.as_deref(),
            os: &opts.os,
            arch: &opts.arch,
            machine: &opts.machine,
            user_toml: user,
        })
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
        let mut note = None;
        let built = (|| -> Result<crate::stt::capture::Plan> {
            let settings = crate::stt::Settings::from_config(&cfg, self.default_sessions_dir())?;
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
                    note = self.first_run_model(&settings, &e);
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

    /// The local model is missing: with `stt.local.auto_download` start fetching it and return the
    /// sentence that tells the user what happens to this recording.
    fn first_run_model(&self, s: &crate::stt::Settings, e: &ConfigError) -> Option<String> {
        if e.code != "model_not_found" || !s.auto_download {
            return None;
        }
        match crate::stt::models::spawn(s, Some(Arc::clone(&self.bus))) {
            Ok(_) => Some(
                "Downloading it now (progress in stt-status); this recording is audio only. Once the model is present, run `ctl stt transcribe <session audio.wav>` to transcribe it"
                    .to_string(),
            ),
            Err(_) => None,
        }
    }

    /// Register a subsystem (audio, hotkeys, ...). It sees every later reload.
    pub fn register(&self, s: Arc<dyn Subsystem>) {
        self.subsystems.write().expect("subsystems").push(s);
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

    pub fn request_shutdown(&self) {
        self.shutdown.store(true, Ordering::SeqCst);
    }

    /// The recording in progress as `(session, paused, audio duration ms)`, `Some(None)` when idle;
    /// the outer None means a start or stop is under way and holds the recorder (never blocks).
    pub fn recording(&self) -> Option<Option<(String, bool, u64)>> {
        self.recorder.active_state()
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
        let stamp = stamp_of(&self.opts.user_config);
        {
            let st = self.state.read().expect("state");
            if stamp == st.stamp {
                return false;
            }
        }
        let h = hash_of(&read_user(&self.opts.user_config).ok().flatten());
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
        st.stamp = stamp_of(&self.opts.user_config);
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
        let text = match read_user(&self.opts.user_config) {
            Ok(t) => t,
            Err(e) => {
                self.reload_failed(source, &e);
                return Err(e);
            }
        };
        let result = Self::resolve_with(&self.opts, text.as_deref())
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
            .ok_or_else(|| ConfigError::new("unknown_key", format!("unknown key '{key}'")))?;
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
            (Kind::Str | Kind::Enum(_), Value::Number(n)) => Value::String(n.to_string()),
            _ => value.clone(),
        })
    }

    /// Persist `key = value` in the user file (never a generated file), validated end to end
    /// (typed config and every subsystem) before the file is touched; emits `setting-changed`.
    pub fn set(&self, key: &str, value: &Value, source: &str) -> Result<Value> {
        let value = Self::coerce(key, value)?;
        let _g = self.write_lock.lock().expect("write lock");
        let text = read_user(&self.opts.user_config)?.unwrap_or_default();
        let mut doc: toml_edit::DocumentMut = text
            .parse()
            .map_err(|e| ConfigError::new("parse_error", format!("user config: {e}")))?;
        set_in_doc(&mut doc, key, &value)?;
        let new_text = doc.to_string();
        let resolved = Self::resolve_with(&self.opts, Some(&new_text))?;
        let path = self.opts.user_config.clone();
        let changed = self.swap(resolved, source, &mut || {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)
                    .map_err(|e| ConfigError::new("io_error", format!("{}: {e}", dir.display())))?;
            }
            let tmp = path.with_extension(format!("toml.tmp{}", std::process::id()));
            std::fs::write(&tmp, &new_text)
                .and_then(|_| std::fs::rename(&tmp, &path))
                .map_err(|e| ConfigError::new("io_error", format!("{}: {e}", path.display())))
        })?;
        self.record(true, &Some(new_text));
        Ok(
            json!({"key": key, "value": value, "changed": !changed.is_empty(), "path": self.opts.user_config}),
        )
    }

    fn get(&self, key: &str) -> Result<Value> {
        self.snapshot()
            .resolved
            .flat
            .get(key)
            .cloned()
            .ok_or_else(|| ConfigError::new("unknown_key", format!("unknown key '{key}'")))
    }

    fn session_saved(&self, id: &Value) {
        self.bus
            .publish("session-saved", json!({"id": id, "change": "record"}));
    }

    fn set_window(&self, visible: bool) -> Value {
        let was = self.window_visible.swap(visible, Ordering::SeqCst);
        if was != visible {
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
            Action::Quit {} => {
                self.bus.publish("quit", json!({}));
                self.request_shutdown();
                Ok(json!({"stopping": true}))
            }
            Action::ConfigGet { key } => Ok(json!({"key": key, "value": self.get(key)?})),
            Action::ConfigSet { key, value } => self.set(key, value, "config-set"),
            Action::ConfigReload {} => {
                let changed = self.reload("config-reload")?;
                Ok(json!({"changed": changed}))
            }
            Action::ConfigCheck {} => {
                let typed = self.resolved().typed()?;
                crate::ics::validate_settings(&typed.ics)?;
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
                self.set(
                    "privacy.hide_from_capture",
                    &Value::Bool(!cur),
                    "privacy-toggle",
                )
            }
            Action::DevicesList {} => self.recorder.devices(&self.audio_settings()?),
            Action::RecordStart { session, title } => {
                let cfg = self.snapshot().config.clone();
                let (plan, why) = self.transcribe_plan(&cfg)?;
                let v = self.recorder.start(
                    &self.audio_settings()?,
                    &self.store(),
                    session.as_deref(),
                    title.as_deref(),
                    plan,
                    why,
                )?;
                self.bus.publish("recording-started", v.clone());
                Ok(v)
            }
            Action::RecordStop {} => {
                let v = self.recorder.stop()?;
                self.bus.publish("recording-stopped", v.clone());
                self.session_saved(&v["session"]);
                Ok(v)
            }
            Action::RecordPause {} => {
                let v = self.recorder.pause()?;
                self.bus.publish("recording-paused", v.clone());
                Ok(v)
            }
            Action::SessionList { from, to } => self.store().list(from.as_deref(), to.as_deref()),
            Action::SessionGet { session } => {
                let mut v = self.store().get(session)?;
                self.recorder.annotate(&mut v, session);
                Ok(v)
            }
            Action::TranscriptGet { session } => self.store().transcript(session),
            Action::SessionRename { session, name } => {
                let v = self.store().rename(session, name)?;
                self.session_event(session, "rename");
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
            Action::NotesGenerate {
                session,
                template,
                dry_run,
            } => {
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
            Action::TemplateSave { name, system_prompt, icon, previous, overwrite } => {
                let tdir = self.templates_dir(&self.snapshot().config);
                templates::save(&tdir, name, system_prompt, icon.as_deref(), previous.as_deref(), overwrite.unwrap_or(false))
            }
            Action::TemplateDelete { name } => templates::delete(&self.templates_dir(&self.snapshot().config), name),
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
            Action::SecretSet { .. } | Action::SecretDelete { .. } | Action::SecretHas { .. } => {
                crate::secrets::dispatch(self, a)
            }
            Action::IcsAdd { .. }
            | Action::IcsRemove { .. }
            | Action::IcsList {}
            | Action::IcsRefresh { .. }
            | Action::IcsEvents { .. } => crate::ics::dispatch(self, a),
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
