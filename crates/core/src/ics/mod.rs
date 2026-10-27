//! ICS calendar subscriptions (dot-4uzgol.17): N configurable feeds, a disk cache, scheduled and
//! manual refresh, and recurrence-expanded events over a time window.
//!
//! Every choice is a config key (`ics.*`, see `config::keys`); the feed list itself is the JSON
//! string `ics.feeds`, edited by `ics-add` / `ics-remove` (or `config-set`). Fetching http(s) and
//! webcal is gated by `network.allow` (error `network_denied`); `file://` by `ics.allow_file`
//! (error `file_denied`). The cache is read for events, so a feed keeps answering when offline.
//!
//! Stable error codes: `network_denied`, `file_denied`, `ics_disabled`, `feed_not_found`,
//! `feed_exists`, `feed_disabled`, `invalid_argument`, `invalid_value` (a bad `ics.*` value),
//! `fetch_failed`, `fetch_timeout`, `http_status`, `feed_too_large`, `ics_not_calendar`,
//! `ics_parse_error`, `ics_bad_event`, `ics_bad_tz`, `ics_bad_rrule`, `io_error`.

pub mod fetch;
pub mod parse;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use chrono::{DateTime, NaiveDate, SecondsFormat, Utc};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::action::Action;
use crate::config::typed::{Ics, IcsAllDay, IcsCancelled, IcsOnBadEvent, IcsUnknownTz, IcsWebcalScheme};
use crate::config::{ConfigError, Result};
use crate::control::engine::Engine;

use fetch::{FetchOpts, Fetched, Source};
use parse::ParseOpts;

fn yes() -> bool {
    true
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Feed {
    pub name: String,
    pub url: String,
    #[serde(default = "yes")]
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_secs: Option<u64>,
    /// One request header, `Name: value`, for feeds behind a token.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub header: Option<String>,
}

/// The `ics.*` keys, parsed and checked.
#[derive(Clone, Debug)]
pub struct Settings {
    pub enabled: bool,
    pub default_refresh: u64,
    pub timeout: u64,
    pub max_bytes: u64,
    pub max_occ: u16,
    pub cache_dir: Option<PathBuf>,
    pub default_tz: Tz,
    pub allow_file: bool,
    pub webcal_https: bool,
    pub stale_on_error: bool,
    pub window_days: i64,
    pub parse: ParseOpts,
}

fn invalid(key: &str, why: impl std::fmt::Display) -> ConfigError {
    ConfigError::new("invalid_value", format!("ics.{key}: {why}"))
}

fn num(key: &str, v: &str) -> Result<u64> {
    v.trim().parse::<u64>().map_err(|_| invalid(key, format!("'{v}' is not a non-negative integer")))
}

impl Settings {
    pub fn from_config(c: &Ics) -> Result<Settings> {
        let default_tz = c
            .default_tz
            .trim()
            .parse::<Tz>()
            .map_err(|_| invalid("default_tz", format!("'{}' is not an IANA zone", c.default_tz)))?;
        let max_occ = num("max_occurrences", &c.max_occurrences)?;
        let window_days = num("window_days", &c.window_days)?;
        Ok(Settings {
            enabled: c.enabled,
            default_refresh: num("default_refresh_secs", &c.default_refresh_secs)?,
            timeout: num("timeout_secs", &c.timeout_secs)?,
            max_bytes: num("max_bytes", &c.max_bytes)?,
            max_occ: u16::try_from(max_occ.min(u64::from(u16::MAX))).unwrap_or(u16::MAX),
            cache_dir: Some(c.cache_dir.trim()).filter(|s| !s.is_empty()).map(PathBuf::from),
            default_tz,
            allow_file: c.allow_file,
            webcal_https: c.webcal_scheme == IcsWebcalScheme::Https,
            stale_on_error: c.stale_on_error,
            window_days: i64::try_from(window_days.min(36_500)).unwrap_or(36_500),
            parse: ParseOpts {
                default_tz,
                tz_fallback: c.unknown_tz == IcsUnknownTz::DefaultTz,
                skip_bad: c.on_bad_event == IcsOnBadEvent::Skip,
                show_cancelled: c.cancelled == IcsCancelled::Show,
                include_all_day: c.all_day == IcsAllDay::Include,
            },
        })
    }
}

/// `config-check`: the `ics.*` strings parse and the feed list is well formed.
pub fn validate_settings(c: &Ics) -> Result<()> {
    Settings::from_config(c)?;
    parse_feeds(&c.feeds)?;
    Ok(())
}

pub fn valid_name(n: &str) -> bool {
    !n.is_empty()
        && n.len() <= 64
        && !n.starts_with('.')
        && n.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
}

fn header_pair(h: &str) -> Result<(String, String)> {
    let bad = || ConfigError::new("invalid_argument", "header must look like 'Name: value'");
    let (k, v) = h.split_once(':').ok_or_else(bad)?;
    let k = k.trim();
    if k.is_empty() || !k.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
        return Err(bad());
    }
    Ok((k.to_string(), v.trim().to_string()))
}

pub fn parse_feeds(text: &str) -> Result<Vec<Feed>> {
    let feeds: Vec<Feed> = serde_json::from_str(if text.trim().is_empty() { "[]" } else { text })
        .map_err(|e| invalid("feeds", format!("not a JSON array of feeds: {e}")))?;
    let mut seen = std::collections::BTreeSet::new();
    for f in &feeds {
        if !valid_name(&f.name) {
            return Err(invalid("feeds", format!("invalid feed name '{}'", f.name)));
        }
        if !seen.insert(f.name.clone()) {
            return Err(invalid("feeds", format!("duplicate feed name '{}'", f.name)));
        }
    }
    Ok(feeds)
}

fn now_secs() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

fn stamp(secs: i64) -> String {
    DateTime::<Utc>::from_timestamp(secs, 0)
        .map(|d| d.to_rfc3339_opts(SecondsFormat::Secs, true))
        .unwrap_or_default()
}

// ---- cache ---------------------------------------------------------------------------------

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct Meta {
    #[serde(default)]
    etag: Option<String>,
    #[serde(default)]
    last_modified: Option<String>,
    /// Unix seconds of the last successful fetch (body or 304).
    #[serde(default)]
    fetched_at: Option<i64>,
    /// Unix seconds of the last attempt; drives the schedule so a failing feed backs off.
    #[serde(default)]
    attempted_at: Option<i64>,
    #[serde(default)]
    events: Option<usize>,
    #[serde(default)]
    skipped: Option<usize>,
    #[serde(default)]
    error: Option<Value>,
}

struct Cache {
    dir: PathBuf,
}

impl Cache {
    fn ics(&self, name: &str) -> PathBuf {
        self.dir.join(format!("{name}.ics"))
    }
    fn meta(&self, name: &str) -> PathBuf {
        self.dir.join(format!("{name}.meta.json"))
    }
    fn read_meta(&self, name: &str) -> Meta {
        std::fs::read_to_string(self.meta(name))
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default()
    }
    fn read_body(&self, name: &str) -> Option<String> {
        std::fs::read_to_string(self.ics(name)).ok()
    }
    fn write(&self, path: &Path, bytes: &[u8]) -> Result<()> {
        let io = |e: std::io::Error| ConfigError::new("io_error", format!("{}: {e}", path.display()));
        std::fs::create_dir_all(&self.dir).map_err(io)?;
        let tmp = path.with_extension(format!("tmp{}", std::process::id()));
        std::fs::write(&tmp, bytes).and_then(|_| std::fs::rename(&tmp, path)).map_err(io)
    }
    fn write_meta(&self, name: &str, m: &Meta) -> Result<()> {
        let text = serde_json::to_vec_pretty(m).expect("meta serializes");
        self.write(&self.meta(name), &text)
    }
    fn remove(&self, name: &str) {
        let _ = std::fs::remove_file(self.ics(name));
        let _ = std::fs::remove_file(self.meta(name));
    }
}

fn err_json(e: &ConfigError) -> Value {
    json!({"code": e.code, "message": e.message})
}

// ---- engine glue ---------------------------------------------------------------------------

static FEED_LIST_LOCK: Mutex<()> = Mutex::new(());

struct Ctx {
    cfg: Ics,
    network_allow: bool,
    settings: Settings,
    cache: Cache,
}

fn ctx(engine: &Engine) -> Result<Ctx> {
    let typed = engine.resolved().typed()?;
    let settings = Settings::from_config(&typed.ics)?;
    let dir = settings.cache_dir.clone().unwrap_or_else(|| {
        let sessions = engine.default_sessions_dir();
        sessions.parent().unwrap_or(sessions).join("ics")
    });
    Ok(Ctx { network_allow: typed.network.allow, cfg: typed.ics, settings, cache: Cache { dir } })
}

fn load_feeds(c: &Ctx) -> Result<Vec<Feed>> {
    parse_feeds(&c.cfg.feeds)
}

fn save_feeds(engine: &Engine, feeds: &[Feed], source: &str) -> Result<()> {
    let text = serde_json::to_string(feeds).expect("feeds serialize");
    engine.set("ics.feeds", &Value::String(text), source).map(|_| ())
}

fn find<'a>(feeds: &'a [Feed], name: &str) -> Result<&'a Feed> {
    feeds
        .iter()
        .find(|f| f.name == name)
        .ok_or_else(|| ConfigError::new("feed_not_found", format!("no feed named '{name}'")))
}

fn public(f: &Feed) -> Value {
    json!({
        "name": f.name, "url": f.url, "enabled": f.enabled,
        "refresh_secs": f.refresh_secs, "has_header": f.header.is_some(),
    })
}

pub fn dispatch(engine: &Engine, a: &Action) -> Result<Value> {
    match a {
        Action::IcsAdd { name, url, enabled, refresh_secs, header } => {
            add(engine, name, url, *enabled, *refresh_secs, header.as_deref())
        }
        Action::IcsRemove { name } => remove(engine, name),
        Action::IcsList {} => list(engine),
        Action::IcsRefresh { name } => refresh(engine, name.as_deref()),
        Action::IcsEvents { from, to, feed } => events(engine, from.as_deref(), to.as_deref(), feed.as_deref()),
        _ => Err(ConfigError::new("bad_request", "not an ics action")),
    }
}

fn add(
    engine: &Engine,
    name: &str,
    url: &str,
    enabled: Option<bool>,
    refresh_secs: Option<u64>,
    header: Option<&str>,
) -> Result<Value> {
    if !valid_name(name) {
        return Err(ConfigError::new(
            "invalid_argument",
            format!("invalid feed name {name:?} (letters, digits, '_', '-', '.'; at most 64)"),
        ));
    }
    let c = ctx(engine)?;
    fetch::classify(url, c.settings.webcal_https)?;
    if let Some(h) = header {
        header_pair(h)?;
    }
    let _g = FEED_LIST_LOCK.lock().expect("feed list lock");
    let mut feeds = load_feeds(&c)?;
    if feeds.iter().any(|f| f.name == name) {
        return Err(ConfigError::new("feed_exists", format!("a feed named '{name}' already exists")));
    }
    let feed = Feed {
        name: name.to_string(),
        url: url.trim().to_string(),
        enabled: enabled.unwrap_or(true),
        refresh_secs,
        header: header.map(str::to_string),
    };
    feeds.push(feed.clone());
    save_feeds(engine, &feeds, "ics-add")?;
    Ok(json!({"feed": public(&feed), "feeds": feeds.len()}))
}

fn remove(engine: &Engine, name: &str) -> Result<Value> {
    let c = ctx(engine)?;
    let _g = FEED_LIST_LOCK.lock().expect("feed list lock");
    let mut feeds = load_feeds(&c)?;
    find(&feeds, name)?;
    feeds.retain(|f| f.name != name);
    save_feeds(engine, &feeds, "ics-remove")?;
    c.cache.remove(name);
    engine.bus.publish("ics-updated", json!({"feed": name, "removed": true}));
    Ok(json!({"removed": name, "feeds": feeds.len()}))
}

fn effective_interval(f: &Feed, s: &Settings) -> u64 {
    f.refresh_secs.unwrap_or(s.default_refresh)
}

fn feed_state(c: &Ctx, f: &Feed, now: i64) -> Value {
    let m = c.cache.read_meta(&f.name);
    let interval = effective_interval(f, &c.settings);
    let due = f.enabled
        && c.settings.enabled
        && interval > 0
        && m.attempted_at.map_or(true, |t| now - t >= interval as i64);
    let scheme = match fetch::classify(&f.url, c.settings.webcal_https) {
        Ok(Source::File(_)) => "file",
        Ok(Source::Http(_)) => "http",
        Err(_) => "invalid",
    };
    let mut v = public(f);
    v["scheme"] = json!(scheme);
    v["effective_refresh_secs"] = json!(interval);
    v["cached"] = json!(c.cache.ics(&f.name).exists());
    v["fetched_at"] = json!(m.fetched_at.map(stamp));
    v["attempted_at"] = json!(m.attempted_at.map(stamp));
    v["events"] = json!(m.events);
    v["skipped"] = json!(m.skipped);
    v["error"] = json!(m.error);
    v["due"] = json!(due);
    v
}

fn list(engine: &Engine) -> Result<Value> {
    let c = ctx(engine)?;
    let now = now_secs();
    let feeds = load_feeds(&c)?;
    let rows: Vec<Value> = feeds.iter().map(|f| feed_state(&c, f, now)).collect();
    Ok(json!({"enabled": c.settings.enabled, "feeds": rows}))
}

fn refresh(engine: &Engine, name: Option<&str>) -> Result<Value> {
    let c = ctx(engine)?;
    if !c.settings.enabled {
        return Err(ConfigError::new("ics_disabled", "ics.enabled is off"));
    }
    let feeds = load_feeds(&c)?;
    match name {
        Some(n) => {
            let f = find(&feeds, n)?;
            let r = refresh_one(engine, &c, f, now_secs())?;
            Ok(json!({"feeds": [r], "failed": 0}))
        }
        None => {
            let mut results = Vec::new();
            let mut failed = 0;
            for f in feeds.iter().filter(|f| f.enabled) {
                match refresh_one(engine, &c, f, now_secs()) {
                    Ok(r) => results.push(r),
                    Err(e) => {
                        failed += 1;
                        results.push(json!({"name": f.name, "ok": false, "error": err_json(&e)}));
                    }
                }
            }
            Ok(json!({"feeds": results, "failed": failed}))
        }
    }
}

/// Fetch one feed into the cache. Policy errors come back before anything is touched.
fn refresh_one(engine: &Engine, c: &Ctx, f: &Feed, now: i64) -> Result<Value> {
    if !f.enabled {
        return Err(ConfigError::new("feed_disabled", format!("feed '{}' is disabled", f.name)));
    }
    let src = fetch::classify(&f.url, c.settings.webcal_https)?;
    match &src {
        Source::Http(_) if !c.network_allow => {
            return Err(ConfigError::new(
                "network_denied",
                format!("fetching '{}' needs network.allow = true", f.name),
            ))
        }
        Source::File(_) if !c.settings.allow_file => {
            return Err(ConfigError::new(
                "file_denied",
                format!("'{}' is a file:// feed and ics.allow_file is off", f.name),
            ))
        }
        _ => {}
    }
    let header = f.header.as_deref().map(header_pair).transpose()?;
    let prior = c.cache.read_meta(&f.name);
    let have_body = c.cache.ics(&f.name).exists();
    let opts = FetchOpts {
        timeout_secs: c.settings.timeout,
        max_bytes: c.settings.max_bytes,
        header,
        etag: if have_body { prior.etag.clone() } else { None },
        last_modified: if have_body { prior.last_modified.clone() } else { None },
    };
    let outcome = fetch::fetch(&src, &opts).and_then(|got| match got {
        Fetched::NotModified => Ok(None),
        Fetched::Body { text, etag, last_modified } => {
            let parsed = parse::parse_feed(&text, &c.settings.parse)?;
            Ok(Some((text, etag, last_modified, parsed)))
        }
    });
    match outcome {
        Ok(None) => {
            let m = Meta { fetched_at: Some(now), attempted_at: Some(now), error: None, ..prior };
            c.cache.write_meta(&f.name, &m)?;
            Ok(json!({"name": f.name, "ok": true, "changed": false, "not_modified": true,
                      "events": m.events, "fetched_at": stamp(now)}))
        }
        Ok(Some((text, etag, last_modified, parsed))) => {
            let changed = c.cache.read_body(&f.name).as_deref() != Some(text.as_str());
            if changed {
                c.cache.write(&c.cache.ics(&f.name), text.as_bytes())?;
            }
            let m = Meta {
                etag,
                last_modified,
                fetched_at: Some(now),
                attempted_at: Some(now),
                events: Some(parsed.events.len()),
                skipped: Some(parsed.skipped),
                error: None,
            };
            c.cache.write_meta(&f.name, &m)?;
            if changed {
                engine.bus.publish(
                    "ics-updated",
                    json!({"feed": f.name, "events": parsed.events.len(), "skipped": parsed.skipped,
                           "fetched_at": stamp(now)}),
                );
            }
            Ok(json!({"name": f.name, "ok": true, "changed": changed, "not_modified": false,
                      "events": parsed.events.len(), "skipped": parsed.skipped, "fetched_at": stamp(now)}))
        }
        Err(e) => {
            let m = Meta { attempted_at: Some(now), error: Some(err_json(&e)), ..prior };
            c.cache.write_meta(&f.name, &m)?;
            if c.settings.stale_on_error && have_body {
                Ok(json!({"name": f.name, "ok": true, "stale": true, "changed": false,
                          "error": err_json(&e), "events": m.events}))
            } else {
                Err(e)
            }
        }
    }
}

fn parse_instant(label: &str, v: &str, tz: Tz) -> Result<DateTime<Utc>> {
    let bad = || ConfigError::new("invalid_argument", format!("{label} '{v}' is not RFC 3339 or YYYY-MM-DD"));
    if let Ok(d) = DateTime::parse_from_rfc3339(v) {
        return Ok(d.with_timezone(&Utc));
    }
    let d = NaiveDate::parse_from_str(v, "%Y-%m-%d").map_err(|_| bad())?;
    Ok(parse::resolve(tz, d.and_hms_opt(0, 0, 0).expect("midnight exists")))
}

fn events(engine: &Engine, from: Option<&str>, to: Option<&str>, feed: Option<&str>) -> Result<Value> {
    let c = ctx(engine)?;
    let tz = c.settings.default_tz;
    let from = match from {
        Some(v) => parse_instant("from", v, tz)?,
        None => DateTime::<Utc>::from_timestamp(now_secs(), 0).expect("now is representable"),
    };
    let to = match to {
        Some(v) => parse_instant("to", v, tz)?,
        None => from + chrono::Duration::days(c.settings.window_days),
    };
    if to < from {
        return Err(ConfigError::new("invalid_argument", "'to' is before 'from'"));
    }
    let feeds = load_feeds(&c)?;
    let chosen: Vec<&Feed> = match feed {
        Some(n) => vec![find(&feeds, n)?],
        None => feeds.iter().filter(|f| f.enabled).collect(),
    };
    let mut all: Vec<Value> = Vec::new();
    let mut info: Vec<Value> = Vec::new();
    let mut errors: Vec<Value> = Vec::new();
    for f in chosen {
        let Some(body) = c.cache.read_body(&f.name) else {
            info.push(json!({"name": f.name, "cached": false}));
            continue;
        };
        let result = parse::parse_feed(&body, &c.settings.parse)
            .and_then(|p| parse::expand(&f.name, &p, &c.settings.parse, c.settings.max_occ, from, to));
        match result {
            Ok(mut evs) => {
                info.push(json!({"name": f.name, "cached": true, "events": evs.len()}));
                all.append(&mut evs);
            }
            Err(e) if feed.is_some() => return Err(e),
            Err(e) => errors.push(json!({"name": f.name, "error": err_json(&e)})),
        }
    }
    all.sort_by(|a, b| {
        a["start"].as_str().cmp(&b["start"].as_str()).then_with(|| a["uid"].as_str().cmp(&b["uid"].as_str()))
    });
    Ok(json!({
        "from": from.to_rfc3339_opts(SecondsFormat::Secs, true),
        "to": to.to_rfc3339_opts(SecondsFormat::Secs, true),
        "events": all, "feeds": info, "errors": errors,
    }))
}

// ---- schedule ------------------------------------------------------------------------------

/// Refresh every feed whose interval has elapsed at `now` (unix seconds); returns their names.
/// A policy denial (`network_denied`, `file_denied`, disabled) skips the feed silently.
pub fn tick(engine: &Engine, now: i64) -> Vec<String> {
    let Ok(c) = ctx(engine) else { return vec![] };
    if !c.settings.enabled {
        return vec![];
    }
    let Ok(feeds) = load_feeds(&c) else { return vec![] };
    let mut done = Vec::new();
    for f in feeds.iter().filter(|f| f.enabled) {
        let interval = effective_interval(f, &c.settings);
        let last = c.cache.read_meta(&f.name).attempted_at;
        if interval == 0 || last.map_or(false, |t| now - t < interval as i64) {
            continue;
        }
        if refresh_one(engine, &c, f, now).is_ok() || c.cache.read_meta(&f.name).attempted_at == Some(now) {
            done.push(f.name.clone());
        }
    }
    done
}

/// A detached thread that calls `tick` about once a second until the engine shuts down.
pub fn spawn_scheduler(engine: Arc<Engine>) {
    let _ = thread::Builder::new().name("oats-ics".into()).spawn(move || {
        while !engine.is_shutdown() {
            for _ in 0..10 {
                if engine.is_shutdown() {
                    return;
                }
                thread::sleep(Duration::from_millis(100));
            }
            let _ = tick(&engine, now_secs());
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn feed_json_defaults() {
        let f = parse_feeds(r#"[{"name":"a","url":"file:///x.ics"}]"#).unwrap();
        assert!(f[0].enabled);
        assert_eq!(f[0].refresh_secs, None);
        assert_eq!(parse_feeds("").unwrap().len(), 0);
        assert_eq!(parse_feeds("nope").unwrap_err().code, "invalid_value");
        assert_eq!(
            parse_feeds(r#"[{"name":"a","url":"u"},{"name":"a","url":"v"}]"#).unwrap_err().code,
            "invalid_value"
        );
        assert_eq!(parse_feeds(r#"[{"name":"../x","url":"u"}]"#).unwrap_err().code, "invalid_value");
    }

    #[test]
    fn header_pairs() {
        assert_eq!(header_pair("Authorization: Bearer t").unwrap(), ("Authorization".into(), "Bearer t".into()));
        assert!(header_pair("nocolon").is_err());
        assert!(header_pair("bad name: v").is_err());
    }

    #[test]
    fn durations() {
        assert_eq!(parse::parse_duration("PT1H30M"), Some(5400));
        assert_eq!(parse::parse_duration("P1DT2H"), Some(93_600));
        assert_eq!(parse::parse_duration("P2W"), Some(1_209_600));
        assert_eq!(parse::parse_duration("-PT15M"), Some(-900));
        assert_eq!(parse::parse_duration("PT"), Some(0));
        assert_eq!(parse::parse_duration("1H"), None);
        assert_eq!(parse::parse_duration("PT5"), None);
    }
}
