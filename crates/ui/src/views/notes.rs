//! Notes tab: one session's meeting notes. Show them (`notes-get`), edit and save them
//! (`notes-set`), or regenerate them from a template (`template-list`, `notes-generate`), the same
//! actions `ctl` uses. `notes.default_template` picks the template, `notes.confirm_regenerate`
//! makes regenerating over existing notes take a second click.
//!
//! The tab's state lives in a gpui global so the view stays inside this file.

use std::sync::Arc;

use gpui::{
    anchored, deferred, div, prelude::*, px, rgb, AnyElement, Context, FocusHandle, FontWeight,
    Global, KeyDownEvent, SharedString,
};
use oats_core::config::Result;
use oats_core::control::engine::Engine;
use serde_json::{json, Value};

use crate::kit::{self, Kind, Size};
use crate::model;
use crate::state::{type_key, EditKey};
use crate::theme::{self, MONO};
use crate::view::MainWindow;

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
    /// ctrl/cmd+a selected the whole editor buffer.
    pub all: bool,
    /// A `notes-generate` is running off the UI thread.
    pub busy: bool,
    /// First click of a confirmed regenerate happened; the next one runs it.
    pub armed: bool,
    pub error: Option<String>,
    /// The session dropdown in the header is open.
    pub picking: bool,
    pub(crate) focus: Option<FocusHandle>,
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
            self.all = false;
            self.armed = false;
        }
    }

    pub fn cancel_edit(&mut self) {
        self.edit = None;
    }

    /// One key press in the editor: enter is a newline, escape cancels, ctrl/cmd-s saves,
    /// ctrl/cmd-a selects all.
    pub fn key(&mut self, key: &str, key_char: Option<&str>, command: bool) -> EditKey {
        let Some(buf) = self.edit.as_mut() else {
            return EditKey::Ignored;
        };
        match key {
            "s" if command => EditKey::Commit,
            "escape" => EditKey::Cancel,
            "enter" => {
                if std::mem::take(&mut self.all) {
                    buf.clear();
                }
                buf.push('\n');
                EditKey::Handled
            }
            _ if type_key(buf, &mut self.all, key, key_char, command) => EditKey::Handled,
            _ => EditKey::Ignored,
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

pub(crate) fn run_regenerate(this: &mut MainWindow, cx: &mut Context<MainWindow>) {
    let confirm = this.engine.snapshot().config.notes.confirm_regenerate;
    // Re-read the picker so a template added or removed outside this tab is honoured.
    cx.global_mut::<Notes>().load_templates(&this.engine);
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

pub(crate) fn save(this: &mut MainWindow, cx: &mut Context<MainWindow>) {
    if cx.global_mut::<Notes>().save(&this.engine) {
        this.state.status = "Saved notes".into();
    }
    cx.notify();
}

/// The notes as a document: `#`/`##` lines are headings, `- ` / `* ` lines bullets, the rest
/// paragraphs. `cursor` puts the editor caret after the last line.
fn text_block(text: &str, cursor: bool) -> gpui::Div {
    let t = theme::current();
    let mut lines: Vec<String> = text.split('\n').map(str::to_string).collect();
    if cursor {
        if let Some(l) = lines.last_mut() {
            l.push('|');
        }
    }
    div()
        .flex()
        .flex_col()
        .w_full()
        .max_w(px(720.))
        .children(lines.into_iter().map(move |l| {
            let line = div().w_full().min_h(px(22.)).whitespace_normal();
            if cursor {
                return line.font_family(MONO).text_size(px(13.)).child(l);
            }
            if let Some(h) = l.strip_prefix("## ").or_else(|| l.strip_prefix("# ")) {
                return line
                    .mt(px(14.))
                    .mb(px(2.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(h.to_string());
            }
            if let Some(item) = l.strip_prefix("- ").or_else(|| l.strip_prefix("* ")) {
                return line
                    .flex()
                    .gap(px(8.))
                    .text_size(px(15.))
                    .line_height(gpui::relative(1.6))
                    .child(div().flex_none().text_color(rgb(t.dim)).child("•"))
                    .child(div().flex_1().min_w_0().child(item.to_string()));
            }
            line.text_size(px(15.))
                .line_height(gpui::relative(1.6))
                .child(l)
        }))
}

pub fn render(w: &MainWindow, cx: &mut Context<MainWindow>) -> AnyElement {
    let th = theme::current();
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
    let (all, picking) = (n.all, n.picking);
    let (templates, template) = (n.templates.clone(), n.template.clone());
    let row = session
        .as_deref()
        .and_then(|id| w.state.sessions.iter().find(|r| r.id == id));
    let title = |r: &crate::state::SessionRow| {
        if r.title.is_empty() {
            r.id.clone()
        } else {
            r.title.clone()
        }
    };
    let shown = match (row, &session) {
        (Some(r), _) => title(r),
        (None, Some(id)) => id.clone(),
        (None, None) => "Pick a session".to_string(),
    };

    // The header's session picker: a dropdown over the session list.
    let mut picker = div().relative().flex_none().child(
        kit::dropdown("notes-session-picker", shown.clone(), picking).on_click(cx.listener(
            |_, _, _, cx| {
                let n = cx.global_mut::<Notes>();
                n.picking = !n.picking;
                cx.notify();
            },
        )),
    );
    if picking {
        let items = w.state.sessions.iter().map(|r| {
            let id = r.id.clone();
            let on = session.as_deref() == Some(r.id.as_str());
            kit::menu_item(format!("notes-session-{}", r.id), title(r), on).on_click(cx.listener(
                move |this, _, _, cx| {
                    let n = cx.global_mut::<Notes>();
                    n.picking = false;
                    n.open(&this.engine, &id);
                    cx.notify();
                },
            ))
        });
        picker = picker.child(
            div().absolute().top(px(34.)).left_0().child(deferred(
                anchored().child(
                    kit::menu()
                        .id("notes-session-menu")
                        .max_h(px(420.))
                        .overflow_y_scroll()
                        .children(items),
                ),
            )),
        );
    }
    let head = kit::head("Notes").child(div().flex_1()).child(picker);

    let body = match (&session, &editing) {
        (None, _) => kit::empty(
            "No session picked",
            "Pick a session above (or select one on the Sessions tab) to read or write its notes.",
        )
        .into_any_element(),
        (Some(_), Some(buf)) => {
            let f = focus.clone();
            div()
                .id("notes-editor")
                .p(px(12.))
                .rounded(px(7.))
                .border_1()
                .border_color(rgb(th.accent))
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
                .child(
                    text_block(buf, !all)
                        .when(all, |b| b.bg(rgb(th.accent)).text_color(rgb(th.accent_ink))),
                )
                .into_any_element()
        }
        (Some(_), None) if text.is_empty() => kit::empty(
            "No notes yet",
            "Pick a template and Regenerate, or Edit to write them.",
        )
        .into_any_element(),
        (Some(_), None) => text_block(&text, false).into_any_element(),
    };
    let meta = row.map(|r| {
        let mut m = vec![r.started_at.clone(), format!("{} lines", r.utterances)];
        if !r.tags.is_empty() {
            m.push(r.tags.join(", "));
        }
        m.join(" · ")
    });
    let doc = div()
        .id("notes-body")
        .flex()
        .flex_col()
        .flex_1()
        .min_w_0()
        .overflow_y_scroll()
        .px(px(34.))
        .py(px(26.))
        .rounded(px(10.))
        .border_1()
        .border_color(rgb(th.line))
        .bg(rgb(th.surface))
        .when(session.is_some(), |d| {
            d.child(
                div()
                    .text_size(px(22.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(shown.clone()),
            )
            .children(meta.map(|m| {
                kit::mono(m)
                    .mb(px(8.))
                    .text_size(px(12.))
                    .text_color(rgb(th.dim))
            }))
        })
        .child(body);

    // The aside: template choice, the actions, how the notes were made.
    let options = templates.into_iter().map(|t| {
        let on = template.as_deref() == Some(t.as_str());
        let pick = t.clone();
        div()
            .id(SharedString::from(format!("notes-template-{t}")))
            .flex()
            .items_center()
            .gap(px(9.))
            .px(px(8.))
            .py(px(7.))
            .rounded(px(7.))
            .text_size(px(13.))
            .cursor_pointer()
            .when(on, |d| d.bg(rgb(th.accent_soft)))
            .when(!on, |d| d.hover(move |s| s.bg(rgb(th.raised))))
            .child(kit::radio(on))
            .child(t)
            .on_click(cx.listener(move |_, _, _, cx| {
                let n = cx.global_mut::<Notes>();
                n.template = Some(pick.clone());
                n.armed = false;
                cx.notify();
            }))
    });
    let card = |title: &'static str| {
        kit::panel().p(px(14.)).child(
            div()
                .mb(px(8.))
                .text_size(px(12.))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(rgb(th.dim))
                .child(title),
        )
    };
    let mut aside = div()
        .id("notes-aside")
        .w(px(250.))
        .flex_none()
        .flex()
        .flex_col()
        .gap(px(12.))
        .overflow_y_scroll()
        .child(card("Template").children(options));
    if editing.is_some() {
        aside = aside
            .child(
                kit::button("notes-save", "Save", Kind::Primary, Size::Md, true)
                    .on_click(cx.listener(|this, _, _, cx| save(this, cx))),
            )
            .child(
                kit::button("notes-cancel", "Cancel", Kind::Secondary, Size::Md, true).on_click(
                    cx.listener(|_, _, _, cx| {
                        cx.global_mut::<Notes>().cancel_edit();
                        cx.notify();
                    }),
                ),
            );
    } else {
        let label = match (busy, armed) {
            (true, _) => "Generating...",
            (_, true) => "Replace notes? Click again",
            _ => "Regenerate notes",
        };
        aside = aside.child(
            kit::button(
                "notes-regenerate",
                label,
                if armed { Kind::Danger } else { Kind::Primary },
                Size::Md,
                !busy,
            )
            .on_click(cx.listener(|this, _, _, cx| run_regenerate(this, cx))),
        );
        if session.is_some() {
            let f = focus.clone();
            aside = aside.child(
                kit::button("notes-edit", "Edit", Kind::Secondary, Size::Md, true).on_click(
                    cx.listener(move |_, _, window, cx| {
                        cx.global_mut::<Notes>().begin_edit();
                        f.focus(window, cx);
                        cx.notify();
                    }),
                ),
            );
        }
    }
    if let Some(e) = error {
        aside = aside.child(kit::banner(true, "Notes failed.", e));
    }
    if !info.is_empty() {
        aside = aside.child(
            card("Generated by").child(
                kit::mono(info)
                    .text_size(px(12.))
                    .text_color(rgb(th.dim))
                    .whitespace_normal(),
            ),
        );
    }

    div()
        .flex()
        .flex_col()
        .size_full()
        .child(head)
        .child(kit::pad().flex_row().gap(px(22.)).child(doc).child(aside))
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
        // ctrl-a selects the whole editor: typing replaces it, backspace empties it.
        view.update(cx, |_, cx| {
            cx.global_mut::<Notes>().begin_edit();
            cx.notify();
        });
        cx.run_until_parked();
        cx.update(|window, cx| focus.focus(window, cx));
        cx.simulate_keystrokes("ctrl-a");
        cx.run_until_parked();
        cx.simulate_keystrokes("n e w");
        view.update(cx, |_, cx| {
            assert_eq!(cx.global::<Notes>().edit.as_deref(), Some("new"));
        });
        cx.simulate_keystrokes("ctrl-a backspace");
        view.update(cx, |_, cx| {
            assert_eq!(cx.global::<Notes>().edit.as_deref(), Some(""));
            cx.global_mut::<Notes>().cancel_edit();
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
    #[test]
    fn templates_created_after_launch_reach_the_notes_picker_without_restart() {
        use crate::views::templates::Templates;
        let (engine, dir) = testutil::engine(&[
            ("notes.confirm_regenerate", json!(false)),
            ("llm.backend", json!("openai-compatible")),
            ("llm.base_url", json!("")),
        ]);
        seed(dir.path(), None);
        for name in ["Alpha", "Beta"] {
            std::fs::remove_file(dir.path().join(format!("templates/{name}.toml"))).unwrap();
        }
        let mut cx = TestAppContext::single();
        let (view, cx) = cx.add_window_view(|_, _| MainWindow::new(engine.clone()));
        // Notes first, with no templates yet: the picker is read and is empty.
        view.update(cx, |v, cx| {
            v.state.select(&v.engine, "s1");
            v.switch_tab(Tab::Notes, cx);
        });
        cx.run_until_parked();
        view.update(cx, |_, cx| {
            assert!(cx.global::<Notes>().templates.is_empty())
        });
        // Templates tab > New > "QA Standup" > ctrl-s.
        view.update(cx, |v, cx| v.switch_tab(Tab::Templates, cx));
        cx.run_until_parked();
        view.update_in(cx, |_, window, cx| {
            cx.global_mut::<Templates>().create();
            let f = cx.global::<Templates>().focus.clone().unwrap();
            f.focus(window, cx);
            cx.notify();
        });
        cx.run_until_parked();
        cx.simulate_input("QA Standup");
        cx.simulate_keystrokes("enter tab");
        cx.simulate_input("##");
        cx.simulate_keystrokes("space");
        cx.simulate_input("Summary");
        cx.simulate_keystrokes("ctrl-s");
        cx.run_until_parked();
        view.update(cx, |_, cx| assert_eq!(cx.global::<Templates>().error, None));
        // Back on Notes the new template is offered and Regenerate gets past the template check
        // to the (unconfigured) backend.
        view.update(cx, |v, cx| v.switch_tab(Tab::Notes, cx));
        cx.run_until_parked();
        view.update(cx, |v, cx| {
            let n = cx.global::<Notes>();
            assert_eq!(n.templates, ["QA Standup"]);
            assert_eq!(n.template.as_deref(), Some("QA Standup"));
            run_regenerate(v, cx);
        });
        cx.run_until_parked();
        view.update(cx, |_, cx| {
            let n = cx.global::<Notes>();
            let e = n.error.clone().unwrap_or_default();
            assert!(e.contains("base_url"), "{e}");
        });
        // A template saved outside the tab (ctl template-save) while Notes is open is picked up
        // by Regenerate itself.
        view.update(cx, |v, cx| {
            cx.global_mut::<Notes>().template = None;
            model::dispatch(&v.engine, "template-delete", json!({"name": "QA Standup"})).unwrap();
            model::dispatch(
                &v.engine,
                "template-save",
                json!({"name": "Retro", "system_prompt": "## Wins"}),
            )
            .unwrap();
            run_regenerate(v, cx);
        });
        cx.run_until_parked();
        view.update(cx, |_, cx| {
            let n = cx.global::<Notes>();
            assert_eq!(n.templates, ["Retro"]);
            assert_eq!(n.template.as_deref(), Some("Retro"));
            let e = n.error.clone().unwrap_or_default();
            assert!(e.contains("base_url"), "{e}");
        });
    }
}
