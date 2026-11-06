//! The Calendar sources panel under the Settings tab's Calendar section (dot-4uzgol.88): every
//! `calendar.sources` entry with its on/off toggle and, for an eventkit (macOS Calendar) source,
//! its accounts and calendars as checkboxes. A click runs the same action as `ctl` (`calendar-select`,
//! `calendar-source-enable`, `calendar-source-add`, `calendar-access`) off the UI thread, since an
//! eventkit source being turned on may wait on the one macOS Calendars prompt; the panel then
//! re-reads `calendar-sources-list`. Nothing here asks for access unless one of those is clicked.
//! The panel re-reads at most once per `ics.poll_interval`, plus after each action.

use std::time::Instant;

use gpui::{div, prelude::*, px, rgb, AnyElement, App, Context, FontWeight, Global, SharedString};
use serde_json::{json, Value};

use crate::kit::{self, Kind, Size};
use crate::model;
use crate::theme;
use crate::view::MainWindow;

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

    /// What `ui-state` shows for the panel.
    pub(crate) fn dump(&self) -> Value {
        json!({"sources": self.list["sources"], "eventkit": self.list["eventkit"],
               "error": self.error, "busy": self.busy})
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
    let label = if kind == "eventkit" {
        "macOS Calendar"
    } else {
        "ICS feeds"
    };
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
    for acct in src["accounts"].as_array().into_iter().flatten() {
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

/// The panel: one row per source, and "Use macOS Calendar" where EventKit exists but no eventkit
/// source does.
pub(crate) fn panel(w: &MainWindow, cx: &mut Context<MainWindow>) -> AnyElement {
    let t = theme::current();
    let s = cx.default_global::<Sources>();
    if s.stale(w) && s.busy.is_none() {
        s.load(w);
    }
    let (rows, error, busy) = (s.rows(), s.error.clone(), s.busy.clone());
    let supported = s.list["eventkit"]["supported"] == true;
    let has_ek = rows.iter().any(|r| r["kind"] == "eventkit");
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
                .when(supported && !has_ek && busy.is_none(), |d| {
                    d.child(
                        kit::button(
                            "calsrc-use-mac",
                            "Use macOS Calendar",
                            Kind::Secondary,
                            Size::Sm,
                            true,
                        )
                        .on_click(cx.listener(|w, _, _, cx| {
                            act(
                                w,
                                "calendar-source-add",
                                json!({"name": "mac", "kind": "eventkit"}),
                                cx,
                            );
                        })),
                    )
                }),
        );
    if let Some(e) = error {
        out = out.child(kit::banner(true, "Calendar sources failed.", e));
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
            assert_eq!(d["view"]["calendar_sources"]["sources"][0]["name"], "ics");
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
            let mac = &d["view"]["calendar_sources"]["sources"][1];
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
            let home = s.list["sources"][1]["accounts"]
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
}
