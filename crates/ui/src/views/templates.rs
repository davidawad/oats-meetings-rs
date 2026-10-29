//! Templates tab: the note templates in `templates.dir` (`template-list`), the selected one's
//! prompt and required headings (`template-get`), and create / edit / rename (`template-save`) and
//! delete (`template-delete`, two clicks). The editor is a small keyboard form over name, icon and
//! prompt: Tab moves between fields, Enter is a newline in the prompt, Ctrl/Cmd+Enter or
//! Ctrl/Cmd+S saves, Esc cancels.
//!
//! The view's own state lives in a gpui global so the tab stays in this one file.

use gpui::{
    div, prelude::*, px, rgb, AnyElement, App, Context, FocusHandle, Global, KeyDownEvent,
    SharedString,
};
use serde_json::{json, Value};

use crate::model;
use crate::view::{MainWindow, ACCENT, BG, DIM, ERR, FG, PANEL};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Item {
    pub name: String,
    pub icon: String,
    pub headings: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Detail {
    pub name: String,
    pub icon: String,
    pub system_prompt: String,
    pub headings: Vec<String>,
    pub openoats_id: String,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Field {
    #[default]
    Name,
    Icon,
    Prompt,
}

impl Field {
    fn next(self) -> Field {
        match self {
            Field::Name => Field::Icon,
            Field::Icon => Field::Prompt,
            Field::Prompt => Field::Name,
        }
    }
}

/// An open create / edit form; each field is one `template-save` parameter.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Draft {
    /// The template being edited (`previous`); `None` creates a new one.
    pub previous: Option<String>,
    pub name: String,
    pub icon: String,
    pub prompt: String,
    pub field: Field,
    /// Create only: replace a template that already has this name (`overwrite`).
    pub overwrite: bool,
}

impl Draft {
    fn buf(&mut self) -> &mut String {
        match self.field {
            Field::Name => &mut self.name,
            Field::Icon => &mut self.icon,
            Field::Prompt => &mut self.prompt,
        }
    }
}

#[derive(Debug, Default)]
pub struct Templates {
    /// `None` until the first `template-list`; the Reload chip clears it.
    pub items: Option<Vec<Item>>,
    pub dir: String,
    pub selected: Option<Detail>,
    pub error: Option<String>,
    pub draft: Option<Draft>,
    /// The Delete chip was clicked once; the next click deletes the selected template.
    pub confirm_delete: bool,
    /// Keyboard focus for the editor form; created on first render.
    pub focus: Option<FocusHandle>,
}

impl Global for Templates {}

fn s(v: &Value, k: &str) -> String {
    v.get(k)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn headings(v: &Value) -> Vec<String> {
    v["headings"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_string)
        .collect()
}

impl Templates {
    /// Re-read the list; keeps the selection when the template still exists.
    pub fn load(&mut self, w: &MainWindow) {
        match model::dispatch(&w.engine, "template-list", json!({})) {
            Ok(v) => {
                self.dir = s(&v, "dir");
                let items: Vec<Item> = v["templates"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|t| Item {
                        name: s(t, "name"),
                        icon: s(t, "icon"),
                        headings: headings(t),
                    })
                    .collect();
                self.error = None;
                let keep = self.selected.as_ref().map(|d| d.name.clone());
                self.items = Some(items);
                self.selected = None;
                if let Some(n) = keep {
                    self.select(w, &n);
                }
            }
            Err(e) => {
                self.items = Some(vec![]);
                self.selected = None;
                self.error = Some(e.to_string());
            }
        }
    }

    pub fn select(&mut self, w: &MainWindow, name: &str) {
        self.confirm_delete = false;
        match model::dispatch(&w.engine, "template-get", json!({ "name": name })) {
            Ok(t) => {
                self.error = None;
                self.selected = Some(Detail {
                    name: s(&t, "name"),
                    icon: s(&t, "icon"),
                    system_prompt: s(&t, "system_prompt"),
                    headings: headings(&t),
                    openoats_id: s(&t, "openoats_id"),
                });
            }
            Err(e) => {
                self.selected = None;
                self.error = Some(e.to_string());
            }
        }
    }

    /// Open an empty form for a new template.
    pub fn create(&mut self) {
        self.confirm_delete = false;
        self.error = None;
        self.draft = Some(Draft::default());
    }

    /// Open the form on the selected template.
    pub fn edit(&mut self) {
        let Some(d) = &self.selected else { return };
        self.confirm_delete = false;
        self.error = None;
        self.draft = Some(Draft {
            previous: Some(d.name.clone()),
            name: d.name.clone(),
            icon: d.icon.clone(),
            prompt: d.system_prompt.clone(),
            ..Draft::default()
        });
    }

    pub fn cancel(&mut self) {
        self.draft = None;
        self.error = None;
    }

    /// Write the form through `template-save`. A rejection keeps the form open with the engine's
    /// message; on success the list reloads with the saved template selected.
    pub fn save(&mut self, w: &MainWindow) {
        let Some(d) = self.draft.clone() else { return };
        let mut params = json!({"name": d.name, "system_prompt": d.prompt, "icon": d.icon});
        match &d.previous {
            Some(p) => params["previous"] = json!(p),
            None => params["overwrite"] = json!(d.overwrite),
        }
        match model::dispatch(&w.engine, "template-save", params) {
            Ok(v) => {
                self.draft = None;
                self.selected = None;
                self.load(w);
                self.select(w, &s(&v, "name"));
            }
            Err(e) => self.error = Some(e.to_string()),
        }
    }

    /// First call arms the confirmation; the second deletes the selected template.
    pub fn delete(&mut self, w: &MainWindow) {
        let Some(name) = self.selected.as_ref().map(|d| d.name.clone()) else {
            return;
        };
        if !self.confirm_delete {
            self.confirm_delete = true;
            return;
        }
        self.confirm_delete = false;
        match model::dispatch(&w.engine, "template-delete", json!({ "name": name })) {
            Ok(_) => {
                self.selected = None;
                self.load(w);
            }
            Err(e) => self.error = Some(e.to_string()),
        }
    }

    /// Feed one key press to the open form; false when it is not the form's key (or no form).
    /// `command` is true when ctrl/alt/cmd is held.
    pub fn key(
        &mut self,
        w: &MainWindow,
        key: &str,
        key_char: Option<&str>,
        command: bool,
    ) -> bool {
        let Some(d) = self.draft.as_mut() else {
            return false;
        };
        match key {
            "escape" => self.cancel(),
            "enter" | "s" if command => self.save(w),
            "tab" => d.field = d.field.next(),
            "enter" if d.field == Field::Prompt => d.prompt.push('\n'),
            "enter" => d.field = d.field.next(),
            "backspace" => {
                d.buf().pop();
            }
            _ => match key_char {
                Some(c) if !command && !c.is_empty() && !c.chars().any(char::is_control) => {
                    d.buf().push_str(c)
                }
                _ => return false,
            },
        }
        true
    }
}

fn chip(id: &'static str, label: impl Into<SharedString>) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .px_2()
        .rounded_md()
        .cursor_pointer()
        .bg(rgb(PANEL))
        .text_sm()
        .child(label.into())
}

fn update(cx: &mut App, w: &MainWindow, f: impl FnOnce(&mut Templates, &MainWindow)) {
    f(cx.default_global::<Templates>(), w);
}

pub fn render(w: &MainWindow, cx: &mut Context<MainWindow>) -> AnyElement {
    if cx
        .try_global::<Templates>()
        .and_then(|t| t.items.as_ref())
        .is_none()
    {
        update(cx, w, |t, w| t.load(w));
    }
    if cx.global::<Templates>().focus.is_none() {
        let h = cx.focus_handle();
        cx.global_mut::<Templates>().focus = Some(h);
    }
    let t = cx.global::<Templates>();
    let items = t.items.clone().unwrap_or_default();
    let (dir, selected, error) = (t.dir.clone(), t.selected.clone(), t.error.clone());
    let (draft, confirm) = (t.draft.clone(), t.confirm_delete);
    let focus = t.focus.clone().expect("set above");
    let sel_name = selected.as_ref().map(|d| d.name.clone());

    let header = div()
        .flex()
        .gap_3()
        .items_center()
        .flex_none()
        .child(div().text_sm().text_color(rgb(DIM)).child(format!(
            "{} templates in {dir} (templates.dir)",
            items.len()
        )))
        .child(
            chip("templates-reload", "Reload").on_click(cx.listener(|this, _, _, cx| {
                update(cx, this, |t, w| t.load(w));
                cx.notify();
            })),
        )
        .child({
            let focus = focus.clone();
            chip("templates-new", "New").on_click(cx.listener(move |this, _, window, cx| {
                update(cx, this, |t, _| t.create());
                focus.focus(window, cx);
                cx.notify();
            }))
        });

    let list = div()
        .id("templates")
        .flex()
        .flex_col()
        .gap_1()
        .w(px(280.))
        .flex_none()
        .overflow_y_scroll()
        .children(items.iter().map(|it| {
            let name = it.name.clone();
            let sel = sel_name.as_deref() == Some(it.name.as_str());
            div()
                .id(SharedString::from(format!("template-{}", it.name)))
                .px_2()
                .py_1()
                .rounded_md()
                .cursor_pointer()
                .bg(rgb(if sel { ACCENT } else { PANEL }))
                .text_color(rgb(if sel { BG } else { FG }))
                .child(format!("{}  ({} headings)", it.name, it.headings.len()))
                .on_click(cx.listener(move |this, _, _, cx| {
                    update(cx, this, |t, w| t.select(w, &name));
                    cx.notify();
                }))
        }));

    let detail = div()
        .id("template-detail")
        .flex()
        .flex_col()
        .gap_2()
        .flex_1()
        .min_w_0()
        .overflow_y_scroll();
    let detail = match (draft, selected) {
        (Some(d), _) => detail.child(editor(d, focus, cx)),
        (None, None) if items.is_empty() => detail.child(div().text_color(rgb(DIM)).child(
            "No templates. Add TOML files to the folder above, or import an OpenOats \
             templates.json with `ctl template-import`.",
        )),
        (None, None) => detail.child(div().text_color(rgb(DIM)).child("Select a template.")),
        (None, Some(d)) => {
            let mut meta = vec![];
            if !d.icon.is_empty() {
                meta.push(format!("icon: {}", d.icon));
            }
            if !d.openoats_id.is_empty() {
                meta.push(format!("imported from OpenOats ({})", d.openoats_id));
            }
            let focus = focus.clone();
            let actions = div()
                .flex()
                .gap_2()
                .child(chip("template-edit", "Edit").on_click(cx.listener(
                    move |this, _, window, cx| {
                        update(cx, this, |t, _| t.edit());
                        focus.focus(window, cx);
                        cx.notify();
                    },
                )))
                .child(
                    chip(
                        "template-delete",
                        if confirm { "Really delete?" } else { "Delete" },
                    )
                    .when(confirm, |c| c.text_color(rgb(ERR)))
                    .on_click(cx.listener(|this, _, _, cx| {
                        update(cx, this, |t, w| t.delete(w));
                        cx.notify();
                    })),
                )
                .children(confirm.then(|| {
                    chip("template-keep", "Keep").on_click(cx.listener(|_, _, _, cx| {
                        cx.global_mut::<Templates>().confirm_delete = false;
                        cx.notify();
                    }))
                }));
            detail
                .child(div().text_color(rgb(ACCENT)).child(d.name.clone()))
                .child(actions)
                .children((!meta.is_empty()).then(|| {
                    div()
                        .text_sm()
                        .text_color(rgb(DIM))
                        .child(meta.join("  ·  "))
                }))
                .child(
                    div()
                        .text_sm()
                        .text_color(rgb(DIM))
                        .child(if d.headings.is_empty() {
                            "Required headings: none".to_string()
                        } else {
                            format!("Required headings: {}", d.headings.join(", "))
                        }),
                )
                .child(
                    div()
                        .p_2()
                        .rounded_md()
                        .bg(rgb(PANEL))
                        .whitespace_normal()
                        .children(
                            d.system_prompt
                                .lines()
                                .map(|l| div().min_h(px(16.)).child(l.to_string()))
                                .collect::<Vec<_>>(),
                        ),
                )
        }
    };

    div()
        .flex()
        .flex_col()
        .gap_2()
        .size_full()
        .child(header)
        .children(error.map(|e| div().text_sm().text_color(rgb(ERR)).child(e)))
        .child(
            div()
                .flex()
                .gap_4()
                .flex_1()
                .min_h_0()
                .child(list)
                .child(detail),
        )
        .into_any_element()
}

/// The create / edit form: one box per `template-save` parameter, the active one marked with a
/// caret. Keys go to `Templates::key` while the form has focus.
fn editor(d: Draft, focus: FocusHandle, cx: &mut Context<MainWindow>) -> impl IntoElement {
    let field = |id: &'static str,
                 label: &'static str,
                 f: Field,
                 text: &str,
                 cx: &mut Context<MainWindow>| {
        let active = d.field == f;
        let mut lines: Vec<String> = text.split('\n').map(str::to_string).collect();
        if active {
            lines.last_mut().expect("split yields one").push('▏');
        }
        let focus = focus.clone();
        div()
            .flex()
            .flex_col()
            .gap_1()
            .child(div().text_sm().text_color(rgb(DIM)).child(label))
            .child(
                div()
                    .id(id)
                    .p_2()
                    .rounded_md()
                    .cursor_text()
                    .bg(rgb(PANEL))
                    .border_1()
                    .border_color(rgb(if active { ACCENT } else { PANEL }))
                    .whitespace_normal()
                    .children(lines.into_iter().map(|l| div().min_h(px(16.)).child(l)))
                    .on_click(cx.listener(move |_, _, window, cx| {
                        if let Some(d) = cx.global_mut::<Templates>().draft.as_mut() {
                            d.field = f;
                        }
                        focus.focus(window, cx);
                        cx.notify();
                    })),
            )
    };
    let title = match &d.previous {
        Some(p) => format!("Edit {p}"),
        None => "New template".to_string(),
    };
    // Same rule as the engine, for a live preview while typing.
    let headings = oats_core::templates::required_headings(&d.prompt);
    let create = d.previous.is_none();
    let overwrite = d.overwrite;
    div()
        .id("template-editor")
        .track_focus(&focus)
        .flex()
        .flex_col()
        .gap_2()
        .on_key_down(cx.listener(|this, ev: &KeyDownEvent, _, cx| {
            let k = &ev.keystroke;
            let command = k.modifiers.control || k.modifiers.alt || k.modifiers.platform;
            let mut handled = false;
            update(cx, this, |t, w| {
                handled = t.key(w, &k.key, k.key_char.as_deref(), command)
            });
            if handled {
                cx.stop_propagation();
                cx.notify();
            }
        }))
        .child(div().text_color(rgb(ACCENT)).child(title))
        .child(field("template-name", "Name", Field::Name, &d.name, cx))
        .child(field("template-icon", "Icon (optional)", Field::Icon, &d.icon, cx))
        .child(field(
            "template-prompt",
            "System prompt (lines starting with \"## \" are required headings)",
            Field::Prompt,
            &d.prompt,
            cx,
        ))
        .child(div().text_sm().text_color(rgb(DIM)).child(if headings.is_empty() {
            "Required headings: none".to_string()
        } else {
            format!("Required headings: {}", headings.join(", "))
        }))
        .child(
            div()
                .flex()
                .gap_2()
                .child(chip("template-save", "Save").on_click(cx.listener(|this, _, _, cx| {
                    update(cx, this, |t, w| t.save(w));
                    cx.notify();
                })))
                .child(chip("template-cancel", "Cancel").on_click(cx.listener(|_, _, _, cx| {
                    cx.global_mut::<Templates>().cancel();
                    cx.notify();
                })))
                .children(create.then(|| {
                    chip(
                        "template-overwrite",
                        if overwrite { "Overwrite existing: on" } else { "Overwrite existing: off" },
                    )
                    .on_click(cx.listener(|_, _, _, cx| {
                        if let Some(d) = cx.global_mut::<Templates>().draft.as_mut() {
                            d.overwrite = !d.overwrite;
                        }
                        cx.notify();
                    }))
                })),
        )
        .child(div().text_sm().text_color(rgb(DIM)).child(
            "Tab: next field · Enter: newline in prompt · Ctrl/Cmd+Enter or Ctrl/Cmd+S: save · Esc: cancel",
        ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::Tab;
    use crate::testutil;
    use gpui::TestAppContext;

    fn write(dir: &std::path::Path, file: &str, body: &str) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join(file), body).unwrap();
    }

    #[test]
    fn templates_tab_lists_selects_and_reloads() {
        let (engine, d) = testutil::engine(&[]);
        let tdir = d.path().join("templates");
        write(
            &tdir,
            "stand-up.toml",
            "schema_version = 1\nname = \"Stand-Up\"\nicon = \"person\"\nsystem_prompt = \"Notes.\\n## Yesterday\\n## Today\"\n",
        );
        let mut cx = TestAppContext::single();
        let (view, cx) = cx.add_window_view(|_, _| MainWindow::new(engine.clone()));
        view.update(cx, |v, cx| {
            v.state.tab = Tab::Templates;
            cx.notify();
        });
        cx.run_until_parked();
        view.update(cx, |v, cx| {
            let t = cx.global::<Templates>();
            assert_eq!(t.error, None);
            let items = t.items.as_ref().unwrap();
            assert_eq!(items.len(), 1);
            assert_eq!(items[0].name, "Stand-Up");
            assert_eq!(items[0].headings, ["## Yesterday", "## Today"]);
            assert!(t.dir.ends_with("templates"));
            update(cx, v, |t, w| t.select(w, "stand-up"));
            cx.notify();
        });
        cx.run_until_parked();
        // A new file appears on Reload; the selection survives it.
        write(
            &tdir,
            "one.toml",
            "schema_version = 1\nname = \"1:1\"\nsystem_prompt = \"## Points\"\n",
        );
        view.update(cx, |v, cx| {
            let d = cx.global::<Templates>().selected.clone().unwrap();
            assert_eq!(d.name, "Stand-Up");
            assert_eq!(d.icon, "person");
            assert!(d.system_prompt.contains("## Today"));
            update(cx, v, |t, w| t.load(w));
            let t = cx.global::<Templates>();
            let names: Vec<_> = t
                .items
                .as_ref()
                .unwrap()
                .iter()
                .map(|i| i.name.as_str())
                .collect();
            assert_eq!(names, ["1:1", "Stand-Up"]);
            assert_eq!(
                t.selected.as_ref().map(|d| d.name.as_str()),
                Some("Stand-Up")
            );
            update(cx, v, |t, w| t.select(w, "absent"));
            assert!(cx.global::<Templates>().error.is_some());
            cx.notify();
        });
        cx.run_until_parked();
    }

    #[test]
    fn templates_tab_creates_edits_and_deletes_by_keyboard_and_clicks() {
        let (engine, d) = testutil::engine(&[]);
        let tdir = d.path().join("templates");
        let mut cx = TestAppContext::single();
        let (view, cx) = cx.add_window_view(|_, _| MainWindow::new(engine.clone()));
        view.update(cx, |v, cx| {
            v.state.tab = Tab::Templates;
            cx.notify();
        });
        cx.run_until_parked();
        // New, then type into the focused form: name, Tab past the icon, a two-line prompt.
        view.update_in(cx, |_, window, cx| {
            cx.global_mut::<Templates>().create();
            let f = cx.global::<Templates>().focus.clone().unwrap();
            f.focus(window, cx);
            cx.notify();
        });
        cx.run_until_parked();
        cx.simulate_input("Retro");
        cx.simulate_keystrokes("enter tab");
        cx.simulate_input("##");
        cx.simulate_keystrokes("space");
        cx.simulate_input("Wins");
        cx.simulate_keystrokes("enter");
        cx.simulate_input("##");
        cx.simulate_keystrokes("space");
        cx.simulate_input("Fixes");
        view.update(cx, |_, cx| {
            let d = cx.global::<Templates>().draft.clone().unwrap();
            assert_eq!((d.name.as_str(), d.icon.as_str()), ("Retro", ""));
            assert_eq!(d.prompt, "## Wins\n## Fixes");
            assert_eq!(d.field, Field::Prompt);
        });
        cx.simulate_keystrokes("ctrl-s");
        view.update(cx, |_, cx| {
            let t = cx.global::<Templates>();
            assert_eq!(t.error, None);
            assert!(t.draft.is_none());
            let sel = t.selected.clone().unwrap();
            assert_eq!(sel.name, "Retro");
            assert_eq!(sel.headings, ["## Wins", "## Fixes"]);
        });
        assert!(tdir.join("retro.toml").is_file());

        // A second create with the same name is refused inline; the form stays open.
        // With overwrite on it replaces the template.
        view.update(cx, |v, cx| {
            update(cx, v, |t, w| {
                t.create();
                let d = t.draft.as_mut().unwrap();
                d.name = "retro".into();
                d.prompt = "## Only".into();
                t.save(w);
                assert!(t.error.as_deref().unwrap().contains("already exists"));
                assert!(t.draft.is_some());
                t.draft.as_mut().unwrap().overwrite = true;
                t.save(w);
                assert_eq!(t.error, None);
                assert_eq!(t.selected.as_ref().unwrap().headings, ["## Only"]);
            });
            cx.notify();
        });
        cx.run_until_parked();

        // Edit: rename and set an icon; Esc on a later edit discards it.
        view.update(cx, |v, cx| {
            update(cx, v, |t, w| {
                t.edit();
                let d = t.draft.as_mut().unwrap();
                assert_eq!(d.previous.as_deref(), Some("retro"));
                d.name = "Retrospective".into();
                d.icon = "star".into();
                t.save(w);
                assert_eq!(t.error, None);
                let names: Vec<_> = t
                    .items
                    .as_ref()
                    .unwrap()
                    .iter()
                    .map(|i| i.name.clone())
                    .collect();
                assert_eq!(names, ["Retrospective"]);
                assert_eq!(t.selected.as_ref().unwrap().icon, "star");
                t.edit();
                assert!(t.key(w, "x", Some("x"), false));
                assert!(t.key(w, "escape", None, false));
                assert!(t.draft.is_none());
                assert!(!t.key(w, "x", Some("x"), false), "no form, key not taken");
            });
            cx.notify();
        });
        cx.run_until_parked();
        assert!(tdir.join("retrospective.toml").is_file() && !tdir.join("retro.toml").exists());

        // Delete takes two clicks; selecting another template disarms it.
        view.update(cx, |v, cx| {
            update(cx, v, |t, w| {
                t.delete(w);
                assert!(t.confirm_delete);
                assert_eq!(t.items.as_ref().unwrap().len(), 1);
                t.delete(w);
                assert_eq!(t.error, None);
                assert!(t.selected.is_none());
                assert_eq!(t.items.as_deref(), Some(&[][..]));
            });
            cx.notify();
        });
        cx.run_until_parked();
        assert!(!tdir.join("retrospective.toml").exists());
    }

    #[test]
    fn templates_tab_renders_empty_and_broken_folders() {
        // Empty folder.
        let (engine, d) = testutil::engine(&[]);
        let mut cx = TestAppContext::single();
        let (view, cx) = cx.add_window_view(|_, _| MainWindow::new(engine.clone()));
        view.update(cx, |v, cx| {
            v.state.tab = Tab::Templates;
            cx.notify();
        });
        cx.run_until_parked();
        view.update(cx, |_, cx| {
            let t = cx.global::<Templates>();
            assert_eq!(t.items.as_deref(), Some(&[][..]));
            assert_eq!(t.error, None);
        });
        // A malformed file is an error line, not a crash.
        write(&d.path().join("templates"), "bad.toml", "name = \"x\"\n");
        view.update(cx, |v, cx| {
            update(cx, v, |t, w| t.load(w));
            assert!(cx.global::<Templates>().error.is_some());
            cx.notify();
        });
        cx.run_until_parked();
    }
}
