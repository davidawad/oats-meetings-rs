//! Configurable hooks (dot-4uzgol.89): `[[hooks]]` entries in the user config run a command when a
//! recording or session event happens (`record.stop`, `notes.generated`, ...).
//!
//! The engine's bus tap (`on_event`) maps the published events onto hook events and starts each
//! enabled matching hook on its own thread, so a hook never blocks a recording, the control socket
//! or the UI. A hook gets the session as `OATS_*` environment variables and the whole event as JSON
//! on stdin; it is killed (its whole process group on unix) past its timeout. Every run (exit code,
//! timing, the first `app.hook_output` bytes of stdout and stderr) is kept for `hook-list` and
//! published as `hook-finished`; a failure is logged and shown, never raised into the session.

use std::collections::VecDeque;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::action::Action;
use crate::config::keys::{self, Kind};
use crate::config::typed::{Config, Dur};
use crate::config::{ConfigError, Result};
use crate::control::engine::Engine;

/// The events a hook can name, in the order a recording meets them.
pub const EVENTS: &[&str] = &[
    "record.start",
    "record.pause",
    "record.stop",
    "transcript.final",
    "calendar.matched",
    "session.saved",
    "notes.generated",
];

/// A `[[hooks]]` table's fields in the order they are written to the user file.
pub const FIELDS: &[&str] = &[
    "name", "event", "command", "shell", "timeout", "enabled", "cwd",
];

/// The key whose range bounds a hook's own `timeout` (and whose value is the default).
const TIMEOUT_KEY: &str = "app.hook_timeout";

fn yes() -> bool {
    true
}

fn is_true(b: &bool) -> bool {
    *b
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// One `[[hooks]]` entry.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Hook {
    /// How `hook-test` / `-edit` / `-remove` name it; empty means `hook-<position>`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    pub event: String,
    /// argv: the program then its arguments. With `shell`, the first item is a shell script and
    /// the rest its positional parameters ($1 ...).
    pub command: Vec<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub shell: bool,
    /// None: `app.hook_timeout`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout: Option<Dur>,
    #[serde(default = "yes", skip_serializing_if = "is_true")]
    pub enabled: bool,
    /// Working directory; empty means the session folder (or the app's own when there is none).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub cwd: String,
}

impl Hook {
    /// `name`, or `hook-<n>` for the n-th (1-based) entry with none.
    pub fn label(&self, index: usize) -> String {
        if self.name.is_empty() {
            format!("hook-{}", index + 1)
        } else {
            self.name.clone()
        }
    }

    pub fn timeout_of(&self, cfg: &Config) -> Dur {
        self.timeout.unwrap_or(cfg.app.hook_timeout)
    }
}

fn bad(msg: impl Into<String>) -> ConfigError {
    ConfigError::new("invalid_value", msg)
}

/// Parse and check a `[[hooks]]` list: known events, a non-empty program, a timeout within the
/// `app.hook_timeout` range and unique names.
pub fn parse(v: &Value) -> Result<Vec<Hook>> {
    let hooks: Vec<Hook> = serde_json::from_value(v.clone())
        .map_err(|e| ConfigError::new("type_mismatch", format!("[[hooks]]: {e}")))?;
    check(&hooks)?;
    Ok(hooks)
}

fn check(hooks: &[Hook]) -> Result<()> {
    let (min_ms, max_ms) = match keys::find(TIMEOUT_KEY).map(|k| k.kind) {
        Some(Kind::Duration { min_ms, max_ms }) => (min_ms, max_ms),
        _ => unreachable!("{TIMEOUT_KEY} is a duration key"),
    };
    let mut seen = std::collections::BTreeSet::new();
    for (i, h) in hooks.iter().enumerate() {
        let label = h.label(i);
        if !EVENTS.contains(&h.event.as_str()) {
            return Err(bad(format!(
                "hook {label}: event \"{}\" is not one of {}",
                h.event,
                EVENTS.join(", ")
            )));
        }
        if h.command.first().is_none_or(|p| p.trim().is_empty()) {
            return Err(bad(format!(
                "hook {label}: command must name a program (an argv array like [\"notify-send\", \"done\"])"
            )));
        }
        if let Some(t) = h.timeout {
            if !(min_ms..=max_ms).contains(&t.0) {
                return Err(bad(format!(
                    "hook {label}: timeout must be from {} to {}",
                    keys::format_duration_ms(min_ms),
                    keys::format_duration_ms(max_ms)
                )));
            }
        }
        if !seen.insert(label.clone()) {
            return Err(bad(format!("hook name {label} is used twice")));
        }
    }
    Ok(())
}

/// The JSON schema of one `[[hooks]]` entry (for `schema.json`).
pub fn item_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["event", "command"],
        "properties": {
            "name": {"type": "string"},
            "event": {"type": "string", "enum": EVENTS},
            "command": {"type": "array", "items": {"type": "string"}, "minItems": 1},
            "shell": {"type": "boolean", "default": false},
            "timeout": {"type": "string", "format": "duration", "pattern": "^[0-9]+(ms|s|m|h|d)$"},
            "enabled": {"type": "boolean", "default": true},
            "cwd": {"type": "string"},
        },
    })
}

/// One finished hook run.
#[derive(Debug, Clone, Serialize)]
pub struct Run {
    pub seq: u64,
    pub hook: String,
    pub event: String,
    pub session: Option<String>,
    /// Started by `hook-test` rather than by the event.
    pub test: bool,
    pub started: String,
    pub duration_ms: u64,
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    /// Why it did not run or did not finish (spawn failure, timeout, signal).
    pub error: Option<String>,
    pub ok: bool,
    pub stdout: String,
    pub stderr: String,
    pub truncated: bool,
}

/// The engine's record of hook runs, newest first.
#[derive(Default)]
pub struct Runner {
    runs: Mutex<VecDeque<Run>>,
    seq: AtomicU64,
}

impl Runner {
    fn keep(&self, run: &Run, max: usize) {
        let mut runs = self.runs.lock().expect("hook runs");
        runs.push_front(run.clone());
        runs.truncate(max.max(1));
    }

    pub fn runs(&self) -> Vec<Run> {
        self.runs
            .lock()
            .expect("hook runs")
            .iter()
            .cloned()
            .collect()
    }
}

/// What a hook is told about the event.
#[derive(Debug, Clone, Default)]
pub struct Ctx {
    pub event: String,
    pub session: Option<String>,
    pub dir: Option<PathBuf>,
    pub title: String,
    pub audio: Option<PathBuf>,
    pub transcript: Option<PathBuf>,
    /// The event's own payload (the `record-stop` answer, ...).
    pub data: Value,
}

impl Ctx {
    fn env(&self, hook: &str) -> Vec<(&'static str, String)> {
        let p = |p: &Option<PathBuf>| {
            p.as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_default()
        };
        vec![
            ("OATS_EVENT", self.event.clone()),
            ("OATS_HOOK", hook.to_string()),
            ("OATS_SESSION_ID", self.session.clone().unwrap_or_default()),
            ("OATS_SESSION_DIR", p(&self.dir)),
            ("OATS_TITLE", self.title.clone()),
            ("OATS_AUDIO", p(&self.audio)),
            ("OATS_TRANSCRIPT", p(&self.transcript)),
        ]
    }

    fn stdin(&self, hook: &str) -> Value {
        json!({
            "event": self.event,
            "hook": hook,
            "session": {
                "id": self.session,
                "dir": self.dir,
                "title": self.title,
                "audio": self.audio,
                "transcript": self.transcript,
            },
            "data": self.data,
        })
    }
}

/// The session's folder, title, audio and transcript (final, else live), as far as they exist.
pub fn context(engine: &Engine, event: &str, session: Option<&str>, data: Value) -> Ctx {
    let mut c = Ctx {
        event: event.to_string(),
        session: session.filter(|s| !s.is_empty()).map(str::to_string),
        data,
        ..Ctx::default()
    };
    let Some(id) = c.session.clone() else {
        return c;
    };
    let Ok(v) = engine.store().get(&id) else {
        return c;
    };
    let dir = v["path"].as_str().map(PathBuf::from);
    c.title = v["title"].as_str().unwrap_or_default().to_string();
    if let Some(d) = &dir {
        let meta = v["metadata"].as_object().cloned().unwrap_or_default();
        let audio = d.join(
            crate::sessions::ext(&meta, "audioFile")
                .and_then(Value::as_str)
                .unwrap_or(crate::record::AUDIO_FILE),
        );
        c.audio = audio.is_file().then_some(audio);
        c.transcript = ["transcript.final.jsonl", "transcript.live.jsonl"]
            .iter()
            .map(|f| d.join(f))
            .find(|p| p.metadata().is_ok_and(|m| m.len() > 0));
    }
    c.dir = dir;
    c
}

/// The hook events a published bus event stands for.
pub fn events_for(topic: &str, data: &Value) -> Vec<&'static str> {
    let matched = data["calendar"]["event"].is_object();
    let mut out = vec![];
    match topic {
        "recording-started" => {
            out.push("record.start");
            if matched {
                out.push("calendar.matched");
            }
        }
        "recording-paused" => out.push("record.pause"),
        "recording-stopped" => {
            out.push("record.stop");
            if data["transcription"]["final"] == "ok" {
                out.push("transcript.final");
            }
            if matched {
                out.push("calendar.matched");
            }
        }
        "session-saved" => {
            out.push("session.saved");
            if data["change"] == "transcribed" {
                out.push("transcript.final");
            }
        }
        _ => {}
    }
    out
}

/// The engine's bus tap: start the hooks for whatever `topic` stands for. Returns at once.
pub fn on_event(engine: &Arc<Engine>, topic: &str, data: &Value) {
    for ev in events_for(topic, data) {
        fire(engine, ev, data.clone());
    }
}

/// Start every enabled hook on `event`, each on its own thread. Returns at once.
pub fn fire(engine: &Arc<Engine>, event: &str, data: Value) {
    let snap = engine.snapshot();
    let hooks: Vec<(String, Hook)> = snap
        .config
        .hooks
        .iter()
        .enumerate()
        .filter(|(_, h)| h.enabled && h.event == event)
        .map(|(i, h)| (h.label(i), h.clone()))
        .collect();
    if hooks.is_empty() {
        return;
    }
    let session = data["session"]
        .as_str()
        .or(data["id"].as_str())
        .map(str::to_string);
    let (ev, event) = (event.to_string(), event.to_string());
    let engine = Arc::clone(engine);
    let spawned = std::thread::Builder::new()
        .name("oats-hooks".into())
        .spawn(move || {
            let ctx = context(&engine, &ev, session.as_deref(), data);
            let threads: Vec<_> = hooks
                .into_iter()
                .map(|(label, hook)| {
                    let (engine, ctx) = (Arc::clone(&engine), ctx.clone());
                    std::thread::Builder::new()
                        .name(format!("oats-hook-{label}"))
                        .spawn(move || run_and_report(&engine, &label, &hook, &ctx, false))
                })
                .collect();
            for t in threads.into_iter().flatten() {
                let _ = t.join();
            }
        });
    if let Err(e) = spawned {
        crate::elog!("oats-meetings-rs: hooks for {event}: cannot start a thread: {e}");
    }
}

/// Run `hook`, keep the run, log a failure and publish `hook-finished`.
fn run_and_report(engine: &Engine, label: &str, hook: &Hook, ctx: &Ctx, test: bool) -> Run {
    let cfg = engine.snapshot().config.clone();
    let mut run = run(label, hook, ctx, &cfg);
    run.test = test;
    run.seq = engine.hooks.seq.fetch_add(1, Ordering::SeqCst) + 1;
    engine.hooks.keep(&run, cfg.app.hook_history as usize);
    if !run.ok {
        crate::elog!(
            "oats-meetings-rs: hook {label} ({}) failed: {}",
            run.event,
            run.error.clone().unwrap_or_else(|| format!(
                "exit code {}",
                run.exit_code.map_or("none".into(), |c| c.to_string())
            ))
        );
    }
    engine.bus.publish(
        "hook-finished",
        serde_json::to_value(&run).unwrap_or(Value::Null),
    );
    run
}

fn command_of(hook: &Hook) -> Command {
    if !hook.shell {
        let mut c = Command::new(&hook.command[0]);
        c.args(&hook.command[1..]);
        return c;
    }
    #[cfg(windows)]
    {
        let mut c = Command::new("cmd");
        c.arg("/C").arg(hook.command.join(" "));
        c
    }
    #[cfg(not(windows))]
    {
        let mut c = Command::new("sh");
        c.arg("-c")
            .arg(&hook.command[0])
            .arg("oats-hook")
            .args(&hook.command[1..]);
        c
    }
}

/// Read all of `r`, keeping the first `max` bytes; true when some were dropped.
pub(crate) fn drain(mut r: impl Read, max: usize) -> (Vec<u8>, bool) {
    let mut kept = Vec::new();
    let mut buf = [0u8; 8192];
    let mut dropped = false;
    loop {
        match r.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                let room = max.saturating_sub(kept.len());
                kept.extend_from_slice(&buf[..n.min(room)]);
                dropped |= n > room;
            }
        }
    }
    (kept, dropped)
}

pub(crate) fn kill(child: &mut Child) {
    #[cfg(unix)]
    {
        // The hook leads its own process group: take its children (a shell's sleep) down too.
        if let Ok(pid) = i32::try_from(child.id()) {
            // SAFETY: kill(2) on a process group we created; no memory is touched.
            unsafe {
                libc::kill(-pid, libc::SIGKILL);
            }
        }
    }
    let _ = child.kill();
}

/// Run one hook to completion (or its timeout) with `ctx`. Never panics, never returns an error:
/// whatever went wrong is in the `Run`.
pub fn run(label: &str, hook: &Hook, ctx: &Ctx, cfg: &Config) -> Run {
    let started = chrono::Utc::now();
    let t0 = Instant::now();
    let mut out = Run {
        seq: 0,
        hook: label.to_string(),
        event: ctx.event.clone(),
        session: ctx.session.clone(),
        test: false,
        started: started.to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        duration_ms: 0,
        exit_code: None,
        timed_out: false,
        error: None,
        ok: false,
        stdout: String::new(),
        stderr: String::new(),
        truncated: false,
    };
    let mut cmd = command_of(hook);
    cmd.envs(ctx.env(label))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if !hook.cwd.is_empty() {
        cmd.current_dir(&hook.cwd);
    } else if let Some(d) = ctx.dir.as_ref().filter(|d| d.is_dir()) {
        cmd.current_dir(d);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            out.error = Some(format!("cannot run {:?}: {e}", hook.command[0]));
            out.duration_ms = t0.elapsed().as_millis() as u64;
            return out;
        }
    };
    let input = serde_json::to_vec(&ctx.stdin(label)).unwrap_or_default();
    let stdin = child.stdin.take().map(|mut w| {
        std::thread::spawn(move || {
            // A hook that never reads its stdin gets a broken pipe here, which is fine.
            let _ = w.write_all(&input);
        })
    });
    let max = cfg.app.hook_output as usize;
    let readers: Vec<_> = [
        child
            .stdout
            .take()
            .map(|r| Box::new(r) as Box<dyn Read + Send>),
        child
            .stderr
            .take()
            .map(|r| Box::new(r) as Box<dyn Read + Send>),
    ]
    .into_iter()
    .map(|r| r.map(|r| std::thread::spawn(move || drain(r, max))))
    .collect();
    let deadline = t0 + hook.timeout_of(cfg).as_duration();
    let poll = cfg.app.hook_poll.as_duration();
    let status = loop {
        match child.try_wait() {
            Ok(Some(st)) => break Some(st),
            Ok(None) if Instant::now() >= deadline => {
                kill(&mut child);
                out.timed_out = true;
                break child.wait().ok();
            }
            Ok(None) => std::thread::sleep(poll),
            Err(e) => {
                out.error = Some(format!("waiting for the hook: {e}"));
                kill(&mut child);
                break child.wait().ok();
            }
        }
    };
    let mut texts = readers.into_iter().map(|r| {
        r.and_then(|h| h.join().ok())
            .unwrap_or_else(|| (Vec::new(), false))
    });
    let (so, so_cut) = texts.next().unwrap_or_default();
    let (se, se_cut) = texts.next().unwrap_or_default();
    if let Some(h) = stdin {
        let _ = h.join();
    }
    out.stdout = String::from_utf8_lossy(&so).into_owned();
    out.stderr = String::from_utf8_lossy(&se).into_owned();
    out.truncated = so_cut || se_cut;
    out.duration_ms = t0.elapsed().as_millis() as u64;
    out.exit_code = status.and_then(|s| s.code());
    if out.timed_out {
        out.error = Some(format!(
            "timed out after {} and was killed",
            keys::format_duration_ms(hook.timeout_of(cfg).0)
        ));
    } else if out.error.is_none() && out.exit_code.is_none() {
        #[cfg(unix)]
        {
            use std::os::unix::process::ExitStatusExt;
            if let Some(sig) = status.and_then(|s| s.signal()) {
                out.error = Some(format!("killed by signal {sig}"));
            }
        }
        if out.error.is_none() {
            out.error = Some("ended without an exit code".into());
        }
    }
    out.ok = !out.timed_out && out.error.is_none() && out.exit_code == Some(0);
    out
}

// ---------- actions ----------

fn hooks_of(engine: &Engine) -> Vec<Hook> {
    engine.snapshot().config.hooks.clone()
}

/// The index of the hook `name` names: its label, or its 1-based position.
fn find(hooks: &[Hook], name: &str) -> Result<usize> {
    hooks
        .iter()
        .enumerate()
        .position(|(i, h)| h.label(i) == name)
        .or_else(|| {
            name.parse::<usize>()
                .ok()
                .filter(|n| (1..=hooks.len()).contains(n))
                .map(|n| n - 1)
        })
        .ok_or_else(|| {
            ConfigError::new(
                "not_found",
                format!(
                    "no hook {name:?}; hooks: {}",
                    hooks
                        .iter()
                        .enumerate()
                        .map(|(i, h)| h.label(i))
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            )
        })
}

fn save(engine: &Engine, hooks: &[Hook], source: &str) -> Result<()> {
    check(hooks)?;
    let v = serde_json::to_value(hooks).map_err(|e| ConfigError::new("internal", e.to_string()))?;
    engine.set("hooks", &v, source).map(|_| ())
}

/// A command given as an argv array, or as one string (split on whitespace; with `shell` it is
/// the script itself).
fn command_arg(v: &Value, shell: bool) -> Result<Vec<String>> {
    match v {
        Value::Array(items) => items
            .iter()
            .map(|x| {
                x.as_str()
                    .map(str::to_string)
                    .ok_or_else(|| bad("command must be an array of strings"))
            })
            .collect(),
        Value::String(s) if shell => Ok(vec![s.clone()]),
        Value::String(s) => Ok(s.split_whitespace().map(str::to_string).collect()),
        _ => Err(bad("command must be an argv array or a string")),
    }
}

fn timeout_arg(t: &str) -> Result<Option<Dur>> {
    if t.trim().is_empty() {
        return Ok(None);
    }
    keys::parse_duration_ms(t)
        .map(|ms| Some(Dur(ms)))
        .ok_or_else(|| bad(format!("timeout \"{t}\" is not a duration like \"30s\"")))
}

fn view(engine: &Engine, i: usize, h: &Hook) -> Value {
    let cfg = &engine.snapshot().config;
    let label = h.label(i);
    let last = engine.hooks.runs().into_iter().find(|r| r.hook == label);
    json!({
        "index": i + 1,
        "name": label,
        "event": h.event,
        "command": h.command,
        "shell": h.shell,
        "timeout": keys::format_duration_ms(h.timeout_of(cfg).0),
        "enabled": h.enabled,
        "cwd": h.cwd,
        "last_run": last,
    })
}

/// The newest session, for a `hook-test` given none.
fn newest_session(engine: &Engine) -> Option<String> {
    let list = engine.store().list(None, None).ok()?;
    list["sessions"]
        .as_array()?
        .iter()
        .max_by(|a, b| {
            a["started_at"]
                .as_str()
                .unwrap_or_default()
                .cmp(b["started_at"].as_str().unwrap_or_default())
        })
        .and_then(|s| s["id"].as_str().map(str::to_string))
}

pub fn dispatch(engine: &Engine, a: &Action) -> Result<Value> {
    match a {
        Action::HookList {} => {
            let hooks = hooks_of(engine);
            Ok(json!({
                "hooks": hooks.iter().enumerate().map(|(i, h)| view(engine, i, h)).collect::<Vec<_>>(),
                "runs": engine.hooks.runs(),
                "events": EVENTS,
            }))
        }
        Action::HookTest {
            hook,
            session,
            event,
        } => {
            let hooks = hooks_of(engine);
            let i = find(&hooks, hook)?;
            let h = &hooks[i];
            let event = event.clone().unwrap_or_else(|| h.event.clone());
            if !EVENTS.contains(&event.as_str()) {
                return Err(bad(format!(
                    "event \"{event}\" is not one of {}",
                    EVENTS.join(", ")
                )));
            }
            let session = match session {
                Some(s) => {
                    engine.store().get(s)?;
                    Some(s.clone())
                }
                None => newest_session(engine),
            };
            let data = json!({"session": session, "test": true});
            let ctx = context(engine, &event, session.as_deref(), data);
            let run = run_and_report(engine, &h.label(i), h, &ctx, true);
            serde_json::to_value(run).map_err(|e| ConfigError::new("internal", e.to_string()))
        }
        Action::HookAdd {
            name,
            event,
            command,
            shell,
            timeout,
            enabled,
            cwd,
        } => {
            let mut hooks = hooks_of(engine);
            let shell = shell.unwrap_or(false);
            let h = Hook {
                name: name.clone(),
                event: event.clone(),
                command: command_arg(command, shell)?,
                shell,
                timeout: timeout_arg(timeout.as_deref().unwrap_or_default())?,
                enabled: enabled.unwrap_or(true),
                cwd: cwd.clone().unwrap_or_default(),
            };
            if name.trim().is_empty() {
                return Err(bad("hook-add needs a name"));
            }
            hooks.push(h);
            save(engine, &hooks, "hook-add")?;
            Ok(view(engine, hooks.len() - 1, &hooks[hooks.len() - 1]))
        }
        Action::HookEdit {
            hook,
            name,
            event,
            command,
            shell,
            timeout,
            enabled,
            cwd,
        } => {
            let mut hooks = hooks_of(engine);
            let i = find(&hooks, hook)?;
            let h = &mut hooks[i];
            if let Some(n) = name {
                h.name = n.clone();
            }
            if let Some(e) = event {
                h.event = e.clone();
            }
            if let Some(s) = shell {
                h.shell = *s;
            }
            if let Some(c) = command {
                h.command = command_arg(c, h.shell)?;
            }
            if let Some(t) = timeout {
                h.timeout = timeout_arg(t)?;
            }
            if let Some(e) = enabled {
                h.enabled = *e;
            }
            if let Some(c) = cwd {
                h.cwd = c.clone();
            }
            save(engine, &hooks, "hook-edit")?;
            Ok(view(engine, i, &hooks[i]))
        }
        Action::HookRemove { hook } => {
            let mut hooks = hooks_of(engine);
            let i = find(&hooks, hook)?;
            let gone = hooks.remove(i);
            save(engine, &hooks, "hook-remove")?;
            Ok(json!({"removed": gone.label(i), "count": hooks.len()}))
        }
        Action::HookEnable { hook, enabled } => {
            let mut hooks = hooks_of(engine);
            let i = find(&hooks, hook)?;
            hooks[i].enabled = enabled.unwrap_or(true);
            save(engine, &hooks, "hook-enable")?;
            Ok(view(engine, i, &hooks[i]))
        }
        _ => unreachable!("not a hook action"),
    }
}
