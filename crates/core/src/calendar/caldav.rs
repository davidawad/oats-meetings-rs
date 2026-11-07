//! The caldav provider: any CalDAV server (iCloud, Fastmail, Nextcloud, Proton Bridge, Radicale,
//! ...) from one URL. Discovery (RFC 6764 / RFC 4791): `PROPFIND` the URL for
//! `current-user-principal` (falling back to `/.well-known/caldav`), the principal for
//! `calendar-home-set`, then the home (Depth 1) for its calendar collections. Each calendar's
//! events are one `REPORT calendar-query` with a time range; the `calendar-data` replies are
//! merged and expanded by the ics parser, so recurrence works as for ICS feeds.
//!
//! Credentials: `username` and `password`, the password a `secret:` reference (an app password
//! in the secrets backend), sent as HTTP Basic. Redirects are followed by hand so a PROPFIND stays
//! a PROPFIND (iCloud redirects to a partition host); hops count against `calendar.max_pages`.

use base64::Engine as _;
use chrono::{DateTime, Utc};
use serde_json::{json, Value};

use super::eventkit::Calendar;
use super::provider::{
    agent, body_text, err_json, ics_text_events, status_error, unreachable, Provider,
};
use super::rules::Event;
use super::Source;
use crate::config::{ConfigError, Result};
use crate::control::engine::Engine;
use crate::ics::fetch::{calendar_data, find_close, find_tag, merge_calendars, xml_text};

pub struct CalDav;

const PRINCIPAL: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<D:propfind xmlns:D="DAV:"><D:prop><D:current-user-principal/><D:resourcetype/></D:prop></D:propfind>
"#;

const HOME: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<D:propfind xmlns:D="DAV:" xmlns:C="urn:ietf:params:xml:ns:caldav"><D:prop><C:calendar-home-set/></D:prop></D:propfind>
"#;

const COLLECTIONS: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<D:propfind xmlns:D="DAV:" xmlns:C="urn:ietf:params:xml:ns:caldav"><D:prop><D:resourcetype/><D:displayname/><C:supported-calendar-component-set/></D:prop></D:propfind>
"#;

fn query(from: DateTime<Utc>, to: DateTime<Utc>) -> String {
    let f = |d: DateTime<Utc>| d.format("%Y%m%dT%H%M%SZ").to_string();
    format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<C:calendar-query xmlns:D="DAV:" xmlns:C="urn:ietf:params:xml:ns:caldav">
  <D:prop><D:getetag/><C:calendar-data/></D:prop>
  <C:filter><C:comp-filter name="VCALENDAR"><C:comp-filter name="VEVENT"><C:time-range start="{}" end="{}"/></C:comp-filter></C:comp-filter></C:filter>
</C:calendar-query>
"#,
        f(from),
        f(to)
    )
}

// ---- XML -------------------------------------------------------------------------------------

/// The inner text of each `name` element (any namespace prefix), in order.
pub fn elements<'a>(xml: &'a str, name: &str) -> Vec<&'a str> {
    let mut out = Vec::new();
    let mut rest = xml;
    while let Some(open) = find_tag(rest, name) {
        let after = &rest[open..];
        let Some(gt) = after.find('>') else { break };
        if after[..gt].ends_with('/') {
            out.push("");
            rest = &after[gt + 1..];
            continue;
        }
        let body = &after[gt + 1..];
        let Some(close) = find_close(body, name) else {
            break;
        };
        out.push(&body[..close]);
        rest = &body[close..];
        rest = rest.find('>').map_or("", |i| &rest[i + 1..]);
    }
    out
}

fn first_text(xml: &str, name: &str) -> Option<String> {
    elements(xml, name)
        .first()
        .map(|t| xml_text(t))
        .filter(|t| !t.is_empty())
}

/// One `response` of a multistatus: its href and its properties' XML.
#[derive(Debug, PartialEq)]
pub struct Response {
    pub href: String,
    pub props: String,
}

pub fn responses(xml: &str) -> Vec<Response> {
    elements(xml, "response")
        .into_iter()
        .filter_map(|r| {
            let href = first_text(r, "href")?;
            // The response's own href comes first; what follows is its propstat.
            let props = find_close(r, "href")
                .and_then(|i| r[i..].find('>').map(|j| &r[i + j + 1..]))
                .unwrap_or(r)
                .to_string();
            Some(Response { href, props })
        })
        .collect()
}

/// A collection that holds events: resourcetype has `calendar`, and the component set (if any)
/// has VEVENT.
pub fn is_event_calendar(props: &str) -> bool {
    let rt = elements(props, "resourcetype");
    let calendar = rt.iter().any(|r| find_tag(r, "calendar").is_some());
    let comps = elements(props, "supported-calendar-component-set");
    let events = comps
        .first()
        .is_none_or(|c| c.trim().is_empty() || c.to_ascii_uppercase().contains("\"VEVENT\""));
    calendar && events
}

/// `href` resolved against `base`.
pub fn absolute(base: &str, href: &str) -> String {
    if href.starts_with("http://") || href.starts_with("https://") {
        return href.to_string();
    }
    let scheme_end = base.find("://").map_or(0, |i| i + 3);
    let origin_end = base[scheme_end..]
        .find('/')
        .map_or(base.len(), |i| scheme_end + i);
    if href.starts_with('/') {
        return format!("{}{href}", &base[..origin_end]);
    }
    let dir = base
        .rfind('/')
        .filter(|i| *i >= origin_end)
        .map_or(base.len(), |i| i + 1);
    let mut b = base[..dir].to_string();
    if !b.ends_with('/') {
        b.push('/');
    }
    format!("{b}{href}")
}

// ---- requests --------------------------------------------------------------------------------

struct Dav<'a> {
    engine: &'a Engine,
    auth: Option<String>,
}

impl Dav<'_> {
    fn new<'a>(engine: &'a Engine, s: &Source) -> Result<Dav<'a>> {
        let auth = if s.password.trim().is_empty() {
            None
        } else {
            let cfg = engine.snapshot().config.clone();
            let store = engine.secret_store(&cfg);
            let pw =
                crate::secrets::resolve(&*store, &s.password, cfg.secrets.max_key_len as usize)?;
            let raw = format!("{}:{pw}", s.username.trim());
            Some(format!(
                "Basic {}",
                base64::engine::general_purpose::STANDARD.encode(raw)
            ))
        };
        Ok(Dav { engine, auth })
    }

    /// One WebDAV request, redirects followed; the final URL and the multistatus body.
    fn call(&self, method: &str, url: &str, depth: &str, body: &str) -> Result<(String, String)> {
        let hops = self.engine.snapshot().config.calendar.max_pages;
        let mut url = url.to_string();
        for _ in 0..=hops {
            let mut req = ureq::http::Request::builder()
                .method(method)
                .uri(&url)
                .header("Depth", depth)
                .header("Content-Type", "application/xml; charset=utf-8")
                .header("Accept", "application/xml, text/xml;q=0.9");
            if let Some(a) = &self.auth {
                req = req.header("Authorization", a.as_str());
            }
            let req = req
                .body(body.to_string())
                .map_err(|e| ConfigError::new("invalid_argument", format!("{url}: {e}")))?;
            let mut resp = agent(self.engine)
                .run(req)
                .map_err(|e| unreachable(&url, e))?;
            let status = resp.status().as_u16();
            if (300..400).contains(&status) {
                let loc = resp
                    .headers()
                    .get("location")
                    .and_then(|v| v.to_str().ok())
                    .map(|l| absolute(&url, l));
                match loc {
                    Some(next) if next != url => {
                        url = next;
                        continue;
                    }
                    _ => return Err(status_error(self.engine, &url, status, "")),
                }
            }
            let text = body_text(self.engine, &url, &mut resp)?;
            if !(200..300).contains(&status) {
                return Err(status_error(self.engine, &url, status, &text));
            }
            return Ok((url, text));
        }
        Err(ConfigError::new(
            "fetch_failed",
            format!("{url}: too many redirects (calendar.max_pages)"),
        ))
    }
}

fn origin(url: &str) -> String {
    absolute(url, "/").trim_end_matches('/').to_string()
}

/// The account (principal) and its event calendars, found from the source's URL.
pub fn discover(engine: &Engine, s: &Source) -> Result<(String, Vec<Calendar>)> {
    let dav = Dav::new(engine, s)?;
    let start = s.url.trim().to_string();
    let principal_of = |url: &str| -> Result<(String, Option<String>, String)> {
        let (at, xml) = dav.call("PROPFIND", url, "0", PRINCIPAL)?;
        let p = elements(&xml, "current-user-principal")
            .first()
            .and_then(|c| first_text(c, "href"))
            .map(|h| absolute(&at, &h));
        Ok((at, p, xml))
    };
    let (at, principal, first_xml) = match principal_of(&start) {
        Ok((at, Some(p), xml)) => (at, Some(p), xml),
        first => {
            let wk = format!("{}/.well-known/caldav", origin(&start));
            match principal_of(&wk) {
                Ok((at, Some(p), xml)) => (at, Some(p), xml),
                _ => {
                    let (at, xml) = match first {
                        Ok((at, _, xml)) => (at, xml),
                        Err(e) => return Err(e),
                    };
                    (at, None, xml)
                }
            }
        }
    };
    // The URL may itself be one calendar.
    let only = responses(&first_xml)
        .into_iter()
        .find(|r| is_event_calendar(&r.props))
        .map(|r| absolute(&at, &r.href));
    let account = if s.username.trim().is_empty() {
        crate::http::host_of(&start).unwrap_or_default()
    } else {
        s.username.trim().to_string()
    };
    let account_id = principal.clone().unwrap_or_else(|| at.clone());
    let cal = |id: String, title: String| Calendar {
        id,
        title,
        account: account.clone(),
        account_id: account_id.clone(),
        account_kind: "caldav".into(),
    };
    let home = match &principal {
        Some(p) => {
            let (pat, xml) = dav.call("PROPFIND", p, "0", HOME)?;
            elements(&xml, "calendar-home-set")
                .first()
                .and_then(|h| first_text(h, "href"))
                .map(|h| absolute(&pat, &h))
                .unwrap_or(pat)
        }
        None => at.clone(),
    };
    let (hat, xml) = dav.call("PROPFIND", &home, "1", COLLECTIONS)?;
    let mut out: Vec<Calendar> = responses(&xml)
        .into_iter()
        .filter(|r| is_event_calendar(&r.props))
        .map(|r| {
            let id = absolute(&hat, &r.href);
            let title = first_text(&r.props, "displayname").unwrap_or_else(|| {
                id.trim_end_matches('/')
                    .rsplit('/')
                    .next()
                    .unwrap_or_default()
                    .to_string()
            });
            cal(id, title)
        })
        .collect();
    if out.is_empty() {
        if let Some(u) = only {
            let title = u
                .trim_end_matches('/')
                .rsplit('/')
                .next()
                .unwrap_or_default()
                .to_string();
            out.push(cal(u, title));
        }
    }
    Ok((account_id, out))
}

impl Provider for CalDav {
    fn networked(&self) -> bool {
        true
    }

    fn calendars(&self, engine: &Engine, s: &Source) -> Result<Vec<Calendar>> {
        discover(engine, s).map(|(_, c)| c)
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
        let dav = Dav::new(engine, s)?;
        let body = query(from, to);
        let mut out = Vec::new();
        for cal in cals {
            let got = dav
                .call("REPORT", &cal.id, "1", &body)
                .and_then(|(_, xml)| {
                    ics_text_events(
                        engine,
                        &merge_calendars(&calendar_data(&xml)),
                        cal,
                        from,
                        to,
                    )
                });
            match got {
                Ok(mut e) => out.append(&mut e),
                Err(e) => errors.push(json!({
                    "source": s.name, "calendar": cal.title, "error": err_json(&e)
                })),
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOME_LIST: &str = r#"<?xml version="1.0"?>
<multistatus xmlns="DAV:" xmlns:C="urn:ietf:params:xml:ns:caldav">
 <response><href>/dav/calendars/me/</href><propstat><prop><resourcetype><collection/></resourcetype><displayname/></prop></propstat></response>
 <response><href>/dav/calendars/me/work/</href><propstat><prop>
   <resourcetype><collection/><C:calendar/></resourcetype><displayname>Work &amp; more</displayname>
   <C:supported-calendar-component-set><C:comp name="VEVENT"/></C:supported-calendar-component-set>
 </prop></propstat></response>
 <response><href>/dav/calendars/me/todo/</href><propstat><prop>
   <resourcetype><collection/><C:calendar/></resourcetype><displayname>Tasks</displayname>
   <C:supported-calendar-component-set><C:comp name="VTODO"/></C:supported-calendar-component-set>
 </prop></propstat></response>
 <response><href>/dav/calendars/me/inbox/</href><propstat><prop><resourcetype><collection/><C:schedule-inbox/></resourcetype></prop></propstat></response>
</multistatus>"#;

    #[test]
    fn only_event_calendars_are_listed() {
        let r = responses(HOME_LIST);
        assert_eq!(r.len(), 4);
        let cals: Vec<&Response> = r.iter().filter(|r| is_event_calendar(&r.props)).collect();
        assert_eq!(cals.len(), 1);
        assert_eq!(cals[0].href, "/dav/calendars/me/work/");
        assert_eq!(
            first_text(&cals[0].props, "displayname").unwrap(),
            "Work & more"
        );
    }

    #[test]
    fn hrefs_resolve_against_the_request_url() {
        assert_eq!(
            absolute("https://a.example:8443/x/y/", "/p/"),
            "https://a.example:8443/p/"
        );
        assert_eq!(
            absolute("https://a.example/x/y/", "z/"),
            "https://a.example/x/y/z/"
        );
        assert_eq!(absolute("https://a.example", "/p"), "https://a.example/p");
        assert_eq!(
            absolute("https://a.example/x", "https://b/c"),
            "https://b/c"
        );
        assert_eq!(
            origin("http://127.0.0.1:5232/u/c/"),
            "http://127.0.0.1:5232"
        );
    }

    #[test]
    fn the_query_names_the_window_in_utc() {
        let q = query(
            DateTime::parse_from_rfc3339("2026-11-07T09:00:00+01:00")
                .unwrap()
                .with_timezone(&Utc),
            DateTime::parse_from_rfc3339("2026-11-08T00:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
        );
        assert!(q.contains(r#"<C:time-range start="20261107T080000Z" end="20261108T000000Z"/>"#));
    }
}
