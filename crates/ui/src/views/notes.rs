//! Notes tab: one session's meeting notes. Show them (`notes-get`), edit and save them
//! (`notes-set`), or regenerate them from a template (`template-list`, `notes-generate`), the same
//! actions `ctl` uses. `notes.default_template` picks the template, `notes.confirm_regenerate`
//! makes regenerating over existing notes take a second click.
//!
//! The tab's state lives in a gpui global so the view stays inside this file.

use std::sync::Arc;

use gpui::{
    div, prelude::*, px, rgb, AnyElement, Context, FocusHandle, Global, KeyDownEvent, SharedString,
};
use oats_core::config::Result;
use oats_core::control::engine::Engine;
use serde_json::{json, Value};

use crate::model;
use crate::state::EditKey;
use crate::view::{MainWindow, ACCENT, BG, DIM, ERR, FG, PANEL};

/// What the Notes tab shows. Free of GPUI apart from the editor's focus handle.
#[derive(Default)]
pub struct Notes {
    /// `template-list` has been read at least once.
    pub loaded: bool,
    pub session: Option<String>,
    /// The saved notes (`notes-get`).
    pub text: String,
    /// How the saved notes were made: template, backend/model, edited.
    pub info: String,
    pub templates: Vec<String>,
    pub template: Option<String>,
    /// The editor buffer while editing.
    pub edit: Option<String>,
    /// A `notes-generate` is running off the UI thread.
    pub busy: bool,
    /// First click of a confirmed regenerate happened; the next one runs it.
    pub armed: bool,
    pub error: Option<String>,
    focus: Option<FocusHandle>,
}

impl Global for Notes {}

fn s(v: &Value, k: &str) -> String {
    v[k].as_str().unwrap_or_default().to_string()
}

impl Notes {
    /// Read the templates; keep the current pick if it still exists, else `notes.default_template`
    /// when present, else the first.
    pub fn load_templates(&mut self, engine: &Engine) {
        self.loaded = true;
        match model::dispatch(engine, "template-list", json!({})) {
            Ok(v) => {
                self.templates = v["templates"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|t| s(t, "name"))
                    .collect();
            }
            Err(e) => self.error = Some(e.to_string()),
        }
        let want = engine.snapshot().config.notes.default_template.clone();
        let has = |n: &str| {
            self.templates
                .iter()
                .find(|t| t.eq_ignore_ascii_case(n))
                .cloned()
        };
        self.template = self
            .template
            .as_deref()
            .and_then(has)
            .or_else(|| has(&want))
            .or_else(|| self.templates.first().cloned());
    }

    /// Show a session's notes; drops any unsaved edit.
    pub fn open(&mut self, engine: &Engine, id: &str) {
        self.session = Some(id.to_string());
        self.edit = None;
        self.armed = false;
        self.error = None;
        match model::dispatch(engine, "notes-get", json!({"session": id})) {
            Ok(v) => {
                self.text = s(&v, "notes");
                let mut info: Vec<String> = vec![];
                if !s(&v, "template").is_empty() {
                    info.push(format!("template {}", s(&v, "template")));
                }
                if !s(&v, "backend").is_empty() {
                    info.push(format!("{} / {}", s(&v, "backend"), s(&v, "model")));
                }
                if v["edited"].as_bool() == Some(true) {
                    info.push("edited".into());
                }
                self.info = info.join(" · ");
            }
            Err(e) => {
                self.text.clear();
                self.info.clear();
                self.error = Some(e.to_string());
            }
        }
    }

    pub fn begin_edit(&mut self) {
        if self.session.is_some() {
            self.edit = Some(self.text.clone());
            self.armed = false;
        }
    }

    pub fn cancel_edit(&mut self) {
        self.edit = None;
    }

    /// One key press in the editor: enter is a newline, escape cancels, ctrl/cmd-s saves.
    pub fn key(&mut self, key: &str, key_char: Option<&str>, command: bool) -> EditKey {
        let Some(buf) = self.edit.as_mut() else {
            return EditKey::Ignored;
        };
        match key {
            "s" if command => EditKey::Commit,
            "escape" => EditKey::Cancel,
            "enter" => {
                buf.push('\n');
                EditKey::Handled
            }
            "backspace" => {
                buf.pop();
                EditKey::Handled
            }
            _ => match key_char {
                Some(c) if !command && !c.is_empty() && !c.chars().any(char::is_control) => {
                    buf.push_str(c);
                    EditKey::Handled
                }
                _ => EditKey::Ignored,
            },
        }
    }

    /// Write the buffer through `notes-set`. On failure the editor stays open with the message.
    pub fn save(&mut self, engine: &Engine) -> bool {
        let (Some(id), Some(buf)) = (self.session.clone(), self.edit.clone()) else {
            return false;
        };
        match model::dispatch(engine, "notes-set", json!({"session": id, "notes": buf})) {
            Ok(_) => {
                self.open(engine, &id);
                true
            }
            Err(e) => {
                self.error = Some(e.to_string());
                false
            }
        }
    }

    /// A Regenerate click: `Some((session, template))` when `notes-generate` should run now.
    /// Over existing notes with `confirm` on, the first click only arms the button.
    pub fn regenerate(&mut self, confirm: bool) -> Option<(String, String)> {
        if self.busy {
            return None;
        }
        let (Some(id), Some(t)) = (self.session.clone(), self.template.clone()) else {
            self.error = Some(if self.session.is_none() {
                "Select a session first.".into()
            } else {
                "No templates: add one in the templates folder (templates.dir).".into()
            });
            return None;
        };
        if confirm && !self.text.is_empty() && !self.armed {
            self.armed = true;
            return None;
        }
        self.armed = false;
        self.busy = true;
        self.error = None;
        Some((id, t))
    }

    /// `notes-generate` finished: reload on success, keep the old notes and show why otherwise.
    pub fn generated(&mut self, engine: &Engine, id: &str, res: Result<Value>) {
        self.busy = false;
        match res {
            Ok(_) if self.session.as_deref() == Some(id) => self.open(engine, id),
            Ok(_) => {}
            Err(e) => self.error = Some(e.to_string()),
        }
    }
}

fn run_regenerate(this: &mut MainWindow, cx: &mut Context<MainWindow>) {
    let confirm = this.engine.snapshot().config.notes.confirm_regenerate;
    let Some((id, template)) = cx.global_mut::<Notes>().regenerate(confirm) else {
        cx.notify();
        return;
    };
    cx.notify();
    let engine = Arc::clone(&this.engine);
    cx.spawn(async move |view, cx| {
        let (eng, sid) = (Arc::clone(&engine), id.clone());
        let res = cx
            .background_executor()
            .spawn(async move {
                model::dispatch(
                    &eng,
                    "notes-generate",
                    json!({"session": sid, "template": template}),
                )
            })
            .await;
        let _ = view.update(cx, |v, cx| {
            v.state.status = match &res {
                Ok(_) => format!("Saved notes for {id}"),
                Err(e) => e.to_string(),
            };
            cx.global_mut::<Notes>().generated(&v.engine, &id, res);
            cx.notify();
        });
    })
    .detach();
}

fn save(this: &mut MainWindow, cx: &mut Context<MainWindow>) {
    if cx.global_mut::<Notes>().save(&this.engine) {
        this.state.status = "Saved notes".into();
    }
    cx.notify();
}

fn chip(id: String, label: String, on: bool) -> gpui::Stateful<gpui::Div> {
    div()
        .id(SharedString::from(id))
        .px_2()
        .rounded_md()
        .cursor_pointer()
        .bg(rgb(if on { ACCENT } else { PANEL }))
        .text_color(rgb(if on { BG } else { FG }))
        .text_sm()
        .child(label)
}

fn text_block(text: &str, cursor: bool) -> gpui::Div {
    let mut lines: Vec<String> = text.split('\n').map(str::to_string).collect();
    if cursor {
        if let Some(l) = lines.last_mut() {
            l.push('|');
        }
    }
    div().flex().flex_col().w_full().children(
        lines
            .into_iter()
            .map(|l| div().w_full().min_h(px(18.)).whitespace_normal().child(l)),
    )
}

pub fn render(w: &MainWindow, cx: &mut Context<MainWindow>) -> AnyElement {
    if !cx.default_global::<Notes>().loaded {
        cx.global_mut::<Notes>().load_templates(&w.engine);
    }
    // Follow the Sessions tab's selection until a session is picked here.
    if cx.global::<Notes>().session.is_none() {
        if let Some(id) = w.state.selected.clone() {
            cx.global_mut::<Notes>().open(&w.engine, &id);
        }
    }
    if cx.global::<Notes>().focus.is_none() {
        let f = cx.focus_handle();
        cx.global_mut::<Notes>().focus = Some(f);
    }
    let focus = cx.global::<Notes>().focus.clone().expect("set above");
    let n = cx.global::<Notes>();
    let (session, editing, busy, armed) = (n.session.clone(), n.edit.clone(), n.busy, n.armed);
    let (text, info, error) = (n.text.clone(), n.info.clone(), n.error.clone());
    let (templates, template) = (n.templates.clone(), n.template.clone());

    let list = div()
        .id("notes-sessions")
        .flex()
        .flex_col()
        .gap_1()
        .w(px(240.))
        .flex_none()
        .overflow_y_scroll()
        .children(w.state.sessions.iter().map(|r| {
            let id = r.id.clone();
            let sel = session.as_deref() == Some(r.id.as_str());
            let title = if r.title.is_empty() { &r.id } else { &r.title };
            chip(format!("notes-session-{}", r.id), title.clone(), sel)
                .py_1()
                .on_click(cx.listener(move |this, _, _, cx| {
                    cx.global_mut::<Notes>().open(&this.engine, &id);
                    cx.notify();
                }))
        }));

    let picker = div()
        .flex()
        .flex_wrap()
        .gap_1()
        .items_center()
        .child(div().text_sm().text_color(rgb(DIM)).child("Template:"))
        .children(templates.into_iter().map(|t| {
            let on = template.as_deref() == Some(t.as_str());
            let pick = t.clone();
            chip(format!("notes-template-{t}"), t, on).on_click(cx.listener(move |_, _, _, cx| {
                let n = cx.global_mut::<Notes>();
                n.template = Some(pick.clone());
                n.armed = false;
                cx.notify();
            }))
        }));

    let mut buttons = div().flex().gap_2().items_center();
    if editing.is_some() {
        buttons = buttons
            .child(
                chip("notes-save".into(), "Save".into(), true)
                    .on_click(cx.listener(|this, _, _, cx| save(this, cx))),
            )
            .child(
                chip("notes-cancel".into(), "Cancel".into(), false).on_click(cx.listener(
                    |_, _, _, cx| {
                        cx.global_mut::<Notes>().cancel_edit();
                        cx.notify();
                    },
                )),
            );
    } else if session.is_some() {
        let f = focus.clone();
        buttons = buttons.child(chip("notes-edit".into(), "Edit".into(), false).on_click(
            cx.listener(move |_, _, window, cx| {
                cx.global_mut::<Notes>().begin_edit();
                f.focus(window, cx);
                cx.notify();
            }),
        ));
    }
    if editing.is_none() {
        let label = match (busy, armed) {
            (true, _) => "Generating...",
            (_, true) => "Replace notes? Click again",
            _ => "Regenerate",
        };
        buttons = buttons.child(
            chip("notes-regenerate".into(), label.into(), armed)
                .on_click(cx.listener(|this, _, _, cx| run_regenerate(this, cx))),
        );
    }

    let body = match (&session, &editing) {
        (None, _) => div()
            .text_color(rgb(DIM))
            .child("Select a session.")
            .into_any_element(),
        (Some(_), Some(buf)) => {
            let f = focus.clone();
            div()
                .id("notes-editor")
                .p_2()
                .rounded_md()
                .border_1()
                .border_color(rgb(ACCENT))
                .cursor_text()
                .track_focus(&focus)
                .on_click(cx.listener(move |_, _, window, cx| f.focus(window, cx)))
                .on_key_down(cx.listener(|this, ev: &KeyDownEvent, _, cx| {
                    let k = &ev.keystroke;
                    let command = k.modifiers.control || k.modifiers.alt || k.modifiers.platform;
                    let r = cx
                        .global_mut::<Notes>()
                        .key(&k.key, k.key_char.as_deref(), command);
                    match r {
                        EditKey::Commit => save(this, cx),
                        EditKey::Cancel => cx.global_mut::<Notes>().cancel_edit(),
                        EditKey::Handled => {}
                        EditKey::Ignored => return,
                    }
                    cx.stop_propagation();
                    cx.notify();
                }))
                .child(text_block(buf, true))
                .into_any_element()
        }
        (Some(_), None) if text.is_empty() => div()
            .text_color(rgb(DIM))
            .child("No notes yet. Pick a template and Regenerate, or Edit to write them.")
            .into_any_element(),
        (Some(_), None) => text_block(&text, false).into_any_element(),
    };

    let detail = div()
        .flex()
        .flex_col()
        .gap_2()
        .flex_1()
        .min_w_0()
        .child(picker)
        .child(buttons)
        .children((!info.is_empty()).then(|| div().text_sm().text_color(rgb(DIM)).child(info)))
        .children(error.map(|e| div().text_sm().text_color(rgb(ERR)).child(e)))
        .child(
            div()
                .id("notes-body")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .child(body),
        );

    div()
        .flex()
        .gap_4()
        .size_full()
        .child(list)
        .child(detail)
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::Tab;
    use crate::testutil;
    use gpui::TestAppContext;

    /// A session with a transcript and two templates; config at `<dir>/config.toml`, so the
    /// templates folder is `<dir>/templates`.
    fn seed(dir: &std::path::Path, notes: Option<&str>) {
        let d = dir.join("sessions/s1");
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(
            d.join("session.json"),
            r#"{"id":"s1","title":"Standup","startedAt":"2026-10-27T10:00:00Z"}"#,
        )
        .unwrap();
        std::fs::write(
            d.join("transcript.final.jsonl"),
            "{\"speaker\":\"you\",\"text\":\"hello\"}\n",
        )
        .unwrap();
        if let Some(n) = notes {
            std::fs::write(d.join("notes.md"), n).unwrap();
        }
        let t = dir.join("templates");
        std::fs::create_dir_all(&t).unwrap();
        for name in ["Alpha", "Beta"] {
            std::fs::write(
                t.join(format!("{name}.toml")),
                format!("schema_version = 1\nname = \"{name}\"\nsystem_prompt = \"## Summary\"\n"),
            )
            .unwrap();
        }
    }

    #[test]
    fn open_shows_saved_notes_and_templates_follow_the_default_key() {
        let (engine, dir) = testutil::engine(&[("notes.default_template", json!("beta"))]);
        seed(dir.path(), Some("## Summary\nold"));
        let mut n = Notes::default();
        n.load_templates(&engine);
        assert_eq!(n.templates, ["Alpha", "Beta"]);
        assert_eq!(n.template.as_deref(), Some("Beta"));
        n.open(&engine, "s1");
        assert_eq!(n.text, "## Summary\nold");
        assert_eq!(n.error, None);
        n.open(&engine, "nope");
        assert!(n.error.is_some() && n.text.is_empty());

        let (engine, dir) = testutil::engine(&[]);
        seed(dir.path(), None);
        let mut n = Notes::default();
        n.load_templates(&engine);
        assert_eq!(
            n.template.as_deref(),
            Some("Alpha"),
            "empty default = first"
        );
        n.open(&engine, "s1");
        assert_eq!(n.text, "");
    }

    #[test]
    fn edit_types_and_saves_through_notes_set_and_cancel_discards() {
        let (engine, dir) = testutil::engine(&[]);
        seed(dir.path(), Some("ab"));
        let mut n = Notes::default();
        n.open(&engine, "s1");
        assert_eq!(
            n.key("x", Some("x"), false),
            EditKey::Ignored,
            "not editing"
        );
        n.begin_edit();
        assert_eq!(n.key("backspace", None, false), EditKey::Handled);
        assert_eq!(n.key("enter", Some("\n"), false), EditKey::Handled);
        assert_eq!(n.key("z", Some("z"), false), EditKey::Handled);
        assert_eq!(n.key("c", Some("c"), true), EditKey::Ignored);
        assert_eq!(n.key("s", Some("s"), true), EditKey::Commit);
        assert_eq!(n.edit.as_deref(), Some("a\nz"));
        assert!(n.save(&engine));
        assert_eq!(n.edit, None);
        assert_eq!(n.text, "a\nz");
        assert!(n.info.contains("edited"), "{}", n.info);
        let d = dir.path().join("sessions/s1");
        assert_eq!(std::fs::read_to_string(d.join("notes.md")).unwrap(), "a\nz");
        let meta: Value =
            serde_json::from_str(&std::fs::read_to_string(d.join("session.json")).unwrap())
                .unwrap();
        assert_eq!(meta["hasNotes"], true);

        n.begin_edit();
        n.key("q", Some("q"), false);
        assert_eq!(n.key("escape", None, false), EditKey::Cancel);
        n.cancel_edit();
        assert_eq!(n.text, "a\nz");
    }

    #[test]
    fn regenerate_asks_twice_over_existing_notes_unless_configured_off() {
        let (engine, dir) = testutil::engine(&[
            ("llm.backend", json!("openai-compatible")),
            ("llm.base_url", json!("")),
        ]);
        seed(dir.path(), Some("old"));
        let mut n = Notes::default();
        assert_eq!(n.regenerate(true), None);
        assert!(n.error.as_deref().unwrap().contains("Select a session"));
        n.load_templates(&engine);
        n.open(&engine, "s1");
        assert_eq!(n.regenerate(true), None);
        assert!(n.armed);
        let req = n.regenerate(true).unwrap();
        assert_eq!(req, ("s1".into(), "Alpha".into()));
        assert!(n.busy && n.regenerate(false).is_none(), "one at a time");
        // No backend URL configured: the engine's error is shown and the old notes stay.
        let res = model::dispatch(
            &engine,
            "notes-generate",
            json!({"session": req.0, "template": req.1}),
        );
        n.generated(&engine, "s1", res);
        assert!(!n.busy);
        assert!(
            n.error.as_deref().unwrap().contains("base_url"),
            "{:?}",
            n.error
        );
        assert_eq!(n.text, "old");
        assert!(
            n.regenerate(false).is_some(),
            "confirm off: straight through"
        );
    }

    #[test]
    fn notes_tab_renders_and_drives_edit_save_and_regenerate() {
        let (engine, dir) = testutil::engine(&[("notes.confirm_regenerate", json!(false))]);
        seed(dir.path(), None);
        let mut cx = TestAppContext::single();
        let (view, cx) = cx.add_window_view(|_, _| MainWindow::new(engine.clone()));
        view.update(cx, |v, cx| {
            v.state.select(&v.engine, "s1");
            v.state.tab = Tab::Notes;
            cx.notify();
        });
        cx.run_until_parked();
        view.update(cx, |_, cx| {
            let n = cx.global::<Notes>();
            assert_eq!(
                n.session.as_deref(),
                Some("s1"),
                "follows Sessions selection"
            );
            assert_eq!(n.templates.len(), 2);
        });
        // Edit, type through the focused editor's key handler, save with ctrl-s.
        view.update(cx, |_, cx| {
            cx.global_mut::<Notes>().begin_edit();
            cx.notify();
        });
        cx.run_until_parked();
        let focus = view.update(cx, |_, cx| cx.global::<Notes>().focus.clone().unwrap());
        cx.update(|window, cx| focus.focus(window, cx));
        cx.simulate_keystrokes("h i enter x");
        view.update(cx, |_, cx| {
            assert_eq!(cx.global::<Notes>().edit.as_deref(), Some("hi\nx"));
        });
        cx.simulate_keystrokes("ctrl-s");
        cx.run_until_parked();
        view.update(cx, |v, cx| {
            assert_eq!(cx.global::<Notes>().edit, None);
            assert_eq!(cx.global::<Notes>().text, "hi\nx");
            assert_eq!(v.state.status, "Saved notes");
        });
        // Regenerate against the default local backend with nothing listening fails cleanly.
        view.update(cx, |v, cx| {
            v.engine
                .dispatch(&serde_json::from_value(json!({"action": "config-set", "key": "llm.backend", "value": "openai-compatible"})).unwrap())
                .unwrap();
            run_regenerate(v, cx);
        });
        cx.run_until_parked();
        view.update(cx, |_, cx| {
            let n = cx.global::<Notes>();
            assert!(!n.busy);
            assert!(n.error.is_some());
            assert_eq!(n.text, "hi\nx");
        });
        assert!(dir.path().join("sessions/s1/notes.md").is_file());
    }
}
