//! Calendar tab: ICS feed subscriptions (add / remove / refresh) and the upcoming meetings from
//! their caches, over the same actions as `ctl ics-*`. Every choice is an `ics.*` config key: the
//! upcoming window is `ics.window_days` (`ics-events` with no bounds), and the view re-reads the
//! feeds and events at most once per `ics.poll_interval` (the scheduler's own cadence), plus after
//! each action. The view's own state is a gpui global so it stays inside this file.

use std::time::Instant;

use gpui::{
    div, prelude::*, px, rgb, AnyElement, App, Context, FocusHandle, Global, KeyDownEvent,
    SharedString,
};
use serde_json::{json, Value};

use crate::model;
use crate::view::{MainWindow, ACCENT, BG, DIM, ERR, FG, PANEL, WARN};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Field {
    Name,
    Url,
}

/// One subscribed feed, from `ics-list`.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct FeedRow {
    pub name: String,
    pub url: String,
    pub enabled: bool,
    pub fetched_at: Option<String>,
    pub events: Option<u64>,
    pub error: Option<String>,
}

/// One meeting occurrence, from `ics-events`.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Meeting {
    pub feed: String,
    pub summary: String,
    pub location: String,
    /// `YYYY-MM-DD` in the event's zone.
    pub day: String,
    /// `HH:MM` in the event's zone, empty for all-day events.
    pub time: String,
}

#[derive(Default)]
pub(crate) struct Cal {
    pub enabled: bool,
    pub feeds: Vec<FeedRow>,
    pub meetings: Vec<Meeting>,
    pub from: String,
    pub to: String,
    /// Why the feeds or events could not be read.
    pub error: Option<String>,
    pub loaded: Option<Instant>,
    /// Feeds being fetched off the UI thread (`""` is "every enabled feed").
    pub refreshing: Option<String>,
    pub name: String,
    pub url: String,
    pub field: Option<Field>,
    focus: Option<FocusHandle>,
}

impl Global for Cal {}

fn s(v: &Value, k: &str) -> String {
    v.get(k)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn err_text(v: &Value) -> Option<String> {
    match v {
        Value::Null => None,
        Value::String(m) => Some(m.clone()),
        v => Some(
            [s(v, "message"), s(v, "code")]
                .into_iter()
                .find(|x| !x.is_empty())
                .unwrap_or_else(|| v.to_string()),
        ),
    }
}

pub(crate) fn feed_of(v: &Value) -> FeedRow {
    FeedRow {
        name: s(v, "name"),
        url: s(v, "url"),
        enabled: v.get("enabled").and_then(Value::as_bool).unwrap_or(true),
        fetched_at: v
            .get("fetched_at")
            .and_then(Value::as_str)
            .map(str::to_string),
        events: v.get("events").and_then(Value::as_u64),
        error: v.get("error").and_then(err_text),
    }
}

pub(crate) fn meeting_of(v: &Value) -> Meeting {
    let all_day = v.get("all_day").and_then(Value::as_bool).unwrap_or(false);
    let local = s(v, "local_start");
    let (day, time) = if all_day {
        let d = s(v, "start_date");
        (
            if d.is_empty() {
                local.chars().take(10).collect()
            } else {
                d
            },
            String::new(),
        )
    } else {
        let (d, t) = local.split_once('T').unwrap_or((local.as_str(), ""));
        (d.to_string(), t.chars().take(5).collect())
    };
    Meeting {
        feed: s(v, "feed"),
        summary: s(v, "summary"),
        location: s(v, "location"),
        day,
        time,
    }
}

impl Cal {
    /// Re-read `ics-list` and `ics-events` (the upcoming window: now + `ics.window_days`).
    pub(crate) fn load(&mut self, w: &MainWindow) {
        self.loaded = Some(Instant::now());
        self.error = None;
        match model::dispatch(&w.engine, "ics-list", json!({})) {
            Ok(v) => {
                self.enabled = v.get("enabled").and_then(Value::as_bool).unwrap_or(false);
                self.feeds = v
                    .get("feeds")
                    .and_then(Value::as_array)
                    .map(|a| a.iter().map(feed_of).collect())
                    .unwrap_or_default();
            }
            Err(e) => {
                self.feeds.clear();
                self.error = Some(e.to_string());
            }
        }
        match model::dispatch(&w.engine, "ics-events", json!({})) {
            Ok(v) => {
                self.meetings = v
                    .get("events")
                    .and_then(Value::as_array)
                    .map(|a| a.iter().map(meeting_of).collect())
                    .unwrap_or_default();
                self.from = s(&v, "from");
                self.to = s(&v, "to");
                // A feed whose cache fails to parse is reported per feed, not as a failed call.
                if let Some(e) = v
                    .get("errors")
                    .and_then(Value::as_array)
                    .and_then(|a| a.first())
                {
                    self.error = Some(format!(
                        "{}: {}",
                        s(e, "name"),
                        e.get("error").and_then(err_text).unwrap_or_default()
                    ));
                }
            }
            Err(e) => {
                self.meetings.clear();
                self.error.get_or_insert(e.to_string());
            }
        }
    }

    fn stale(&self, w: &MainWindow) -> bool {
        let every = w.engine.snapshot().config.ics.poll_interval.as_duration();
        self.loaded.is_none_or(|t| t.elapsed() >= every)
    }

    fn buf(&mut self, f: Field) -> &mut String {
        match f {
            Field::Name => &mut self.name,
            Field::Url => &mut self.url,
        }
    }
}

/// Subscribe to the feed typed into the form (`ics-add`); the form clears on success.
pub(crate) fn add(w: &mut MainWindow, cx: &mut App) {
    let cal = cx.default_global::<Cal>();
    let (name, url) = (cal.name.trim().to_string(), cal.url.trim().to_string());
    match model::dispatch(&w.engine, "ics-add", json!({"name": name, "url": url})) {
        Ok(_) => {
            w.state.status = format!("Added feed {name}.");
            cal.name.clear();
            cal.url.clear();
            cal.field = None;
        }
        Err(e) => w.state.status = e.to_string(),
    }
    cal.load(w);
}

/// Unsubscribe (`ics-remove`); the engine drops the feed's cache.
pub(crate) fn remove(w: &mut MainWindow, name: &str, cx: &mut App) {
    w.state.status = match model::dispatch(&w.engine, "ics-remove", json!({"name": name})) {
        Ok(_) => format!("Removed feed {name}."),
        Err(e) => e.to_string(),
    };
    cx.default_global::<Cal>().load(w);
}

/// Fetch one feed, or every enabled feed when `name` is empty (`ics-refresh`), off the UI thread.
pub(crate) fn refresh(w: &mut MainWindow, name: String, cx: &mut Context<MainWindow>) {
    let cal = cx.default_global::<Cal>();
    if cal.refreshing.is_some() {
        return;
    }
    cal.refreshing = Some(name.clone());
    cx.notify();
    let engine = w.engine.clone();
    cx.spawn(async move |this, cx| {
        let params = if name.is_empty() {
            json!({})
        } else {
            json!({"name": name})
        };
        let res = cx
            .background_executor()
            .spawn(async move { model::dispatch(&engine, "ics-refresh", params) })
            .await;
        let _ = this.update(cx, |w, cx| {
            w.state.status = match res {
                Ok(v) => refresh_summary(&v),
                Err(e) => e.to_string(),
            };
            let cal = cx.default_global::<Cal>();
            cal.refreshing = None;
            cal.load(w);
            cx.notify();
        });
    })
    .detach();
}

pub(crate) fn refresh_summary(v: &Value) -> String {
    let rows = v.get("feeds").and_then(Value::as_array);
    let total = rows.map_or(0, Vec::len);
    let failed: Vec<String> = rows
        .into_iter()
        .flatten()
        .filter(|r| r.get("ok").and_then(Value::as_bool) == Some(false))
        .map(|r| {
            format!(
                "{}: {}",
                s(r, "name"),
                r.get("error").and_then(err_text).unwrap_or_default()
            )
        })
        .collect();
    if failed.is_empty() {
        format!("Refreshed {total} feed(s).")
    } else {
        format!(
            "Refreshed {} of {total} feed(s); {}",
            total - failed.len(),
            failed.join("; ")
        )
    }
}

/// A key typed while a form field has focus. Returns false when the key is not the form's.
pub(crate) fn key(
    w: &mut MainWindow,
    key: &str,
    key_char: Option<&str>,
    command: bool,
    cx: &mut App,
) -> bool {
    let cal = cx.default_global::<Cal>();
    let Some(f) = cal.field else { return false };
    match key {
        "enter" => {
            add(w, cx);
            return true;
        }
        "escape" => cal.field = None,
        "tab" => {
            cal.field = Some(if f == Field::Name {
                Field::Url
            } else {
                Field::Name
            })
        }
        "backspace" => {
            cal.buf(f).pop();
        }
        _ => match key_char {
            Some(c) if !command && !c.is_empty() && !c.chars().any(char::is_control) => {
                cal.buf(f).push_str(c)
            }
            _ => return false,
        },
    }
    true
}

fn chip(id: impl Into<SharedString>, label: &str, on: bool) -> gpui::Stateful<gpui::Div> {
    let c = div()
        .id(id.into())
        .px_2()
        .rounded_md()
        .text_sm()
        .child(label.to_string());
    if on {
        c.cursor_pointer().bg(rgb(ACCENT)).text_color(rgb(BG))
    } else {
        c.bg(rgb(PANEL)).text_color(rgb(DIM))
    }
}

fn input(
    cal: &Cal,
    f: Field,
    placeholder: &str,
    width: f32,
    cx: &mut Context<MainWindow>,
) -> impl IntoElement {
    let (text, active) = match f {
        Field::Name => (&cal.name, cal.field == Some(f)),
        Field::Url => (&cal.url, cal.field == Some(f)),
    };
    let shown = match (text.is_empty(), active) {
        (_, true) => format!("{text}|"),
        (true, false) => placeholder.to_string(),
        (false, false) => text.clone(),
    };
    let focus = cal.focus.clone();
    div()
        .id(match f {
            Field::Name => "ics-name",
            Field::Url => "ics-url",
        })
        .px_2()
        .w(px(width))
        .rounded_md()
        .cursor_text()
        .bg(rgb(PANEL))
        .border_1()
        .border_color(rgb(if active { ACCENT } else { PANEL }))
        .text_color(rgb(if text.is_empty() && !active { DIM } else { FG }))
        .child(shown)
        .on_click(cx.listener(move |_, _, window, cx| {
            cx.default_global::<Cal>().field = Some(f);
            if let Some(h) = &focus {
                h.focus(window, cx);
            }
            cx.notify();
        }))
}

fn feed_row(f: &FeedRow, busy: bool, cx: &mut Context<MainWindow>) -> impl IntoElement {
    let state = match (&f.error, &f.fetched_at, f.enabled) {
        (_, _, false) => ("disabled".to_string(), DIM),
        (Some(e), _, _) => (e.clone(), ERR),
        (None, Some(t), _) => (
            format!("{} events, fetched {t}", f.events.unwrap_or(0)),
            DIM,
        ),
        (None, None, _) => ("not fetched yet".to_string(), WARN),
    };
    let (refresh_name, remove_name) = (f.name.clone(), f.name.clone());
    let mut refresh_chip = chip(
        format!("ics-refresh-{}", f.name),
        "Refresh",
        !busy && f.enabled,
    );
    if !busy && f.enabled {
        refresh_chip = refresh_chip
            .on_click(cx.listener(move |w, _, _, cx| refresh(w, refresh_name.clone(), cx)));
    }
    div()
        .id(SharedString::from(format!("ics-feed-{}", f.name)))
        .flex()
        .flex_col()
        .flex_none()
        .gap_1()
        .px_2()
        .py_1()
        .rounded_md()
        .bg(rgb(PANEL))
        .child(
            div()
                .flex()
                .gap_2()
                .items_center()
                .child(div().text_color(rgb(ACCENT)).child(f.name.clone()))
                .child(div().flex_1())
                .child(refresh_chip)
                .child(
                    chip(format!("ics-remove-{}", f.name), "Remove", !busy).on_click(cx.listener(
                        move |w, _, _, cx| {
                            remove(w, &remove_name, cx);
                            cx.notify();
                        },
                    )),
                ),
        )
        .child(
            div()
                .text_sm()
                .text_color(rgb(DIM))
                .whitespace_normal()
                .child(f.url.clone()),
        )
        .child(div().text_sm().text_color(rgb(state.1)).child(state.0))
}

fn sources(cal: &Cal, cx: &mut Context<MainWindow>) -> impl IntoElement {
    let busy = cal.refreshing.is_some();
    let mut all = chip(
        "ics-refresh-all",
        match cal.refreshing.as_deref() {
            Some(_) => "Refreshing...",
            None => "Refresh all",
        },
        !busy && cal.enabled && !cal.feeds.is_empty(),
    );
    if !busy && cal.enabled && !cal.feeds.is_empty() {
        all = all.on_click(cx.listener(|w, _, _, cx| refresh(w, String::new(), cx)));
    }
    let can_add = !cal.name.trim().is_empty() && !cal.url.trim().is_empty();
    let mut add_chip = chip("ics-add", "Add", can_add);
    if can_add {
        add_chip = add_chip.on_click(cx.listener(|w, _, _, cx| {
            add(w, cx);
            cx.notify();
        }));
    }
    let mut col = div()
        .id("ics-sources")
        .flex()
        .flex_col()
        .gap_2()
        .w(px(360.))
        .flex_none()
        .overflow_y_scroll()
        .child(
            div()
                .flex()
                .gap_2()
                .items_center()
                .child(div().text_color(rgb(ACCENT)).child("Calendar feeds"))
                .child(div().flex_1())
                .child(all)
                .child(
                    chip("ics-reload", "Reload", true).on_click(cx.listener(|w, _, _, cx| {
                        cx.default_global::<Cal>().load(w);
                        cx.notify();
                    })),
                ),
        );
    if !cal.enabled {
        col = col.child(
            div()
                .text_sm()
                .text_color(rgb(WARN))
                .child("Calendar feeds are off (ics.enabled)."),
        );
    }
    if cal.feeds.is_empty() {
        col = col.child(
            div()
                .text_sm()
                .text_color(rgb(DIM))
                .child("No feeds. Add an http(s), webcal or file URL below."),
        );
    }
    col.children(cal.feeds.iter().map(|f| feed_row(f, busy, cx)))
        .child(
            div()
                .flex()
                .flex_col()
                .flex_none()
                .gap_1()
                .child(div().text_sm().text_color(rgb(DIM)).child("Add a feed"))
                .child(
                    div()
                        .flex()
                        .gap_2()
                        .items_center()
                        .child(input(cal, Field::Name, "name", 100., cx))
                        .child(input(cal, Field::Url, "https://... .ics", 180., cx))
                        .child(add_chip),
                ),
        )
}

fn upcoming(cal: &Cal) -> impl IntoElement {
    let mut col = div()
        .id("ics-upcoming")
        .flex()
        .flex_col()
        .gap_1()
        .flex_1()
        .min_w_0()
        .overflow_y_scroll()
        .child(
            div()
                .flex_none()
                .text_color(rgb(ACCENT))
                .child("Upcoming meetings"),
        );
    if !cal.from.is_empty() {
        col = col.child(
            div()
                .flex_none()
                .text_sm()
                .text_color(rgb(DIM))
                .child(format!("{} to {} (ics.window_days)", cal.from, cal.to)),
        );
    }
    if let Some(e) = &cal.error {
        col = col.child(
            div()
                .flex_none()
                .text_sm()
                .text_color(rgb(ERR))
                .child(e.clone()),
        );
    }
    if cal.meetings.is_empty() {
        return col.child(
            div()
                .text_color(rgb(DIM))
                .child("No upcoming meetings in the cached feeds."),
        );
    }
    let mut day = "";
    for m in &cal.meetings {
        if m.day != day {
            day = &m.day;
            col = col.child(
                div()
                    .flex_none()
                    .mt_2()
                    .text_color(rgb(FG))
                    .child(m.day.clone()),
            );
        }
        let mut what = m.summary.clone();
        if what.is_empty() {
            what = "(no title)".into();
        }
        let mut meta = m.feed.clone();
        if !m.location.is_empty() {
            meta = format!("{meta} · {}", m.location);
        }
        col = col.child(
            div()
                .flex()
                .flex_none()
                .gap_2()
                .items_start()
                .w_full()
                .child(div().w(px(64.)).flex_none().text_color(rgb(ACCENT)).child(
                    if m.time.is_empty() {
                        "all day".to_string()
                    } else {
                        m.time.clone()
                    },
                ))
                .child(div().flex_1().min_w_0().whitespace_normal().child(what))
                .child(div().flex_none().text_sm().text_color(rgb(DIM)).child(meta)),
        );
    }
    col
}

pub fn render(w: &MainWindow, cx: &mut Context<MainWindow>) -> AnyElement {
    let focus = cx.focus_handle();
    let cal = cx.default_global::<Cal>();
    let focus = cal.focus.get_or_insert(focus).clone();
    if cal.stale(w) && cal.refreshing.is_none() {
        cal.load(w);
    }
    // Clone out so the listeners below can borrow `cx`; the lists are small.
    let snap = Cal {
        enabled: cal.enabled,
        feeds: cal.feeds.clone(),
        meetings: cal.meetings.clone(),
        from: cal.from.clone(),
        to: cal.to.clone(),
        error: cal.error.clone(),
        loaded: cal.loaded,
        refreshing: cal.refreshing.clone(),
        name: cal.name.clone(),
        url: cal.url.clone(),
        field: cal.field,
        focus: Some(focus.clone()),
    };
    div()
        .id("calendar")
        .track_focus(&focus)
        .on_key_down(cx.listener(|w, ev: &KeyDownEvent, _, cx| {
            let k = &ev.keystroke;
            let command = k.modifiers.control || k.modifiers.alt || k.modifiers.platform;
            if key(w, &k.key, k.key_char.as_deref(), command, cx) {
                cx.stop_propagation();
                cx.notify();
            }
        }))
        .flex()
        .gap_4()
        .size_full()
        .child(sources(&snap, cx))
        .child(upcoming(&snap))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::Tab;
    use crate::testutil;
    use gpui::TestAppContext;

    /// A daily meeting since 2026-01-01, so the upcoming window always holds one.
    const ICS: &str = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//oats//test//EN\r\n\
BEGIN:VEVENT\r\nUID:standup@test\r\nDTSTAMP:20260101T000000Z\r\nDTSTART:20260101T090000Z\r\n\
DTEND:20260101T091500Z\r\nRRULE:FREQ=DAILY\r\nSUMMARY:Standup\r\nLOCATION:Room 1\r\n\
END:VEVENT\r\nEND:VCALENDAR\r\n";

    #[test]
    fn meeting_rows_split_local_start_and_all_day() {
        let m = meeting_of(&json!({"feed":"w","summary":"S","local_start":"2026-10-29T09:30:00"}));
        assert_eq!((m.day.as_str(), m.time.as_str()), ("2026-10-29", "09:30"));
        let d = meeting_of(
            &json!({"all_day":true,"start_date":"2026-10-30","local_start":"2026-10-30T00:00:00"}),
        );
        assert_eq!((d.day.as_str(), d.time.as_str()), ("2026-10-30", ""));
        let f = feed_of(
            &json!({"name":"w","url":"u","error":{"code":"fetch_failed","message":"boom"}}),
        );
        assert_eq!(f.error.as_deref(), Some("boom"));
        assert!(f.enabled);
        assert_eq!(
            refresh_summary(
                &json!({"feeds":[{"name":"a","ok":true},{"name":"b","ok":false,"error":{"code":"x","message":"no"}}],"failed":1})
            ),
            "Refreshed 1 of 2 feed(s); b: no"
        );
    }

    #[test]
    fn calendar_view_adds_refreshes_lists_and_removes_a_feed() {
        let (engine, dir) = testutil::engine(&[]);
        let path = dir.path().join("work.ics");
        std::fs::write(&path, ICS).unwrap();
        let url = format!("file://{}", path.display());
        let mut cx = TestAppContext::single();
        let (view, cx) = cx.add_window_view(|_, _| MainWindow::new(engine.clone()));
        view.update(cx, |v, cx| {
            v.state.tab = Tab::Calendar;
            cx.notify();
        });
        cx.run_until_parked();

        // Type into the form as the keyboard would, then Enter adds the feed.
        view.update(cx, |v, cx| {
            cx.default_global::<Cal>().field = Some(Field::Name);
            for ch in ["w", "o", "r", "k"] {
                assert!(key(v, ch, Some(ch), false, cx));
            }
            assert!(key(v, "tab", None, false, cx));
            for ch in url.chars() {
                let c = ch.to_string();
                key(v, &c, Some(&c), false, cx);
            }
            assert!(
                !key(v, "c", Some("c"), true, cx),
                "ctrl-chords are not typed"
            );
            assert!(key(v, "enter", None, false, cx));
            cx.notify();
        });
        cx.run_until_parked();
        view.read_with(cx, |v, cx| {
            let cal = cx.global::<Cal>();
            assert_eq!(v.state.status, "Added feed work.");
            assert_eq!(cal.feeds.len(), 1);
            assert_eq!(cal.feeds[0].url, url);
            assert!(cal.feeds[0].fetched_at.is_none());
            assert!(cal.name.is_empty() && cal.field.is_none());
        });

        // A duplicate name is the engine's error, shown in the status line.
        view.update(cx, |v, cx| {
            let cal = cx.default_global::<Cal>();
            cal.name = "work".into();
            cal.url = url.clone();
            add(v, cx);
            cx.notify();
        });
        view.read_with(cx, |v, _| {
            assert!(v.state.status.contains("already exists"))
        });

        view.update(cx, |v, cx| refresh(v, "work".into(), cx));
        cx.run_until_parked();
        view.read_with(cx, |v, cx| {
            let cal = cx.global::<Cal>();
            assert_eq!(v.state.status, "Refreshed 1 feed(s).");
            assert!(cal.refreshing.is_none());
            assert!(cal.feeds[0].fetched_at.is_some());
            assert!(!cal.meetings.is_empty());
            assert_eq!(cal.meetings[0].summary, "Standup");
            assert_eq!(cal.meetings[0].location, "Room 1");
            assert_eq!(cal.meetings[0].time, "09:00");
        });

        view.update(cx, |v, cx| {
            remove(v, "work", cx);
            cx.notify();
        });
        cx.run_until_parked();
        view.read_with(cx, |v, cx| {
            let cal = cx.global::<Cal>();
            assert_eq!(v.state.status, "Removed feed work.");
            assert!(cal.feeds.is_empty() && cal.meetings.is_empty());
        });
    }

    #[test]
    fn calendar_view_renders_with_feeds_off() {
        let (engine, _d) = testutil::engine(&[("ics.enabled", json!(false))]);
        let mut cx = TestAppContext::single();
        let (view, cx) = cx.add_window_view(|_, _| MainWindow::new(engine.clone()));
        view.update(cx, |v, cx| {
            v.state.tab = Tab::Calendar;
            cx.notify();
        });
        cx.run_until_parked();
        view.update(cx, |v, cx| refresh(v, String::new(), cx));
        cx.run_until_parked();
        view.read_with(cx, |v, cx| {
            assert!(!cx.global::<Cal>().enabled);
            assert!(v.state.status.contains("ics.enabled"), "{}", v.state.status);
        });
    }
}
