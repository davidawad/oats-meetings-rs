//! Which calendar event titles a recording, and the title it gets. Pure: the events come from the
//! sources (`calendar::gather`), the rules from the `calendar.*` keys.
//!
//! An event matches the span `[from, to]` (a recording start is `from == to == now`) when it starts
//! at most `calendar.match_lead` after `to` and ends after `from`. Ineligible events are dropped
//! with a reason (all-day, declined, cancelled, per the `ignore_*` keys). The rest are ranked:
//! a video-call link first (`prefer_video`), then attendees (`prefer_attendees`), then the longest
//! overlap with the span, then the start nearest `from`, then the id (so the pick is stable).

use chrono::{DateTime, Duration, FixedOffset, Utc};
use serde_json::{json, Value};

use crate::config::typed::Calendar;
use crate::config::{ConfigError, Result};

/// One attendee of an event.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Person {
    pub name: String,
    pub email: String,
    /// Upper case: `ACCEPTED`, `DECLINED`, `TENTATIVE`, `NEEDS-ACTION`, ... or empty.
    pub status: String,
    /// The calendar's owner (EventKit `isCurrentUser`, or an address in `calendar.me`).
    pub me: bool,
}

impl Person {
    /// The name, else the address.
    pub fn label(&self) -> &str {
        if self.name.trim().is_empty() {
            &self.email
        } else {
            &self.name
        }
    }

    pub fn to_json(&self) -> Value {
        json!({"name": self.name, "email": self.email, "status": self.status, "me": self.me})
    }
}

/// One event occurrence from any source.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Event {
    /// The `calendar.sources` entry it came from.
    pub source: String,
    /// The source's kind: `os-local`, `google`, `microsoft`, `caldav`, `ics` or `eventkit`.
    pub kind: String,
    /// Stable per occurrence within its source: the ICS UID or the EventKit event identifier.
    pub id: String,
    /// The iCalendar UID (Google `iCalUID`, Graph `iCalUId`, EventKit's external identifier);
    /// the same meeting through two sources shares it. Empty when the source has none.
    pub uid: String,
    pub title: String,
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    pub all_day: bool,
    /// The ICS feed name, or the EventKit calendar title.
    pub calendar: String,
    pub calendar_id: String,
    /// The EventKit account (EKSource title, often the address); empty for ICS.
    pub account: String,
    pub location: String,
    pub notes: String,
    pub url: String,
    pub attendees: Vec<Person>,
    /// The owner declined it (EventKit reports this directly).
    pub declined: bool,
    pub cancelled: bool,
    /// All-day events: their own date (`YYYY-MM-DD`), whatever zone `start` is in.
    pub start_date: Option<String>,
}

impl Event {
    pub fn to_json(&self, rules: &Rules) -> Value {
        json!({
            "source": self.source, "kind": self.kind, "id": self.id, "uid": self.uid,
            "title": self.title,
            "start": self.start.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            "end": self.end.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            "all_day": self.all_day, "calendar": self.calendar, "calendar_id": self.calendar_id,
            "account": self.account, "location": self.location, "url": self.url,
            "video_url": video_link(self, &rules.video_hosts),
            "attendees": self.attendees.iter().map(Person::to_json).collect::<Vec<_>>(),
            "declined": declined(self, rules), "cancelled": self.cancelled,
            "start_date": self.start_date,
        })
    }
}

/// The `calendar.*` matching keys.
#[derive(Clone, Debug, PartialEq)]
pub struct Rules {
    pub lead: Duration,
    pub prefer_video: bool,
    pub prefer_attendees: bool,
    pub ignore_all_day: bool,
    pub ignore_declined: bool,
    pub ignore_cancelled: bool,
    /// Lower case.
    pub video_hosts: Vec<String>,
    /// Lower case.
    pub me: Vec<String>,
}

fn list(s: &str) -> Vec<String> {
    s.split(',')
        .map(|x| x.trim().to_ascii_lowercase())
        .filter(|x| !x.is_empty())
        .collect()
}

impl Rules {
    pub fn from_config(c: &Calendar) -> Rules {
        Rules {
            lead: Duration::milliseconds(i64::try_from(c.match_lead.0).unwrap_or(i64::MAX)),
            prefer_video: c.prefer_video,
            prefer_attendees: c.prefer_attendees,
            ignore_all_day: c.ignore_all_day,
            ignore_declined: c.ignore_declined,
            ignore_cancelled: c.ignore_cancelled,
            video_hosts: list(&c.video_hosts),
            me: list(&c.me),
        }
    }

    fn is_me(&self, p: &Person) -> bool {
        p.me || self.me.contains(&p.email.trim().to_ascii_lowercase())
    }
}

/// The host of an http(s) URL, lower case.
fn host_of(url: &str) -> Option<String> {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))?;
    let host = rest.split(['/', '?', '#', ':']).next()?;
    Some(host.to_ascii_lowercase())
}

fn host_matches(host: &str, hosts: &[String]) -> bool {
    hosts
        .iter()
        .any(|h| host == h || host.ends_with(&format!(".{h}")))
}

/// The first video-call link in the event's URL, location or notes.
pub fn video_link(e: &Event, hosts: &[String]) -> Option<String> {
    for text in [&e.url, &e.location, &e.notes] {
        for word in
            text.split(|c: char| c.is_whitespace() || matches!(c, '<' | '>' | '"' | '(' | ')'))
        {
            let at = word.find("https://").or_else(|| word.find("http://"));
            let Some(at) = at else { continue };
            let url = word[at..].trim_end_matches(['.', ',', ';']);
            if host_of(url).is_some_and(|h| host_matches(&h, hosts)) {
                return Some(url.to_string());
            }
        }
    }
    None
}

/// The owner declined: EventKit says so, or an attendee that is you has PARTSTAT=DECLINED.
pub fn declined(e: &Event, r: &Rules) -> bool {
    e.declined
        || e.attendees
            .iter()
            .any(|a| r.is_me(a) && a.status == "DECLINED")
}

/// The attendees other than you, as participant names (the OpenOats export's participants).
pub fn participants(e: &Event, r: &Rules) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for a in e.attendees.iter().filter(|a| !r.is_me(a)) {
        let l = a.label().trim();
        if !l.is_empty() && !out.iter().any(|x| x == l) {
            out.push(l.to_string());
        }
    }
    out
}

/// Why `e` may not title a recording, or None when it may.
pub fn ineligible(e: &Event, r: &Rules) -> Option<&'static str> {
    if r.ignore_all_day && e.all_day {
        return Some("all_day");
    }
    if r.ignore_cancelled && e.cancelled {
        return Some("cancelled");
    }
    if r.ignore_declined && declined(e, r) {
        return Some("declined");
    }
    None
}

/// `e` overlaps the span `[from, to]`, counting `lead` before its start.
pub fn overlaps(e: &Event, from: DateTime<Utc>, to: DateTime<Utc>, r: &Rules) -> bool {
    e.start - r.lead <= to && e.end > from
}

/// The result of a match: the pick (an index into the events given) and every overlapping
/// candidate with why it lost (`picked`, `ranked_lower`, or an `ineligible` reason).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Match {
    pub picked: Option<usize>,
    pub candidates: Vec<(usize, &'static str)>,
}

pub fn pick(events: &[Event], from: DateTime<Utc>, to: DateTime<Utc>, r: &Rules) -> Match {
    let mut ok: Vec<usize> = Vec::new();
    let mut out = Match::default();
    for (i, e) in events.iter().enumerate() {
        if !overlaps(e, from, to, r) {
            continue;
        }
        match ineligible(e, r) {
            Some(why) => out.candidates.push((i, why)),
            None => ok.push(i),
        }
    }
    let key = |i: &usize| {
        let e = &events[*i];
        let video = r.prefer_video && video_link(e, &r.video_hosts).is_some();
        let people = r.prefer_attendees && e.attendees.iter().any(|a| !r.is_me(a));
        let overlap = (e.end.min(to) - e.start.max(from)).max(Duration::zero());
        let near = (e.start - from).abs();
        (
            std::cmp::Reverse(video),
            std::cmp::Reverse(people),
            std::cmp::Reverse(overlap),
            near,
            e.source.clone(),
            e.id.clone(),
        )
    };
    ok.sort_by_key(key);
    out.picked = ok.first().copied();
    for (n, i) in ok.into_iter().enumerate() {
        out.candidates
            .push((i, if n == 0 { "picked" } else { "ranked_lower" }));
    }
    out.candidates.sort_by_key(|(i, _)| *i);
    out
}

// ---- title template ------------------------------------------------------------------------

const EVENT_VARS: &[&str] = &[
    "event.title",
    "event.calendar",
    "event.account",
    "event.location",
    "event.start",
];
const TIME_VARS: &[&str] = &["date", "time", "weekday"];

/// Split a template into literal text and `{var}` names; `{{` and `}}` are literal braces.
fn tokens(t: &str) -> std::result::Result<Vec<(bool, String)>, String> {
    let mut out = Vec::new();
    let mut lit = String::new();
    let mut chars = t.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '{' if chars.peek() == Some(&'{') => {
                chars.next();
                lit.push('{');
            }
            '}' if chars.peek() == Some(&'}') => {
                chars.next();
                lit.push('}');
            }
            '{' => {
                let mut name = String::new();
                loop {
                    match chars.next() {
                        Some('}') => break,
                        Some(c) => name.push(c),
                        None => return Err(format!("unclosed '{{' in {t:?}")),
                    }
                }
                out.push((false, std::mem::take(&mut lit)));
                out.push((true, name.trim().to_string()));
            }
            '}' => return Err(format!("stray '}}' in {t:?} (write '}}}}')")),
            c => lit.push(c),
        }
    }
    out.push((false, lit));
    Ok(out)
}

/// A template is well formed and names only known variables (`with_event`: the event ones too).
pub fn check_template(key: &str, t: &str, with_event: bool) -> Result<()> {
    let bad = |m: String| ConfigError::new("invalid_value", format!("{key}: {m}"));
    for (var, name) in tokens(t).map_err(bad)? {
        let known = TIME_VARS.contains(&name.as_str())
            || (with_event && EVENT_VARS.contains(&name.as_str()));
        if var && !known {
            let mut vars: Vec<&str> = TIME_VARS.to_vec();
            if with_event {
                vars.extend(EVENT_VARS);
            }
            return Err(bad(format!(
                "unknown variable {{{name}}} (known: {})",
                vars.join(", ")
            )));
        }
    }
    Ok(())
}

/// Fill a checked template; `at` is the recording start on the display clock. Whitespace runs
/// collapse and the ends are trimmed, so an empty variable leaves no gap.
pub fn render(t: &str, e: Option<&Event>, at: DateTime<FixedOffset>) -> String {
    let Ok(parts) = tokens(t) else {
        return String::new();
    };
    let mut s = String::new();
    for (var, text) in parts {
        if !var {
            s.push_str(&text);
            continue;
        }
        let v = match (text.as_str(), e) {
            ("date", _) => at.format("%Y-%m-%d").to_string(),
            ("time", _) => at.format("%H:%M").to_string(),
            ("weekday", _) => at.format("%A").to_string(),
            ("event.title", Some(e)) => e.title.clone(),
            ("event.calendar", Some(e)) => e.calendar.clone(),
            ("event.account", Some(e)) => e.account.clone(),
            ("event.location", Some(e)) => e.location.clone(),
            ("event.start", Some(e)) => e
                .start
                .with_timezone(at.offset())
                .format("%H:%M")
                .to_string(),
            _ => String::new(),
        };
        s.push_str(&v);
    }
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}
