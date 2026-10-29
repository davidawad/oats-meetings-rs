//! Trash tab: sessions in the OS trash (`trash-list`), restore one (`trash-restore`), delete one or
//! all for good (`trash-empty`). Permanent deletes ask twice while `sessions.confirm_delete` is on,
//! the same policy `ctl trash-empty --confirm` enforces.

use gpui::{div, prelude::*, rgb, AnyElement, Context, SharedString};
use oats_core::control::engine::Engine;
use serde_json::{json, Value};

use crate::model;
use crate::view::{MainWindow, ACCENT, BG, DIM, ERR, FG, PANEL, WARN};

#[derive(Debug, Clone, PartialEq)]
pub struct Item {
    pub id: String,
    /// The session's title when it had one (`trash-list`).
    pub title: String,
    pub deleted_at: String,
}

/// What a permanent delete would remove: one session, or the whole trash.
#[derive(Debug, Clone, PartialEq)]
pub enum Purge {
    One(String),
    All,
}

#[derive(Debug, Default)]
pub struct Trash {
    pub items: Vec<Item>,
    /// The last `trash-list` or action error (e.g. `unsupported` on macOS).
    pub error: Option<String>,
    /// A permanent delete waiting for its second click.
    pub confirming: Option<Purge>,
}

impl Trash {
    pub fn load(&mut self, engine: &Engine) {
        match model::dispatch(engine, "trash-list", json!({})) {
            Ok(v) => {
                self.items = v["trash"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|t| Item {
                        id: str_of(t, "id"),
                        title: str_of(t, "title"),
                        deleted_at: str_of(t, "deleted_at"),
                    })
                    .collect();
                self.error = None;
            }
            Err(e) => {
                self.items.clear();
                self.error = Some(e.to_string());
            }
        }
    }

    fn after(&mut self, engine: &Engine, res: oats_core::config::Result<Value>) {
        let err = res.err().map(|e| e.to_string());
        self.confirming = None;
        self.load(engine);
        if err.is_some() {
            self.error = err;
        }
    }

    pub fn restore(&mut self, engine: &Engine, id: &str) {
        let res = model::dispatch(engine, "trash-restore", json!({"session": id}));
        self.after(engine, res);
    }

    /// First press arms the confirmation while `sessions.confirm_delete` is on; the second (or the
    /// only one, when it is off) deletes for good.
    pub fn purge(&mut self, engine: &Engine, what: Purge) {
        let ask = engine.snapshot().config.sessions.confirm_delete;
        if ask && self.confirming.as_ref() != Some(&what) {
            self.confirming = Some(what);
            return;
        }
        let params = match &what {
            Purge::One(id) => json!({"session": id, "confirm": true}),
            Purge::All => json!({"confirm": true}),
        };
        let res = model::dispatch(engine, "trash-empty", params);
        self.after(engine, res);
    }
}

fn str_of(v: &Value, k: &str) -> String {
    v[k].as_str().unwrap_or_default().to_string()
}

fn button(id: SharedString, label: String, colour: u32) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .px_3()
        .py_1()
        .rounded_md()
        .cursor_pointer()
        .bg(rgb(colour))
        .text_color(rgb(BG))
        .child(label)
}

pub fn render(w: &MainWindow, cx: &mut Context<MainWindow>) -> AnyElement {
    let t = &w.trash;
    let all_armed = t.confirming == Some(Purge::All);
    let mut bar = div().flex().flex_none().gap_2().items_center().child(
        button("trash-refresh".into(), "Refresh".into(), PANEL)
            .text_color(rgb(FG))
            .on_click(cx.listener(|this, _, _, cx| {
                this.trash.confirming = None;
                this.trash.load(&this.engine);
                cx.notify();
            })),
    );
    if !t.items.is_empty() {
        let label = if all_armed {
            format!("Confirm: delete {} for good", t.items.len())
        } else {
            "Empty trash".to_string()
        };
        bar = bar.child(
            button(
                "trash-empty".into(),
                label,
                if all_armed { ERR } else { WARN },
            )
            .on_click(cx.listener(|this, _, _, cx| {
                this.trash.purge(&this.engine, Purge::All);
                this.state.refresh(&this.engine);
                cx.notify();
            })),
        );
    }
    if t.confirming.is_some() {
        bar = bar.child(
            button("trash-cancel".into(), "Cancel".into(), PANEL)
                .text_color(rgb(FG))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.trash.confirming = None;
                    cx.notify();
                })),
        );
    }
    bar = bar.child(div().text_sm().text_color(rgb(DIM)).child(format!(
        "{} trashed session{}",
        t.items.len(),
        if t.items.len() == 1 { "" } else { "s" }
    )));

    let rows = t.items.iter().map(|it| {
        let (rid, pid) = (it.id.clone(), it.id.clone());
        let armed = t.confirming == Some(Purge::One(it.id.clone()));
        div()
            .id(SharedString::from(format!("trash-{}", it.id)))
            .flex()
            .gap_3()
            .items_center()
            .px_2()
            .py_1()
            .rounded_md()
            .bg(rgb(PANEL))
            .child(
                div()
                    .debug_selector(|| format!("trash-title-{}", it.id))
                    .flex_1()
                    .min_w_0()
                    .child(if it.title.is_empty() {
                        it.id.clone()
                    } else {
                        it.title.clone()
                    }),
            )
            .child(
                div()
                    .debug_selector(|| format!("trash-id-{}", it.id))
                    .text_sm()
                    .text_color(rgb(DIM))
                    .child(if it.title.is_empty() {
                        format!("deleted {}", it.deleted_at)
                    } else {
                        format!("{}  deleted {}", it.id, it.deleted_at)
                    }),
            )
            .child(
                button(
                    format!("trash-restore-{}", it.id).into(),
                    "Restore".into(),
                    ACCENT,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.trash.restore(&this.engine, &rid);
                    this.state.refresh(&this.engine);
                    cx.notify();
                })),
            )
            .child(
                button(
                    format!("trash-purge-{}", it.id).into(),
                    (if armed {
                        "Confirm delete"
                    } else {
                        "Delete forever"
                    })
                    .into(),
                    if armed { ERR } else { WARN },
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.trash.purge(&this.engine, Purge::One(pid.clone()));
                    cx.notify();
                })),
            )
    });

    let mut col = div()
        .id("trash")
        .flex()
        .flex_col()
        .gap_2()
        .size_full()
        .child(bar);
    if let Some(e) = &t.error {
        col = col.child(div().text_color(rgb(ERR)).child(e.clone()));
    }
    let list = div()
        .id("trash-list")
        .flex()
        .flex_col()
        .gap_1()
        .flex_1()
        .min_h_0()
        .overflow_y_scroll();
    let list = if t.items.is_empty() && t.error.is_none() {
        list.child(div().text_color(rgb(DIM)).child(
            "Trash is empty. Sessions deleted with sessions.delete_mode = trash appear here.",
        ))
    } else {
        list.children(rows)
    };
    col.child(list).into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::Tab;
    use crate::testutil;
    use gpui::TestAppContext;

    fn seed(dir: &std::path::Path, id: &str) {
        let s = dir.join("sessions").join(id);
        std::fs::create_dir_all(&s).unwrap();
        std::fs::write(
            s.join("session.json"),
            format!(r#"{{"id":"{id}","title":"T","startedAt":"2026-10-27T10:00:00Z"}}"#),
        )
        .unwrap();
    }

    /// One test: it points `XDG_DATA_HOME` (the Freedesktop trash) at a temp dir for the process,
    /// and no other ui test touches the trash.
    #[cfg(target_os = "linux")]
    #[test]
    fn trash_view_lists_restores_and_empties_through_the_engine() {
        let (engine, dir) = testutil::engine(&[
            ("sessions.delete_mode", json!("trash")),
            ("sessions.confirm_delete", json!(true)),
        ]);
        std::env::set_var("XDG_DATA_HOME", dir.path().join("xdg"));
        for id in ["s1", "s2", "s3"] {
            seed(dir.path(), id);
            model::dispatch(
                &engine,
                "session-delete",
                json!({"session": id, "confirm": true}),
            )
            .unwrap();
        }
        let mut cx = TestAppContext::single();
        let (view, cx) = cx.add_window_view(|_, _| MainWindow::new(engine.clone()));
        view.update(cx, |v, cx| {
            v.state.tab = Tab::Trash;
            v.refresh(cx);
        });
        cx.run_until_parked();
        view.read_with(cx, |v, _| {
            let mut ids: Vec<_> = v.trash.items.iter().map(|i| i.id.as_str()).collect();
            ids.sort();
            assert_eq!(ids, ["s1", "s2", "s3"], "{:?}", v.trash.error);
            assert!(v.state.sessions.is_empty());
        });
        // Rows lead with the session's title; the id is the secondary text.
        view.read_with(cx, |v, _| {
            assert!(
                v.trash.items.iter().all(|i| i.title == "T"),
                "{:?}",
                v.trash.items
            )
        });
        assert!(cx.debug_bounds("trash-title-s1").is_some());
        assert!(cx.debug_bounds("trash-id-s1").is_some());

        // Restore: back in the store and the Sessions list, out of the trash.
        view.update(cx, |v, cx| {
            v.trash.restore(&v.engine, "s1");
            v.refresh(cx);
        });
        cx.run_until_parked();
        view.read_with(cx, |v, _| {
            assert_eq!(v.trash.items.len(), 2);
            assert_eq!(v.state.sessions.len(), 1);
            assert_eq!(v.state.sessions[0].id, "s1");
        });

        // confirm_delete on: the first press only arms; the second deletes s2 for good.
        view.update(cx, |v, cx| {
            v.trash.purge(&v.engine, Purge::One("s2".into()));
            cx.notify();
        });
        cx.run_until_parked();
        view.read_with(cx, |v, _| {
            assert_eq!(v.trash.confirming, Some(Purge::One("s2".into())));
            assert_eq!(v.trash.items.len(), 2);
        });
        view.update(cx, |v, cx| {
            v.trash.purge(&v.engine, Purge::One("s2".into()));
            cx.notify();
        });
        cx.run_until_parked();
        view.read_with(cx, |v, _| {
            assert_eq!(v.trash.confirming, None);
            assert_eq!(v.trash.items.len(), 1);
            assert_eq!(v.trash.items[0].id, "s3");
        });

        // confirm_delete off: Empty trash acts on the first press.
        model::dispatch(
            &engine,
            "config-set",
            json!({"key": "sessions.confirm_delete", "value": false}),
        )
        .unwrap();
        view.update(cx, |v, cx| {
            v.trash.purge(&v.engine, Purge::All);
            cx.notify();
        });
        cx.run_until_parked();
        view.read_with(cx, |v, _| {
            assert!(v.trash.items.is_empty());
            assert_eq!(v.trash.error, None);
        });

        // Restoring something that is gone shows the engine's error instead of failing silently.
        view.update(cx, |v, cx| {
            v.trash.restore(&v.engine, "s2");
            cx.notify();
        });
        cx.run_until_parked();
        view.read_with(cx, |v, _| {
            assert!(v.trash.error.as_deref().unwrap_or("").contains("s2"))
        });
    }
}
