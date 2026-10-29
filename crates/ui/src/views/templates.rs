//! Templates tab: the note templates in `templates.dir` (`template-list`) and the selected one's
//! prompt and required headings (`template-get`). Templates are TOML files; the engine has no
//! create / edit / delete verbs yet, so the view shows the folder to edit them in.
//!
//! The view's own state lives in a gpui global so the tab stays in this one file.

use gpui::{div, prelude::*, px, rgb, AnyElement, App, Context, Global, SharedString};
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

#[derive(Debug, Default)]
pub struct Templates {
    /// `None` until the first `template-list`; the Reload chip clears it.
    pub items: Option<Vec<Item>>,
    pub dir: String,
    pub selected: Option<Detail>,
    pub error: Option<String>,
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
}

fn chip(id: &'static str, label: &'static str) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .px_2()
        .rounded_md()
        .cursor_pointer()
        .bg(rgb(PANEL))
        .text_sm()
        .child(label)
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
    let t = cx.global::<Templates>();
    let items = t.items.clone().unwrap_or_default();
    let (dir, selected, error) = (t.dir.clone(), t.selected.clone(), t.error.clone());
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
        );

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
    let detail = match selected {
        None if items.is_empty() => detail.child(div().text_color(rgb(DIM)).child(
            "No templates. Add TOML files to the folder above, or import an OpenOats \
             templates.json with `ctl template-import`.",
        )),
        None => detail.child(div().text_color(rgb(DIM)).child("Select a template.")),
        Some(d) => {
            let mut meta = vec![];
            if !d.icon.is_empty() {
                meta.push(format!("icon: {}", d.icon));
            }
            if !d.openoats_id.is_empty() {
                meta.push(format!("imported from OpenOats ({})", d.openoats_id));
            }
            detail
                .child(div().text_color(rgb(ACCENT)).child(d.name.clone()))
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
