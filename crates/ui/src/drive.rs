//! Headless driving of the GUI (dot-4uzgol.53): the `ui-select`, `ui-state` and `ui-invoke`
//! actions reach the open window through a `Port` the host registers on the engine. Each request
//! runs on the GPUI thread at the next host tick, against the same view functions a click calls,
//! and answers with `dump`: the active tab and what its view shows (rows, labels, errors). So an
//! agent can switch views, press their buttons and check the result with `ctl` alone, without
//! foreground input or accessibility nodes (GPUI content exposes none).

use std::collections::BTreeMap;
use std::sync::mpsc::{sync_channel, SyncSender};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use gpui::Context;
use oats_core::config::{ConfigError, Result};
use oats_core::control::engine::UiPort;
use serde_json::{json, Value};

use crate::settings::Widget;
use crate::state::{badge_text, Control, Controls, Rec, Tab};
use crate::view::MainWindow;
use crate::views::{
    self, calendar, calendar_sources, models, notes::Notes, sessions, templates::Templates, trash,
};

/// One `ui-*` request waiting for the GPUI thread, and where its answer goes.
pub struct Request {
    pub req: Value,
    pub reply: SyncSender<Result<Value>>,
}

/// The engine's handle on the GUI: requests queue here, the host tick answers them.
#[derive(Default)]
pub struct Port {
    queue: Mutex<Vec<Request>>,
}

impl Port {
    /// The queued requests, oldest first.
    pub fn take(&self) -> Vec<Request> {
        std::mem::take(&mut *self.queue.lock().expect("ui queue"))
    }
}

impl UiPort for Port {
    fn call(&self, req: Value, wait: Duration) -> Result<Value> {
        let (tx, rx) = sync_channel(1);
        self.queue
            .lock()
            .expect("ui queue")
            .push(Request { req, reply: tx });
        rx.recv_timeout(wait).unwrap_or_else(|_| {
            Err(ConfigError::new(
                "timeout",
                "the GUI did not answer the ui-* request",
            ))
        })
    }
}

/// `ui-invoke` ops: (op, args, what it presses). `view.verb`; the view is the tab it switches to.
pub const OPS: &[(&str, &str, &str)] = &[
    ("record.start", "", "the Start button"),
    ("record.pause", "", "the Pause / Resume button"),
    ("record.stop", "", "the Stop button"),
    (
        "session.new",
        "",
        "the New session button: clear Live of the finished session (idle only; it stays in Sessions)",
    ),
    (
        "sessions.search",
        "query",
        "type into the search box (empty clears it)",
    ),
    ("sessions.select", "session", "click a session in the list"),
    ("sessions.rename", "name", "rename the selected session"),
    (
        "sessions.tag",
        "tag",
        "add tags (comma-separated) to the selected session",
    ),
    (
        "sessions.untag",
        "tag",
        "remove a tag chip from the selected session",
    ),
    (
        "sessions.delete",
        "session?",
        "Delete (arms first while sessions.confirm_delete is on)",
    ),
    (
        "sessions.transcribe",
        "session?",
        "Transcribe (runs in the background)",
    ),
    ("notes.open", "session", "pick the session whose notes show"),
    ("notes.template", "name", "pick the template for Regenerate"),
    ("notes.save", "notes", "Edit, replace the text, Save"),
    (
        "notes.regenerate",
        "",
        "Regenerate (arms first while notes.confirm_regenerate is on)",
    ),
    ("templates.reload", "", "Reload the template list"),
    ("templates.select", "name", "click a template in the list"),
    (
        "templates.save",
        "name, system_prompt, icon?, previous?, overwrite?",
        "New (or Edit previous), fill the form, Save",
    ),
    (
        "templates.delete",
        "name?",
        "Delete (first call arms, second deletes)",
    ),
    ("calendar.add", "name, url", "fill the add-feed form, Add"),
    ("calendar.remove", "name", "Remove a feed"),
    (
        "calendar.refresh",
        "name?",
        "Refresh one feed, or every enabled feed",
    ),
    ("trash.refresh", "", "Refresh the trash listing"),
    ("trash.restore", "session", "Restore a trashed session"),
    (
        "trash.purge",
        "session?",
        "Delete one (or every) trashed session for good (arms first)",
    ),
    (
        "models.select",
        "model",
        "Pick a catalog model (its row's radio; sets the default, downloads it when auto_download and network.allow)",
    ),
    ("models.use", "model", "Use a catalog model (the row's Use button)"),
    ("models.download", "model", "Download a catalog model"),
    ("models.delete", "model", "Delete a downloaded model"),
    ("settings.set", "key, value", "change one setting row"),
    (
        "settings.search",
        "query",
        "type into the Settings search bar and press Enter (empty clears it)",
    ),
    (
        "keys.press",
        "keys",
        "press one keystroke (space, 1, mod-shift-r) as the keyboard would: bare keys only while nothing is being typed in",
    ),
    (
        "keys.rebind",
        "command, keys",
        "Rebind a shortcut (refused with conflict while another command has the keys)",
    ),
    ("keys.reset", "command", "Reset a shortcut to its default"),
    ("keys.clear", "command", "Clear a shortcut (no keys)"),
    (
        "settings.calendar_select",
        "source, account? | calendar?, selected?",
        "tick (default) or untick a calendar or account checkbox under Calendar sources",
    ),
    (
        "settings.calendar_enable",
        "name, enabled?",
        "a calendar source's on/off toggle (turning eventkit on asks for Calendars access once)",
    ),
    (
        "settings.calendar_use_mac",
        "",
        "Use macOS Calendar: add the eventkit source (asks for Calendars access once)",
    ),
    (
        "settings.calendar_access",
        "",
        "Allow Calendars access (the one macOS prompt, only while undetermined)",
    ),
    (
        "settings.calendar_add",
        "kind, name?, url?, username?, password?, client_id?, client_secret?, tenant?",
        "add a calendar source of a kind (os-local, google, microsoft, caldav, ics, eventkit), as the Add buttons do",
    ),
    (
        "settings.calendar_command",
        "name, command?, source?, format?, timeout?, cwd?, env?",
        "Calendar sources: Add command feed (or a command source's Edit, source=<its name>), fill the form (command: program and arguments, or a JSON array; env: NAME=value ...), Save",
    ),
    (
        "settings.calendar_connect",
        "source",
        "Connect: sign a google / microsoft source in (opens the browser); discover a caldav source's calendars",
    ),
    (
        "settings.calendar_disconnect",
        "source",
        "Sign out: forget a google / microsoft source's tokens",
    ),
    (
        "settings.hook_add",
        "name, event, command, shell?, timeout?, cwd?",
        "Hooks: Add hook, fill the form (command: program and arguments, or with shell the script), Save",
    ),
    (
        "settings.hook_edit",
        "hook, name?, event?, command?, shell?, timeout?, cwd?",
        "Hooks: a hook's Edit, change the given fields, Save",
    ),
    (
        "settings.hook_enable",
        "hook, enabled?",
        "Hooks: a hook's on/off toggle",
    ),
    (
        "settings.hook_test",
        "hook",
        "Hooks: a hook's Test (runs it now in the background; the run shows on its row)",
    ),
    (
        "settings.hook_remove",
        "hook",
        "Hooks: a hook's Remove (first call arms, second removes)",
    ),
    (
        "settings.hook_dismiss",
        "",
        "Dismiss the failed-hook banner",
    ),
];

fn bad(msg: impl Into<String>) -> ConfigError {
    ConfigError::new("bad_request", msg)
}

pub fn tab_named(name: &str) -> Result<Tab> {
    let shown = Tab::shown();
    if let Some(t) = shown
        .iter()
        .copied()
        .find(|t| t.label().eq_ignore_ascii_case(name.trim()))
    {
        return Ok(t);
    }
    if Tab::Trash.label().eq_ignore_ascii_case(name.trim()) {
        return Err(ConfigError::new(
            "unsupported",
            "there is no Trash tab on this OS: trashed sessions are in the Finder Trash (Put Back restores them)",
        ));
    }
    let all: Vec<&str> = shown.iter().map(|t| t.label()).collect();
    Err(bad(format!("no tab '{name}'; tabs: {}", all.join(", "))))
}

fn arg<'a>(args: &'a Value, k: &str) -> Option<&'a str> {
    args.get(k).and_then(Value::as_str)
}

fn need<'a>(args: &'a Value, op: &str, k: &str) -> Result<&'a str> {
    arg(args, k).ok_or_else(|| bad(format!("{op} needs args.{k} (a string)")))
}

fn help() -> Value {
    json!({"ops": OPS
        .iter()
        .map(|(op, args, doc)| json!({"op": op, "args": args, "doc": doc}))
        .collect::<Vec<_>>()})
}

fn widget_value(w: &Widget) -> Value {
    match w {
        Widget::Toggle(b) => json!(b),
        Widget::Choice { current, .. } => json!(current),
        Widget::Text(t) => json!(t),
        Widget::Number { value, .. } => json!(value),
        Widget::Duration { text, .. } => json!(text),
        Widget::Keys(keys) => json!(keys
            .iter()
            .map(|k| crate::shortcuts::display(k))
            .collect::<Vec<_>>()),
    }
}

impl MainWindow {
    /// Answer one `ui-*` request (see `Port`).
    pub fn drive(&mut self, req: &Value, cx: &mut Context<Self>) -> Result<Value> {
        match req["op"].as_str() {
            Some("state") => {}
            Some("select") => {
                let t = tab_named(req["tab"].as_str().unwrap_or_default())?;
                self.switch_tab(t, cx);
            }
            Some("invoke") => {
                let name = req["name"].as_str().unwrap_or_default();
                if name == "help" {
                    return Ok(help());
                }
                if name == "keys.press" {
                    let text = need(&req["args"], name, "keys")?.to_string();
                    let ran = self.press_keys(&text, cx).map_err(bad)?;
                    cx.notify();
                    let mut d = self.dump(cx);
                    d["keys"]["pressed"] = json!({"keys": text, "command": ran});
                    return Ok(d);
                }
                self.invoke(name, &req["args"], cx)?;
            }
            other => return Err(bad(format!("unknown ui request {other:?}"))),
        }
        cx.notify();
        Ok(self.dump(cx))
    }

    /// One `ui-invoke` op: switch to its view, then do what the button does.
    fn invoke(&mut self, op: &str, args: &Value, cx: &mut Context<Self>) -> Result<()> {
        let (view, _) = op
            .split_once('.')
            .ok_or_else(|| bad(format!("op '{op}' is not view.verb (ui-invoke op=help)")))?;
        if !OPS.iter().any(|o| o.0 == op) {
            return Err(bad(format!("no op '{op}' (ui-invoke op=help lists them)")));
        }
        if view == "session" {
            if self.state.tab != Tab::Live {
                self.switch_tab(Tab::Live, cx);
            }
        } else if view != "record" && view != "keys" {
            let t = tab_named(view)?;
            if self.state.tab != t {
                self.switch_tab(t, cx);
            }
        }
        // What the view's render reads lazily must be there before a button acts on it.
        self.prepare(cx);
        let selected = || self.state.selected.clone();
        match op {
            "record.start" => self.press(Control::Start, cx),
            "record.pause" => self.press(Control::Pause, cx),
            "record.stop" => self.press(Control::Stop, cx),
            "session.new" => {
                self.new_session(cx);
            }
            "sessions.search" => {
                self.state.cancel_edit();
                self.state.sessions_ui.query = arg(args, "query").unwrap_or_default().trim().into();
                sessions::sync_hits(&mut self.state, &self.engine);
            }
            "sessions.select" => {
                let id = need(args, op, "session")?.to_string();
                self.state.cancel_edit();
                self.state.sessions_ui.confirm = None;
                self.state.select(&self.engine, &id);
            }
            "sessions.rename" | "sessions.tag" => {
                if selected().is_none() {
                    return Err(bad(format!("{op}: select a session first")));
                }
                let (path, k) = if op == "sessions.rename" {
                    (sessions::RENAME, "name")
                } else {
                    (sessions::TAG, "tag")
                };
                let text = need(args, op, k)?.to_string();
                self.state.begin_edit(path, &text);
                self.state.commit_edit(&self.engine);
            }
            "sessions.untag" => {
                let id = selected().ok_or_else(|| bad("sessions.untag: select a session first"))?;
                let tag = need(args, op, "tag")?.to_string();
                sessions::untag(&mut self.state, &self.engine, &id, &tag);
            }
            "sessions.delete" => {
                let id = arg(args, "session")
                    .map(str::to_string)
                    .or_else(selected)
                    .ok_or_else(|| bad("sessions.delete needs args.session or a selection"))?;
                sessions::delete(&mut self.state, &self.engine, &id);
            }
            "sessions.transcribe" => {
                let id = arg(args, "session")
                    .map(str::to_string)
                    .or_else(selected)
                    .ok_or_else(|| bad("sessions.transcribe needs args.session or a selection"))?;
                sessions::transcribe(self, id, cx);
            }
            "notes.open" => {
                let id = need(args, op, "session")?.to_string();
                cx.global_mut::<Notes>().open(&self.engine, &id);
            }
            "notes.template" => {
                let want = need(args, op, "name")?;
                let n = cx.global_mut::<Notes>();
                let t = n
                    .templates
                    .iter()
                    .find(|t| t.eq_ignore_ascii_case(want))
                    .cloned()
                    .ok_or_else(|| bad(format!("notes.template: no template '{want}'")))?;
                n.template = Some(t);
                n.armed = false;
            }
            "notes.save" => {
                let text = need(args, op, "notes")?.to_string();
                let n = cx.global_mut::<Notes>();
                n.begin_edit();
                if n.edit.is_none() {
                    return Err(bad("notes.save: no session open (notes.open first)"));
                }
                n.edit = Some(text);
                views::notes::save(self, cx);
            }
            "notes.regenerate" => views::notes::run_regenerate(self, cx),
            "templates.reload" => {
                let t = cx.default_global::<Templates>();
                t.items = None;
                t.load(self);
            }
            "templates.select" => {
                let name = need(args, op, "name")?.to_string();
                cx.default_global::<Templates>().select(self, &name);
            }
            "templates.save" => {
                let name = need(args, op, "name")?.to_string();
                let prompt = need(args, op, "system_prompt")?.to_string();
                let t = cx.default_global::<Templates>();
                match arg(args, "previous") {
                    Some(p) => {
                        t.select(self, p);
                        if t.selected.is_none() {
                            return Ok(()); // the error is in the dump
                        }
                        t.edit();
                    }
                    None => t.create(),
                }
                if let Some(d) = t.draft.as_mut() {
                    d.name = name;
                    d.prompt = prompt;
                    if let Some(i) = arg(args, "icon") {
                        d.icon = i.to_string();
                    }
                    d.overwrite = args.get("overwrite").and_then(Value::as_bool) == Some(true);
                }
                t.save(self);
            }
            "templates.delete" => {
                let t = cx.default_global::<Templates>();
                if let Some(n) = arg(args, "name") {
                    if t.selected.as_ref().map(|d| d.name.as_str()) != Some(n) {
                        t.select(self, n);
                    }
                }
                if t.selected.is_none() {
                    return Err(bad("templates.delete needs args.name or a selection"));
                }
                t.delete(self);
            }
            "calendar.add" => {
                let (name, url) = (need(args, op, "name")?, need(args, op, "url")?);
                let c = cx.default_global::<calendar::Cal>();
                c.name = name.to_string();
                c.url = url.to_string();
                calendar::add(self, cx);
            }
            "calendar.remove" => {
                let name = need(args, op, "name")?.to_string();
                calendar::remove(self, &name, cx);
            }
            "calendar.refresh" => {
                calendar::refresh(self, arg(args, "name").unwrap_or_default().to_string(), cx)
            }
            "trash.refresh" => {
                self.trash.confirming = None;
                self.trash.load(&self.engine);
            }
            "trash.restore" => {
                let id = need(args, op, "session")?.to_string();
                self.trash.restore(&self.engine, &id);
            }
            "trash.purge" => {
                let what = match arg(args, "session") {
                    Some(id) => trash::Purge::One(id.to_string()),
                    None => trash::Purge::All,
                };
                self.trash.purge(&self.engine, what);
            }
            "models.select" => {
                let model = need(args, op, "model")?.to_string();
                models::select(self, &model);
            }
            "models.use" | "models.download" | "models.delete" => {
                let model = need(args, op, "model")?.to_string();
                let o = match op {
                    "models.use" => models::Op::Use,
                    "models.download" => models::Op::Download,
                    _ => models::Op::Delete,
                };
                models::apply(self, o, &model);
            }
            "keys.rebind" | "keys.reset" | "keys.clear" => {
                let command = need(args, op, "command")?.to_string();
                match op {
                    "keys.rebind" => {
                        let keys = need(args, op, "keys")?.to_string();
                        self.rebind(&command, &keys, cx)?
                    }
                    "keys.reset" => self.reset_keys(&command, cx)?,
                    _ => self.rebind(&command, "", cx)?,
                }
            }
            "settings.set" => {
                let key = need(args, op, "key")?.to_string();
                let value = args
                    .get("value")
                    .cloned()
                    .ok_or_else(|| bad("settings.set needs args.value"))?;
                self.state.act(
                    &self.engine,
                    "config-set",
                    json!({"key": key, "value": value}),
                );
            }
            "settings.calendar_select" => {
                let source = need(args, op, "source")?;
                let (account, cal) = (arg(args, "account"), arg(args, "calendar"));
                if account.is_none() && cal.is_none() {
                    return Err(bad(format!("{op} needs args.account or args.calendar")));
                }
                let selected = args
                    .get("selected")
                    .and_then(Value::as_bool)
                    .unwrap_or(true);
                let params = json!({"source": source, "account": account, "calendar": cal,
                                    "selected": selected});
                calendar_sources::act(self, "calendar-select", params, cx);
            }
            "settings.calendar_enable" => {
                let name = need(args, op, "name")?;
                let on = args.get("enabled").and_then(Value::as_bool).unwrap_or(true);
                let params = json!({"name": name, "enabled": on});
                calendar_sources::act(self, "calendar-source-enable", params, cx);
            }
            "settings.calendar_use_mac" => calendar_sources::act(
                self,
                "calendar-source-add",
                json!({"name": "mac", "kind": "eventkit"}),
                cx,
            ),
            "settings.calendar_access" => {
                calendar_sources::act(self, "calendar-access", json!({"request": true}), cx)
            }
            "settings.calendar_add" => {
                let kind = need(args, op, "kind")?;
                let name = arg(args, "name").map(str::to_string).unwrap_or_else(|| {
                    if kind == "os-local" {
                        "local".into()
                    } else {
                        kind.to_string()
                    }
                });
                let mut params = json!({"name": name, "kind": kind});
                for k in [
                    "url",
                    "username",
                    "password",
                    "client_id",
                    "client_secret",
                    "tenant",
                ] {
                    if let Some(v) = arg(args, k) {
                        params[k] = json!(v);
                    }
                }
                calendar_sources::act(self, "calendar-source-add", params, cx);
            }
            "settings.calendar_command" => {
                let row = match arg(args, "source") {
                    Some(src) => {
                        calendar_sources::prepare(self, cx);
                        let rows = cx.global::<calendar_sources::Sources>().list["sources"].clone();
                        let row = rows
                            .as_array()
                            .into_iter()
                            .flatten()
                            .find(|r| r["name"] == src && r["kind"] == "command")
                            .cloned()
                            .ok_or_else(|| bad(format!("{op}: no command source named '{src}'")))?;
                        Some(row)
                    }
                    None => {
                        need(args, op, "name")?;
                        need(args, op, "command")?;
                        None
                    }
                };
                calendar_sources::open(&mut self.state, row.as_ref());
                if let Some(d) = self.state.calsrc_draft.as_mut() {
                    for (k, f) in [
                        ("name", &mut d.name),
                        ("command", &mut d.command),
                        ("format", &mut d.format),
                        ("timeout", &mut d.timeout),
                        ("cwd", &mut d.cwd),
                        ("env", &mut d.env),
                    ] {
                        if let Some(v) = arg(args, k) {
                            *f = v.to_string();
                        }
                    }
                }
                calendar_sources::save_and_reload(self, cx);
            }
            "settings.calendar_connect" => {
                let source = need(args, op, "source")?;
                calendar_sources::act(self, "calendar-connect", json!({"source": source}), cx);
            }
            "settings.calendar_disconnect" => {
                let source = need(args, op, "source")?;
                calendar_sources::act(self, "calendar-disconnect", json!({"source": source}), cx);
            }
            "settings.search" => {
                let q = arg(args, "query").unwrap_or_default().to_string();
                views::settings::clear(&mut self.state);
                views::settings::begin_search(&mut self.state);
                if let Some(e) = self.state.edit.as_mut() {
                    e.buf = q;
                }
                views::settings::commit(&mut self.state);
            }
            "settings.hook_add" | "settings.hook_edit" => {
                let hook = if op == "settings.hook_edit" {
                    Some(need(args, op, "hook")?.to_string())
                } else {
                    for k in ["name", "event", "command"] {
                        need(args, op, k)?;
                    }
                    None
                };
                views::hooks::prepare(&mut self.state, &self.engine);
                views::hooks::open(&mut self.state, hook.as_deref());
                if let Some(d) = self.state.hooks_ui.draft.as_mut() {
                    for (k, f) in [
                        ("name", &mut d.name),
                        ("event", &mut d.event),
                        ("command", &mut d.command),
                        ("timeout", &mut d.timeout),
                        ("cwd", &mut d.cwd),
                    ] {
                        if let Some(v) = arg(args, k) {
                            *f = v.to_string();
                        }
                    }
                    if let Some(b) = args.get("shell").and_then(Value::as_bool) {
                        d.shell = b;
                    }
                }
                views::hooks::save(&mut self.state, &self.engine);
            }
            "settings.hook_enable" => {
                let hook = need(args, op, "hook")?.to_string();
                let on = args.get("enabled").and_then(Value::as_bool).unwrap_or(true);
                views::hooks::enable(&mut self.state, &self.engine, &hook, on);
            }
            "settings.hook_test" => {
                let hook = need(args, op, "hook")?.to_string();
                views::hooks::test(self, &hook, cx);
            }
            "settings.hook_remove" => {
                let hook = need(args, op, "hook")?.to_string();
                views::hooks::remove(&mut self.state, &self.engine, &hook);
            }
            "settings.hook_dismiss" => self.state.hook_failure = None,
            _ => unreachable!("listed in OPS"),
        }
        Ok(())
    }

    /// The reads the active view's render does on first show, so a dump matches the window.
    fn prepare(&mut self, cx: &mut Context<Self>) {
        match self.state.tab {
            Tab::Sessions => sessions::sync_hits(&mut self.state, &self.engine),
            Tab::Notes => {
                if !cx.default_global::<Notes>().loaded {
                    cx.global_mut::<Notes>().load_templates(&self.engine);
                }
                if cx.global::<Notes>().session.is_none() {
                    if let Some(id) = self.state.selected.clone() {
                        cx.global_mut::<Notes>().open(&self.engine, &id);
                    }
                }
            }
            Tab::Templates => {
                let t = cx.default_global::<Templates>();
                if t.items.is_none() {
                    t.load(self);
                }
            }
            Tab::Calendar => {
                let c = cx.default_global::<calendar::Cal>();
                if c.loaded.is_none() && c.refreshing.is_none() {
                    c.load(self);
                }
            }
            Tab::Settings => {
                calendar_sources::prepare(self, cx);
                views::hooks::prepare(&mut self.state, &self.engine);
            }
            _ => {}
        }
    }

    /// What the window shows now: the `ui-state` answer.
    pub fn dump(&mut self, cx: &mut Context<Self>) -> Value {
        self.prepare(cx);
        let st = &self.state;
        let cfg = self.engine.snapshot();
        let controls = if st.pending.is_some() {
            Controls::BUSY
        } else {
            Controls::of(&st.rec)
        };
        let rec_state = match st.rec {
            Rec::Idle => "idle",
            Rec::Recording { .. } => "recording",
            Rec::Paused { .. } => "paused",
        };
        let badge = match st.pending {
            Some(p) => p.busy_label().to_string(),
            None => badge_text(&st.rec, Instant::now(), cfg.config.ui.show_elapsed),
        };
        // `speaker` is the transcript's id, `label` what the window shows for it.
        let lines = |ls: &[crate::state::Line], labels: &BTreeMap<String, String>| -> Vec<Value> {
            ls.iter()
                .map(|l| json!({"speaker": l.speaker, "label": labels.get(&l.speaker), "text": l.text}))
                .collect()
        };
        let (live_labels, transcript_labels) = (self.live_labels(), self.transcript_labels());
        let edit = st
            .edit
            .as_ref()
            .map(|e| json!({"path": e.path, "text": e.buf, "error": e.error}));
        let view = match st.tab {
            Tab::Live => json!({
                "session": st.rec.session().map(str::to_string).or_else(|| st.live_session.clone()),
                "devices": st.devices,
                "devices_note": st.devices_note,
                "lines": lines(&st.live, &live_labels),
                "speakers": live_labels,
            }),
            Tab::Sessions => json!({
                "query": sessions::query(st),
                "rows": sessions::visible(st).iter().map(|r| json!({
                    "id": r.id,
                    "title": r.title,
                    "started_at": r.started_at,
                    "utterances": r.utterances,
                    "tags": r.tags,
                    "snippet": st.sessions_ui.hits.get(&r.id),
                    "selected": st.selected.as_deref() == Some(r.id.as_str()),
                })).collect::<Vec<_>>(),
                "selected": st.selected,
                "transcript": lines(&st.transcript, &transcript_labels),
                "speakers": transcript_labels,
                "confirm_delete": st.sessions_ui.confirm,
                "delete_label": sessions::delete_label(self.engine.snapshot().config.sessions.delete_mode),
                "transcribing": st.sessions_ui.transcribing,
            }),
            Tab::Notes => {
                let n = cx.global::<Notes>();
                json!({
                    "session": n.session,
                    "notes": n.text,
                    "info": n.info,
                    "templates": n.templates,
                    "template": n.template,
                    "editing": n.edit,
                    "busy": n.busy,
                    "armed": n.armed,
                    "error": n.error,
                })
            }
            Tab::Templates => {
                let t = cx.global::<Templates>();
                json!({
                    "dir": t.dir,
                    "rows": t.items.iter().flatten().map(|i| json!({
                        "name": i.name, "icon": i.icon, "headings": i.headings,
                    })).collect::<Vec<_>>(),
                    "selected": t.selected.as_ref().map(|d| json!({
                        "name": d.name,
                        "icon": d.icon,
                        "system_prompt": d.system_prompt,
                        "headings": d.headings,
                    })),
                    "draft": t.draft.as_ref().map(|d| json!({
                        "previous": d.previous, "name": d.name, "icon": d.icon, "system_prompt": d.prompt,
                    })),
                    "confirm_delete": t.confirm_delete,
                    "error": t.error,
                })
            }
            Tab::Calendar => {
                let c = cx.global::<calendar::Cal>();
                json!({
                    "enabled": c.enabled,
                    "feeds": c.feeds.iter().map(|f| json!({
                        "name": f.name,
                        "url": f.url,
                        "enabled": f.enabled,
                        "fetched_at": f.fetched_at,
                        "events": f.events,
                        "error": f.error,
                    })).collect::<Vec<_>>(),
                    "rows": c.meetings.iter().map(|m| json!({
                        "feed": m.feed,
                        "summary": m.summary,
                        "location": m.location,
                        "day": m.day,
                        "time": calendar::time_label(m),
                    })).collect::<Vec<_>>(),
                    "from": c.from,
                    "to": c.to,
                    "refreshing": c.refreshing,
                    "form": {"name": c.name, "url": c.url},
                    "error": c.error,
                })
            }
            Tab::Trash => {
                let t = &self.trash;
                json!({
                    "rows": t.items.iter().map(|i| json!({
                        "id": i.id, "title": i.title, "deleted_at": i.deleted_at,
                    })).collect::<Vec<_>>(),
                    "confirming": t.confirming.as_ref().map(|p| match p {
                        trash::Purge::One(id) => json!(id),
                        trash::Purge::All => json!("all"),
                    }),
                    "error": t.error,
                })
            }
            Tab::Models => {
                let l = models::load(&self.engine);
                json!({
                    "header": l.header,
                    "note": l.note,
                    "selected": l.selected().map(|r| r.id.clone()),
                    "auto_fetch": l.auto_fetch,
                    "rows": l.rows.iter().map(|r| json!({
                        "id": r.id,
                        "name": r.name,
                        "facts": r.facts(),
                        "size": r.size,
                        "wer": r.wer,
                        "speed": r.speed,
                        "languages": r.languages,
                        "runtime": r.runtime,
                        "percent": r.percent,
                        "status": r.status,
                        "selected": r.selected,
                        "can_use": r.can_use,
                        "can_download": r.can_download,
                        "can_delete": r.can_delete,
                    })).collect::<Vec<_>>(),
                })
            }
            Tab::Settings => json!({
                "search": {
                    "query": views::settings::query(st),
                    "open": st.edit.as_ref().is_some_and(|e| e.path == views::settings::SEARCH),
                    "matches": views::settings::visible(st).iter().map(|g| g.rows.len()).sum::<usize>(),
                    "shown": views::settings::visible(st).iter().flat_map(|g| g.rows.iter().map(|r| r.path)).collect::<Vec<_>>(),
                    "target": st.settings_ui.target,
                },
                "groups": st.settings.iter().map(|g| json!({
                    "name": g.name,
                    "rows": g.rows.iter().map(|r| json!({
                        "path": r.path, "value": widget_value(&r.widget),
                    })).collect::<Vec<_>>(),
                })).collect::<Vec<_>>(),
                "calendar_sources": cx
                    .default_global::<calendar_sources::Sources>()
                    .dump(st.calsrc_draft.as_ref()),
                "hooks": st.hooks_ui.dump(),
            }),
        };
        json!({
            "tab": st.tab.label(),
            "tabs": Tab::shown().into_iter().map(Tab::label).collect::<Vec<_>>(),
            "theme": self.theme.name(),
            "status": st.status,
            "banner": st.stt_problem.as_ref().map(|p| json!({"message": p.message, "fix": p.fix})),
            "hook_banner": st.hook_failure,
            "record_banner": self.rec_warning,
            "stop_banner": st.stop_notice,
            "auto_banner": st.auto_notice,
            "auto_stop_pending": st.auto_pending,
            "transcribe_after_stop": st.after_stop.as_ref().map(|a| json!({"state": a.label(),
                "error": match a { crate::state::AfterStop::Failed(m) => json!(m), _ => Value::Null }})),
            "recording": {
                "state": rec_state,
                "session": st.rec.session(),
                "badge": badge,
                "clock": crate::state::fmt_clock(st.clock_ms(Instant::now())),
                "start": controls.start,
                "pause": controls.pause,
                "pause_label": controls.pause_label,
                "stop": controls.stop,
                "new_session": st.can_new_session(),
                "shown": cfg.config.ui.record_controls,
                "chrome": crate::view::chrome(&st.rec),
            },
            "meters": {
                "source": self.meter_source,
                "mic": self.levels.0,
                "system": self.levels.1,
                "monitor": cfg.config.ui.monitor,
                "error": self.monitor_error,
                "repaints": self.meters(cx).read(cx).repaints,
                "frames": self.gauge_renders(cx),
                "visible": self.visible,
            },
            "edit": edit,
            "keys": {
                "map": self.keymap().json(),
                "help": self.keys.help,
                "listening": self.keys.listening,
                "note": self.keys.note.as_ref().map(|(c, why)| json!({"command": c, "message": why})),
                "typing": self.typing(cx),
            },
            "view": view,
            "frames": self.frames,
            "body_frames": self.body_renders(cx),
            "chrome_frames": self.chrome_renders(cx),
        })
    }
}
