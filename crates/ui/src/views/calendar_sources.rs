//! The Calendar sources panel under the Settings tab's Calendar section (dot-4uzgol.88,
//! dot-4uzgol.100): every `calendar.sources` entry with its on/off toggle and its provider's
//! accounts and calendars as checkboxes (this computer's accounts, Google, Microsoft 365, CalDAV,
//! the macOS Calendar app), plus Connect / Sign out for the OAuth providers and buttons that add
//! one. A click runs the same action as `ctl` (`calendar-select`, `calendar-source-enable`,
//! `calendar-source-add`, `calendar-access`, `calendar-connect`, `calendar-disconnect`) off the UI
//! thread, since a source being turned on may wait on the one macOS Calendars prompt or the
//! network; the panel then re-reads `calendar-sources-list`. Connect opens the provider's sign-in
//! page in the browser. Nothing here asks for access unless one of those is clicked.
//! A command source (dot-4uzgol.105) shows its command and its last run (the error when it
//! failed); Add command feed and its Edit open a form whose text boxes are the window's one editor
//! (`State::edit`) and whose Save runs `calendar-source-add` / `calendar-source-edit`; Run now is
//! `calendar-connect`, a test run off the UI thread.
//! The panel re-reads at most once per `ics.poll_interval`, plus after each action.

use std::time::Instant;

use gpui::{div, prelude::*, px, rgb, AnyElement, App, Context, FontWeight, Global, SharedString};
use serde_json::{json, Value};

use crate::kit::{self, Kind, Size};
use crate::model;
use crate::state::State;
use crate::theme;
use crate::view::MainWindow;

/// The command-source form's text boxes, as `Edit` paths.
pub(crate) const NAME: &str = "calsrc:name";
pub(crate) const COMMAND: &str = "calsrc:command";
pub(crate) const TIMEOUT: &str = "calsrc:timeout";
pub(crate) const CWD: &str = "calsrc:cwd";
pub(crate) const ENV: &str = "calsrc:env";

/// The formats a command source's output can be read as.
const FORMATS: &[&str] = &["auto", "ics", "json"];

/// The Add / Edit form of a command source.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Draft {
    /// The source being edited (its name), None for a new one.
    pub editing: Option<String>,
    pub name: String,
    /// The program and its arguments split on spaces, or a JSON array of them.
    pub command: String,
    pub format: String,
    /// Empty: `calendar.command_timeout`.
    pub timeout: String,
    pub cwd: String,
    /// `NAME=value` pairs split on spaces.
    pub env: String,
    pub error: Option<String>,
}

impl Draft {
    fn dump(&self) -> Value {
        json!({"editing": self.editing, "name": self.name, "command": self.command,
               "format": self.format, "timeout": self.timeout, "cwd": self.cwd, "env": self.env,
               "error": self.error})
    }
}

/// argv as the form shows it: words joined by spaces, or a JSON array when one holds a space.
pub(crate) fn argv_text(argv: &[String]) -> String {
    if argv
        .iter()
        .any(|a| a.is_empty() || a.contains(char::is_whitespace))
    {
        serde_json::to_string(argv).unwrap_or_default()
    } else {
        argv.join(" ")
    }
}

/// The form's command text as argv: a JSON array of strings, else split on whitespace.
pub(crate) fn argv_of(text: &str) -> Value {
    let t = text.trim();
    if t.starts_with('[') {
        if let Ok(v) = serde_json::from_str::<Vec<String>>(t) {
            return json!(v);
        }
    }
    json!(t.split_whitespace().collect::<Vec<_>>())
}

pub fn is_ours(path: &str) -> bool {
    path.starts_with("calsrc:")
}

/// Enter in a form box: keep its text in the form.
pub fn commit(st: &mut State) {
    let Some(e) = st.edit.take() else { return };
    let Some(d) = st.calsrc_draft.as_mut() else {
        return;
    };
    let v = e.buf.trim().to_string();
    match e.path {
        NAME => d.name = v,
        COMMAND => d.command = v,
        TIMEOUT => d.timeout = v,
        CWD => d.cwd = v,
        ENV => d.env = v,
        _ => {}
    }
}

/// Open the Add form (`src` None) or the Edit form for the command source `src` (a
/// `calendar-sources-list` row).
pub fn open(st: &mut State, src: Option<&Value>) {
    st.cancel_edit();
    let strs = |v: &Value| -> Vec<String> {
        v.as_array()
            .into_iter()
            .flatten()
            .filter_map(|x| x.as_str().map(str::to_string))
            .collect()
    };
    st.calsrc_draft = Some(match src {
        None => Draft {
            name: "command".into(),
            format: "auto".into(),
            ..Draft::default()
        },
        Some(s) => {
            let text = |k: &str| s[k].as_str().unwrap_or_default().to_string();
            let env: Vec<String> = s["env"]
                .as_object()
                .into_iter()
                .flatten()
                .map(|(k, v)| format!("{k}={}", v.as_str().unwrap_or_default()))
                .collect();
            Draft {
                editing: Some(text("name")),
                name: text("name"),
                command: argv_text(&strs(&s["argv"])),
                format: if text("format").is_empty() {
                    "auto".into()
                } else {
                    text("format")
                },
                timeout: text("timeout"),
                cwd: text("cwd"),
                env: env.join(" "),
                error: None,
            }
        }
    });
}

/// Save the form through `calendar-source-add` / `-edit` (neither runs the command); the form
/// stays open with the error on refusal. True when saved.
pub fn save(st: &mut State, engine: &oats_core::control::engine::Engine) -> bool {
    if st.edit.as_ref().is_some_and(|e| is_ours(e.path)) {
        commit(st);
    }
    let Some(d) = st.calsrc_draft.clone() else {
        return false;
    };
    let mut params = json!({"argv": argv_of(&d.command), "format": d.format,
                            "timeout": d.timeout, "cwd": d.cwd, "env": d.env});
    let id = match &d.editing {
        None => {
            params["name"] = json!(d.name);
            params["kind"] = json!("command");
            "calendar-source-add"
        }
        Some(old) => {
            params["name"] = json!(old);
            if old != &d.name {
                params["rename"] = json!(d.name);
            }
            "calendar-source-edit"
        }
    };
    match model::dispatch(engine, id, params) {
        Ok(_) => {
            st.calsrc_draft = None;
            st.status = format!("Saved calendar source {}.", d.name);
            true
        }
        Err(e) => {
            if let Some(d) = st.calsrc_draft.as_mut() {
                d.error = Some(e.to_string());
            }
            false
        }
    }
}

pub fn cancel(st: &mut State) {
    if st.edit.as_ref().is_some_and(|e| is_ours(e.path)) {
        st.edit = None;
    }
    st.calsrc_draft = None;
}

/// Save the form, and on success re-read the panel and the Calendar tab.
pub(crate) fn save_and_reload(w: &mut MainWindow, cx: &mut Context<MainWindow>) {
    if save(&mut w.state, &w.engine) {
        cx.default_global::<Sources>().load(w);
        cx.default_global::<super::calendar::Cal>().loaded = None;
    }
    cx.notify();
}

#[derive(Default)]
pub(crate) struct Sources {
    /// The last `calendar-sources-list` answer.
    pub list: Value,
    pub error: Option<String>,
    pub loaded: Option<Instant>,
    /// The action running off the UI thread, if any.
    pub busy: Option<String>,
}

impl Global for Sources {}

impl Sources {
    pub(crate) fn load(&mut self, w: &MainWindow) {
        self.loaded = Some(Instant::now());
        match model::dispatch(&w.engine, "calendar-sources-list", json!({})) {
            Ok(v) => {
                self.list = v;
                self.error = None;
            }
            Err(e) => self.error = Some(e.to_string()),
        }
    }

    fn stale(&self, w: &MainWindow) -> bool {
        let every = w.engine.snapshot().config.ics.poll_interval.as_duration();
        self.loaded.is_none_or(|t| t.elapsed() >= every)
    }

    fn rows(&self) -> Vec<Value> {
        self.list["sources"].as_array().cloned().unwrap_or_default()
    }

    /// What `ui-state` shows for the panel, with the command-source form when open.
    pub(crate) fn dump(&self, draft: Option<&Draft>) -> Value {
        json!({"sources": self.list["sources"], "eventkit": self.list["eventkit"],
               "error": self.error, "busy": self.busy, "draft": draft.map(Draft::dump)})
    }
}

/// Read the list now when it never was (ui-state and ui-invoke need it before a render).
pub(crate) fn prepare(w: &MainWindow, cx: &mut App) {
    let s = cx.default_global::<Sources>();
    if s.loaded.is_none() && s.busy.is_none() {
        s.load(w);
    }
}

/// Run `id` with `params` off the UI thread, then re-read the panel.
pub(crate) fn act(
    w: &mut MainWindow,
    id: &'static str,
    params: Value,
    cx: &mut Context<MainWindow>,
) {
    let s = cx.default_global::<Sources>();
    if s.busy.is_some() {
        return;
    }
    s.busy = Some(id.to_string());
    cx.notify();
    let engine = w.engine.clone();
    cx.spawn(async move |this, cx| {
        let res = cx
            .background_executor()
            .spawn(async move { model::dispatch(&engine, id, params) })
            .await;
        let _ = this.update(cx, |w, cx| {
            w.state.status = match &res {
                Ok(v) => summary(id, v),
                Err(e) => e.to_string(),
            };
            if let (Ok(v), "calendar-connect") = (&res, id) {
                if let Some(url) = v["url"].as_str() {
                    cx.open_url(url);
                }
            }
            let s = cx.default_global::<Sources>();
            s.busy = None;
            s.load(w);
            // The Calendar tab's meetings come from the sources too.
            cx.default_global::<super::calendar::Cal>().loaded = None;
            cx.notify();
        });
    })
    .detach();
}

fn summary(id: &str, v: &Value) -> String {
    let access = v["access"]["authorization"]
        .as_str()
        .or(v["authorization"].as_str());
    let what = match id {
        "calendar-select" => format!(
            "Calendars: {} of {} selected.",
            v["selected"], v["calendars"]
        ),
        "calendar-source-add" => format!(
            "Added calendar source {}.",
            v["source"]["name"].as_str().unwrap_or_default()
        ),
        "calendar-access" => "Calendars access checked.".to_string(),
        "calendar-connect" if v["kind"] == "command" => format!(
            "Calendar source {} ran: {} events{}.",
            v["source"].as_str().unwrap_or_default(),
            v["events"],
            match v["errors"][0]["error"]["message"].as_str() {
                Some(e) => format!(" ({e})"),
                None => String::new(),
            }
        ),
        "calendar-connect" if v["url"].is_string() => format!(
            "Sign in to {} in the browser that just opened.",
            v["source"].as_str().unwrap_or_default()
        ),
        "calendar-connect" => format!(
            "Calendar source {}: {} calendars found.",
            v["source"].as_str().unwrap_or_default(),
            v["calendars"]
        ),
        "calendar-disconnect" => {
            format!("Signed {} out.", v["source"].as_str().unwrap_or_default())
        }
        _ => format!(
            "Calendar source {} is {}.",
            v["source"]["name"].as_str().unwrap_or_default(),
            if v["source"]["enabled"] == true {
                "on"
            } else {
                "off"
            }
        ),
    };
    match access {
        Some(a) if a != "full_access" => format!("{what} Calendars access: {a}."),
        _ => what,
    }
}

/// A 16px checkbox: filled with a tick when on.
fn checkbox(id: impl Into<SharedString>, on: bool, enabled: bool) -> gpui::Stateful<gpui::Div> {
    let t = theme::current();
    let b = div()
        .id(id.into())
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(px(16.))
        .rounded(px(4.))
        .text_size(px(11.))
        .text_color(rgb(0xFFFFFF));
    let b = if on {
        b.bg(rgb(t.accent)).child("✓")
    } else {
        b.border(px(1.5)).border_color(rgb(t.faint))
    };
    if enabled {
        b.cursor_pointer()
    } else {
        b.opacity(0.5)
    }
}

fn check_row(
    id: String,
    label: String,
    note: String,
    on: bool,
    enabled: bool,
    indent: f32,
    params: Value,
    cx: &mut Context<MainWindow>,
) -> impl IntoElement {
    let t = theme::current();
    let mut b = checkbox(id, on, enabled);
    if enabled {
        let mut params = params;
        params["selected"] = json!(!on);
        b = b.on_click(cx.listener(move |w, _, _, cx| {
            act(w, "calendar-select", params.clone(), cx);
        }));
    }
    div()
        .flex()
        .items_center()
        .gap(px(8.))
        .pl(px(indent))
        .py(px(3.))
        .child(b)
        .child(kit::ellipsis(label).text_size(px(13.)))
        .child(
            div()
                .text_size(px(11.))
                .text_color(rgb(t.faint))
                .child(note),
        )
}

/// What the panel calls a source kind.
pub(crate) fn kind_label(kind: &str) -> &'static str {
    match kind {
        "os-local" => "This computer",
        "google" => "Google",
        "microsoft" => "Microsoft 365",
        "caldav" => "CalDAV",
        "eventkit" => "macOS Calendar",
        "command" => "Command",
        _ => "ICS feeds",
    }
}

fn source_row(src: &Value, busy: bool, last: bool, cx: &mut Context<MainWindow>) -> AnyElement {
    let t = theme::current();
    let name = src["name"].as_str().unwrap_or_default().to_string();
    let kind = src["kind"].as_str().unwrap_or_default().to_string();
    let on = src["enabled"].as_bool().unwrap_or(true);
    let mut toggle = kit::toggle(format!("calsrc-enable-{name}"), on);
    if !busy {
        let n = name.clone();
        toggle = toggle.on_click(cx.listener(move |w, _, _, cx| {
            act(
                w,
                "calendar-source-enable",
                json!({"name": n, "enabled": !on}),
                cx,
            );
        }));
    }
    let label = kind_label(&kind);
    let mut col = div()
        .id(SharedString::from(format!("calsrc-{name}")))
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
                .child(kit::tag(label, false))
                .child(div().flex_1())
                .child(toggle),
        );
    if kind == "google" || kind == "microsoft" {
        let connected = src["connected"] == true;
        let account = src["account"].as_str().unwrap_or_default().to_string();
        let mut line = div().flex().items_center().gap(px(8.)).child(
            div().text_size(px(12.)).text_color(rgb(t.dim)).child(
                if connected && !account.is_empty() {
                    format!("Signed in as {account}")
                } else if connected {
                    "Signed in".to_string()
                } else {
                    "Not signed in".to_string()
                },
            ),
        );
        if !busy {
            let n = name.clone();
            line = line.child(
                kit::button(
                    format!("calsrc-connect-{name}"),
                    if connected { "Reconnect" } else { "Connect" },
                    if connected {
                        Kind::Secondary
                    } else {
                        Kind::Primary
                    },
                    Size::Sm,
                    true,
                )
                .on_click(cx.listener(move |w, _, _, cx| {
                    act(w, "calendar-connect", json!({"source": n}), cx);
                })),
            );
            if connected {
                let n = name.clone();
                line = line.child(
                    kit::button(
                        format!("calsrc-signout-{name}"),
                        "Sign out",
                        Kind::Secondary,
                        Size::Sm,
                        true,
                    )
                    .on_click(cx.listener(move |w, _, _, cx| {
                        act(w, "calendar-disconnect", json!({"source": n}), cx);
                    })),
                );
            }
        }
        col = col.child(line);
    }
    if kind == "command" {
        let argv: Vec<String> = src["argv"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|x| x.as_str().map(str::to_string))
            .collect();
        let r = &src["last_run"];
        let ran = if r.is_object() {
            format!(
                "Last run {}: {} events, {} ms{}",
                r["ran_at"].as_str().unwrap_or_default(),
                r["events"],
                r["duration_ms"],
                if r["kept_last_good"] == true {
                    "; showing the last good run"
                } else {
                    ""
                }
            )
        } else {
            "Not run yet: it runs on each calendar read.".to_string()
        };
        let mut line = div()
            .flex()
            .items_center()
            .gap(px(8.))
            .child(
                kit::mono(argv_text(&argv))
                    .text_size(px(12.))
                    .text_color(rgb(t.dim))
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap(),
            )
            .child(div().flex_1());
        if !busy {
            let n = name.clone();
            let row = src.clone();
            line = line
                .child(
                    kit::button(
                        format!("calsrc-run-{name}"),
                        "Run now",
                        Kind::Secondary,
                        Size::Sm,
                        true,
                    )
                    .on_click(cx.listener(move |w, _, _, cx| {
                        act(w, "calendar-connect", json!({"source": n}), cx);
                    })),
                )
                .child(
                    kit::button(
                        format!("calsrc-edit-{name}"),
                        "Edit",
                        Kind::Secondary,
                        Size::Sm,
                        true,
                    )
                    .on_click(cx.listener(move |w, _, _, cx| {
                        open(&mut w.state, Some(&row));
                        cx.notify();
                    })),
                );
        }
        col = col
            .child(line)
            .child(div().text_size(px(12.)).text_color(rgb(t.dim)).child(ran));
    }
    if let Some(e) = src["error"]["message"].as_str() {
        col = col.child(
            div()
                .text_size(px(12.))
                .text_color(rgb(t.warn))
                .child(e.to_string()),
        );
        if src["authorization"] == "not_determined" && !busy {
            col = col.child(
                div().flex().child(
                    kit::button(
                        format!("calsrc-access-{name}"),
                        "Allow Calendars access",
                        Kind::Secondary,
                        Size::Sm,
                        true,
                    )
                    .on_click(cx.listener(|w, _, _, cx| {
                        act(w, "calendar-access", json!({"request": true}), cx);
                    })),
                ),
            );
        }
    }
    if kind == "ics" {
        let feeds: Vec<String> = src["available"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|f| f["selected"] == true)
            .filter_map(|f| f["name"].as_str().map(str::to_string))
            .collect();
        col = col.child(div().text_size(px(12.)).text_color(rgb(t.dim)).child(
            if feeds.is_empty() {
                "No feeds read (add one in the Calendar tab).".to_string()
            } else {
                format!("Reads {}", feeds.join(", "))
            },
        ));
    }
    // A command source has one calendar: no checkboxes.
    let accounts = if kind == "command" {
        None
    } else {
        src["accounts"].as_array()
    };
    for acct in accounts.into_iter().flatten() {
        let title = acct["title"].as_str().unwrap_or_default().to_string();
        let akind = acct["kind"].as_str().unwrap_or_default().to_string();
        col = col.child(check_row(
            format!("calsrc-{name}-acct-{title}"),
            title.clone(),
            akind,
            acct["selected"] == true,
            !busy,
            0.,
            json!({"source": name, "account": acct["id"]}),
            cx,
        ));
        for c in acct["calendars"].as_array().into_iter().flatten() {
            let ctitle = c["title"].as_str().unwrap_or_default().to_string();
            let id = c["id"].as_str().unwrap_or_default().to_string();
            col = col.child(check_row(
                format!("calsrc-{name}-cal-{id}"),
                ctitle,
                String::new(),
                c["selected"] == true,
                !busy,
                24.,
                json!({"source": name, "calendar": id}),
                cx,
            ));
        }
    }
    col.into_any_element()
}

/// A header button that adds a source of `kind` named `name`.
fn add_button(
    id: &'static str,
    label: &'static str,
    name: &'static str,
    kind: &'static str,
    cx: &mut Context<MainWindow>,
) -> gpui::Stateful<gpui::Div> {
    kit::button(id, label, Kind::Secondary, Size::Sm, true).on_click(cx.listener(
        move |w, _, _, cx| {
            act(
                w,
                "calendar-source-add",
                json!({"name": name, "kind": kind}),
                cx,
            );
        },
    ))
}

/// The Add / Edit form of a command source.
fn form(w: &MainWindow, d: &Draft, cx: &mut Context<MainWindow>) -> AnyElement {
    use super::hooks::{labelled, text_box};
    let t = theme::current();
    let chips = div().flex().gap(px(6.)).children(FORMATS.iter().map(|f| {
        let on = d.format == *f;
        kit::button(
            format!("calsrc-format-{f}"),
            *f,
            if on { Kind::Primary } else { Kind::Secondary },
            Size::Sm,
            true,
        )
        .on_click(cx.listener(move |this, _, _, cx| {
            if let Some(d) = this.state.calsrc_draft.as_mut() {
                d.format = f.to_string();
            }
            cx.notify();
        }))
    }));
    let mut col = kit::panel()
        .p(px(14.))
        .gap(px(8.))
        .mb(px(10.))
        .child(
            div()
                .text_size(px(12.))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(rgb(t.dim))
                .child(match &d.editing {
                    Some(n) => format!("Edit calendar source {n}"),
                    None => "Add a command feed: a program that prints ICS or JSON events".into(),
                }),
        )
        .child(labelled(
            "Name",
            text_box(w, NAME, &d.name, "name", 260., cx),
        ))
        .child(labelled(
            "Command",
            text_box(
                w,
                COMMAND,
                &d.command,
                "program and arguments, e.g. my-calendar events --json (no shell)",
                480.,
                cx,
            ),
        ))
        .child(labelled("Format", chips.into_any_element()))
        .child(labelled(
            "Timeout",
            text_box(
                w,
                TIMEOUT,
                &d.timeout,
                "default (calendar.command_timeout)",
                200.,
                cx,
            ),
        ))
        .child(labelled(
            "Folder",
            text_box(w, CWD, &d.cwd, "the app's own", 360., cx),
        ))
        .child(labelled(
            "Env",
            text_box(w, ENV, &d.env, "NAME=value NAME2=value", 360., cx),
        ));
    if let Some(e) = &d.error {
        col = col.child(
            div()
                .text_size(px(12.))
                .text_color(rgb(t.err))
                .child(e.clone()),
        );
    }
    col.child(
        div()
            .flex()
            .gap(px(8.))
            .child(
                kit::button("calsrc-save", "Save", Kind::Primary, Size::Sm, true)
                    .on_click(cx.listener(|this, _, _, cx| save_and_reload(this, cx))),
            )
            .child(
                kit::button("calsrc-cancel", "Cancel", Kind::Secondary, Size::Sm, true).on_click(
                    cx.listener(|this, _, _, cx| {
                        cancel(&mut this.state);
                        cx.notify();
                    }),
                ),
            ),
    )
    .into_any_element()
}

/// The panel: one row per source, and buttons that add this computer's accounts (or the macOS
/// Calendar app), Google and Microsoft 365 where no such source is listed yet, and a command feed.
pub(crate) fn panel(w: &MainWindow, cx: &mut Context<MainWindow>) -> AnyElement {
    let t = theme::current();
    let s = cx.default_global::<Sources>();
    if s.stale(w) && s.busy.is_none() {
        s.load(w);
    }
    let (rows, error, busy) = (s.rows(), s.error.clone(), s.busy.clone());
    let has = |k: &str| rows.iter().any(|r| r["kind"] == k);
    let has_local = has("os-local") || has("eventkit");
    let (has_google, has_ms) = (has("google"), has("microsoft"));
    let n = rows.len();
    let mut panel = kit::panel();
    if rows.is_empty() {
        panel = panel.child(
            div()
                .p(px(14.))
                .text_size(px(13.))
                .text_color(rgb(t.dim))
                .child("No calendar sources: sessions are not titled from a calendar."),
        );
    }
    for (i, r) in rows.iter().enumerate() {
        panel = panel.child(source_row(r, busy.is_some(), i + 1 == n, cx));
    }
    let mut out = div()
        .flex()
        .flex_col()
        .flex_none()
        .max_w(px(820.))
        .mb(px(22.))
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(10.))
                .child(
                    kit::section("Calendar sources")
                        .text_color(rgb(t.dim))
                        .pt_0(),
                )
                .child(div().flex_1())
                .when(busy.is_some(), |d| {
                    d.child(
                        div()
                            .text_size(px(12.))
                            .text_color(rgb(t.faint))
                            .child("Working..."),
                    )
                })
                .when(!has_local && busy.is_none(), |d| {
                    d.child(add_button(
                        "calsrc-use-local",
                        "Use this computer's calendars",
                        "local",
                        "os-local",
                        cx,
                    ))
                })
                .when(!has_google && busy.is_none(), |d| {
                    d.child(add_button(
                        "calsrc-add-google",
                        "Add Google",
                        "google",
                        "google",
                        cx,
                    ))
                })
                .when(!has_ms && busy.is_none(), |d| {
                    d.child(add_button(
                        "calsrc-add-microsoft",
                        "Add Microsoft 365",
                        "microsoft",
                        "microsoft",
                        cx,
                    ))
                })
                .when(busy.is_none() && w.state.calsrc_draft.is_none(), |d| {
                    d.child(
                        kit::button(
                            "calsrc-add-command",
                            "Add command feed",
                            Kind::Secondary,
                            Size::Sm,
                            true,
                        )
                        .on_click(cx.listener(|w, _, _, cx| {
                            open(&mut w.state, None);
                            cx.notify();
                        })),
                    )
                }),
        );
    if let Some(e) = error {
        out = out.child(kit::banner(true, "Calendar sources failed.", e));
    }
    if let Some(d) = &w.state.calsrc_draft {
        out = out.child(form(w, d, cx));
    }
    out.child(panel).into_any_element()
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::state::Tab;
    use crate::testutil;
    use gpui::TestAppContext;
    use oats_core::calendar::eventkit::{Access, Calendar, EventStore};

    /// EventKit with two accounts, undetermined until asked.
    struct Kit {
        access: Mutex<Access>,
        asked: AtomicUsize,
    }

    impl EventStore for Kit {
        fn supported(&self) -> bool {
            true
        }
        fn authorization(&self) -> Access {
            *self.access.lock().unwrap()
        }
        fn request_access(&self, _: std::time::Duration) -> oats_core::config::Result<Access> {
            self.asked.fetch_add(1, Ordering::SeqCst);
            *self.access.lock().unwrap() = Access::FullAccess;
            Ok(Access::FullAccess)
        }
        fn calendars(&self) -> oats_core::config::Result<Vec<Calendar>> {
            let c = |id: &str, title: &str, account: &str| Calendar {
                id: id.into(),
                title: title.into(),
                account: account.into(),
                account_id: format!("src-{account}"),
                account_kind: "caldav".into(),
            };
            Ok(if self.authorization().readable() {
                vec![c("w1", "Work", "me@davidaw.ad"), c("h1", "Home", "iCloud")]
            } else {
                vec![]
            })
        }
        fn events(
            &self,
            _: chrono::DateTime<chrono::Utc>,
            _: chrono::DateTime<chrono::Utc>,
            _: &[String],
        ) -> oats_core::config::Result<Vec<oats_core::calendar::rules::Event>> {
            Ok(vec![])
        }
    }

    fn sources(engine: &oats_core::control::engine::Engine) -> String {
        engine.snapshot().config.calendar.sources.clone()
    }

    #[test]
    fn use_mac_calendar_asks_once_and_the_checkboxes_select_calendars() {
        let (engine, _d) = testutil::engine(&[]);
        let kit = Arc::new(Kit {
            access: Mutex::new(Access::NotDetermined),
            asked: AtomicUsize::new(0),
        });
        engine.set_event_store(kit.clone());
        let mut cx = TestAppContext::single();
        let (view, cx) = cx.add_window_view(|_, _| MainWindow::new(engine.clone()));
        view.update(cx, |v, cx| {
            v.state.tab = Tab::Settings;
            cx.notify();
        });
        cx.run_until_parked();
        // Showing the panel lists the sources and asks nothing.
        view.update(cx, |v, cx| {
            let d = v.dump(cx);
            assert_eq!(d["view"]["calendar_sources"]["eventkit"]["supported"], true);
            assert_eq!(
                d["view"]["calendar_sources"]["sources"][0]["kind"],
                "os-local"
            );
            assert_eq!(d["view"]["calendar_sources"]["sources"][1]["name"], "ics");
        });
        assert_eq!(kit.asked.load(Ordering::SeqCst), 0);
        // "Use macOS Calendar", as ui-invoke clicks it.
        view.update(cx, |v, cx| {
            v.drive(
                &json!({"op": "invoke", "name": "settings.calendar_use_mac"}),
                cx,
            )
            .unwrap();
        });
        cx.run_until_parked();
        assert_eq!(kit.asked.load(Ordering::SeqCst), 1);
        view.update(cx, |v, cx| {
            let d = v.dump(cx);
            let mac = &d["view"]["calendar_sources"]["sources"][2];
            assert_eq!(mac["name"], "mac");
            assert_eq!(mac["accounts"].as_array().unwrap().len(), 2);
            assert_eq!(mac["accounts"][0]["calendars"][0]["selected"], true);
        });
        // Untick Home: only the me@ account stays.
        view.update(cx, |v, cx| {
            v.drive(
                &json!({"op": "invoke", "name": "settings.calendar_select",
                        "args": {"source": "mac", "calendar": "h1", "selected": false}}),
                cx,
            )
            .unwrap();
        });
        cx.run_until_parked();
        assert!(
            sources(&engine).contains(r#""accounts":["me@davidaw.ad"]"#),
            "{}",
            sources(&engine)
        );
        view.read_with(cx, |v, cx| {
            assert!(
                v.state.status.contains("1 of 2 selected"),
                "{}",
                v.state.status
            );
            let s = cx.global::<Sources>();
            let home = s.list["sources"][2]["accounts"]
                .as_array()
                .unwrap()
                .iter()
                .find(|a| a["title"] == "iCloud")
                .unwrap()
                .clone();
            assert_eq!(home["selected"], false);
        });
        // The toggle turns the source off; a select op needs a target.
        view.update(cx, |v, cx| {
            v.drive(
                &json!({"op": "invoke", "name": "settings.calendar_enable",
                        "args": {"name": "mac", "enabled": false}}),
                cx,
            )
            .unwrap();
            let e = v
                .drive(
                    &json!({"op": "invoke", "name": "settings.calendar_select",
                            "args": {"source": "mac"}}),
                    cx,
                )
                .unwrap_err();
            assert_eq!(e.code, "bad_request");
        });
        cx.run_until_parked();
        assert!(sources(&engine).contains(r#""enabled":false"#));
        assert_eq!(kit.asked.load(Ordering::SeqCst), 1, "asked once in all");
    }

    #[test]
    fn add_google_then_connect_opens_the_sign_in_page() {
        let (engine, _d) = testutil::engine(&[]);
        engine.set("network.allow", &json!(true), "test").unwrap();
        engine
            .set("calendar.google_client_id", &json!("cid.apps"), "test")
            .unwrap();
        let mut cx = TestAppContext::single();
        let (view, cx) = cx.add_window_view(|_, _| MainWindow::new(engine.clone()));
        view.update(cx, |v, cx| {
            v.state.tab = Tab::Settings;
            v.drive(
                &json!({"op": "invoke", "name": "settings.calendar_add", "args": {"kind": "google"}}),
                cx,
            )
            .unwrap();
        });
        cx.run_until_parked();
        view.update(cx, |v, cx| {
            let d = v.dump(cx);
            let g = d["view"]["calendar_sources"]["sources"]
                .as_array()
                .unwrap()
                .iter()
                .find(|s| s["kind"] == "google")
                .unwrap()
                .clone();
            assert_eq!(g["name"], "google");
            assert_eq!(g["connected"], false);
            assert_eq!(g["error"]["code"], "calendar_not_connected");
            v.drive(
                &json!({"op": "invoke", "name": "settings.calendar_connect", "args": {"source": "google"}}),
                cx,
            )
            .unwrap();
        });
        cx.run_until_parked();
        view.read_with(cx, |v, _| {
            assert!(
                v.state.status.contains("Sign in to google"),
                "{}",
                v.state.status
            );
        });
        assert_eq!(kind_label("os-local"), "This computer");
        assert_eq!(kind_label("microsoft"), "Microsoft 365");
    }

    #[test]
    fn a_command_feed_is_added_edited_and_shows_its_last_run() {
        let (engine, _d) = testutil::engine(&[]);
        let mut cx = TestAppContext::single();
        let (view, cx) = cx.add_window_view(|_, _| MainWindow::new(engine.clone()));
        let invoke = |v: &mut MainWindow, args: Value, cx: &mut Context<MainWindow>| {
            v.drive(
                &json!({"op": "invoke", "name": "settings.calendar_command", "args": args}),
                cx,
            )
            .unwrap();
        };
        view.update(cx, |v, cx| {
            v.state.tab = Tab::Settings;
            // A refused save keeps the form open with the reason.
            invoke(v, json!({"name": "mine", "command": "  "}), cx);
            let d = v.dump(cx);
            let draft = &d["view"]["calendar_sources"]["draft"];
            assert!(
                draft["error"].as_str().unwrap().contains("argv"),
                "{draft}"
            );
            cancel(&mut v.state);
            invoke(
                v,
                json!({"name": "mine", "command": "/bin/echo [] ", "env": "A=1 B=2", "timeout": "5s"}),
                cx,
            );
            let d = v.dump(cx);
            assert_eq!(d["view"]["calendar_sources"]["draft"], Value::Null, "{d}");
            let src = d["view"]["calendar_sources"]["sources"]
                .as_array()
                .unwrap()
                .iter()
                .find(|s| s["name"] == "mine")
                .unwrap()
                .clone();
            assert_eq!(src["kind"], "command");
            assert_eq!(src["argv"], json!(["/bin/echo", "[]"]));
            assert_eq!(src["env"], json!({"A": "1", "B": "2"}));
            assert!(src["last_run"].is_null());
            // Edit: a quoted argument, another format, a new name.
            invoke(
                v,
                json!({"source": "mine", "name": "feed", "command": "[\"/bin/echo\", \"[ ]\"]",
                       "format": "json", "timeout": ""}),
                cx,
            );
        });
        cx.run_until_parked();
        let saved: Value = serde_json::from_str(&sources(&engine)).unwrap();
        let feed = saved
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["kind"] == "command")
            .unwrap()
            .clone();
        assert_eq!(feed["name"], "feed");
        assert_eq!(feed["argv"], json!(["/bin/echo", "[ ]"]));
        assert_eq!(feed["format"], "json");
        assert!(feed.get("timeout").is_none());
        // Opening its Edit form shows the quoted argument as a JSON array.
        view.update(cx, |v, cx| {
            let row = cx.global::<Sources>().list["sources"]
                .as_array()
                .unwrap()
                .iter()
                .find(|s| s["name"] == "feed")
                .unwrap()
                .clone();
            open(&mut v.state, Some(&row));
            let d = v.state.calsrc_draft.clone().unwrap();
            assert_eq!(d.command, r#"["/bin/echo","[ ]"]"#);
            assert_eq!(d.env, "A=1 B=2");
            cancel(&mut v.state);
            // Run now: the panel then shows the run.
            v.drive(
                &json!({"op": "invoke", "name": "settings.calendar_connect", "args": {"source": "feed"}}),
                cx,
            )
            .unwrap();
        });
        cx.run_until_parked();
        view.read_with(cx, |v, cx| {
            assert!(
                v.state.status.contains("feed ran: 0 events"),
                "{}",
                v.state.status
            );
            let s = cx.global::<Sources>();
            let row = s.list["sources"]
                .as_array()
                .unwrap()
                .iter()
                .find(|s| s["name"] == "feed")
                .unwrap()
                .clone();
            assert_eq!(row["last_run"]["ok"], true, "{row}");
        });
        assert_eq!(kind_label("command"), "Command");
        assert_eq!(argv_of("a  b"), json!(["a", "b"]));
    }
}
