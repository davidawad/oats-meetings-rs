//! Calendar tab: the upcoming meetings of every calendar source (`calendar-events`: the ICS feed
//! caches, and the macOS Calendar app when an eventkit source is on) as a month-style grid (one row
//! per week of the window, today marked), beside the feed subscriptions (add / remove / refresh), over the same actions as `ctl ics-*`. Every choice is an `ics.*` config key: the
//! upcoming window is `ics.window_days` (`ics-events` with no bounds), and the view re-reads the
//! feeds and events at most once per `ics.poll_interval` (the scheduler's own cadence), plus after
//! each action, and whenever the engine publishes `ics-updated` (a fetch changed a cache). Times
//! show in `ui.tz` (empty: the system zone) with the zone's abbreviation. The view's own state is
//! a gpui global so it stays inside this file.

use std::time::Instant;

use chrono::{DateTime, Datelike, Duration as Days, NaiveDate};
use chrono_tz::Tz;
use gpui::{
    div, prelude::*, px, rgb, AnyElement, App, Context, FocusHandle, FontWeight, Global,
    KeyDownEvent, SharedString,
};
use serde_json::{json, Value};

use oats_core::control::engine::Engine;

use crate::kit::{self, Kind, Size};
use crate::model;
use crate::state::type_key;
use crate::theme;
use crate::view::MainWindow;

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
    /// `YYYY-MM-DD` in the display zone (all-day events: their own date).
    pub day: String,
    /// `HH:MM` in the display zone, empty for all-day events.
    pub time: String,
    /// The display zone's abbreviation at `time` (`EDT`, `UTC`), empty for all-day events.
    pub zone: String,
}

#[derive(Default)]
pub(crate) struct Cal {
    pub enabled: bool,
    pub feeds: Vec<FeedRow>,
    pub meetings: Vec<Meeting>,
    pub from: String,
    pub to: String,
    /// The zone the times are shown in (`ui.tz`, else the system zone).
    pub tz: Option<Tz>,
    /// Why the feeds or events could not be read.
    pub error: Option<String>,
    pub loaded: Option<Instant>,
    /// Feeds being fetched off the UI thread (`""` is "every enabled feed").
    pub refreshing: Option<String>,
    pub name: String,
    pub url: String,
    pub field: Option<Field>,
    /// ctrl/cmd+a selected the whole active field.
    pub all: bool,
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

/// The zone `zone` names, else the system zone (`TZ`, then the OS setting), else UTC.
pub(crate) fn zone_named(zone: &str) -> Tz {
    let parse = |z: &str| {
        // `TZ` may be `:Area/City` or a zoneinfo path.
        let z = z.trim().trim_start_matches(':');
        let z = z.rsplit_once("zoneinfo/").map_or(z, |(_, n)| n);
        z.parse::<Tz>().ok()
    };
    parse(zone)
        .or_else(|| std::env::var("TZ").ok().as_deref().and_then(parse))
        .or_else(|| {
            iana_time_zone::get_timezone()
                .ok()
                .as_deref()
                .and_then(parse)
        })
        .unwrap_or(Tz::UTC)
}

/// The zone meeting times are shown in: `ui.tz`, empty for the system zone.
pub(crate) fn display_zone(engine: &Engine) -> Tz {
    zone_named(&engine.snapshot().config.ui.tz)
}

pub(crate) fn meeting_of(v: &Value, tz: Tz) -> Meeting {
    let all_day = v.get("all_day").and_then(Value::as_bool).unwrap_or(false);
    // `local_start` is the event's own wall clock, `%Y-%m-%dT%H:%M:%S`: split on its separators.
    let local = s(v, "local_start");
    let (d, t) = local.split_once('T').unwrap_or((local.as_str(), ""));
    let start = DateTime::parse_from_rfc3339(&s(v, "start")).ok();
    let (day, time, zone) = match (all_day, start) {
        (true, _) => {
            let sd = s(v, "start_date");
            (
                if sd.is_empty() { d.to_string() } else { sd },
                String::new(),
                String::new(),
            )
        }
        // `start` is the instant: show it on the display zone's clock.
        (false, Some(at)) => {
            let at = at.with_timezone(&tz);
            (
                at.format("%Y-%m-%d").to_string(),
                at.format("%H:%M").to_string(),
                at.format("%Z").to_string(),
            )
        }
        (false, None) => {
            let hm = t.rsplit_once(':').map_or(t, |(hm, _)| hm);
            (d.to_string(), hm.to_string(), s(v, "tzid"))
        }
    };
    Meeting {
        feed: s(v, "feed"),
        summary: s(v, "summary"),
        location: s(v, "location"),
        day,
        time,
        zone,
    }
}

impl Cal {
    /// Re-read `ics-list` and `ics-events` (the upcoming window: now + `ics.window_days`).
    pub(crate) fn load(&mut self, w: &MainWindow) {
        self.loaded = Some(Instant::now());
        self.error = None;
        let tz = display_zone(&w.engine);
        self.tz = Some(tz);
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
        // Every calendar source's meetings (the ICS feeds and, on macOS, the Calendar app).
        match model::dispatch(&w.engine, "calendar-events", json!({})) {
            Ok(v) => {
                self.meetings = v
                    .get("events")
                    .and_then(Value::as_array)
                    .map(|a| a.iter().map(|m| meeting_of(m, tz)).collect())
                    .unwrap_or_default();
                self.from = s(&v, "from");
                self.to = s(&v, "to");
                // A feed whose cache fails to parse is reported per feed, not as a failed call.
                if let Some(e) = v
                    .get("errors")
                    .and_then(Value::as_array)
                    .and_then(|a| a.first())
                {
                    let name = [s(e, "feed"), s(e, "source"), s(e, "name")]
                        .into_iter()
                        .find(|n| !n.is_empty())
                        .unwrap_or_default();
                    self.error = Some(format!(
                        "{}: {}",
                        name,
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

/// Subscribe to the feed typed into the form (`ics-add`); the form clears on success, and the new
/// feed is fetched at once (off the UI thread) so its meetings show without a Refresh click.
pub(crate) fn add(w: &mut MainWindow, cx: &mut Context<MainWindow>) {
    let cal = cx.default_global::<Cal>();
    let (name, url) = (cal.name.trim().to_string(), cal.url.trim().to_string());
    let added = match model::dispatch(&w.engine, "ics-add", json!({"name": name, "url": url})) {
        Ok(v) => {
            w.state.status = format!("Added feed {name}.");
            cal.name.clear();
            cal.url.clear();
            cal.field = None;
            v.pointer("/feed/enabled").and_then(Value::as_bool) != Some(false)
        }
        Err(e) => {
            w.state.status = e.to_string();
            false
        }
    };
    cal.load(w);
    if added && cal.enabled {
        fetch(w, name.clone(), format!("Added feed {name}. "), cx);
    }
}

/// An engine bus line: `ics-updated` (a fetch, the scheduler's too, changed a cache) re-reads.
pub(crate) fn on_event(line: &str, cx: &mut App) {
    let ics = serde_json::from_str::<Value>(line)
        .is_ok_and(|v| v.get("event").and_then(Value::as_str) == Some("ics-updated"));
    if ics {
        cx.default_global::<Cal>().loaded = None;
    }
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
    fetch(w, name, String::new(), cx);
}

/// `refresh`, with `prefix` before its summary in the status line.
fn fetch(w: &mut MainWindow, name: String, prefix: String, cx: &mut Context<MainWindow>) {
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
                Ok(v) => format!("{prefix}{}", refresh_summary(&v)),
                Err(e) => format!("{prefix}{e}"),
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
    cx: &mut Context<MainWindow>,
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
            });
            cal.all = false;
        }
        _ => {
            let mut all = cal.all;
            let handled = type_key(cal.buf(f), &mut all, key, key_char, command);
            cal.all = all;
            return handled;
        }
    }
    true
}

fn chip(id: impl Into<SharedString>, label: &str, on: bool) -> gpui::Stateful<gpui::Div> {
    kit::button(id, label.to_string(), Kind::Secondary, Size::Sm, on)
}

fn input(
    cal: &Cal,
    f: Field,
    placeholder: &str,
    width: f32,
    cx: &mut Context<MainWindow>,
) -> impl IntoElement {
    let th = theme::current();
    let (text, active) = match f {
        Field::Name => (&cal.name, cal.field == Some(f)),
        Field::Url => (&cal.url, cal.field == Some(f)),
    };
    let selected = active && cal.all;
    let shown = match (text.is_empty(), active) {
        (_, true) if selected => text.clone(),
        (_, true) => format!("{text}|"),
        (true, false) => placeholder.to_string(),
        (false, false) => text.clone(),
    };
    let focus = cal.focus.clone();
    let id = match f {
        Field::Name => "ics-name",
        Field::Url => "ics-url",
    };
    // One line at a fixed width so a long URL never grows the row and moves the Add chip; while
    // typing the start is elided so the caret end stays in view.
    let line = div().overflow_hidden().whitespace_nowrap();
    let line = if active {
        line.text_ellipsis_start()
    } else {
        line.text_ellipsis()
    };
    kit::field(id, active)
        .debug_selector(|| id.to_string())
        .w(px(width))
        .flex_none()
        .text_size(px(13.))
        .text_color(rgb(if text.is_empty() && !active {
            th.faint
        } else {
            th.text
        }))
        .child(
            line.when(selected, |l| {
                l.bg(rgb(th.accent)).text_color(rgb(th.accent_ink))
            })
            .child(shown),
        )
        .on_click(cx.listener(move |_, _, window, cx| {
            let cal = cx.default_global::<Cal>();
            cal.field = Some(f);
            cal.all = false;
            if let Some(h) = &focus {
                h.focus(window, cx);
            }
            cx.notify();
        }))
}

fn feed_row(f: &FeedRow, busy: bool, last: bool, cx: &mut Context<MainWindow>) -> impl IntoElement {
    let th = theme::current();
    let state = match (&f.error, &f.fetched_at, f.enabled) {
        (_, _, false) => ("disabled".to_string(), th.faint),
        (Some(e), _, _) => (e.clone(), th.err),
        (None, Some(t), _) => (
            format!("{} events, fetched {t}", f.events.unwrap_or(0)),
            th.dim,
        ),
        (None, None, _) => ("not fetched yet".to_string(), th.warn),
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
    let mut remove_chip = kit::button(
        format!("ics-remove-{}", f.name),
        "Remove",
        Kind::Danger,
        Size::Sm,
        !busy,
    );
    if !busy {
        remove_chip = remove_chip.on_click(cx.listener(move |w, _, _, cx| {
            remove(w, &remove_name, cx);
            cx.notify();
        }));
    }
    div()
        .id(SharedString::from(format!("ics-feed-{}", f.name)))
        .flex()
        .flex_col()
        .flex_none()
        .gap(px(4.))
        .px(px(14.))
        .py(px(10.))
        .when(!last, |d| d.border_b_1().border_color(rgb(th.line)))
        .child(
            div()
                .flex()
                .gap(px(8.))
                .items_center()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(f.name.clone()),
                )
                .child(refresh_chip)
                .child(remove_chip),
        )
        .child(
            kit::mono(f.url.clone())
                .text_size(px(11.))
                .text_color(rgb(th.dim))
                .overflow_hidden()
                .text_ellipsis()
                .whitespace_nowrap(),
        )
        .child(
            div()
                .text_size(px(12.))
                .text_color(rgb(state.1))
                .child(state.0),
        )
}

fn sources(cal: &Cal, cx: &mut Context<MainWindow>) -> impl IntoElement {
    let th = theme::current();
    let busy = cal.refreshing.is_some();
    let can_add = !cal.name.trim().is_empty() && !cal.url.trim().is_empty();
    let mut add_chip = kit::button("ics-add", "Add", Kind::Primary, Size::Sm, can_add)
        .debug_selector(|| "ics-add".into());
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
        .gap(px(12.))
        .w(px(300.))
        .flex_none()
        .overflow_y_scroll();
    if !cal.enabled {
        col = col.child(kit::banner(
            false,
            "Calendar feeds are off.",
            "Turn on ics.enabled in Settings.",
        ));
    }
    let n = cal.feeds.len();
    let mut feeds = kit::panel();
    if cal.feeds.is_empty() {
        feeds = feeds.child(
            div()
                .p(px(14.))
                .text_size(px(13.))
                .text_color(rgb(th.dim))
                .child("No feeds. Add an http(s), webcal or file URL below."),
        );
    }
    feeds = feeds.children(
        cal.feeds
            .iter()
            .enumerate()
            .map(|(i, f)| feed_row(f, busy, i + 1 == n, cx)),
    );
    col.child(kit::section("Feeds").pt_0().text_color(rgb(th.dim)))
        .child(feeds)
        .child(
            kit::panel()
                .p(px(14.))
                .gap(px(8.))
                .child(
                    div()
                        .text_size(px(12.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(rgb(th.dim))
                        .child("Add ICS feed"),
                )
                .child(input(cal, Field::Name, "name", 270., cx))
                .child(
                    div()
                        .flex()
                        .gap(px(8.))
                        .items_center()
                        .child(input(cal, Field::Url, "https://... .ics", 210., cx))
                        .child(add_chip),
                ),
        )
}

/// `HH:MM ZONE`, the zone abbreviation of the display zone at that time.
pub(crate) fn time_label(m: &Meeting) -> String {
    format!("{} {}", m.time, m.zone).trim_end().to_string()
}

fn date_of(s: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(s.get(..10)?, "%Y-%m-%d").ok()
}

/// The grid's weeks: Monday-first rows from the week holding `from` through the week holding
/// `to` (else the last meeting), so every meeting of the window has a cell.
pub(crate) fn weeks(from: NaiveDate, to: NaiveDate) -> Vec<[NaiveDate; 7]> {
    let mut start = from - Days::days(i64::from(from.weekday().num_days_from_monday()));
    let mut out = vec![];
    while start <= to {
        out.push(std::array::from_fn(|i| start + Days::days(i as i64)));
        start += Days::days(7);
    }
    out
}

fn grid(cal: &Cal) -> impl IntoElement {
    let th = theme::current();
    let tz = cal.tz.unwrap_or(Tz::UTC);
    let today = chrono::Utc::now().with_timezone(&tz).date_naive();
    let from = date_of(&cal.from).unwrap_or(today);
    let last = cal.meetings.iter().filter_map(|m| date_of(&m.day)).max();
    let to = date_of(&cal.to).or(last).unwrap_or(from).max(from);
    let mut col = div()
        .id("ics-upcoming")
        .flex()
        .flex_col()
        .flex_1()
        .min_w_0()
        .overflow_y_scroll();
    if let Some(e) = &cal.error {
        col = col.child(
            div()
                .mb(px(12.))
                .child(kit::banner(true, "A feed failed.", e.clone())),
        );
    }
    let head = div()
        .flex()
        .flex_none()
        .bg(rgb(th.raised))
        .border_b_1()
        .border_color(rgb(th.line))
        .children(["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"].map(|d| {
            div()
                .flex_1()
                .px(px(8.))
                .py(px(8.))
                .text_size(px(11.))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(rgb(th.dim))
                .child(d)
        }));
    let mut table = kit::panel().flex_none().child(head);
    for week in weeks(from, to) {
        let row = div()
            .flex()
            .flex_none()
            .border_b_1()
            .border_color(rgb(th.line))
            .children(week.iter().enumerate().map(|(i, day)| {
                let key = day.format("%Y-%m-%d").to_string();
                let is_today = *day == today;
                let outside = *day < from || *day > to;
                let events = cal.meetings.iter().filter(|m| m.day == key).map(|m| {
                    let what = if m.summary.is_empty() {
                        "(no title)".to_string()
                    } else {
                        m.summary.clone()
                    };
                    let label = if m.time.is_empty() {
                        what
                    } else {
                        format!("{} {what}", m.time)
                    };
                    let mut tip = vec![m.feed.clone()];
                    if !m.time.is_empty() {
                        tip.push(time_label(m));
                    }
                    if !m.location.is_empty() {
                        tip.push(m.location.clone());
                    }
                    div()
                        .mt(px(4.))
                        .px(px(6.))
                        .py(px(2.))
                        .rounded(px(4.))
                        .border_l(px(3.))
                        .border_color(rgb(if m.time.is_empty() {
                            th.remote[0]
                        } else {
                            th.accent
                        }))
                        .bg(rgb(if m.time.is_empty() {
                            th.warn_soft
                        } else {
                            th.accent_soft
                        }))
                        .text_size(px(11.))
                        .text_color(rgb(th.text))
                        .overflow_hidden()
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .child(label)
                });
                div()
                    .flex_1()
                    .min_w_0()
                    .min_h(px(104.))
                    .p(px(6.))
                    .when(i < 6, |d| d.border_r_1().border_color(rgb(th.line)))
                    .when(is_today, |d| d.bg(rgb(th.accent_soft)))
                    .child(
                        div()
                            .text_size(px(11.))
                            .font_weight(if is_today {
                                FontWeight::BOLD
                            } else {
                                FontWeight::NORMAL
                            })
                            .text_color(rgb(if is_today {
                                th.accent
                            } else if outside {
                                th.line2
                            } else {
                                th.faint
                            }))
                            .child(if day.day() == 1 {
                                day.format("%b %-d").to_string()
                            } else {
                                day.day().to_string()
                            }),
                    )
                    .children(events)
            }));
        table = table.child(row);
    }
    col = col.child(table);
    if cal.meetings.is_empty() {
        col = col.child(
            div()
                .mt(px(12.))
                .text_color(rgb(th.dim))
                .child("No upcoming meetings in the cached feeds."),
        );
    }
    col
}

pub fn render(w: &MainWindow, cx: &mut Context<MainWindow>) -> AnyElement {
    let th = theme::current();
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
        tz: cal.tz,
        error: cal.error.clone(),
        loaded: cal.loaded,
        refreshing: cal.refreshing.clone(),
        name: cal.name.clone(),
        url: cal.url.clone(),
        field: cal.field,
        all: cal.all,
        focus: Some(focus.clone()),
    };
    let tz = snap.tz.unwrap_or(Tz::UTC);
    let month = date_of(&snap.from)
        .unwrap_or_else(|| chrono::Utc::now().with_timezone(&tz).date_naive())
        .format("%B %Y")
        .to_string();
    let busy = snap.refreshing.is_some();
    let can_all = !busy && snap.enabled && !snap.feeds.is_empty();
    let mut all = kit::button(
        "ics-refresh-all",
        if busy { "Refreshing..." } else { "Refresh all" },
        Kind::Secondary,
        Size::Md,
        can_all,
    );
    if can_all {
        all = all.on_click(cx.listener(|w, _, _, cx| refresh(w, String::new(), cx)));
    }
    let n = snap.feeds.len();
    let head = kit::head("Calendar")
        .child(kit::mono(month).text_color(rgb(th.dim)))
        .child(
            div()
                .text_size(px(12.))
                .text_color(rgb(th.faint))
                .child(format!("times in {}", tz.name())),
        )
        .child(div().flex_1())
        .child(kit::tag(
            format!("{n} feed{}", if n == 1 { "" } else { "s" }),
            false,
        ))
        .child(
            kit::button("ics-reload", "Reload", Kind::Secondary, Size::Md, true).on_click(
                cx.listener(|w, _, _, cx| {
                    cx.default_global::<Cal>().load(w);
                    cx.notify();
                }),
            ),
        )
        .child(all);
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
        .flex_col()
        .size_full()
        .child(head)
        .child(
            kit::pad()
                .flex_row()
                .gap(px(20.))
                .child(grid(&snap))
                .child(sources(&snap, cx)),
        )
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

    /// A weekly 15:00Z meeting since 2026-01-01 (the QA2 fixture's shape).
    const WEEKLY: &str = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//oats//test//EN\r\n\
BEGIN:VEVENT\r\nUID:sync@test\r\nDTSTAMP:20260101T000000Z\r\nDTSTART:20260101T150000Z\r\n\
DTEND:20260101T153000Z\r\nRRULE:FREQ=WEEKLY\r\nSUMMARY:Sync\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";

    const NY: Tz = chrono_tz::America::New_York;

    #[test]
    fn the_grid_spans_monday_first_weeks_from_the_window_start_to_its_end() {
        let d = |s: &str| NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap();
        // 2026-11-04 is a Wednesday: the first row starts Monday the 2nd.
        let w = weeks(d("2026-11-04"), d("2026-11-14"));
        assert_eq!(w.len(), 2);
        assert_eq!(w[0][0], d("2026-11-02"));
        assert_eq!(w[1][6], d("2026-11-15"));
        assert_eq!(weeks(d("2026-11-04"), d("2026-11-04")).len(), 1);
        assert!(weeks(d("2026-11-04"), d("2026-11-01")).is_empty());
    }

    #[test]
    fn meeting_rows_split_local_start_and_all_day() {
        let m = meeting_of(
            &json!({"feed":"w","summary":"S","local_start":"2026-10-29T09:30:00","tzid":"UTC"}),
            Tz::UTC,
        );
        assert_eq!((m.day.as_str(), m.time.as_str()), ("2026-10-29", "09:30"));
        let d = meeting_of(
            &json!({"all_day":true,"start_date":"2026-10-30","local_start":"2026-10-30T00:00:00"}),
            NY,
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
    fn times_show_in_the_display_zone_with_its_abbreviation() {
        let ev = |start: &str| json!({"feed":"w","summary":"S","start":start,"local_start":"x","tzid":"UTC"});
        // The QA2 case: 15:00Z is 11:00 on an EDT clock, 10:00 once DST ends (2026-11-01).
        let m = meeting_of(&ev("2026-10-29T15:00:00Z"), NY);
        assert_eq!(
            (m.day.as_str(), time_label(&m).as_str()),
            ("2026-10-29", "11:00 EDT")
        );
        let m = meeting_of(&ev("2026-11-02T15:00:00Z"), NY);
        assert_eq!(time_label(&m), "10:00 EST");
        // The day follows the display zone too.
        let m = meeting_of(&ev("2026-10-30T02:00:00Z"), NY);
        assert_eq!(
            (m.day.as_str(), time_label(&m).as_str()),
            ("2026-10-29", "22:00 EDT")
        );
        assert_eq!(
            time_label(&meeting_of(&ev("2026-10-29T15:00:00Z"), Tz::UTC)),
            "15:00 UTC"
        );
        // ui.tz spellings: a name, TZ's `:Name`, a zoneinfo path.
        assert_eq!(zone_named("America/New_York"), NY);
        assert_eq!(zone_named(":America/New_York"), NY);
        assert_eq!(
            zone_named("/usr/share/zoneinfo/Europe/Berlin"),
            chrono_tz::Europe::Berlin
        );
    }

    #[test]
    fn add_shows_the_feeds_meetings_without_a_refresh_click_in_ui_tz() {
        let (engine, dir) = testutil::engine(&[("ui.tz", json!("America/New_York"))]);
        let mut cx = TestAppContext::single();
        let (view, cx) = cx.add_window_view(|_, _| MainWindow::new(engine.clone()));
        view.update(cx, |v, cx| {
            v.state.tab = Tab::Calendar;
            cx.notify();
        });
        cx.run_until_parked();
        let path = dir.path().join("team.ics");
        std::fs::write(&path, WEEKLY).unwrap();
        view.update(cx, |v, cx| {
            let cal = cx.default_global::<Cal>();
            cal.name = "qa2cal".into();
            cal.url = format!("file://{}", path.display());
            add(v, cx);
            cx.notify();
        });
        cx.run_until_parked();
        view.read_with(cx, |v, cx| {
            let cal = cx.global::<Cal>();
            assert_eq!(v.state.status, "Added feed qa2cal. Refreshed 1 feed(s).");
            assert!(cal.feeds[0].fetched_at.is_some(), "{:?}", cal.feeds);
            assert_eq!(cal.tz, Some(NY));
            let m = cal
                .meetings
                .first()
                .expect("the weekly meeting is upcoming");
            assert_eq!(m.summary, "Sync");
            // 15:00Z on a New York clock: EDT or EST depending on today's date.
            let label = time_label(m);
            assert!(label == "11:00 EDT" || label == "10:00 EST", "{label}");
        });
    }

    #[test]
    fn an_ics_updated_event_reloads_the_view() {
        let (engine, dir) = testutil::engine(&[("ui.tz", json!("UTC"))]);
        let mut cx = TestAppContext::single();
        let (view, cx) = cx.add_window_view(|_, _| MainWindow::new(engine.clone()));
        view.update(cx, |v, cx| {
            v.state.tab = Tab::Calendar;
            cx.notify();
        });
        cx.run_until_parked();
        let path = dir.path().join("team.ics");
        let url = format!("file://{}", path.display());
        // Subscribed elsewhere (ctl) before the file exists: nothing cached yet.
        model::dispatch(&engine, "ics-add", json!({"name": "team", "url": url})).unwrap();
        view.update(cx, |v, cx| {
            cx.default_global::<Cal>().load(v);
            cx.notify();
        });
        view.read_with(cx, |_, cx| assert!(cx.global::<Cal>().meetings.is_empty()));
        // A fetch outside the view (the scheduler, ctl ics-refresh) publishes ics-updated.
        let rx = engine.bus.subscribe(vec![]);
        std::fs::write(&path, WEEKLY).unwrap();
        model::dispatch(&engine, "ics-refresh", json!({"name": "team"})).unwrap();
        let lines: Vec<String> = rx.try_iter().collect();
        assert!(lines.iter().any(|l| l.contains("ics-updated")), "{lines:?}");
        view.update(cx, |v, cx| {
            for l in &lines {
                v.event(l, cx);
            }
        });
        cx.run_until_parked();
        view.read_with(cx, |_, cx| {
            let cal = cx.global::<Cal>();
            assert!(cal.feeds[0].fetched_at.is_some());
            assert_eq!(cal.meetings[0].summary, "Sync");
            assert_eq!(time_label(&cal.meetings[0]), "15:00 UTC");
        });
    }

    #[test]
    fn calendar_view_adds_refreshes_lists_and_removes_a_feed() {
        let (engine, dir) = testutil::engine(&[("ui.tz", json!("UTC"))]);
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
            // The new feed is fetched straight away.
            assert_eq!(v.state.status, "Added feed work. Refreshed 1 feed(s).");
            assert_eq!(cal.feeds.len(), 1);
            assert_eq!(cal.feeds[0].url, url);
            assert!(cal.feeds[0].fetched_at.is_some());
            assert_eq!(cal.meetings[0].summary, "Standup");
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
            assert_eq!(time_label(&cal.meetings[0]), "09:00 UTC");
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
    fn add_feed_row_keeps_one_line_and_ctrl_a_replaces_the_url() {
        let (engine, _d) = testutil::engine(&[]);
        let mut cx = TestAppContext::single();
        let (view, cx) = cx.add_window_view(|_, _| MainWindow::new(engine.clone()));
        view.update(cx, |v, cx| {
            v.state.tab = Tab::Calendar;
            cx.default_global::<Cal>().field = Some(Field::Name);
            cx.notify();
        });
        cx.run_until_parked();
        let bounds = |cx: &mut gpui::VisualTestContext| {
            (
                cx.debug_bounds("ics-url").expect("url input"),
                cx.debug_bounds("ics-add").expect("add chip"),
            )
        };
        let before = bounds(cx);
        // A long URL typed through the focused form's key handler.
        let focus = view.update(cx, |_, cx| cx.global::<Cal>().focus.clone().unwrap());
        cx.update(|window, cx| focus.focus(window, cx));
        cx.simulate_keystrokes("n tab");
        let long = "https://calendar.example.com/very/long/path/to/a/feed/basic.ics";
        view.update(cx, |_, cx| {
            cx.global_mut::<Cal>().url = long.into();
            cx.notify();
        });
        cx.run_until_parked();
        assert_eq!(
            bounds(cx),
            before,
            "url input and Add chip do not move or grow"
        );
        cx.simulate_keystrokes("ctrl-a");
        cx.run_until_parked();
        assert!(cx.update(|_, cx| cx.global::<Cal>().all));
        assert_eq!(bounds(cx), before);
        cx.simulate_keystrokes("x");
        view.update(cx, |_, cx| {
            let cal = cx.global::<Cal>();
            assert_eq!((cal.name.as_str(), cal.url.as_str()), ("n", "x"));
            assert!(!cal.all);
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
