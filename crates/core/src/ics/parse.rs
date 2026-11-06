//! ICS parsing and recurrence expansion.
//!
//! The syntax layer is the `icalendar` crate's parser (unfolding, components, properties); the
//! recurrence maths is the `rrule` crate. Time zones are `chrono-tz`.
//!
//! DST handling: every recurring event is expanded in WALL-CLOCK time (a naive local datetime
//! labelled UTC for the `rrule` crate, which then never meets a DST rule), and each occurrence is
//! mapped back to an instant through the event's own zone. So "09:00 America/New_York every week"
//! stays 09:00 local across a transition. A wall time that does not exist (spring-forward gap)
//! is read with the offset from before the gap; an ambiguous one (fall-back) takes its first
//! occurrence, as RFC 5545 section 3.3.5 says.

use chrono::{
    DateTime, Duration, LocalResult, NaiveDate, NaiveDateTime, SecondsFormat, TimeZone, Utc,
};
use chrono_tz::Tz;
use icalendar::parser::{Parameter, Property};
use serde_json::{json, Value};

use crate::config::{ConfigError, Result};

#[derive(Clone, Debug)]
pub struct ParseOpts {
    pub default_tz: Tz,
    /// Unknown TZID falls back to `default_tz` instead of failing the event.
    pub tz_fallback: bool,
    /// A malformed event is dropped and counted instead of failing the feed.
    pub skip_bad: bool,
    pub show_cancelled: bool,
    pub include_all_day: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum When {
    Date(NaiveDate),
    Wall(NaiveDateTime, Tz),
}

#[derive(Clone, Debug)]
pub struct RawEvent {
    pub uid: String,
    pub summary: String,
    pub location: Option<String>,
    pub description: Option<String>,
    pub status: String,
    pub start: When,
    pub end: Option<When>,
    pub duration: Option<i64>,
    pub rrule: Option<String>,
    pub rdates: Vec<When>,
    pub exdates: Vec<When>,
    pub recurrence_id: Option<When>,
    pub url: Option<String>,
    pub attendees: Vec<Attendee>,
}

/// One ATTENDEE: its CN, its address (the `mailto:` stripped) and its PARTSTAT (upper case, empty
/// when absent).
#[derive(Clone, Debug, PartialEq)]
pub struct Attendee {
    pub name: String,
    pub email: String,
    pub status: String,
}

fn attendee_of(p: &Property<'_>) -> Attendee {
    let v = p.val.as_str().trim();
    let email = match v.get(..7) {
        Some(head) if head.eq_ignore_ascii_case("mailto:") => &v[7..],
        _ => v,
    };
    Attendee {
        name: param(&p.params, "CN").unwrap_or_default().to_string(),
        email: email.to_string(),
        status: param(&p.params, "PARTSTAT")
            .unwrap_or_default()
            .to_ascii_uppercase(),
    }
}

#[derive(Clone, Debug, Default)]
pub struct Parsed {
    pub events: Vec<RawEvent>,
    /// Malformed events dropped because `on_bad_event = skip`.
    pub skipped: usize,
}

fn bad_event(msg: impl Into<String>) -> ConfigError {
    ConfigError::new("ics_bad_event", msg)
}

/// Wall time -> instant in `tz` (see the module note for gaps and overlaps).
pub fn resolve(tz: Tz, n: NaiveDateTime) -> DateTime<Utc> {
    match tz.from_local_datetime(&n) {
        LocalResult::Single(d) => d.with_timezone(&Utc),
        LocalResult::Ambiguous(first, _) => first.with_timezone(&Utc),
        LocalResult::None => {
            let shifted = n + Duration::hours(1);
            tz.from_local_datetime(&shifted)
                .earliest()
                .map(|d| d.with_timezone(&Utc))
                .unwrap_or_else(|| Utc.from_utc_datetime(&n))
        }
    }
}

fn midnight(d: NaiveDate) -> NaiveDateTime {
    d.and_hms_opt(0, 0, 0).expect("midnight exists")
}

fn param<'b>(params: &'b [Parameter<'_>], key: &str) -> Option<&'b str> {
    params
        .iter()
        .find(|p| p.key.as_str().eq_ignore_ascii_case(key))
        .and_then(|p| p.val.as_ref())
        .map(|v| v.as_str().trim_matches('"'))
}

fn when_from(val: &str, params: &[Parameter<'_>], o: &ParseOpts) -> Result<When> {
    let v = val.trim();
    let value_date = param(params, "VALUE").map_or(false, |x| x.eq_ignore_ascii_case("DATE"));
    if value_date || (v.len() == 8 && !v.contains('T')) {
        return NaiveDate::parse_from_str(v, "%Y%m%d")
            .map(When::Date)
            .map_err(|_| bad_event(format!("invalid date '{v}'")));
    }
    let (core, zulu) = match v.strip_suffix('Z').or_else(|| v.strip_suffix('z')) {
        Some(c) => (c, true),
        None => (v, false),
    };
    let n = NaiveDateTime::parse_from_str(core, "%Y%m%dT%H%M%S")
        .map_err(|_| bad_event(format!("invalid date-time '{v}'")))?;
    let tz = if zulu {
        Tz::UTC
    } else if let Some(id) = param(params, "TZID") {
        match id.parse::<Tz>() {
            Ok(t) => t,
            Err(_) if o.tz_fallback => o.default_tz,
            Err(_) => {
                return Err(ConfigError::new(
                    "ics_bad_tz",
                    format!("unknown TZID '{id}'"),
                ))
            }
        }
    } else {
        o.default_tz
    };
    Ok(When::Wall(n, tz))
}

fn whens_from(p: &Property<'_>, o: &ParseOpts, out: &mut Vec<When>) -> Result<()> {
    for part in p.val.as_str().split(',').filter(|s| !s.trim().is_empty()) {
        out.push(when_from(part, &p.params, o)?);
    }
    Ok(())
}

/// RFC 5545 DURATION (`P1DT2H30M`, `PT45M`, `P2W`, optional sign) in seconds.
pub fn parse_duration(s: &str) -> Option<i64> {
    let s = s.trim();
    let (neg, s) = match s.strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, s.strip_prefix('+').unwrap_or(s)),
    };
    let s = s.strip_prefix('P')?;
    let mut total = 0i64;
    let mut num = String::new();
    let mut in_time = false;
    for c in s.chars() {
        match c {
            'T' => in_time = true,
            '0'..='9' => num.push(c),
            'W' | 'D' | 'H' | 'M' | 'S' => {
                let n: i64 = num.parse().ok()?;
                num.clear();
                let unit = match (c, in_time) {
                    ('W', _) => 604_800,
                    ('D', _) => 86_400,
                    ('H', _) => 3_600,
                    ('M', true) => 60,
                    ('S', _) => 1,
                    _ => return None,
                };
                total += n * unit;
            }
            _ => return None,
        }
    }
    if !num.is_empty() {
        return None;
    }
    Some(if neg { -total } else { total })
}

fn zone_of(w: &When) -> Tz {
    match w {
        When::Date(_) => Tz::UTC,
        When::Wall(_, z) => *z,
    }
}

/// The wall-clock reading of `w` in `tz`.
fn wall_in(w: &When, tz: Tz) -> NaiveDateTime {
    match w {
        When::Date(d) => midnight(*d),
        When::Wall(n, z) => resolve(*z, *n).with_timezone(&tz).naive_local(),
    }
}

fn fix_until(rule: &str, tz: Tz, all_day: bool) -> std::result::Result<String, String> {
    let mut parts: Vec<String> = Vec::new();
    for part in rule.split(';') {
        let up = part.to_ascii_uppercase();
        if let Some(v) = up.strip_prefix("UNTIL=") {
            let wall = if let Some(core) = v.strip_suffix('Z') {
                let n = NaiveDateTime::parse_from_str(core, "%Y%m%dT%H%M%S")
                    .map_err(|_| format!("invalid UNTIL '{v}'"))?;
                if all_day {
                    n
                } else {
                    Utc.from_utc_datetime(&n).with_timezone(&tz).naive_local()
                }
            } else if v.len() == 8 {
                NaiveDate::parse_from_str(v, "%Y%m%d")
                    .map_err(|_| format!("invalid UNTIL '{v}'"))?
                    .and_hms_opt(23, 59, 59)
                    .expect("end of day exists")
            } else {
                NaiveDateTime::parse_from_str(v, "%Y%m%dT%H%M%S")
                    .map_err(|_| format!("invalid UNTIL '{v}'"))?
            };
            parts.push(format!("UNTIL={}Z", wall.format("%Y%m%dT%H%M%S")));
        } else {
            parts.push(part.to_string());
        }
    }
    Ok(parts.join(";"))
}

/// The `rrule` crate input, written in wall-clock time (see the module note).
fn rule_text(ev: &RawEvent) -> std::result::Result<String, String> {
    let tz = zone_of(&ev.start);
    let all_day = matches!(ev.start, When::Date(_));
    let fmt = |n: NaiveDateTime| n.format("%Y%m%dT%H%M%SZ").to_string();
    let mut s = format!("DTSTART:{}", fmt(wall_in(&ev.start, tz)));
    if let Some(r) = &ev.rrule {
        s.push_str("\nRRULE:");
        s.push_str(&fix_until(r, tz, all_day)?);
    }
    if !ev.exdates.is_empty() {
        let list: Vec<String> = ev.exdates.iter().map(|w| fmt(wall_in(w, tz))).collect();
        s.push_str("\nEXDATE:");
        s.push_str(&list.join(","));
    }
    if ev.rrule.is_some() && !ev.rdates.is_empty() {
        let list: Vec<String> = ev.rdates.iter().map(|w| fmt(wall_in(w, tz))).collect();
        s.push_str("\nRDATE:");
        s.push_str(&list.join(","));
    }
    Ok(s)
}

fn rule_set(ev: &RawEvent) -> Result<rrule::RRuleSet> {
    let text =
        rule_text(ev).map_err(|e| ConfigError::new("ics_bad_rrule", format!("{}: {e}", ev.uid)))?;
    text.parse::<rrule::RRuleSet>()
        .map_err(|e| ConfigError::new("ics_bad_rrule", format!("{}: {e}", ev.uid)))
}

fn build_event(
    comp: &icalendar::parser::Component<'_>,
    idx: usize,
    o: &ParseOpts,
) -> Result<RawEvent> {
    let find = |n: &str| {
        comp.properties
            .iter()
            .find(|p| p.name.as_str().eq_ignore_ascii_case(n))
    };
    let text = |n: &str| find(n).map(|p| p.val.clone().unescape_text().as_str().to_string());
    let uid = find("UID")
        .map(|p| p.val.as_str().trim().to_string())
        .filter(|u| !u.is_empty())
        .unwrap_or_else(|| format!("no-uid-{idx}"));
    let dt = find("DTSTART").ok_or_else(|| bad_event(format!("{uid}: VEVENT without DTSTART")))?;
    let start = when_from(dt.val.as_str(), &dt.params, o)
        .map_err(|e| ConfigError::new(e.code, format!("{uid}: {}", e.message)))?;
    let end = match find("DTEND") {
        Some(p) => Some(
            when_from(p.val.as_str(), &p.params, o)
                .map_err(|e| ConfigError::new(e.code, format!("{uid}: {}", e.message)))?,
        ),
        None => None,
    };
    let duration =
        match find("DURATION") {
            Some(p) => Some(parse_duration(p.val.as_str()).ok_or_else(|| {
                bad_event(format!("{uid}: invalid DURATION '{}'", p.val.as_str()))
            })?),
            None => None,
        };
    let mut rdates = Vec::new();
    let mut exdates = Vec::new();
    let mut attendees = Vec::new();
    for p in &comp.properties {
        let n = p.name.as_str();
        if n.eq_ignore_ascii_case("ATTENDEE") {
            attendees.push(attendee_of(p));
        }
        let wrap = |e: ConfigError| ConfigError::new(e.code, format!("{uid}: {}", e.message));
        if n.eq_ignore_ascii_case("RDATE") {
            whens_from(p, o, &mut rdates).map_err(wrap)?;
        } else if n.eq_ignore_ascii_case("EXDATE") {
            whens_from(p, o, &mut exdates).map_err(wrap)?;
        }
    }
    let recurrence_id = match find("RECURRENCE-ID") {
        Some(p) => Some(
            when_from(p.val.as_str(), &p.params, o)
                .map_err(|e| ConfigError::new(e.code, format!("{uid}: {}", e.message)))?,
        ),
        None => None,
    };
    let ev = RawEvent {
        uid,
        summary: text("SUMMARY").unwrap_or_default(),
        location: text("LOCATION").filter(|s| !s.is_empty()),
        description: text("DESCRIPTION").filter(|s| !s.is_empty()),
        status: find("STATUS")
            .map(|p| p.val.as_str().trim().to_ascii_uppercase())
            .unwrap_or_default(),
        start,
        end,
        duration,
        rrule: find("RRULE").map(|p| p.val.as_str().trim().to_string()),
        rdates,
        exdates,
        recurrence_id,
        url: find("URL")
            .map(|p| p.val.as_str().trim().to_string())
            .filter(|s| !s.is_empty()),
        attendees,
    };
    if ev.rrule.is_some() {
        rule_set(&ev)?; // a bad RRULE fails at refresh time, not on a later query
    }
    Ok(ev)
}

pub fn parse_feed(text: &str, o: &ParseOpts) -> Result<Parsed> {
    let text = text.trim_start_matches('\u{feff}');
    let head_ok = text
        .trim_start()
        .get(..15)
        .map_or(false, |h| h.eq_ignore_ascii_case("BEGIN:VCALENDAR"));
    if !head_ok {
        return Err(ConfigError::new(
            "ics_not_calendar",
            "the body does not start with BEGIN:VCALENDAR",
        ));
    }
    let unfolded = icalendar::parser::unfold(text);
    let cal = icalendar::parser::read_calendar(&unfolded)
        .map_err(|e| ConfigError::new("ics_parse_error", e))?;
    let mut out = Parsed::default();
    for (idx, comp) in cal
        .components
        .iter()
        .filter(|c| c.name.as_str().eq_ignore_ascii_case("VEVENT"))
        .enumerate()
    {
        match build_event(comp, idx, o) {
            Ok(ev) => out.events.push(ev),
            Err(_) if o.skip_bad => out.skipped += 1,
            Err(e) => return Err(e),
        }
    }
    Ok(out)
}

fn rfc3339(d: DateTime<Utc>) -> String {
    d.to_rfc3339_opts(SecondsFormat::Secs, true)
}

fn instant_of(w: &When, o: &ParseOpts) -> DateTime<Utc> {
    match w {
        When::Date(d) => resolve(o.default_tz, midnight(*d)),
        When::Wall(n, z) => resolve(*z, *n),
    }
}

/// Expand the feed into the events overlapping `[from, to)`, sorted by start.
pub fn expand(
    feed: &str,
    p: &Parsed,
    o: &ParseOpts,
    max_occ: u16,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
) -> Result<Vec<Value>> {
    // Occurrences replaced by a RECURRENCE-ID override.
    let overridden = |uid: &str, at: DateTime<Utc>| {
        p.events.iter().any(|e| {
            e.uid == uid
                && e.recurrence_id
                    .as_ref()
                    .map_or(false, |r| instant_of(r, o) == at)
        })
    };
    let mut out: Vec<(DateTime<Utc>, Value)> = Vec::new();
    for ev in &p.events {
        if ev.status == "CANCELLED" && !o.show_cancelled {
            continue;
        }
        let all_day = matches!(ev.start, When::Date(_));
        if all_day && !o.include_all_day {
            continue;
        }
        let tz = if all_day {
            o.default_tz
        } else {
            zone_of(&ev.start)
        };
        let start0 = instant_of(&ev.start, o);
        let span = if all_day {
            let days = match (&ev.end, ev.duration) {
                (Some(When::Date(e)), _) => {
                    let s = match ev.start {
                        When::Date(s) => s,
                        _ => unreachable!("all_day implies a date start"),
                    };
                    (*e - s).num_days().max(1)
                }
                (_, Some(secs)) => ((secs + 86_399) / 86_400).max(1),
                _ => 1,
            };
            (days, Duration::days(days))
        } else {
            let end0 = match (&ev.end, ev.duration) {
                (Some(e), _) => instant_of(e, o),
                (None, Some(secs)) => start0 + Duration::seconds(secs),
                (None, None) => start0,
            };
            let d = (end0 - start0).max(Duration::zero());
            (0, d)
        };
        // Wall-clock starts of every occurrence that might touch the window.
        let walls: Vec<NaiveDateTime> = if ev.rrule.is_some() {
            let set = rule_set(ev)?;
            let wtz = zone_of(&ev.start);
            let lo = (from - span.1 - Duration::days(2))
                .with_timezone(&wtz)
                .naive_local();
            let hi = (to + Duration::days(2)).with_timezone(&wtz).naive_local();
            let set = set
                .after(rrule::Tz::UTC.from_utc_datetime(&lo))
                .before(rrule::Tz::UTC.from_utc_datetime(&hi));
            set.all(max_occ)
                .dates
                .iter()
                .map(|d| d.naive_local())
                .collect()
        } else {
            let wtz = zone_of(&ev.start);
            let mut v = vec![wall_in(&ev.start, wtz)];
            v.extend(ev.rdates.iter().map(|w| wall_in(w, wtz)));
            let ex: Vec<NaiveDateTime> = ev.exdates.iter().map(|w| wall_in(w, wtz)).collect();
            v.retain(|w| !ex.contains(w));
            v.sort();
            v.dedup();
            v
        };
        let recurring = ev.rrule.is_some() || !ev.rdates.is_empty();
        let wtz = zone_of(&ev.start);
        for wall in walls {
            let s = if all_day {
                resolve(o.default_tz, wall)
            } else {
                resolve(wtz, wall)
            };
            if recurring && overridden(&ev.uid, s) {
                continue;
            }
            let e = if all_day {
                resolve(o.default_tz, wall + span.1)
            } else {
                s + span.1
            };
            let overlaps = s < to && (e > from || (e == s && s >= from));
            if !overlaps {
                continue;
            }
            let mut v = json!({
                "feed": feed,
                "uid": ev.uid,
                "summary": ev.summary,
                "location": ev.location,
                "description": ev.description,
                "status": ev.status,
                "all_day": all_day,
                "start": rfc3339(s),
                "end": rfc3339(e),
                "tzid": tz.name(),
                "local_start": wall.format("%Y-%m-%dT%H:%M:%S").to_string(),
                "recurring": recurring,
                "override": ev.recurrence_id.is_some(),
                "url": ev.url,
                "attendees": ev.attendees.iter().map(|a| json!({
                    "name": a.name, "email": a.email, "status": a.status,
                })).collect::<Vec<_>>(),
            });
            if all_day {
                v["start_date"] = json!(wall.date().format("%Y-%m-%d").to_string());
                v["end_date"] = json!((wall + span.1).date().format("%Y-%m-%d").to_string());
            }
            out.push((s, v));
        }
    }
    out.sort_by(|a, b| {
        a.0.cmp(&b.0)
            .then_with(|| a.1["uid"].as_str().cmp(&b.1["uid"].as_str()))
    });
    Ok(out.into_iter().map(|(_, v)| v).collect())
}
