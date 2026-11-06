//! Getting the feed bytes: http(s) through `ureq`, `webcal(s)://` as http(s), `file://` from disk,
//! and CalDAV collections (`caldav://` over https, `caldav+http://` for a plain-http server such
//! as a loopback one): one `REPORT calendar-query` for every VEVENT, the `calendar-data` of each
//! resource merged into one VCALENDAR so the rest of the feed pipeline is the same (dot-4uzgol.96).
//! CalDAV needs no OS permission on any platform; its credentials are the feed's header secret
//! (`Authorization: secret:caldav/<feed>`, the value `Basic <base64 user:password>` or `Bearer ..`).
//! Policy (network.allow, ics.allow_file) is enforced by the caller before anything here runs.

use std::path::PathBuf;
use std::time::Duration;

use crate::config::{ConfigError, Result};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    Http(String),
    File(PathBuf),
    /// A CalDAV calendar collection, as the http(s) URL the REPORT goes to.
    CalDav(String),
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            let hex = std::str::from_utf8(&b[i + 1..i + 3])
                .ok()
                .and_then(|h| u8::from_str_radix(h, 16).ok());
            if let Some(v) = hex {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn file_path(rest: &str) -> PathBuf {
    let rest = rest.strip_prefix("localhost").unwrap_or(rest);
    let decoded = percent_decode(rest);
    // file:///C:/x on Windows carries a leading slash before the drive letter.
    let b = decoded.as_bytes();
    let drive = b.len() >= 3 && b[0] == b'/' && b[1].is_ascii_alphabetic() && b[2] == b':';
    PathBuf::from(if drive {
        decoded[1..].to_string()
    } else {
        decoded
    })
}

/// Classify a feed URL; the only error is `invalid_argument`.
pub fn classify(url: &str, webcal_https: bool) -> Result<Source> {
    let bad = |why: &str| ConfigError::new("invalid_argument", format!("feed url '{url}': {why}"));
    let (scheme, rest) = url
        .trim()
        .split_once("://")
        .ok_or_else(|| bad("missing scheme://"))?;
    if rest.is_empty() {
        return Err(bad("empty after the scheme"));
    }
    match scheme.to_ascii_lowercase().as_str() {
        "http" | "https" => Ok(Source::Http(url.trim().to_string())),
        "webcal" => Ok(Source::Http(format!(
            "{}://{rest}",
            if webcal_https { "https" } else { "http" }
        ))),
        "webcals" => Ok(Source::Http(format!("https://{rest}"))),
        "file" => Ok(Source::File(file_path(rest))),
        "caldav" | "caldavs" => Ok(Source::CalDav(format!("https://{rest}"))),
        "caldav+http" => Ok(Source::CalDav(format!("http://{rest}"))),
        "caldav+https" => Ok(Source::CalDav(format!("https://{rest}"))),
        other => Err(bad(&format!(
            "unsupported scheme '{other}' (http, https, webcal, webcals, file, caldav, caldav+http)"
        ))),
    }
}

#[derive(Clone, Debug)]
pub struct FetchOpts {
    pub timeout: Duration,
    pub max_bytes: u64,
    pub header: Option<(String, String)>,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
}

#[derive(Debug)]
pub enum Fetched {
    Body {
        text: String,
        etag: Option<String>,
        last_modified: Option<String>,
    },
    NotModified,
}

fn too_large(max: u64) -> ConfigError {
    ConfigError::new(
        "feed_too_large",
        format!("feed body is larger than ics.max_bytes ({max})"),
    )
}

pub fn fetch(src: &Source, o: &FetchOpts) -> Result<Fetched> {
    match src {
        Source::File(path) => {
            let meta = std::fs::metadata(path).map_err(|e| {
                ConfigError::new("fetch_failed", format!("{}: {e}", path.display()))
            })?;
            if meta.len() > o.max_bytes {
                return Err(too_large(o.max_bytes));
            }
            let bytes = std::fs::read(path).map_err(|e| {
                ConfigError::new("fetch_failed", format!("{}: {e}", path.display()))
            })?;
            Ok(Fetched::Body {
                text: String::from_utf8_lossy(&bytes).into_owned(),
                etag: None,
                last_modified: None,
            })
        }
        Source::Http(url) => fetch_http(url, o),
        Source::CalDav(url) => fetch_caldav(url, o),
    }
}

fn map_ureq(e: ureq::Error) -> ConfigError {
    match e {
        ureq::Error::Timeout(_) => ConfigError::new("fetch_timeout", e.to_string()),
        other => ConfigError::new("fetch_failed", other.to_string()),
    }
}

fn agent(o: &FetchOpts) -> ureq::Agent {
    let builder = ureq::Agent::config_builder()
        .timeout_global(Some(o.timeout))
        .http_status_as_error(false)
        // REPORT is WebDAV's, not one of http's standard methods.
        .allow_non_standard_methods(true);
    #[cfg(windows)]
    let builder = builder.tls_config(
        ureq::tls::TlsConfig::builder()
            .provider(ureq::tls::TlsProvider::NativeTls)
            .build(),
    );
    builder.build().into()
}

fn read_limited(resp: &mut ureq::http::Response<ureq::Body>, max: u64) -> Result<String> {
    let bytes = resp
        .body_mut()
        .with_config()
        .limit(max)
        .read_to_vec()
        .map_err(|e| {
            if matches!(e, ureq::Error::BodyExceedsLimit(_)) || e.to_string().contains("exceed") {
                too_large(max)
            } else {
                map_ureq(e)
            }
        })?;
    if bytes.len() as u64 > max {
        return Err(too_large(max));
    }
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

fn fetch_http(url: &str, o: &FetchOpts) -> Result<Fetched> {
    let mut req = agent(o)
        .get(url)
        .header("Accept", "text/calendar, text/plain;q=0.5, */*;q=0.1");
    if let Some((k, v)) = &o.header {
        req = req.header(k.as_str(), v.as_str());
    }
    if let Some(t) = &o.etag {
        req = req.header("If-None-Match", t.as_str());
    }
    if let Some(t) = &o.last_modified {
        req = req.header("If-Modified-Since", t.as_str());
    }
    let mut resp = req.call().map_err(map_ureq)?;
    let status = resp.status().as_u16();
    if status == 304 && (o.etag.is_some() || o.last_modified.is_some()) {
        return Ok(Fetched::NotModified);
    }
    if !(200..300).contains(&status) {
        return Err(ConfigError::new(
            "http_status",
            format!("{url}: HTTP {status}"),
        ));
    }
    let hdr = |name: &str| {
        resp.headers()
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string)
    };
    let etag = hdr("etag");
    let last_modified = hdr("last-modified");
    Ok(Fetched::Body {
        text: read_limited(&mut resp, o.max_bytes)?,
        etag,
        last_modified,
    })
}

/// RFC 4791 calendar-query: every VEVENT of the collection, with its iCalendar data.
const CALENDAR_QUERY: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<C:calendar-query xmlns:D="DAV:" xmlns:C="urn:ietf:params:xml:ns:caldav">
  <D:prop><D:getetag/><C:calendar-data/></D:prop>
  <C:filter><C:comp-filter name="VCALENDAR"><C:comp-filter name="VEVENT"/></C:comp-filter></C:filter>
</C:calendar-query>
"#;

fn fetch_caldav(url: &str, o: &FetchOpts) -> Result<Fetched> {
    let mut req = ureq::http::Request::builder()
        .method("REPORT")
        .uri(url)
        .header("Depth", "1")
        .header("Content-Type", "application/xml; charset=utf-8")
        .header("Accept", "application/xml, text/xml;q=0.9");
    if let Some((k, v)) = &o.header {
        req = req.header(k.as_str(), v.as_str());
    }
    let req = req
        .body(CALENDAR_QUERY)
        .map_err(|e| ConfigError::new("invalid_argument", format!("{url}: {e}")))?;
    let mut resp = agent(o).run(req).map_err(map_ureq)?;
    let status = resp.status().as_u16();
    if !(200..300).contains(&status) {
        return Err(ConfigError::new(
            "http_status",
            format!("{url}: HTTP {status}"),
        ));
    }
    let xml = read_limited(&mut resp, o.max_bytes)?;
    Ok(Fetched::Body {
        text: merge_calendars(&calendar_data(&xml)),
        etag: None,
        last_modified: None,
    })
}

/// The text of every `calendar-data` element of a multistatus reply, whatever its namespace
/// prefix, CDATA or entity-escaped.
pub fn calendar_data(xml: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = xml;
    while let Some(open) = find_tag(rest, "calendar-data") {
        let after = &rest[open..];
        let Some(gt) = after.find('>') else { break };
        if after[..gt].ends_with('/') {
            rest = &after[gt + 1..];
            continue;
        }
        let body = &after[gt + 1..];
        let Some(close) = find_close(body, "calendar-data") else {
            break;
        };
        out.push(xml_text(&body[..close]));
        rest = &body[close..];
        rest = rest.find('>').map_or("", |i| &rest[i + 1..]);
    }
    out
}

/// Byte offset of the next `<name` or `<prefix:name` start tag.
fn find_tag(s: &str, name: &str) -> Option<usize> {
    let mut from = 0;
    while let Some(i) = s[from..].find('<').map(|i| i + from) {
        let tag = &s[i + 1..];
        let end = tag
            .find(|c: char| c.is_whitespace() || c == '>' || c == '/')
            .unwrap_or(tag.len());
        let local = tag[..end].rsplit(':').next().unwrap_or("");
        if !tag.starts_with('/') && local == name {
            return Some(i);
        }
        from = i + 1;
    }
    None
}

/// Byte offset of the `</..name>` end tag.
fn find_close(s: &str, name: &str) -> Option<usize> {
    let mut from = 0;
    while let Some(i) = s[from..].find("</").map(|i| i + from) {
        let tag = &s[i + 2..];
        let end = tag.find('>').unwrap_or(tag.len());
        if tag[..end].trim().rsplit(':').next() == Some(name) {
            return Some(i);
        }
        from = i + 2;
    }
    None
}

fn xml_text(raw: &str) -> String {
    let t = raw.trim();
    if let Some(inner) = t
        .strip_prefix("<![CDATA[")
        .and_then(|x| x.strip_suffix("]]>"))
    {
        return inner.to_string();
    }
    let mut out = String::with_capacity(t.len());
    let mut rest = t;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        let ent = &rest[i + 1..];
        let Some(semi) = ent.find(';') else {
            out.push_str(&rest[i..]);
            rest = "";
            break;
        };
        let name = &ent[..semi];
        let ch = match name {
            "lt" => Some('<'),
            "gt" => Some('>'),
            "amp" => Some('&'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            n => n
                .strip_prefix("#x")
                .or_else(|| n.strip_prefix("#X"))
                .and_then(|h| u32::from_str_radix(h, 16).ok())
                .or_else(|| n.strip_prefix('#').and_then(|d| d.parse().ok()))
                .and_then(char::from_u32),
        };
        match ch {
            Some(c) => out.push(c),
            None => out.push_str(&rest[i..i + semi + 2]),
        }
        rest = &ent[semi + 1..];
    }
    out.push_str(rest);
    out
}

/// One VCALENDAR holding the components (VTIMEZONE, VEVENT ..) of every resource; each
/// resource's own VERSION / PRODID lines are dropped, a VTIMEZONE repeated by several resources
/// is kept once.
pub fn merge_calendars(cals: &[String]) -> String {
    let mut out = String::from(
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//oats-meetings-rs//caldav//EN\r\n",
    );
    let mut zones = std::collections::BTreeSet::new();
    for cal in cals {
        let mut depth = 0usize;
        let mut block = String::new();
        let mut keep = false;
        for line in cal.lines() {
            let line = line.trim_end_matches('\r');
            let folded = line.starts_with([' ', '\t']);
            let upper = line.to_ascii_uppercase();
            if !folded && upper.starts_with("BEGIN:") {
                depth += 1;
                if depth == 2 {
                    block.clear();
                    keep = true;
                }
            }
            if depth >= 2 && keep {
                block.push_str(line);
                block.push_str("\r\n");
            }
            if !folded && upper.starts_with("END:") {
                if depth == 2 && keep {
                    let zone = upper.starts_with("END:VTIMEZONE");
                    if !zone || zones.insert(block.clone()) {
                        out.push_str(&block);
                    }
                    keep = false;
                }
                depth = depth.saturating_sub(1);
            }
        }
    }
    out.push_str("END:VCALENDAR\r\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schemes_classify() {
        assert_eq!(
            classify("https://a/b.ics", true).unwrap(),
            Source::Http("https://a/b.ics".into())
        );
        assert_eq!(
            classify("webcal://a/b.ics", true).unwrap(),
            Source::Http("https://a/b.ics".into())
        );
        assert_eq!(
            classify("webcal://a/b.ics", false).unwrap(),
            Source::Http("http://a/b.ics".into())
        );
        assert_eq!(
            classify("WEBCALS://a/b.ics", false).unwrap(),
            Source::Http("https://a/b.ics".into())
        );
        assert_eq!(
            classify("file:///tmp/a%20b.ics", true).unwrap(),
            Source::File("/tmp/a b.ics".into())
        );
        assert_eq!(
            classify("file:///C:/x/y.ics", true).unwrap(),
            Source::File("C:/x/y.ics".into())
        );
        assert_eq!(
            classify("caldav://dav.example/cal/me/", true).unwrap(),
            Source::CalDav("https://dav.example/cal/me/".into())
        );
        assert_eq!(
            classify("caldav+http://127.0.0.1:5232/u/c/", true).unwrap(),
            Source::CalDav("http://127.0.0.1:5232/u/c/".into())
        );
        for bad in ["ftp://x/y", "nope", "http://", "file://", "caldav://"] {
            assert_eq!(
                classify(bad, true).unwrap_err().code,
                "invalid_argument",
                "{bad}"
            );
        }
    }

    const MULTISTATUS: &str = r#"<?xml version="1.0"?>
<d:multistatus xmlns:d="DAV:" xmlns:cal="urn:ietf:params:xml:ns:caldav">
 <d:response><d:href>/c/a.ics</d:href><d:propstat><d:prop>
  <d:getetag>"1"</d:getetag>
  <cal:calendar-data>BEGIN:VCALENDAR&#13;
VERSION:2.0&#13;
PRODID:-//x//y//EN&#13;
BEGIN:VTIMEZONE&#13;
TZID:Europe/Berlin&#13;
END:VTIMEZONE&#13;
BEGIN:VEVENT&#13;
UID:a&#13;
SUMMARY:Standup &amp; review&#13;
DESCRIPTION:long line&#13;
 folded&#13;
BEGIN:VALARM&#13;
ACTION:DISPLAY&#13;
END:VALARM&#13;
END:VEVENT&#13;
END:VCALENDAR&#13;
</cal:calendar-data></d:prop></d:propstat></d:response>
 <d:response><d:href>/c/b.ics</d:href><d:propstat><d:prop>
  <C:calendar-data xmlns:C="urn:ietf:params:xml:ns:caldav"><![CDATA[BEGIN:VCALENDAR
VERSION:2.0
BEGIN:VTIMEZONE
TZID:Europe/Berlin
END:VTIMEZONE
BEGIN:VEVENT
UID:b
SUMMARY:<1:1>
END:VEVENT
END:VCALENDAR
]]></C:calendar-data></d:prop></d:propstat></d:response>
 <d:response><d:href>/c/</d:href><d:propstat><d:prop><cal:calendar-data/></d:prop></d:propstat></d:response>
</d:multistatus>"#;

    #[test]
    fn calendar_data_is_extracted_whatever_the_prefix_or_escaping() {
        let cals = calendar_data(MULTISTATUS);
        assert_eq!(cals.len(), 2, "{cals:?}");
        assert!(
            cals[0].contains("SUMMARY:Standup & review\r\n"),
            "{}",
            cals[0]
        );
        assert!(cals[1].contains("SUMMARY:<1:1>"), "{}", cals[1]);
    }

    #[test]
    fn resources_merge_into_one_calendar() {
        let merged = merge_calendars(&calendar_data(MULTISTATUS));
        assert!(merged.starts_with("BEGIN:VCALENDAR\r\n"));
        assert!(merged.ends_with("END:VCALENDAR\r\n"));
        assert_eq!(merged.matches("BEGIN:VCALENDAR").count(), 1);
        assert_eq!(merged.matches("BEGIN:VEVENT").count(), 2);
        assert_eq!(merged.matches("BEGIN:VTIMEZONE").count(), 1, "{merged}");
        assert_eq!(merged.matches("VERSION:").count(), 1);
        assert!(!merged.contains("PRODID:-//x//y//EN"));
        assert!(merged.contains("DESCRIPTION:long line\r\n folded\r\n"));
        assert!(merged.contains("BEGIN:VALARM\r\nACTION:DISPLAY\r\nEND:VALARM\r\n"));
        assert_eq!(merge_calendars(&[]), "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//oats-meetings-rs//caldav//EN\r\nEND:VCALENDAR\r\n");
    }
}
