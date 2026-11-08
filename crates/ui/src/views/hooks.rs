//! The Hooks panel under the Settings tab's Hooks section (dot-4uzgol.89): every `[[hooks]]` entry
//! with its on/off toggle, Test (runs it now, off the UI thread, as `ctl hook-test`), Edit and
//! Remove (first click arms), and its last run: exit code, time and output. Add / Edit open a form
//! whose text boxes are the window's one editor (`State::edit`, Enter keeps the field); Save runs
//! `hook-add` or `hook-edit`. A failed hook run also raises the window's hook banner
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
    pub cwd: String,
    pub error: Option<String>,
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

    /// What `ui-state` shows for the panel.
    pub fn dump(&self) -> Value {
        json!({
            "hooks": self.list["hooks"],
            "runs": self.list["runs"],
            "error": self.error,
            "busy": self.busy,
            "arming": self.arming,
            "draft": self.draft.as_ref().map(|d| json!({
                "editing": d.editing, "name": d.name, "event": d.event, "command": d.command,
                "shell": d.shell, "timeout": d.timeout, "cwd": d.cwd, "error": d.error,
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
            Draft {
                editing: Some(n.to_string()),
                name: n.to_string(),
                event: h["event"].as_str().unwrap_or(&first).to_string(),
                command: words.join(" "),
                shell: h["shell"] == true,
                timeout: h["timeout"].as_str().unwrap_or_default().to_string(),
                cwd: h["cwd"].as_str().unwrap_or_default().to_string(),
                error: None,
            }
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
                   "timeout": d.timeout, "cwd": d.cwd}),
        ),
        Some(old) => (
            "hook-edit",
            json!({"hook": old, "name": d.name, "event": d.event, "command": command,
                   "shell": d.shell, "timeout": d.timeout, "cwd": d.cwd}),
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
        .child(labelled("Event", chips.into_any_element()))
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
        .child(labelled(
            "Folder",
            text_box(w, CWD, &d.cwd, "the session folder", 360., cx),
        ));
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
                json!({"name": "ok", "event": "session.saved", "command": "true"}),
                cx,
            );
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
