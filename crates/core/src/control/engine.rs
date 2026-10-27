//! The engine: executes `Action`s against live state. No I/O transport here; the server and (later)
//! the GUI both call `dispatch`.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::SystemTime;

use serde_json::{json, Value};

use crate::action::{self, Action};
use crate::config::keys::{self, Kind};
use crate::config::resolve::{self, Flat, Inputs, Resolved};
use crate::config::{cli, ConfigError, Result, SCHEMA_VERSION};
use crate::sessions::Store;

use super::bus::Bus;

#[derive(Clone, Debug)]
pub struct EngineOpts {
    pub user_config: PathBuf,
    pub pack: Option<String>,
    pub os: String,
    pub arch: String,
    pub machine: String,
    /// Used when `sessions.root` is empty.
    pub sessions_dir: PathBuf,
}

type Stamp = Option<(SystemTime, u64)>;

struct State {
    resolved: Resolved,
    stamp: Stamp,
}

pub struct Engine {
    opts: EngineOpts,
    state: RwLock<State>,
    // Serializes read-modify-write of the user file (config-set vs reload vs watcher).
    write_lock: Mutex<()>,
    pub bus: Bus,
    shutdown: AtomicBool,
    window_visible: AtomicBool,
}

fn stamp_of(path: &std::path::Path) -> Stamp {
    std::fs::metadata(path).ok().map(|m| (m.modified().unwrap_or(SystemTime::UNIX_EPOCH), m.len()))
}

fn read_user(path: &std::path::Path) -> Result<Option<String>> {
    match std::fs::read_to_string(path) {
        Ok(s) => Ok(Some(s)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(ConfigError::new("io_error", format!("{}: {e}", path.display()))),
    }
}

/// Set `key` (dotted) in a TOML document, keeping every comment and the layout around it.
fn set_in_doc(doc: &mut toml_edit::DocumentMut, key: &str, value: &Value) -> Result<()> {
    use toml_edit::{Item, Table, TableLike};
    let new_val: toml_edit::Value = match value {
        Value::Bool(b) => (*b).into(),
        Value::String(s) => s.as_str().into(),
        other => {
            return Err(ConfigError::new("type_mismatch", format!("cannot store {other} in TOML")))
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
            .ok_or_else(|| ConfigError::new("type_mismatch", format!("'{part}' in user config is not a table")))?;
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
    fn store(&self) -> Store {
        let root = match self.state.read().expect("state").resolved.flat.get("sessions.root") {
            Some(Value::String(s)) if !s.is_empty() => PathBuf::from(s),
            _ => self.opts.sessions_dir.clone(),
        };
        Store::new(root)
    }

    fn confirm_delete(&self) -> bool {
        !matches!(
            self.state.read().expect("state").resolved.flat.get("sessions.confirm_delete"),
            Some(Value::Bool(false))
        )
    }

    fn session_event(&self, id: &str, change: &str) {
        self.bus.publish("session-saved", json!({"id": id, "change": change}));
    }

    pub fn new(opts: EngineOpts) -> Result<Arc<Engine>> {
        let text = read_user(&opts.user_config)?;
        let resolved = Self::resolve_with(&opts, text.as_deref())?;
        let stamp = stamp_of(&opts.user_config);
        Ok(Arc::new(Engine {
            opts,
            state: RwLock::new(State { resolved, stamp }),
            write_lock: Mutex::new(()),
            bus: Bus::default(),
            shutdown: AtomicBool::new(false),
            window_visible: AtomicBool::new(false),
        }))
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

    pub fn is_shutdown(&self) -> bool {
        self.shutdown.load(Ordering::SeqCst)
    }

    pub fn request_shutdown(&self) {
        self.shutdown.store(true, Ordering::SeqCst);
    }

    pub fn resolved(&self) -> Resolved {
        self.state.read().expect("state").resolved.clone()
    }

    pub fn hot_reload_enabled(&self) -> bool {
        matches!(
            self.state.read().expect("state").resolved.flat.get("config.hot_reload"),
            Some(Value::Bool(true))
        )
    }

    /// True when the user file changed on disk since the engine last read or wrote it.
    pub fn file_changed(&self) -> bool {
        stamp_of(&self.opts.user_config) != self.state.read().expect("state").stamp
    }

    fn install(&self, resolved: Resolved, source: &str) -> Vec<String> {
        let mut st = self.state.write().expect("state");
        let changed: Vec<String> = resolved
            .flat
            .iter()
            .filter(|(k, v)| st.resolved.flat.get(*k) != Some(*v))
            .map(|(k, _)| k.clone())
            .collect();
        let old: Flat = std::mem::replace(&mut st.resolved, resolved).flat;
        st.stamp = stamp_of(&self.opts.user_config);
        let new = st.resolved.flat.clone();
        drop(st);
        for k in &changed {
            self.bus.publish(
                "setting-changed",
                json!({"key": k, "old": old.get(k), "new": new.get(k), "source": source}),
            );
        }
        changed
    }

    /// Re-read the user file; publish `setting-changed` for every key whose value changed.
    pub fn reload(&self, source: &str) -> Result<Vec<String>> {
        let _g = self.write_lock.lock().expect("write lock");
        let text = read_user(&self.opts.user_config)?;
        let resolved = Self::resolve_with(&self.opts, text.as_deref())?;
        let changed = self.install(resolved, source);
        self.bus.publish("config-reloaded", json!({"source": source, "changed": changed}));
        Ok(changed)
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
            (Kind::Str | Kind::Enum(_), Value::Bool(b)) => Value::String(b.to_string()),
            (Kind::Str | Kind::Enum(_), Value::Number(n)) => Value::String(n.to_string()),
            _ => value.clone(),
        })
    }

    /// Persist `key = value` in the user file (never a generated file), validated by resolving
    /// the would-be file first; emits `setting-changed` when the resolved value changes.
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
        if let Some(dir) = self.opts.user_config.parent() {
            std::fs::create_dir_all(dir)
                .map_err(|e| ConfigError::new("io_error", format!("{}: {e}", dir.display())))?;
        }
        let tmp = self
            .opts
            .user_config
            .with_extension(format!("toml.tmp{}", std::process::id()));
        std::fs::write(&tmp, &new_text)
            .and_then(|_| std::fs::rename(&tmp, &self.opts.user_config))
            .map_err(|e| ConfigError::new("io_error", format!("{}: {e}", self.opts.user_config.display())))?;
        let changed = self.install(resolved, source);
        Ok(json!({"key": key, "value": value, "changed": !changed.is_empty(), "path": self.opts.user_config}))
    }

    fn get(&self, key: &str) -> Result<Value> {
        self.state
            .read()
            .expect("state")
            .resolved
            .flat
            .get(key)
            .cloned()
            .ok_or_else(|| ConfigError::new("unknown_key", format!("unknown key '{key}'")))
    }

    fn set_window(&self, visible: bool) -> Value {
        let was = self.window_visible.swap(visible, Ordering::SeqCst);
        if was != visible {
            self.bus.publish("window-changed", json!({"visible": visible}));
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
                self.resolved().typed()?;
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
                self.set("privacy.hide_from_capture", &Value::Bool(!cur), "privacy-toggle")
            }
            Action::SessionList { from, to } => self.store().list(from.as_deref(), to.as_deref()),
            Action::SessionGet { session } => self.store().get(session),
            Action::TranscriptGet { session } => self.store().transcript(session),
            Action::SessionRename { session, name } => {
                let v = self.store().rename(session, name)?;
                self.session_event(session, "rename");
                Ok(v)
            }
            Action::SessionTag { session, tag, remove } => {
                let v = self.store().tag(session, tag, remove.unwrap_or(false))?;
                self.session_event(session, "tag");
                Ok(v)
            }
            Action::SessionDelete { session, confirm } => {
                if self.confirm_delete() && *confirm != Some(true) {
                    return Err(ConfigError::new(
                        "confirmation_required",
                        "deleting a session needs confirm=true (--confirm); sessions.confirm_delete is on",
                    ));
                }
                let v = self.store().delete(session)?;
                self.bus.publish("session-deleted", json!({"id": session}));
                Ok(v)
            }
            Action::SessionImport { from, overwrite } => {
                let v = self.store().import(std::path::Path::new(from), overwrite.unwrap_or(false))?;
                for id in v["imported"].as_array().into_iter().flatten().filter_map(Value::as_str) {
                    self.session_event(id, "import");
                }
                Ok(v)
            }
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
