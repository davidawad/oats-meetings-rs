//! The command provider (dot-4uzgol.105): a calendar source whose events come from a program the
//! user names (`argv`), run on every calendar read, never through a shell. Whatever feed tool the
//! user has (a calendar CLI, a script over an API, a file someone else writes) prints its events
//! on stdout as ICS or JSON:
//!
//! - ICS: one VCALENDAR, read with the `ics.*` parser like a feed.
//! - JSON: an array of events, or an object holding one under `items`, `events` or `value`; each
//!   event is a Google Calendar API event (`summary`, `start.dateTime` / `start.date`, `iCalUID`,
//!   `attendees[].email`, `status`), a Microsoft Graph event (`subject`, `start.dateTime` +
//!   `start.timeZone`, `iCalUId`) or a plain `{title | summary, start, end, uid, location,
//!   attendees}` with RFC 3339 or `YYYY-MM-DD` times. Cancelled events are dropped.
//!
//! `format = auto` (the default) sniffs `BEGIN:VCALENDAR` against JSON. The program gets the
//! source's `env` on top of the app's environment, runs in `cwd` (else the app's own), is killed
//! (its process group on unix) past `timeout` (else `calendar.command_timeout`) and may print at
//! most `calendar.command_max_bytes`. A failed run (it could not start, a nonzero exit, a timeout,
//! too much or unreadable output) is the source's error in `calendar-events`,
//! `calendar-sources-list` and Settings; the events of the last good run of the same source are
//! still used, so a flaky tool never empties the calendar.

use std::collections::HashMap;
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::Instant;

use chrono::{DateTime, Duration, NaiveDate, NaiveDateTime, Utc};
use chrono_tz::Tz;
use serde_json::{json, Value};

use super::eventkit::Calendar;
use super::provider::{err_json, Provider};
use super::rules::{Event, Person};
use super::Source;
use crate::config::keys::{self, Kind};
use crate::config::typed::Config;
use crate::config::{ConfigError, Result};
use crate::control::engine::Engine;

/// The key whose range bounds a source's own `timeout` (and whose value is the default).
pub const TIMEOUT_KEY: &str = "calendar.command_timeout";

/// The `format` values; empty is `auto`.
pub const FORMATS: &[&str] = &["auto", "ics", "json"];

/// The range a source's `timeout` must be in, in ms.
pub fn timeout_range() -> (u64, u64) {
    match keys::find(TIMEOUT_KEY).map(|k| k.kind) {
        Some(Kind::Duration { min_ms, max_ms }) => (min_ms, max_ms),
        _ => unreachable!("{TIMEOUT_KEY} is a duration key"),
    }
}

/// What a command source's fields must hold.
pub fn check(s: &Source) -> std::result::Result<(), String> {
    if s.argv.first().is_none_or(|p| p.trim().is_empty()) {
        return Err(
            "a command source needs argv, the program and its arguments (an array like [\"my-calendar\", \"--json\"])"
                .into(),
        );
    }
    if !s.format.is_empty() && !FORMATS.contains(&s.format.as_str()) {
        return Err(format!(
            "format \"{}\" is not one of {}",
            s.format,
            FORMATS.join(", ")
        ));
    }
    if let Some(t) = s.timeout {
        let (min, max) = timeout_range();
        if !(min..=max).contains(&t.0) {
            return Err(format!(
                "timeout must be from {} to {}",
                keys::format_duration_ms(min),
                keys::format_duration_ms(max)
            ));
        }
    }
    if s.env.keys().any(|k| k.is_empty() || k.contains('=')) {
        return Err("env names must be non-empty and hold no '='".into());
    }
    Ok(())
}

fn failed(msg: String) -> ConfigError {
    ConfigError::new("command_failed", msg)
}

fn bad_output(msg: String) -> ConfigError {
    ConfigError::new("command_output", msg)
}

// ---- running ---------------------------------------------------------------------------------

/// Run the source's program to completion (or its timeout) and return its stdout as text.
/// `command_failed` when it cannot start or exits nonzero (with the start of its stderr),
/// `command_timeout` when it is killed, `command_output` when it prints too much.
pub fn run(s: &Source, cfg: &Config) -> Result<String> {
    let program = &s.argv[0];
    let mut cmd = Command::new(program);
    cmd.args(&s.argv[1..])
        .envs(&s.env)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if !s.cwd.trim().is_empty() {
        cmd.current_dir(s.cwd.trim());
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    let t0 = Instant::now();
    let mut child = cmd
        .spawn()
        .map_err(|e| failed(format!("cannot run {program:?}: {e}")))?;
    let max = cfg.calendar.command_max_bytes as usize;
    let snippet = cfg.network.error_snippet as usize;
    let out = child
        .stdout
        .take()
        .map(|r| std::thread::spawn(move || crate::hooks::drain(r, max)));
    let err = child
        .stderr
        .take()
        .map(|r| std::thread::spawn(move || crate::hooks::drain(r, snippet)));
    let limit = s.timeout.unwrap_or(cfg.calendar.command_timeout);
    let deadline = t0 + limit.as_duration();
    let poll = cfg.app.hook_poll.as_duration();
    let mut timed_out = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(st)) => break Some(st),
            Ok(None) if Instant::now() >= deadline => {
                crate::hooks::kill(&mut child);
                timed_out = true;
                break child.wait().ok();
            }
            Ok(None) => std::thread::sleep(poll),
            Err(_) => {
                crate::hooks::kill(&mut child);
                break child.wait().ok();
            }
        }
    };
    let join = |h: Option<std::thread::JoinHandle<(Vec<u8>, bool)>>| {
        h.and_then(|h| h.join().ok()).unwrap_or_default()
    };
    let (stdout, too_much) = join(out);
    let (stderr, _) = join(err);
    let stderr = String::from_utf8_lossy(&stderr).trim().to_string();
    if timed_out {
        return Err(ConfigError::new(
            "command_timeout",
            format!(
                "{program:?} ran past its timeout of {} and was killed",
                keys::format_duration_ms(limit.0)
            ),
        ));
    }
    match status.and_then(|st| st.code()) {
        Some(0) => {}
        Some(code) => {
            return Err(failed(format!(
                "{program:?} exited with code {code}{}",
                if stderr.is_empty() {
                    String::new()
                } else {
                    format!(": {stderr}")
                }
            )))
        }
        None => return Err(failed(format!("{program:?} was killed by a signal"))),
    }
    if too_much {
        return Err(bad_output(format!(
            "{program:?} printed more than calendar.command_max_bytes ({max} bytes)"
        )));
    }
    Ok(String::from_utf8_lossy(&stdout).into_owned())
}

// ---- parsing ---------------------------------------------------------------------------------

/// The format `text` is in: `format` unless it is `auto` (or empty), then sniffed. A refusal
/// quotes the first `snippet` characters.
pub fn sniff(text: &str, format: &str, snippet: usize) -> Result<&'static str> {
    match format {
        "ics" => return Ok("ics"),
        "json" => return Ok("json"),
        _ => {}
    }
    let t = text.trim_start_matches('\u{feff}').trim_start();
    if t.is_empty() {
        return Err(bad_output("the command printed nothing".into()));
    }
    if t.get(..15)
        .is_some_and(|h| h.eq_ignore_ascii_case("BEGIN:VCALENDAR"))
    {
        return Ok("ics");
    }
    if t.starts_with('[') || t.starts_with('{') {
        return Ok("json");
    }
    let head: String = t.chars().take(snippet).collect();
    Err(bad_output(format!(
        "output is neither ICS (BEGIN:VCALENDAR) nor JSON: {head:?}"
    )))
}

/// The event objects of a JSON reply: an array, `{items | events | value: [...]}`, or one event.
fn items(v: &Value) -> Result<Vec<Value>> {
    if let Some(a) = v.as_array() {
        return Ok(a.clone());
    }
    for k in ["items", "events", "value"] {
        if let Some(a) = v.get(k).and_then(Value::as_array) {
            return Ok(a.clone());
        }
    }
    if v.get("start").is_some() {
        return Ok(vec![v.clone()]);
    }
    Err(bad_output(
        "JSON is not an array of events nor an object with items, events or value".into(),
    ))
}

/// An RFC 3339 instant, a wall time in `tz`, or a `YYYY-MM-DD` day (its midnight in `tz`, and
/// the day itself).
fn when_text(s: &str, tz: Tz) -> Option<(DateTime<Utc>, Option<String>)> {
    let s = s.trim();
    if let Ok(d) = DateTime::parse_from_rfc3339(s) {
        return Some((d.with_timezone(&Utc), None));
    }
    if let Ok(n) = NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S%.f") {
        return Some((crate::ics::parse::resolve(tz, n), None));
    }
    let day = NaiveDate::parse_from_str(s, "%Y-%m-%d").ok()?;
    let at = crate::ics::parse::resolve(tz, day.and_hms_opt(0, 0, 0)?);
    Some((at, Some(s.to_string())))
}

/// An attendee given as an address or as `{email, name, status}`.
fn person(v: &Value) -> Option<Person> {
    if let Some(e) = v.as_str() {
        return Some(Person {
            email: e.trim().to_string(),
            ..Person::default()
        });
    }
    let text = |k: &str| v[k].as_str().unwrap_or_default().trim().to_string();
    let email = text("email");
    let name = if text("name").is_empty() {
        text("displayName")
    } else {
        text("name")
    };
    (!email.is_empty() || !name.is_empty()).then(|| Person {
        name,
        email,
        status: text("status").to_ascii_uppercase(),
        me: v["self"] == true,
    })
}

/// The plain shape: `{title | summary, start, end, uid, location, attendees, url, description}`.
fn plain(v: &Value, cal: &Calendar, tz: Tz) -> Option<Event> {
    let text = |k: &str| v[k].as_str().unwrap_or_default().to_string();
    let (start, start_date) = when_text(v["start"].as_str()?, tz)?;
    let end = v["end"]
        .as_str()
        .and_then(|e| when_text(e, tz))
        .map(|w| w.0)
        .unwrap_or_else(|| {
            if start_date.is_some() {
                start + Duration::days(1)
            } else {
                start
            }
        });
    let uid = ["uid", "iCalUID", "id"]
        .iter()
        .map(|k| text(k))
        .find(|u| !u.trim().is_empty())
        .unwrap_or_default();
    let title = if text("title").is_empty() {
        text("summary")
    } else {
        text("title")
    };
    let attendees: Vec<Person> = v["attendees"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(person)
        .collect();
    Some(Event {
        id: text("id"),
        uid,
        title,
        start,
        end: end.max(start),
        all_day: start_date.is_some(),
        calendar: cal.title.clone(),
        calendar_id: cal.id.clone(),
        account: cal.account.clone(),
        location: text("location"),
        notes: text("description"),
        url: text("url"),
        declined: attendees.iter().any(|p| p.me && p.status == "DECLINED"),
        attendees,
        cancelled: text("status").eq_ignore_ascii_case("cancelled"),
        start_date,
        ..Event::default()
    })
}

/// One JSON event, whichever of the three shapes it is in.
fn event_of(v: &Value, cal: &Calendar, tz: Tz) -> Option<Event> {
    let graph = v.get("subject").is_some() || v["start"].get("timeZone").is_some();
    let mut e = if graph {
        super::graph::event_of(v, cal)?
    } else if v["start"].is_object() {
        super::google::event_of(v, cal, tz)?
    } else {
        plain(v, cal, tz)?
    };
    if e.id.is_empty() {
        e.id = format!(
            "{}@{}",
            if e.uid.is_empty() { &e.title } else { &e.uid },
            e.start.timestamp()
        );
    }
    Some(e)
}

/// The events of a JSON reply filed under `cal`, all-day dates read in `tz`; cancelled ones are
/// dropped. Also how many objects were not events (no readable start).
pub fn parse_json(text: &str, cal: &Calendar, tz: Tz) -> Result<(Vec<Event>, usize)> {
    let v: Value = serde_json::from_str(text.trim_start_matches('\u{feff}'))
        .map_err(|e| bad_output(format!("not JSON: {e}")))?;
    let all = items(&v)?;
    let mut skipped = 0;
    let mut out = Vec::new();
    for item in &all {
        match event_of(item, cal, tz) {
            Some(e) if e.cancelled => {}
            Some(e) => out.push(e),
            None => skipped += 1,
        }
    }
    Ok((out, skipped))
}

/// Whether `e` overlaps `[from, to)` (an instant event counts at its start).
fn overlaps(e: &Event, from: DateTime<Utc>, to: DateTime<Utc>) -> bool {
    e.start < to && (e.end > from || e.start >= from)
}

/// The events of `text` in `[from, to)` filed under `cal`, and a note on what was skipped.
pub fn parse(
    engine: &Engine,
    s: &Source,
    text: &str,
    cal: &Calendar,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
) -> Result<(Vec<Event>, usize)> {
    let snippet = engine.snapshot().config.network.error_snippet as usize;
    match sniff(text, &s.format, snippet)? {
        "ics" => {
            let mut evs = super::provider::ics_text_events(engine, text, cal, from, to)
                .map_err(|e| bad_output(format!("ICS: {}", e.message)))?;
            evs.retain(|e| !e.cancelled);
            Ok((evs, 0))
        }
        _ => {
            let tz = engine
                .snapshot()
                .config
                .ics
                .default_tz
                .trim()
                .parse::<Tz>()
                .unwrap_or(Tz::UTC);
            let (mut evs, skipped) = parse_json(text, cal, tz)?;
            evs.retain(|e| overlaps(e, from, to));
            Ok((evs, skipped))
        }
    }
}

// ---- the provider ----------------------------------------------------------------------------

/// The one calendar a command source has: named after the source, under its program.
pub fn calendar_of(s: &Source) -> Calendar {
    let program = s.argv.first().map(String::as_str).unwrap_or_default();
    let base = std::path::Path::new(program)
        .file_name()
        .and_then(|f| f.to_str())
        .unwrap_or(program)
        .to_string();
    Calendar {
        id: s.name.clone(),
        title: s.name.clone(),
        account: base,
        account_id: "command".into(),
        account_kind: "command".into(),
    }
}

/// One source's last run and last good output.
#[derive(Debug, Clone)]
struct Last {
    /// The source as configured when it ran: an edited source starts afresh.
    config: Source,
    ran_at: DateTime<Utc>,
    duration_ms: u64,
    error: Option<ConfigError>,
    good: Option<String>,
    good_at: Option<DateTime<Utc>>,
    events: usize,
}

/// Every command source's last run, kept on the engine (`calendar-sources-list` shows them).
#[derive(Default)]
pub struct Runs(Mutex<HashMap<String, Last>>);

impl Runs {
    fn record(
        &self,
        s: &Source,
        duration_ms: u64,
        got: std::result::Result<(String, usize), ConfigError>,
    ) {
        let mut m = self.0.lock().expect("command runs");
        let prev = m.remove(&s.name).filter(|l| l.config == *s);
        let (mut good, mut good_at) = prev.map(|l| (l.good, l.good_at)).unwrap_or((None, None));
        let now = Utc::now();
        let (error, events) = match got {
            Ok((text, n)) => {
                good = Some(text);
                good_at = Some(now);
                (None, n)
            }
            Err(e) => (Some(e), 0),
        };
        m.insert(
            s.name.clone(),
            Last {
                config: s.clone(),
                ran_at: now,
                duration_ms,
                error,
                good,
                good_at,
                events,
            },
        );
    }

    /// The last good output of `s` as configured now.
    fn good(&self, s: &Source) -> Option<String> {
        let m = self.0.lock().expect("command runs");
        m.get(&s.name)
            .filter(|l| l.config == *s)
            .and_then(|l| l.good.clone())
    }

    /// The last run of `s` for `calendar-sources-list`: when, how long, how many events, its
    /// error, and whether an older good result is standing in. None before the first run.
    pub fn status(&self, s: &Source) -> Option<(Value, Option<ConfigError>)> {
        let m = self.0.lock().expect("command runs");
        let l = m.get(&s.name).filter(|l| l.config == *s)?;
        let v = json!({
            "ran_at": super::stamp(l.ran_at),
            "duration_ms": l.duration_ms,
            "ok": l.error.is_none(),
            "events": l.events,
            "error": l.error.as_ref().map(err_json),
            "last_good_at": l.good_at.map(super::stamp),
            "kept_last_good": l.error.is_some() && l.good.is_some(),
        });
        Some((v, l.error.clone()))
    }

    pub fn forget(&self, name: &str) {
        self.0.lock().expect("command runs").remove(name);
    }
}

/// Run `s` now and read its events in `[from, to)`: the fresh ones, or on a failed run the last
/// good run's (the failure goes to `errors`), or the failure itself when there never was one.
pub fn read(
    engine: &Engine,
    s: &Source,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    errors: &mut Vec<Value>,
) -> Result<Vec<Event>> {
    let cal = calendar_of(s);
    let cfg = engine.snapshot();
    let t0 = Instant::now();
    let got = run(s, &cfg.config).and_then(|text| {
        let (evs, skipped) = parse(engine, s, &text, &cal, from, to)?;
        Ok((text, evs, skipped))
    });
    let ms = t0.elapsed().as_millis() as u64;
    let runs = engine.calendar_command_runs();
    match got {
        Ok((text, evs, skipped)) => {
            if skipped > 0 {
                errors.push(
                    json!({"source": s.name, "error": err_json(&bad_output(format!(
                        "{skipped} JSON objects had no readable start and were skipped"
                    )))}),
                );
            }
            runs.record(s, ms, Ok((text, evs.len())));
            Ok(evs)
        }
        Err(e) => {
            let old = runs.good(s);
            runs.record(s, ms, Err(e.clone()));
            let Some(text) = old else { return Err(e) };
            errors.push(json!({"source": s.name, "error": err_json(&e), "kept_last_good": true}));
            Ok(parse(engine, s, &text, &cal, from, to)
                .map(|x| x.0)
                .unwrap_or_default())
        }
    }
}

pub struct CommandProvider;

impl Provider for CommandProvider {
    fn networked(&self) -> bool {
        false // a local program; what it reads is its own business
    }

    fn calendars(&self, _: &Engine, s: &Source) -> Result<Vec<Calendar>> {
        Ok(vec![calendar_of(s)])
    }

    fn events(
        &self,
        engine: &Engine,
        s: &Source,
        _: &[Calendar],
        from: DateTime<Utc>,
        to: DateTime<Utc>,
        errors: &mut Vec<Value>,
    ) -> Result<Vec<Event>> {
        read(engine, s, from, to, errors)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cal() -> Calendar {
        Calendar {
            id: "feed".into(),
            title: "feed".into(),
            account: "my-cal".into(),
            account_id: "command".into(),
            account_kind: "command".into(),
        }
    }

    fn t(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    fn tz() -> Tz {
        "Europe/Berlin".parse().unwrap()
    }

    #[test]
    fn sniffs_ics_against_json() {
        assert_eq!(
            sniff("\u{feff}  BEGIN:VCALENDAR\r\n", "auto", 40).unwrap(),
            "ics"
        );
        assert_eq!(sniff("begin:vcalendar\n", "", 40).unwrap(), "ics");
        assert_eq!(sniff(" [ ]", "auto", 40).unwrap(), "json");
        assert_eq!(sniff("{\"items\":[]}", "auto", 40).unwrap(), "json");
        assert_eq!(sniff("whatever", "json", 40).unwrap(), "json");
        assert_eq!(sniff("whatever", "ics", 40).unwrap(), "ics");
        assert_eq!(sniff("", "auto", 40).unwrap_err().code, "command_output");
        assert_eq!(
            sniff("hello", "auto", 40).unwrap_err().code,
            "command_output"
        );
    }

    #[test]
    fn google_calendar_api_events_with_cancelled_dropped() {
        let text = r#"[
          {"id": "g1", "iCalUID": "uid-1@example.com", "summary": "Planning",
           "start": {"dateTime": "2026-11-09T10:00:00+01:00"},
           "end": {"dateTime": "2026-11-09T10:30:00+01:00"},
           "location": "Room 1", "status": "confirmed",
           "attendees": [{"email": "a@example.com", "responseStatus": "accepted"},
                         {"email": "me@example.com", "self": true, "responseStatus": "declined"}]},
          {"id": "g2", "iCalUID": "uid-2@example.com", "summary": "Gone",
           "start": {"dateTime": "2026-11-09T11:00:00Z"}, "end": {"dateTime": "2026-11-09T12:00:00Z"},
           "status": "cancelled"},
          {"id": "g3", "summary": "Holiday", "start": {"date": "2026-11-10"}, "end": {"date": "2026-11-11"}}
        ]"#;
        let (evs, skipped) = parse_json(text, &cal(), tz()).unwrap();
        assert_eq!(skipped, 0);
        assert_eq!(evs.len(), 2, "{evs:?}");
        let e = &evs[0];
        assert_eq!(e.title, "Planning");
        assert_eq!(e.uid, "uid-1@example.com");
        assert_eq!(e.id, "g1");
        assert_eq!(e.start, t("2026-11-09T09:00:00Z"));
        assert_eq!(e.end, t("2026-11-09T09:30:00Z"));
        assert_eq!(e.location, "Room 1");
        assert_eq!(e.calendar, "feed");
        assert_eq!(e.attendees.len(), 2);
        assert_eq!(e.attendees[0].email, "a@example.com");
        assert!(e.declined);
        let h = &evs[1];
        assert!(h.all_day);
        assert_eq!(h.start_date.as_deref(), Some("2026-11-10"));
        assert_eq!(h.start, t("2026-11-09T23:00:00Z"));
        assert_eq!(h.id, "g3");
    }

    #[test]
    fn google_list_reply_under_items_or_events() {
        for key in ["items", "events"] {
            let text = format!(
                r#"{{"kind": "calendar#events", "{key}": [{{"summary": "A", "iCalUID": "u",
                   "start": {{"dateTime": "2026-11-09T10:00:00Z"}}, "end": {{"dateTime": "2026-11-09T11:00:00Z"}}}}]}}"#
            );
            let (evs, _) = parse_json(&text, &cal(), tz()).unwrap();
            assert_eq!(evs.len(), 1);
            assert_eq!(evs[0].title, "A");
            assert_eq!(
                evs[0].id,
                format!("u@{}", t("2026-11-09T10:00:00Z").timestamp())
            );
        }
    }

    #[test]
    fn plain_generic_events() {
        let text = r#"{"events": [
          {"title": "Standup", "start": "2026-11-09T09:00:00+01:00", "end": "2026-11-09T09:15:00+01:00",
           "uid": "s-1", "location": "Video", "attendees": ["x@example.com", {"email": "y@example.com", "name": "Y"}]},
          {"summary": "Offsite", "start": "2026-11-12", "end": "2026-11-14"},
          {"summary": "Day", "start": "2026-11-15"},
          {"title": "Dropped", "start": "2026-11-09T12:00:00Z", "status": "Cancelled"},
          {"title": "No start"},
          {"title": "Bad start", "start": "tomorrow"}
        ]}"#;
        let (evs, skipped) = parse_json(text, &cal(), tz()).unwrap();
        assert_eq!(skipped, 2);
        assert_eq!(evs.len(), 3, "{evs:?}");
        let s = &evs[0];
        assert_eq!(s.title, "Standup");
        assert_eq!(s.uid, "s-1");
        assert_eq!(s.start, t("2026-11-09T08:00:00Z"));
        assert_eq!(s.end, t("2026-11-09T08:15:00Z"));
        assert_eq!(s.location, "Video");
        assert_eq!(s.attendees[0].email, "x@example.com");
        assert_eq!(s.attendees[1].name, "Y");
        assert_eq!(s.id, format!("s-1@{}", s.start.timestamp()));
        let o = &evs[1];
        assert_eq!(o.title, "Offsite");
        assert!(o.all_day);
        assert_eq!(o.start_date.as_deref(), Some("2026-11-12"));
        assert_eq!(o.end, t("2026-11-13T23:00:00Z"));
        let d = &evs[2];
        assert_eq!(d.end - d.start, Duration::days(1));
    }

    #[test]
    fn microsoft_graph_events() {
        let text = r#"{"value": [
          {"id": "m1", "iCalUId": "ms-uid-1", "subject": "Review",
           "start": {"dateTime": "2026-11-09T14:00:00.0000000", "timeZone": "Europe/Berlin"},
           "end": {"dateTime": "2026-11-09T15:00:00.0000000", "timeZone": "Europe/Berlin"},
           "location": {"displayName": "HQ"},
           "attendees": [{"type": "required", "emailAddress": {"name": "Z", "address": "z@example.com"},
                          "status": {"response": "accepted"}}]},
          {"id": "m2", "subject": "Called off", "isCancelled": true,
           "start": {"dateTime": "2026-11-09T16:00:00", "timeZone": "UTC"},
           "end": {"dateTime": "2026-11-09T17:00:00", "timeZone": "UTC"}}
        ]}"#;
        let (evs, _) = parse_json(text, &cal(), tz()).unwrap();
        assert_eq!(evs.len(), 1);
        let e = &evs[0];
        assert_eq!(e.title, "Review");
        assert_eq!(e.uid, "ms-uid-1");
        assert_eq!(e.start, t("2026-11-09T13:00:00Z"));
        assert_eq!(e.location, "HQ");
        assert_eq!(e.attendees[0].email, "z@example.com");
        assert_eq!(e.attendees[0].status, "ACCEPTED");
    }

    #[test]
    fn json_that_is_no_event_list_is_an_error() {
        assert_eq!(
            parse_json("{\"nope\": 1}", &cal(), tz()).unwrap_err().code,
            "command_output"
        );
        assert_eq!(
            parse_json("[1, 2", &cal(), tz()).unwrap_err().code,
            "command_output"
        );
        let (one, _) = parse_json(
            r#"{"title": "Solo", "start": "2026-11-09T10:00:00Z"}"#,
            &cal(),
            tz(),
        )
        .unwrap();
        assert_eq!(one[0].title, "Solo");
    }

    #[test]
    fn overlap_window() {
        let e = Event {
            start: t("2026-11-09T10:00:00Z"),
            end: t("2026-11-09T11:00:00Z"),
            ..Event::default()
        };
        assert!(overlaps(
            &e,
            t("2026-11-09T10:30:00Z"),
            t("2026-11-09T12:00:00Z")
        ));
        assert!(!overlaps(
            &e,
            t("2026-11-09T11:00:00Z"),
            t("2026-11-09T12:00:00Z")
        ));
        assert!(!overlaps(
            &e,
            t("2026-11-09T08:00:00Z"),
            t("2026-11-09T10:00:00Z")
        ));
        let instant = Event {
            end: e.start,
            ..e.clone()
        };
        assert!(overlaps(
            &instant,
            t("2026-11-09T10:00:00Z"),
            t("2026-11-09T10:00:01Z")
        ));
    }
}
