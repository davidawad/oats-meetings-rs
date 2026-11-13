//! The Hooks panel under the Settings tab's Hooks section (dot-4uzgol.89): every `[[hooks]]` entry
//! with its on/off toggle, Test (runs it now, off the UI thread, as `ctl hook-test`), Edit and
//! Remove (first click arms), and its last run: exit code, time and output. Add / Edit open a form
//! whose text boxes are the window's one editor (`State::edit`, Enter keeps the field); Save runs
//! `hook-add` or `hook-edit`, its Match fields (title, attendees, calendar source, calendar
//! only; for `transcript.phrase` phrases, speaker and cooldown, dot-4uzgol.111.6; for
//! `calendar.ending` before end, dot-4uzgol.111.7) as the `match` object (dot-4uzgol.111.4), and
//! on a `record.before_*` event its Blocking toggle (dot-4uzgol.111.7). A failed hook run also raises the window's hook banner
//! (`State::hook_failure`), whatever started it.

use gpui::{div, prelude::*, px, rgb, AnyElement, Context, FontWeight, SharedString, Window};
use serde_json::{json, Value};

use crate::kit::{self, Kind, Size};
use crate::model;
use crate::state::State;
use crate::theme::{self, MONO};
use crate::view::{edit_text, MainWindow};

/// The form's text boxes, as `Edit` paths.
pub(crate) const NAME: &str = "hooks:name";
pub(crate) const COMMAND: &str = "hooks:command";
pub(crate) const TIMEOUT: &str = "hooks:timeout";
pub(crate) const CWD: &str = "hooks:cwd";
pub(crate) const MATCH_TITLE: &str = "hooks:match_title";
pub(crate) const MATCH_ATTENDEES: &str = "hooks:match_attendees";
pub(crate) const MATCH_SOURCE: &str = "hooks:match_source";
pub(crate) const MATCH_PHRASES: &str = "hooks:match_phrases";
pub(crate) const MATCH_COOLDOWN: &str = "hooks:match_cooldown";
pub(crate) const MATCH_BEFORE_END: &str = "hooks:match_before_end";

/// What separates phrases in the Phrases box (a regex may hold a comma).
pub(crate) const PHRASE_SEP: char = ';';

/// The Add / Edit form.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Draft {
    /// The hook being edited (its name), None for a new one.
    pub editing: Option<String>,
    pub name: String,
    pub event: String,
    /// argv split on spaces, or with `shell` the script.
    pub command: String,
    pub shell: bool,
    /// Empty: `app.hook_timeout`.
    pub timeout: String,
    /// `blocking` (sent only on an event that may block).
    pub blocking: bool,
    pub cwd: String,
    /// `match.title`, `match.attendees` (comma-separated), `match.source`, `match.calendar`.
    pub match_title: String,
    pub match_attendees: String,
    pub match_source: String,
    pub match_calendar: Option<bool>,
    /// `match.phrases` (`PHRASE_SEP`-separated), `match.speaker` (empty: any), `match.cooldown`
    /// (empty: `app.hook_cooldown`).
    pub match_phrases: String,
    pub match_speaker: String,
    pub match_cooldown: String,
    /// `match.before_end` (empty: `app.hook_before_end`).
    pub match_before_end: String,
    /// The hook's whole `match` as it was (the fields the form has no box for are kept).
    pub match_rest: Value,
    pub error: Option<String>,
}

impl Draft {
    /// Fill the Match fields from a `match` object.
    pub fn set_match(&mut self, m: &Value) {
        let s = |k: &str| m[k].as_str().unwrap_or_default().to_string();
        self.match_title = s("title");
        self.match_source = s("source");
        self.match_attendees = match &m["attendees"] {
            Value::Array(a) => a
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(", "),
            Value::String(a) => a.clone(),
            _ => String::new(),
        };
        self.match_calendar = m["calendar"].as_bool();
        self.match_phrases = match &m["phrases"] {
            Value::Array(a) => a
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(&format!("{PHRASE_SEP} ")),
            Value::String(a) => a.clone(),
            _ => String::new(),
        };
        self.match_speaker = s("speaker");
        self.match_cooldown = s("cooldown");
        self.match_before_end = s("before_end");
    }

    /// The `blocking` `hook-add` / `hook-edit` get: the toggle on an event that may block (a
    /// `record.before_*`), else false (the toggle is hidden, and blocking elsewhere is refused).
    pub fn blocking_value(&self) -> bool {
        self.blocking && oats_core::hooks::events::may_block(&self.event)
    }

    /// The `match` object `hook-add` / `hook-edit` get: the old one with the form's fields over it.
    pub fn match_value(&self) -> Value {
        let mut m = self.match_rest.as_object().cloned().unwrap_or_default();
        let mut put = |k: &str, v: Option<Value>| {
            match v {
                Some(v) => m.insert(k.into(), v),
                None => m.remove(k),
            };
        };
        let text = |s: &str| Some(s.trim()).filter(|s| !s.is_empty()).map(|s| json!(s));
        put("title", text(&self.match_title));
        put("source", text(&self.match_source));
        let people: Vec<&str> = self
            .match_attendees
            .split(',')
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .collect();
        put("attendees", (!people.is_empty()).then(|| json!(people)));
        put("calendar", self.match_calendar.map(Value::Bool));
        let phrases: Vec<&str> = self
            .match_phrases
            .split(PHRASE_SEP)
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .collect();
        put("phrases", (!phrases.is_empty()).then(|| json!(phrases)));
        put("speaker", text(&self.match_speaker).filter(|s| s != "any"));
        put("cooldown", text(&self.match_cooldown));
        put("before_end", text(&self.match_before_end));
        Value::Object(m)
    }
}

/// Hooks-panel state.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Ui {
    /// The last `hook-list` answer; Null until read (and after anything that changes it).
    pub list: Value,
    pub error: Option<String>,
    pub draft: Option<Draft>,
    /// The hook whose Test is running off the UI thread.
    pub busy: Option<String>,
    /// The hook whose Remove was clicked once.
    pub arming: Option<String>,
}

impl Ui {
    pub fn hooks(&self) -> Vec<Value> {
        self.list["hooks"].as_array().cloned().unwrap_or_default()
    }

    fn events(&self) -> Vec<String> {
        self.list["events"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|e| e.as_str().map(str::to_string))
            .collect()
    }

    /// What the selected event is, from the hook event table (`hook-list` event_docs).
    pub fn event_doc(&self, event: &str) -> String {
        self.list["event_docs"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|e| e["name"] == event)
            .and_then(|e| e["description"].as_str())
            .unwrap_or_default()
            .to_string()
    }

    /// What `ui-state` shows for the panel.
    pub fn dump(&self) -> Value {
        json!({
            "hooks": self.list["hooks"],
            "runs": self.list["runs"],
            "warnings": self.list["warnings"],
            "error": self.error,
            "busy": self.busy,
            "arming": self.arming,
            "draft": self.draft.as_ref().map(|d| json!({
                "editing": d.editing, "name": d.name, "event": d.event, "command": d.command,
                "shell": d.shell, "timeout": d.timeout, "blocking": d.blocking_value(),
                "cwd": d.cwd, "match": d.match_value(), "error": d.error,
                "event_doc": self.event_doc(&d.event),
            })),
        })
    }
}

/// Read `hook-list` into the panel.
pub fn load(st: &mut State, engine: &oats_core::control::engine::Engine) {
    match model::dispatch(engine, "hook-list", json!({})) {
        Ok(v) => {
            st.hooks_ui.list = v;
            st.hooks_ui.error = None;
        }
        Err(e) => st.hooks_ui.error = Some(e.to_string()),
    }
}

/// Read the list when it never was, or was marked stale (before a frame of the Settings tab).
pub fn prepare(st: &mut State, engine: &oats_core::control::engine::Engine) {
    if st.hooks_ui.list.is_null() {
        load(st, engine);
    }
}

pub fn is_ours(path: &str) -> bool {
    path.starts_with("hooks:")
}

/// Enter in a form box: keep its text in the form.
pub fn commit(st: &mut State) {
    let Some(e) = st.edit.take() else { return };
    let Some(d) = st.hooks_ui.draft.as_mut() else {
        return;
    };
    let v = e.buf.trim().to_string();
    match e.path {
        NAME => d.name = v,
        COMMAND => d.command = v,
        TIMEOUT => d.timeout = v,
        CWD => d.cwd = v,
        MATCH_TITLE => d.match_title = v,
        MATCH_ATTENDEES => d.match_attendees = v,
        MATCH_SOURCE => d.match_source = v,
        MATCH_PHRASES => d.match_phrases = v,
        MATCH_COOLDOWN => d.match_cooldown = v,
        MATCH_BEFORE_END => d.match_before_end = v,
        _ => {}
    }
}

/// Open the Add form (`editing` None) or the Edit form for the hook named `name`.
pub fn open(st: &mut State, name: Option<&str>) {
    st.cancel_edit();
    let ui = &mut st.hooks_ui;
    ui.arming = None;
    let first = ui
        .events()
        .first()
        .cloned()
        .unwrap_or_else(|| "record.stop".into());
    ui.draft = Some(match name {
        None => Draft {
            event: "record.stop".into(),
            ..Draft::default()
        },
        Some(n) => {
            let h = ui
                .hooks()
                .into_iter()
                .find(|h| h["name"] == n)
                .unwrap_or_default();
            let words: Vec<String> = h["command"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|x| x.as_str().map(str::to_string))
                .collect();
            let mut d = Draft {
                editing: Some(n.to_string()),
                name: n.to_string(),
                event: h["event"].as_str().unwrap_or(&first).to_string(),
                command: words.join(" "),
                shell: h["shell"] == true,
                timeout: h["timeout"].as_str().unwrap_or_default().to_string(),
                blocking: h["blocking"] == true,
                cwd: h["cwd"].as_str().unwrap_or_default().to_string(),
                match_rest: h["match"].clone(),
                ..Draft::default()
            };
            d.set_match(&h["match"]);
            d
        }
    });
}

/// Save the form through `hook-add` / `hook-edit`; the form stays open with the error on refusal.
pub fn save(st: &mut State, engine: &oats_core::control::engine::Engine) {
    if st.edit.as_ref().is_some_and(|e| is_ours(e.path)) {
        commit(st);
    }
    let Some(d) = st.hooks_ui.draft.clone() else {
        return;
    };
    let command = if d.shell {
        json!([d.command])
    } else {
        json!(d.command.split_whitespace().collect::<Vec<_>>())
    };
    let (id, params) = match &d.editing {
        None => (
            "hook-add",
            json!({"name": d.name, "event": d.event, "command": command, "shell": d.shell,
                   "timeout": d.timeout, "blocking": d.blocking_value(), "cwd": d.cwd,
                   "match": d.match_value()}),
        ),
        Some(old) => (
            "hook-edit",
            json!({"hook": old, "name": d.name, "event": d.event, "command": command,
                   "shell": d.shell, "timeout": d.timeout, "blocking": d.blocking_value(),
                   "cwd": d.cwd, "match": d.match_value()}),
        ),
    };
    match model::dispatch(engine, id, params) {
        Ok(_) => {
            st.hooks_ui.draft = None;
            st.status = format!("Saved hook {}.", d.name);
            load(st, engine);
        }
        Err(e) => {
            if let Some(d) = st.hooks_ui.draft.as_mut() {
                d.error = Some(e.to_string());
            }
        }
    }
}

pub fn cancel(st: &mut State) {
    if st.edit.as_ref().is_some_and(|e| is_ours(e.path)) {
        st.edit = None;
    }
    st.hooks_ui.draft = None;
}

/// Turn a hook on or off (`hook-enable`).
pub fn enable(st: &mut State, engine: &oats_core::control::engine::Engine, name: &str, on: bool) {
    st.status = match model::dispatch(engine, "hook-enable", json!({"hook": name, "enabled": on})) {
        Ok(_) => format!("Hook {name} is {}.", if on { "on" } else { "off" }),
        Err(e) => e.to_string(),
    };
    load(st, engine);
}

/// Remove: the first call arms, the second (on the same hook) removes it.
pub fn remove(st: &mut State, engine: &oats_core::control::engine::Engine, name: &str) {
    if st.hooks_ui.arming.as_deref() != Some(name) {
        st.hooks_ui.arming = Some(name.to_string());
        st.status = format!("Click Remove again to remove hook {name}.");
        return;
    }
    st.hooks_ui.arming = None;
    st.status = match model::dispatch(engine, "hook-remove", json!({"hook": name})) {
        Ok(_) => format!("Removed hook {name}."),
        Err(e) => e.to_string(),
    };
    load(st, engine);
}

/// A run as one line: how it ended and how long it took.
pub fn run_line(r: &Value) -> String {
    let how = if r["ok"] == true {
        "ok".to_string()
    } else if let Some(e) = r["error"].as_str() {
        e.to_string()
    } else {
        format!("exit code {}", r["exit_code"])
    };
    let when = r["started"].as_str().unwrap_or_default();
    format!(
        "{}{how} · {} ms · {when}",
        if r["test"] == true { "test: " } else { "" },
        r["duration_ms"]
    )
}

/// Test: run the hook now off the UI thread (it may run up to its timeout), then re-read.
pub(crate) fn test(w: &mut MainWindow, name: &str, cx: &mut Context<MainWindow>) {
    if w.state.hooks_ui.busy.is_some() {
        return;
    }
    w.state.hooks_ui.busy = Some(name.to_string());
    cx.notify();
    let engine = w.engine.clone();
    let hook = name.to_string();
    cx.spawn(async move |this, cx| {
        let res = cx
            .background_executor()
            .spawn(async move { model::dispatch(&engine, "hook-test", json!({"hook": hook})) })
            .await;
        let _ = this.update(cx, |w, cx| {
            let name = w.state.hooks_ui.busy.take().unwrap_or_default();
            w.state.status = match &res {
                Ok(r) => format!("Hook {name}: {}", run_line(r)),
                Err(e) => e.to_string(),
            };
            load(&mut w.state, &w.engine);
            cx.notify();
        });
    })
    .detach();
}

fn open_box(
    this: &mut MainWindow,
    path: &'static str,
    seed: &str,
    window: &mut Window,
    cx: &mut Context<MainWindow>,
) {
    this.menu = None;
    this.state.begin_edit(path, seed);
    if let Some(f) = &this.focus {
        f.focus(window, cx);
    }
    cx.notify();
}

pub(crate) fn text_box(
    w: &MainWindow,
    path: &'static str,
    value: &str,
    placeholder: &str,
    width: f32,
    cx: &mut Context<MainWindow>,
) -> AnyElement {
    let t = theme::current();
    let editing = w.state.edit.as_ref().filter(|e| e.path == path);
    let shown = match editing {
        Some(e) => edit_text(e).into_any_element(),
        None if value.is_empty() => div()
            .text_color(rgb(t.faint))
            .child(placeholder.to_string())
            .into_any_element(),
        None => div().child(value.to_string()).into_any_element(),
    };
    let mut f = kit::field(path, editing.is_some())
        .w(px(width))
        .flex_none()
        .overflow_hidden()
        .whitespace_nowrap()
        .text_size(px(13.))
        .child(shown);
    if editing.is_none() {
        let seed = value.to_string();
        f = f.on_click(
            cx.listener(move |this, _, window, cx| open_box(this, path, &seed, window, cx)),
        );
    }
    f.into_any_element()
}

pub(crate) fn labelled(label: &'static str, control: AnyElement) -> impl IntoElement {
    let t = theme::current();
    div()
        .flex()
        .items_center()
        .gap(px(10.))
        .child(
            div()
                .w(px(90.))
                .flex_none()
                .text_size(px(12.))
                .text_color(rgb(t.dim))
                .child(label),
        )
        .child(control)
}

fn form(w: &MainWindow, d: &Draft, cx: &mut Context<MainWindow>) -> AnyElement {
    let t = theme::current();
    let events = w.state.hooks_ui.events();
    let chips = div()
        .flex()
        .flex_wrap()
        .gap(px(6.))
        .children(events.into_iter().map(|e| {
            let on = e == d.event;
            let ev = e.clone();
            kit::button(
                format!("hook-event-{e}"),
                e,
                if on { Kind::Primary } else { Kind::Secondary },
                Size::Sm,
                true,
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                if let Some(d) = this.state.hooks_ui.draft.as_mut() {
                    d.event = ev.clone();
                }
                cx.notify();
            }))
        }));
    let shell = kit::toggle("hook-shell", d.shell).on_click(cx.listener(|this, _, _, cx| {
        if let Some(d) = this.state.hooks_ui.draft.as_mut() {
            d.shell = !d.shell;
        }
        cx.notify();
    }));
    let blocking =
        kit::toggle("hook-blocking", d.blocking).on_click(cx.listener(|this, _, _, cx| {
            if let Some(d) = this.state.hooks_ui.draft.as_mut() {
                d.blocking = !d.blocking;
            }
            cx.notify();
        }));
    let calendar = div().flex().gap(px(6.)).children(
        [
            (None, "any session"),
            (Some(true), "calendar-matched only"),
            (Some(false), "no calendar event"),
        ]
        .into_iter()
        .map(|(v, label)| {
            let id = match v {
                None => "hook-match-calendar-any",
                Some(true) => "hook-match-calendar-yes",
                Some(false) => "hook-match-calendar-no",
            };
            kit::button(
                id,
                label,
                if d.match_calendar == v {
                    Kind::Primary
                } else {
                    Kind::Secondary
                },
                Size::Sm,
                true,
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                if let Some(d) = this.state.hooks_ui.draft.as_mut() {
                    d.match_calendar = v;
                }
                cx.notify();
            }))
        }),
    );
    let speaker = div().flex().gap(px(6.)).children(
        [
            ("", "anyone"),
            ("you", "you (the mic)"),
            ("remote", "remote"),
        ]
        .into_iter()
        .map(|(v, label)| {
            let on = d.match_speaker == v || (v.is_empty() && d.match_speaker == "any");
            kit::button(
                format!(
                    "hook-match-speaker-{}",
                    if v.is_empty() { "any" } else { v }
                ),
                label,
                if on { Kind::Primary } else { Kind::Secondary },
                Size::Sm,
                true,
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                if let Some(d) = this.state.hooks_ui.draft.as_mut() {
                    d.match_speaker = v.to_string();
                }
                cx.notify();
            }))
        }),
    );
    let can_save =
        !d.name.trim().is_empty() || w.state.edit.as_ref().is_some_and(|e| e.path == NAME);
    let mut save_btn = kit::button("hook-save", "Save", Kind::Primary, Size::Sm, can_save);
    if can_save {
        save_btn = save_btn.on_click(cx.listener(|this, _, _, cx| {
            save(&mut this.state, &this.engine);
            cx.notify();
        }));
    }
    let mut col = kit::panel()
        .p(px(14.))
        .gap(px(8.))
        .child(
            div()
                .text_size(px(12.))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(rgb(t.dim))
                .child(match &d.editing {
                    Some(n) => format!("Edit hook {n}"),
                    None => "Add hook".to_string(),
                }),
        )
        .child(labelled(
            "Name",
            text_box(w, NAME, &d.name, "name", 260., cx),
        ))
        .child(labelled(
            "Event",
            div()
                .flex()
                .flex_col()
                .gap(px(4.))
                .child(chips)
                .child(
                    div()
                        .text_size(px(11.))
                        .text_color(rgb(t.dim))
                        .child(w.state.hooks_ui.event_doc(&d.event)),
                )
                .into_any_element(),
        ))
        .child(labelled(
            "Command",
            text_box(
                w,
                COMMAND,
                &d.command,
                if d.shell {
                    "shell script, e.g. echo \"$OATS_SESSION_ID\" >> ~/stops.txt"
                } else {
                    "program and arguments, e.g. notify-send Saved"
                },
                480.,
                cx,
            ),
        ))
        .child(labelled("Shell", shell.into_any_element()))
        .child(labelled(
            "Timeout",
            text_box(
                w,
                TIMEOUT,
                &d.timeout,
                "default (app.hook_timeout)",
                200.,
                cx,
            ),
        ))
        .when(oats_core::hooks::events::may_block(&d.event), |c| {
            c.child(labelled(
                "Blocking",
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(blocking)
                    .child(
                        div()
                            .text_size(px(11.))
                            .text_color(rgb(t.dim))
                            .child("the recorder waits for it up to its timeout, then goes on whatever happens"),
                    )
                    .into_any_element(),
            ))
        })
        .child(labelled(
            "Folder",
            text_box(w, CWD, &d.cwd, "the session folder", 360., cx),
        ))
        .child(labelled(
            "Match title",
            text_box(
                w,
                MATCH_TITLE,
                &d.match_title,
                "any title; a glob like Standup* or re:<regex>",
                360.,
                cx,
            ),
        ))
        .child(labelled(
            "Attendees",
            text_box(
                w,
                MATCH_ATTENDEES,
                &d.match_attendees,
                "anyone; globs, comma-separated, e.g. *@acme.com",
                360.,
                cx,
            ),
        ))
        .child(labelled(
            "Cal. source",
            text_box(
                w,
                MATCH_SOURCE,
                &d.match_source,
                "any calendar source",
                260.,
                cx,
            ),
        ))
        .child(labelled("Calendar", calendar.into_any_element()));
    if d.event == "transcript.phrase" {
        col = col
            .child(labelled(
                "Phrases",
                text_box(
                    w,
                    MATCH_PHRASES,
                    &d.match_phrases,
                    "words said, ;-separated, e.g. pricing; next steps; re:\\bdiscounts?\\b",
                    480.,
                    cx,
                ),
            ))
            .child(labelled("Speaker", speaker.into_any_element()))
            .child(labelled(
                "Cooldown",
                text_box(
                    w,
                    MATCH_COOLDOWN,
                    &d.match_cooldown,
                    "default (app.hook_cooldown)",
                    200.,
                    cx,
                ),
            ));
    }
    if d.event == "calendar.ending" {
        col = col.child(labelled(
            "Before end",
            text_box(
                w,
                MATCH_BEFORE_END,
                &d.match_before_end,
                "default (app.hook_before_end), e.g. 5m",
                200.,
                cx,
            ),
        ));
    }
    if let Some(e) = &d.error {
        col = col.child(
            div()
                .text_size(px(12.))
                .text_color(rgb(t.err))
                .child(e.clone()),
        );
    }
    col.child(div().flex().gap(px(8.)).child(save_btn).child(
        kit::button("hook-cancel", "Cancel", Kind::Secondary, Size::Sm, true).on_click(
            cx.listener(|this, _, _, cx| {
                cancel(&mut this.state);
                cx.notify();
            }),
        ),
    ))
    .into_any_element()
}

fn hook_row(w: &MainWindow, h: &Value, last: bool, cx: &mut Context<MainWindow>) -> AnyElement {
    let t = theme::current();
    let ui = &w.state.hooks_ui;
    let name = h["name"].as_str().unwrap_or_default().to_string();
    let on = h["enabled"] != false;
    let busy = ui.busy.is_some();
    let n = name.clone();
    let toggle = kit::toggle(format!("hook-enable-{name}"), on).on_click(cx.listener(
        move |this, _, _, cx| {
            enable(&mut this.state, &this.engine, &n, !on);
            cx.notify();
        },
    ));
    let n = name.clone();
    let mut test_btn = kit::button(
        format!("hook-test-{name}"),
        if ui.busy.as_deref() == Some(name.as_str()) {
            "Running..."
        } else {
            "Test"
        },
        Kind::Secondary,
        Size::Sm,
        !busy,
    );
    if !busy {
        test_btn = test_btn.on_click(cx.listener(move |this, _, _, cx| test(this, &n, cx)));
    }
    let n = name.clone();
    let edit_btn = kit::button(
        format!("hook-edit-{name}"),
        "Edit",
        Kind::Secondary,
        Size::Sm,
        true,
    )
    .on_click(cx.listener(move |this, _, _, cx| {
        open(&mut this.state, Some(&n));
        cx.notify();
    }));
    let n = name.clone();
    let armed = ui.arming.as_deref() == Some(name.as_str());
    let remove_btn = kit::button(
        format!("hook-remove-{name}"),
        if armed { "Remove?" } else { "Remove" },
        if armed { Kind::Rec } else { Kind::Secondary },
        Size::Sm,
        true,
    )
    .on_click(cx.listener(move |this, _, _, cx| {
        remove(&mut this.state, &this.engine, &n);
        cx.notify();
    }));
    let command: Vec<String> = h["command"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|x| x.as_str().map(str::to_string))
        .collect();
    let mut col = div()
        .id(SharedString::from(format!("hook-{name}")))
        .flex()
        .flex_col()
        .gap(px(4.))
        .px(px(16.))
        .py(px(10.))
        .when(!last, |d| d.border_b_1().border_color(rgb(t.line)))
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(10.))
                .child(
                    div()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_size(px(13.))
                        .child(name.clone()),
                )
                .child(kit::tag(
                    h["event"].as_str().unwrap_or_default().to_string(),
                    false,
                ))
                .when(h["shell"] == true, |d| d.child(kit::tag("shell", false)))
                .child(
                    div()
                        .text_size(px(11.))
                        .text_color(rgb(t.faint))
                        .child(format!(
                            "timeout {}",
                            h["timeout"].as_str().unwrap_or_default()
                        )),
                )
                .child(div().flex_1())
                .child(test_btn)
                .child(edit_btn)
                .child(remove_btn)
                .child(toggle),
        )
        .child(
            kit::mono(command.join(" "))
                .text_size(px(12.))
                .text_color(rgb(t.dim))
                .overflow_hidden()
                .text_ellipsis()
                .whitespace_nowrap(),
        );
    if let Some(m) = h["match"].as_object().filter(|m| !m.is_empty()) {
        col = col.child(
            div()
                .text_size(px(12.))
                .text_color(rgb(t.dim))
                .child(format!("Match: {}", Value::Object(m.clone()))),
        );
    }
    let dec = &h["last_decision"];
    if dec.is_object() {
        col = col.child(
            div()
                .text_size(px(12.))
                .text_color(rgb(t.faint))
                .child(format!(
                    "Last decision: {} ({})",
                    if dec["fire"] == true {
                        "fired"
                    } else {
                        "skipped"
                    },
                    dec["reason"].as_str().unwrap_or_default()
                )),
        );
    }
    let r = &h["last_run"];
    if r.is_object() {
        col = col.child(
            div()
                .text_size(px(12.))
                .text_color(rgb(if r["ok"] == true { t.dim } else { t.err }))
                .child(format!("Last run: {}", run_line(r))),
        );
        for (label, key) in [("stdout", "stdout"), ("stderr", "stderr")] {
            let text = r[key].as_str().unwrap_or_default().trim_end();
            if !text.is_empty() {
                col = col.child(
                    div()
                        .font_family(MONO)
                        .text_size(px(11.))
                        .text_color(rgb(t.faint))
                        .whitespace_normal()
                        .child(format!("{label}: {text}")),
                );
            }
        }
    }
    col.into_any_element()
}

/// The panel: one row per hook, the form while open, and Add.
pub(crate) fn panel(w: &MainWindow, cx: &mut Context<MainWindow>) -> AnyElement {
    let t = theme::current();
    let ui = w.state.hooks_ui.clone();
    let rows = ui.hooks();
    let n = rows.len();
    let mut list = kit::panel();
    if rows.is_empty() {
        list = list.child(
            div()
                .p(px(14.))
                .text_size(px(13.))
                .text_color(rgb(t.dim))
                .child("No hooks: nothing runs on recording or session events. Add one to run a command after Stop."),
        );
    }
    for (i, h) in rows.iter().enumerate() {
        list = list.child(hook_row(w, h, i + 1 == n, cx));
    }
    let mut out = div()
        .flex()
        .flex_col()
        .flex_none()
        .gap(px(10.))
        .max_w(px(820.))
        .mb(px(22.))
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(10.))
                .child(kit::section("Hooks").text_color(rgb(t.dim)).pt_0())
                .child(div().flex_1())
                .when(ui.draft.is_none(), |d| {
                    d.child(
                        kit::button("hook-add", "Add hook", Kind::Secondary, Size::Sm, true)
                            .on_click(cx.listener(|this, _, _, cx| {
                                open(&mut this.state, None);
                                cx.notify();
                            })),
                    )
                }),
        );
    if let Some(e) = &ui.error {
        out = out.child(kit::banner(true, "Hooks failed.", e.clone()));
    }
    // What config-check warns about (a phrase hook that cannot fire mid-meeting).
    for w in ui.list["warnings"].as_array().into_iter().flatten() {
        out = out.child(
            div()
                .text_size(px(12.))
                .text_color(rgb(t.dim))
                .child(w.as_str().unwrap_or_default().to_string()),
        );
    }
    out = out.child(list);
    if let Some(d) = &ui.draft {
        out = out.child(form(w, d, cx));
    }
    out.into_any_element()
}

#[cfg(test)]
#[cfg(unix)]
mod tests {
    use super::*;
    use crate::state::Tab;
    use crate::testutil;
    use gpui::TestAppContext;

    fn invoke(v: &mut MainWindow, name: &str, args: Value, cx: &mut Context<MainWindow>) {
        v.drive(&json!({"op": "invoke", "name": name, "args": args}), cx)
            .unwrap();
    }

    /// The event picker and its description come from the hook event table, through hook-list.
    #[test]
    fn the_event_picker_reads_the_hook_event_table() {
        let (engine, _d) = testutil::engine(&[]);
        let ui = Ui {
            list: model::dispatch(&engine, "hook-list", json!({})).unwrap(),
            ..Ui::default()
        };
        assert_eq!(ui.events(), oats_core::hooks::names());
        for spec in oats_core::hooks::events::TABLE {
            assert_eq!(ui.event_doc(spec.name), spec.description);
        }
        assert_eq!(ui.event_doc("record.explode"), "");
    }

    /// The Match fields go out as the `match` object over the fields the form has no box for, and
    /// an emptied field leaves it.
    #[test]
    fn the_match_fields_round_trip_through_the_draft() {
        let old = json!({"title": "Standup*", "phrases": ["pricing"], "calendar": false});
        let mut d = Draft {
            match_rest: old.clone(),
            ..Draft::default()
        };
        d.set_match(&old);
        assert_eq!(d.match_value(), old);
        d.match_title = String::new();
        d.match_attendees = " *@acme.com, bo@x.org ,".into();
        d.match_source = "work".into();
        d.match_calendar = None;
        assert_eq!(
            d.match_value(),
            json!({"phrases": ["pricing"], "attendees": ["*@acme.com", "bo@x.org"], "source": "work"})
        );
    }

    /// The phrase fields (dot-4uzgol.111.6): phrases are ;-separated (a regex keeps its comma),
    /// speaker any and an empty cooldown leave the match.
    #[test]
    fn the_phrase_fields_round_trip_through_the_draft() {
        let old =
            json!({"phrases": ["pricing", "re:a{1,3}"], "speaker": "remote", "cooldown": "2m"});
        let mut d = Draft {
            match_rest: old.clone(),
            ..Draft::default()
        };
        d.set_match(&old);
        assert_eq!(d.match_phrases, "pricing; re:a{1,3}");
        assert_eq!(d.match_value(), old);
        d.match_speaker = "any".into();
        d.match_cooldown = String::new();
        d.match_phrases = " next steps ;; ".into();
        assert_eq!(d.match_value(), json!({"phrases": ["next steps"]}));
    }

    /// A transcript.phrase hook added through the form keeps its phrases, speaker and cooldown;
    /// one with no phrases is refused and the form stays open with why.
    #[test]
    fn the_hooks_panel_adds_a_phrase_hook() {
        let (engine, _d) = testutil::engine(&[]);
        let mut cx = TestAppContext::single();
        let (view, cx) = cx.add_window_view(|_, _| MainWindow::new(engine.clone()));
        view.update(cx, |v, cx| {
            v.state.tab = Tab::Settings;
            cx.notify();
        });
        cx.run_until_parked();
        view.update(cx, |v, cx| {
            invoke(
                v,
                "settings.hook_add",
                json!({"name": "flag-pricing", "event": "transcript.phrase", "command": "true",
                       "match": {"phrases": ["pricing", "re:\\bdiscounts?\\b"], "speaker": "remote",
                                 "cooldown": "2m"}}),
                cx,
            );
            let d = v.dump(cx);
            assert_eq!(
                d["view"]["hooks"]["hooks"][0]["match"],
                json!({"phrases": ["pricing", "re:\\bdiscounts?\\b"], "speaker": "remote", "cooldown": "2m"}),
                "{d}"
            );
            invoke(
                v,
                "settings.hook_add",
                json!({"name": "no-words", "event": "transcript.phrase", "command": "true"}),
                cx,
            );
            let d = v.dump(cx);
            let err = d["view"]["hooks"]["draft"]["error"].as_str().unwrap();
            assert!(err.contains("hook no-words") && err.contains("match.phrases"), "{err}");
            cancel(&mut v.state);
        });
    }

    /// A record.before_start hook added through the form keeps its Blocking toggle and a
    /// calendar.ending hook its Before end (dot-4uzgol.111.7); the toggle is not sent for an
    /// event that cannot block, and the engine refuses blocking there.
    #[test]
    fn the_hooks_panel_adds_a_blocking_and_a_calendar_ending_hook() {
        let (engine, _d) = testutil::engine(&[]);
        let mut cx = TestAppContext::single();
        let (view, cx) = cx.add_window_view(|_, _| MainWindow::new(engine.clone()));
        view.update(cx, |v, cx| {
            v.state.tab = Tab::Settings;
            cx.notify();
        });
        cx.run_until_parked();
        view.update(cx, |v, cx| {
            invoke(
                v,
                "settings.hook_add",
                json!({"name": "vpn-up", "event": "record.before_start", "command": "true",
                       "blocking": true, "timeout": "5s"}),
                cx,
            );
            invoke(
                v,
                "settings.hook_add",
                json!({"name": "wrap-up", "event": "calendar.ending", "command": "true",
                       "blocking": true, "match": {"before_end": "2m"}}),
                cx,
            );
            let d = v.dump(cx);
            let hooks = &d["view"]["hooks"]["hooks"];
            assert_eq!(hooks[0]["blocking"], true, "{d}");
            assert_eq!(hooks[1]["blocking"], false, "{d}");
            assert_eq!(hooks[1]["match"], json!({"before_end": "2m"}), "{d}");
        });
        let e = model::dispatch(
            &engine,
            "hook-edit",
            json!({"hook": "wrap-up", "blocking": true}),
        )
        .unwrap_err();
        assert!(e.to_string().contains("blocking = true only on"), "{e}");
    }

    /// Add a hook through the form, test-run it, see the failed run on its row and as the banner,
    /// fix it with Edit, turn it off, and remove it (two clicks), all as ui-invoke clicks.
    #[test]
    fn the_hooks_panel_adds_tests_edits_toggles_and_removes_a_hook() {
        let (engine, _d) = testutil::engine(&[]);
        let failed = engine.bus.subscribe(vec!["hook-finished".into()]);
        let mut cx = TestAppContext::single();
        let (view, cx) = cx.add_window_view(|_, _| MainWindow::new(engine.clone()));
        view.update(cx, |v, cx| {
            v.state.tab = Tab::Settings;
            cx.notify();
        });
        cx.run_until_parked();
        view.update(cx, |v, cx| {
            let d = v.dump(cx);
            assert_eq!(d["view"]["hooks"]["hooks"], json!([]), "{d}");
            assert!(d["view"]["groups"]
                .as_array()
                .unwrap()
                .iter()
                .any(|g| g["name"] == "Hooks"));
            invoke(
                v,
                "settings.hook_add",
                json!({"name": "after-stop", "event": "record.stop",
                       "command": "echo nope >&2; exit 4", "shell": true}),
                cx,
            );
            let d = v.dump(cx);
            let h = &d["view"]["hooks"]["hooks"][0];
            assert_eq!(h["name"], "after-stop", "{d}");
            assert_eq!(h["shell"], true);
            assert_eq!(d["view"]["hooks"]["draft"], Value::Null);
            invoke(v, "settings.hook_test", json!({"hook": "after-stop"}), cx);
        });
        cx.run_until_parked();
        let line = failed
            .recv_timeout(std::time::Duration::from_secs(20))
            .unwrap();
        view.update(cx, |v, cx| {
            v.state.on_event(&v.engine, &line);
            let d = v.dump(cx);
            let run = &d["view"]["hooks"]["hooks"][0]["last_run"];
            assert_eq!(run["exit_code"], 4, "{d}");
            assert_eq!(run["stderr"], "nope\n");
            let banner = d["hook_banner"].as_str().unwrap();
            assert!(
                banner.contains("after-stop") && banner.contains("exit code 4"),
                "{banner}"
            );
            assert!(d["status"].as_str().unwrap().contains("exit code 4"), "{d}");
            invoke(v, "settings.hook_dismiss", json!({}), cx);
            invoke(
                v,
                "settings.hook_edit",
                json!({"hook": "after-stop", "command": "true", "shell": false, "timeout": "5s"}),
                cx,
            );
            invoke(
                v,
                "settings.hook_enable",
                json!({"hook": "after-stop", "enabled": false}),
                cx,
            );
            let d = v.dump(cx);
            assert_eq!(d["hook_banner"], Value::Null);
            let h = &d["view"]["hooks"]["hooks"][0];
            assert_eq!(h["command"], json!(["true"]), "{d}");
            assert_eq!(h["timeout"], "5s");
            assert_eq!(h["enabled"], false);
            // A refused form stays open with the reason.
            invoke(
                v,
                "settings.hook_add",
                json!({"name": "bad", "event": "record.explode", "command": "true"}),
                cx,
            );
            let d = v.dump(cx);
            assert!(d["view"]["hooks"]["draft"]["error"]
                .as_str()
                .unwrap()
                .contains("record.explode"));
            cancel(&mut v.state);
            invoke(v, "settings.hook_remove", json!({"hook": "after-stop"}), cx);
            assert_eq!(
                v.engine.snapshot().config.hooks.len(),
                1,
                "first click arms"
            );
            invoke(v, "settings.hook_remove", json!({"hook": "after-stop"}), cx);
            assert!(v.engine.snapshot().config.hooks.is_empty());
        });
        // The panel renders with a hook, the form open and a run on the row.
        view.update(cx, |v, cx| {
            invoke(
                v,
                "settings.hook_add",
                json!({"name": "ok", "event": "session.saved", "command": "true",
                       "match": {"title": "Standup*", "attendees": ["*@acme.com"], "calendar": true}}),
                cx,
            );
            let d = v.dump(cx);
            assert_eq!(
                d["view"]["hooks"]["hooks"][0]["match"],
                json!({"title": "Standup*", "attendees": ["*@acme.com"], "calendar": true}),
                "{d}"
            );
            // A bad glob is refused with the hook's name, the form stays open.
            invoke(
                v,
                "settings.hook_edit",
                json!({"hook": "ok", "match": {"title": "[oops"}}),
                cx,
            );
            let d = v.dump(cx);
            let err = d["view"]["hooks"]["draft"]["error"].as_str().unwrap();
            assert!(err.contains("hook ok") && err.contains("match.title"), "{err}");
            cancel(&mut v.state);
            open(&mut v.state, Some("ok"));
            v.state.begin_edit(COMMAND, "true");
            cx.notify();
        });
        cx.run_until_parked();
        view.update(cx, |v, cx| {
            let d = v.dump(cx);
            assert_eq!(d["view"]["hooks"]["draft"]["editing"], "ok", "{d}");
            assert!(v.state.edit.is_some());
        });
    }
}
