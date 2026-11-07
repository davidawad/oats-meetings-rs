//! The microsoft provider: Microsoft Graph (`calendar.microsoft_api`) with the source's OAuth
//! tokens (`oauth`). One source is one Microsoft 365 / Outlook.com account; its calendars come
//! from `/me/calendars`, their events from `/me/calendars/<id>/calendarView` (occurrences
//! expanded by Graph, times in UTC through `Prefer: outlook.timezone="UTC"`), following
//! `@odata.nextLink` up to `calendar.max_pages`.

use chrono::{DateTime, NaiveDateTime, Utc};
use chrono_tz::Tz;
use serde_json::Value;

use super::eventkit::Calendar;
use super::oauth;
use super::provider::{enc, err_json, get_json, Provider};
use super::rules::{Event, Person};
use super::Source;
use crate::config::Result;
use crate::control::engine::Engine;

pub struct Graph;

const PREFER_UTC: (&str, &str) = ("Prefer", "outlook.timezone=\"UTC\"");

fn api(engine: &Engine) -> String {
    engine
        .snapshot()
        .config
        .calendar
        .microsoft_api
        .trim()
        .trim_end_matches('/')
        .to_string()
}

/// Every page of a Graph collection (`value`, `@odata.nextLink`).
fn pages(engine: &Engine, url: &str, query: &[(&str, String)], token: &str) -> Result<Vec<Value>> {
    let max = engine.snapshot().config.calendar.max_pages;
    let mut out = Vec::new();
    let mut url = url.to_string();
    let mut query = query.to_vec();
    for _ in 0..max {
        let v = get_json(engine, &url, &query, token, &[PREFER_UTC])?;
        out.extend(v["value"].as_array().cloned().unwrap_or_default());
        match v["@odata.nextLink"].as_str() {
            // The link carries the whole query already.
            Some(next) => {
                url = next.to_string();
                query.clear();
            }
            None => break,
        }
    }
    Ok(out)
}

/// The address of the signed-in account.
pub fn account(engine: &Engine, token: &str) -> Result<String> {
    let url = format!("{}/me", api(engine));
    let v = get_json(
        engine,
        &url,
        &[("$select", "mail,userPrincipalName".into())],
        token,
        &[],
    )?;
    Ok(v["mail"]
        .as_str()
        .filter(|m| !m.is_empty())
        .or(v["userPrincipalName"].as_str())
        .unwrap_or_default()
        .to_string())
}

/// A Graph `dateTimeTimeZone`: a wall time and its zone (UTC under our Prefer header).
fn when(v: &Value) -> Option<DateTime<Utc>> {
    let wall = v["dateTime"].as_str()?;
    let n = NaiveDateTime::parse_from_str(wall, "%Y-%m-%dT%H:%M:%S%.f").ok()?;
    let tz = v["timeZone"].as_str().unwrap_or("UTC");
    Some(match tz.parse::<Tz>() {
        Ok(z) => crate::ics::parse::resolve(z, n),
        Err(_) => n.and_utc(),
    })
}

fn status(s: &str) -> String {
    match s {
        "accepted" | "organizer" => "ACCEPTED".into(),
        "tentativelyAccepted" => "TENTATIVE".into(),
        "declined" => "DECLINED".into(),
        "notResponded" => "NEEDS-ACTION".into(),
        _ => String::new(),
    }
}

/// One `calendarView` item as an event of calendar `cal`.
pub fn event_of(v: &Value, cal: &Calendar) -> Option<Event> {
    let start = when(&v["start"])?;
    let end = when(&v["end"]).unwrap_or(start);
    let text = |k: &str| v[k].as_str().unwrap_or_default().to_string();
    let all_day = v["isAllDay"] == true;
    let attendees: Vec<Person> = v["attendees"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|a| a["type"] != "resource")
        .map(|a| Person {
            name: a["emailAddress"]["name"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
            email: a["emailAddress"]["address"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
            status: status(a["status"]["response"].as_str().unwrap_or_default()),
            me: false,
        })
        .collect();
    let video = v["onlineMeeting"]["joinUrl"]
        .as_str()
        .or(v["onlineMeetingUrl"].as_str())
        .filter(|u| !u.is_empty())
        .map(str::to_string);
    Some(Event {
        id: text("id"),
        uid: text("iCalUId"),
        title: text("subject"),
        start,
        end,
        all_day,
        calendar: cal.title.clone(),
        calendar_id: cal.id.clone(),
        account: cal.account.clone(),
        location: v["location"]["displayName"]
            .as_str()
            .unwrap_or_default()
            .to_string(),
        notes: text("bodyPreview"),
        url: video.unwrap_or_else(|| text("webLink")),
        attendees,
        declined: v["responseStatus"]["response"] == "declined",
        cancelled: v["isCancelled"] == true,
        start_date: all_day.then(|| start.format("%Y-%m-%d").to_string()),
        ..Event::default()
    })
}

impl Provider for Graph {
    fn networked(&self) -> bool {
        true
    }

    fn calendars(&self, engine: &Engine, s: &Source) -> Result<Vec<Calendar>> {
        let t = oauth::access_token(engine, s)?;
        let page = engine.snapshot().config.calendar.page_size;
        let items = pages(
            engine,
            &format!("{}/me/calendars", api(engine)),
            &[
                ("$select", "id,name,owner".into()),
                ("$top", page.to_string()),
            ],
            &t.access_token,
        )?;
        Ok(items
            .iter()
            .filter_map(|c| {
                let id = c["id"].as_str()?;
                let owner = c["owner"]["address"].as_str().unwrap_or_default();
                let account = if t.account.is_empty() {
                    owner.to_string()
                } else {
                    t.account.clone()
                };
                Some(Calendar {
                    id: id.to_string(),
                    title: c["name"].as_str().unwrap_or(id).to_string(),
                    account: account.clone(),
                    account_id: account,
                    account_kind: "microsoft".into(),
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
        let page = engine.snapshot().config.calendar.page_size;
        let query = [
            ("startDateTime", super::stamp(from)),
            ("endDateTime", super::stamp(to)),
            ("$top", page.to_string()),
            ("$select", "id,iCalUId,subject,start,end,isAllDay,isCancelled,responseStatus,attendees,location,bodyPreview,webLink,onlineMeeting,onlineMeetingUrl".to_string()),
        ];
        let mut out = Vec::new();
        for cal in cals {
            let url = format!("{}/me/calendars/{}/calendarView", api(engine), enc(&cal.id));
            match pages(engine, &url, &query, &t.access_token) {
                Ok(items) => out.extend(items.iter().filter_map(|v| event_of(v, cal))),
                Err(e) => errors.push(serde_json::json!({
                    "source": s.name, "calendar": cal.title, "error": err_json(&e)
                })),
            }
        }
        Ok(out)
    }
}
