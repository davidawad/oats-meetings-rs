//! The google provider: Google Calendar API v3 (`calendar.google_api`) with the source's OAuth
//! tokens (`oauth`). One source is one Google account; its calendars come from
//! `users/me/calendarList`, their events from `calendars/<id>/events` with `singleEvents=true`
//! (recurring events expanded by Google), paged up to `calendar.max_pages`.

use chrono::{DateTime, NaiveDate, Utc};
use chrono_tz::Tz;
use serde_json::Value;

use super::eventkit::Calendar;
use super::oauth;
use super::provider::{enc, err_json, get_json, Provider};
use super::rules::{Event, Person};
use super::Source;
use crate::config::Result;
use crate::control::engine::Engine;

pub struct Google;

fn api(engine: &Engine) -> String {
    engine
        .snapshot()
        .config
        .calendar
        .google_api
        .trim()
        .trim_end_matches('/')
        .to_string()
}

/// Every page of a Google list reply (`items`, `nextPageToken`).
fn pages(engine: &Engine, url: &str, query: &[(&str, String)], token: &str) -> Result<Vec<Value>> {
    let max = engine.snapshot().config.calendar.max_pages;
    let mut out = Vec::new();
    let mut next: Option<String> = None;
    for _ in 0..max {
        let mut q = query.to_vec();
        if let Some(p) = &next {
            q.push(("pageToken", p.clone()));
        }
        let v = get_json(engine, url, &q, token, &[])?;
        out.extend(v["items"].as_array().cloned().unwrap_or_default());
        next = v["nextPageToken"].as_str().map(str::to_string);
        if next.is_none() {
            break;
        }
    }
    Ok(out)
}

/// The address of the signed-in account: the id of its primary calendar.
pub fn account(engine: &Engine, token: &str) -> Result<String> {
    let url = format!("{}/users/me/calendarList/primary", api(engine));
    let v = get_json(engine, &url, &[], token, &[])?;
    Ok(v["id"].as_str().unwrap_or_default().to_string())
}

fn when(v: &Value, tz: Tz) -> Option<(DateTime<Utc>, Option<String>)> {
    if let Some(d) = v["dateTime"].as_str() {
        return DateTime::parse_from_rfc3339(d)
            .ok()
            .map(|d| (d.with_timezone(&Utc), None));
    }
    let day = v["date"].as_str()?;
    let date = NaiveDate::parse_from_str(day, "%Y-%m-%d").ok()?;
    let at = crate::ics::parse::resolve(tz, date.and_hms_opt(0, 0, 0)?);
    Some((at, Some(day.to_string())))
}

fn status(s: &str) -> String {
    match s {
        "needsAction" => "NEEDS-ACTION".into(),
        other => other.to_ascii_uppercase(),
    }
}

/// One `events` item as an event of calendar `cal`; all-day dates are read in `tz`.
pub fn event_of(v: &Value, cal: &Calendar, tz: Tz) -> Option<Event> {
    let (start, start_date) = when(&v["start"], tz)?;
    let end = when(&v["end"], tz).map(|w| w.0).unwrap_or(start);
    let text = |k: &str| v[k].as_str().unwrap_or_default().to_string();
    let attendees: Vec<Person> = v["attendees"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|a| a["resource"] != true)
        .map(|a| Person {
            name: a["displayName"].as_str().unwrap_or_default().to_string(),
            email: a["email"].as_str().unwrap_or_default().to_string(),
            status: status(a["responseStatus"].as_str().unwrap_or_default()),
            me: a["self"] == true,
        })
        .collect();
    let video = v["hangoutLink"].as_str().map(str::to_string).or_else(|| {
        v["conferenceData"]["entryPoints"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|e| e["entryPointType"] == "video")
            .and_then(|e| e["uri"].as_str())
            .map(str::to_string)
    });
    Some(Event {
        id: text("id"),
        uid: text("iCalUID"),
        title: text("summary"),
        start,
        end,
        all_day: start_date.is_some(),
        calendar: cal.title.clone(),
        calendar_id: cal.id.clone(),
        account: cal.account.clone(),
        location: text("location"),
        notes: text("description"),
        url: video.unwrap_or_else(|| text("htmlLink")),
        declined: attendees.iter().any(|p| p.me && p.status == "DECLINED"),
        attendees,
        cancelled: v["status"] == "cancelled",
        start_date,
        ..Event::default()
    })
}

impl Provider for Google {
    fn networked(&self) -> bool {
        true
    }

    fn calendars(&self, engine: &Engine, s: &Source) -> Result<Vec<Calendar>> {
        let t = oauth::access_token(engine, s)?;
        let page = engine.snapshot().config.calendar.page_size.min(250);
        let items = pages(
            engine,
            &format!("{}/users/me/calendarList", api(engine)),
            &[
                ("minAccessRole", "reader".into()),
                ("maxResults", page.to_string()),
            ],
            &t.access_token,
        )?;
        let account = if t.account.is_empty() {
            items
                .iter()
                .find(|c| c["primary"] == true)
                .and_then(|c| c["id"].as_str())
                .unwrap_or(&s.name)
                .to_string()
        } else {
            t.account.clone()
        };
        Ok(items
            .iter()
            .filter(|c| c["deleted"] != true)
            .filter_map(|c| {
                let id = c["id"].as_str()?;
                let title = c["summaryOverride"]
                    .as_str()
                    .or(c["summary"].as_str())
                    .unwrap_or(id);
                Some(Calendar {
                    id: id.to_string(),
                    title: title.to_string(),
                    account: account.clone(),
                    account_id: account.clone(),
                    account_kind: "google".into(),
                })
            })
            .collect())
    }

    fn events(
        &self,
        engine: &Engine,
        s: &Source,
        cals: &[Calendar],
        from: DateTime<Utc>,
        to: DateTime<Utc>,
        errors: &mut Vec<Value>,
    ) -> Result<Vec<Event>> {
        let t = oauth::access_token(engine, s)?;
        let c = engine.snapshot();
        let tz = c
            .config
            .ics
            .default_tz
            .trim()
            .parse::<Tz>()
            .unwrap_or(Tz::UTC);
        let query = [
            ("timeMin", super::stamp(from)),
            ("timeMax", super::stamp(to)),
            ("singleEvents", "true".to_string()),
            ("orderBy", "startTime".to_string()),
            ("maxResults", c.config.calendar.page_size.to_string()),
        ];
        let mut out = Vec::new();
        for cal in cals {
            let url = format!("{}/calendars/{}/events", api(engine), enc(&cal.id));
            match pages(engine, &url, &query, &t.access_token) {
                Ok(items) => out.extend(items.iter().filter_map(|v| event_of(v, cal, tz))),
                Err(e) => errors.push(serde_json::json!({
                    "source": s.name, "calendar": cal.title, "error": err_json(&e)
                })),
            }
        }
        Ok(out)
    }
}
