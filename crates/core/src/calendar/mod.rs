//! Calendar sources and titling sessions from the overlapping event (dot-4uzgol.88,
//! dot-4uzgol.100).
//!
//! `calendar.sources` is a JSON array of sources, each read through one provider (`provider`):
//! `os-local` (this computer's accounts, the default), `google`, `microsoft`, `caldav`, `ics`
//! (the cached `ics.feeds`, all or some by name) or `eventkit` (the macOS Calendar app). Every
//! provider lists accounts and their calendars, chosen by account title / email and by calendar
//! title or id. `calendar-sources-list` shows each source with what it can read (ticked or not);
//! `calendar-select` ticks and unticks them, as the Settings checkboxes do; `calendar-connect`
//! signs a google / microsoft source in. An event seen through two sources (the same iCalUID and
//! start) is kept once, from the earlier source in the list. Providers that read the network
//! obey `network.allow`; os-local needs none.
//!
//! On `record-start` the event overlapping now (`rules::pick`) names the session through
//! `calendar.title_template` (else `calendar.title_fallback`), and its id, attendees and
//! participants go into `session.json` (`calendarEvent`, `attendees`, `participants`). On
//! `record-stop` a session that matched nothing is matched again over its whole span. The title
//! stays the user's: an explicit `record-start title=` or a `session-rename` is never overwritten
//! (`autoTitle` records what was set automatically).
//!
//! The macOS Calendars grant is asked for only when an eventkit source is enabled (add, enable,
//! select) or on `calendar-access request=true`; nothing else prompts.
//!
//! Stable error codes: `invalid_value` (a bad `calendar.*` value), `invalid_argument`,
//! `source_exists`, `source_not_found`, `unsupported` (eventkit off macOS), `calendar_access`
//! (no grant), `calendar_access_failed`, `calendar_access_timeout`, `network_denied`,
//! `calendar_not_connected` (an OAuth source never signed in), `calendar_auth` (refused or
//! expired credentials), `calendar_connect_timeout`, `calendar_not_found`, `fetch_failed`,
//! `fetch_timeout`, `http_status`.

pub mod caldav;
pub mod eventkit;
pub mod google;
pub mod graph;
pub mod local;
pub mod oauth;
pub mod provider;
pub mod rules;
#[cfg(windows)]
pub mod windows;

use std::sync::Mutex;

use chrono::{DateTime, Duration, FixedOffset, SecondsFormat, Utc};
use chrono_tz::Tz;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{json, Map, Value};

use crate::action::Action;
use crate::config::typed::{Calendar as CalendarCfg, Config};
use crate::config::{ConfigError, Result};
use crate::control::engine::Engine;
use crate::control::subsystem::Subsystem;

use eventkit::{Access, EventStore};
use provider::uses_eventkit;
use rules::{Event, Rules};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum SourceKind {
    #[default]
    #[serde(rename = "os-local")]
    OsLocal,
    #[serde(rename = "google")]
    Google,
    #[serde(rename = "microsoft")]
    Microsoft,
    #[serde(rename = "caldav")]
    Caldav,
    #[serde(rename = "ics")]
    Ics,
    #[serde(rename = "eventkit")]
    Eventkit,
}

impl SourceKind {
    pub const ALL: [SourceKind; 6] = [
        SourceKind::OsLocal,
        SourceKind::Google,
        SourceKind::Microsoft,
        SourceKind::Caldav,
        SourceKind::Ics,
        SourceKind::Eventkit,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            SourceKind::OsLocal => "os-local",
            SourceKind::Google => "google",
            SourceKind::Microsoft => "microsoft",
            SourceKind::Caldav => "caldav",
            SourceKind::Ics => "ics",
            SourceKind::Eventkit => "eventkit",
        }
    }

    pub fn parse(k: &str) -> Option<SourceKind> {
        SourceKind::ALL.into_iter().find(|x| x.as_str() == k)
    }

    /// Signs in with OAuth (`calendar-connect`).
    pub fn oauth(self) -> bool {
        matches!(self, SourceKind::Google | SourceKind::Microsoft)
    }
}

fn yes() -> bool {
    true
}

/// `"x"` or `["x", "y"]`.
fn one_or_many<'de, D: Deserializer<'de>>(d: D) -> std::result::Result<Vec<String>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum OneOrMany {
        One(String),
        Many(Vec<String>),
    }
    Ok(match OneOrMany::deserialize(d)? {
        OneOrMany::One(s) if s.trim().is_empty() => vec![],
        OneOrMany::One(s) => vec![s],
        OneOrMany::Many(v) => v,
    })
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Source {
    pub name: String,
    pub kind: SourceKind,
    #[serde(default = "yes")]
    pub enabled: bool,
    /// ics: the feeds read (names in `ics.feeds`); empty is every enabled feed.
    #[serde(
        default,
        deserialize_with = "one_or_many",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub feeds: Vec<String>,
    /// eventkit: whole accounts, by title (often the address) or id.
    #[serde(
        default,
        alias = "account",
        deserialize_with = "one_or_many",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub accounts: Vec<String>,
    /// Single calendars, by title or id. Both lists empty is every calendar.
    #[serde(
        default,
        deserialize_with = "one_or_many",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub calendars: Vec<String>,
    /// caldav: the server (or principal, or calendar home) discovery starts from.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub url: String,
    /// caldav: the user name sent with the password.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub username: String,
    /// caldav: the (app) password, as a `secret:` reference.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub password: String,
    /// google / microsoft: the OAuth client id (else `calendar.<kind>_client_id`).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub client_id: String,
    /// google / microsoft: the OAuth client secret, as a `secret:` reference (Google desktop
    /// clients have one).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub client_secret: String,
    /// microsoft: the Entra tenant (else `calendar.microsoft_tenant`).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub tenant: String,
}

fn invalid(key: &str, why: impl std::fmt::Display) -> ConfigError {
    ConfigError::new("invalid_value", format!("calendar.{key}: {why}"))
}

/// The source list. Names are letters, digits, `_`, `-` and `.` (they never name a file, so no
/// length cap).
pub fn parse_sources(text: &str) -> Result<Vec<Source>> {
    let text = if text.trim().is_empty() { "[]" } else { text };
    let sources: Vec<Source> = serde_json::from_str(text)
        .map_err(|e| invalid("sources", format!("not a JSON array of sources: {e}")))?;
    let mut seen = std::collections::BTreeSet::new();
    for s in &sources {
        if !crate::ics::valid_name(&s.name, usize::MAX) {
            return Err(invalid(
                "sources",
                format!("invalid source name '{}'", s.name),
            ));
        }
        if !seen.insert(s.name.clone()) {
            return Err(invalid(
                "sources",
                format!("duplicate source name '{}'", s.name),
            ));
        }
        check_fields(s).map_err(|why| invalid("sources", format!("'{}': {why}", s.name)))?;
    }
    Ok(sources)
}

/// Each field belongs to the kinds that read it; credentials are `secret:` references.
fn check_fields(s: &Source) -> std::result::Result<(), String> {
    use SourceKind::*;
    let k = s.kind;
    let only = |field: &str, set: bool, kinds: &[SourceKind]| {
        if set && !kinds.contains(&k) {
            let names: Vec<&str> = kinds.iter().map(|x| x.as_str()).collect();
            Err(format!("{field} is for {} sources", names.join(" / ")))
        } else {
            Ok(())
        }
    };
    only("feeds", !s.feeds.is_empty(), &[Ics])?;
    only(
        "accounts",
        !s.accounts.is_empty(),
        &[OsLocal, Eventkit, Google, Microsoft, Caldav],
    )?;
    only(
        "calendars",
        !s.calendars.is_empty(),
        &[OsLocal, Eventkit, Google, Microsoft, Caldav],
    )?;
    only("url", !s.url.is_empty(), &[Caldav])?;
    only("username", !s.username.is_empty(), &[Caldav])?;
    only("password", !s.password.is_empty(), &[Caldav])?;
    only("client_id", !s.client_id.is_empty(), &[Google, Microsoft])?;
    only(
        "client_secret",
        !s.client_secret.is_empty(),
        &[Google, Microsoft],
    )?;
    only("tenant", !s.tenant.is_empty(), &[Microsoft])?;
    if k == Caldav && !crate::http::valid_url(s.url.trim()) {
        return Err("a caldav source needs url, an http(s) URL of the server".into());
    }
    for (field, v) in [
        ("password", &s.password),
        ("client_secret", &s.client_secret),
    ] {
        if !v.is_empty() && crate::secrets::parse_ref(v).is_none() {
            return Err(format!(
                "{field} must be a secret reference like secret:caldav/{}, not the secret itself; store it with `secret-set`",
                s.name
            ));
        }
    }
    Ok(())
}

/// The `calendar.*` strings parse: the source list and both title templates.
pub fn validate(c: &CalendarCfg) -> Result<()> {
    parse_sources(&c.sources)?;
    rules::check_template("calendar.title_template", &c.title_template, true)?;
    rules::check_template("calendar.title_fallback", &c.title_fallback, false)?;
    Ok(())
}

/// Vetoes a reload with a bad `calendar.*` value.
pub struct CalendarSubsystem;

impl Subsystem for CalendarSubsystem {
    fn name(&self) -> &'static str {
        "calendar"
    }

    fn validate(&self, new: &Config) -> Result<()> {
        validate(&new.calendar)
    }

    fn apply(&self, _old: &Config, _new: &Config) {}
}

static SOURCE_LIST_LOCK: Mutex<()> = Mutex::new(());

fn load(engine: &Engine) -> Result<Vec<Source>> {
    parse_sources(&engine.snapshot().config.calendar.sources)
}

fn save(engine: &Engine, sources: &[Source], by: &str) -> Result<()> {
    let text = serde_json::to_string(sources).expect("sources serialize");
    engine
        .set("calendar.sources", &Value::String(text), by)
        .map(|_| ())
}

fn find_mut<'a>(sources: &'a mut [Source], name: &str) -> Result<&'a mut Source> {
    sources.iter_mut().find(|s| s.name == name).ok_or_else(|| {
        ConfigError::new(
            "source_not_found",
            format!("no calendar source named '{name}'"),
        )
    })
}

fn err_json(e: &ConfigError) -> Value {
    json!({"code": e.code, "message": e.message})
}

fn stamp(d: DateTime<Utc>) -> String {
    d.to_rfc3339_opts(SecondsFormat::Secs, true)
}

/// The clock titles and all-day dates are read on: `ui.tz`, else the system zone.
fn on_clock(c: &Config, at: DateTime<Utc>) -> DateTime<FixedOffset> {
    match c.ui.tz.trim().parse::<Tz>() {
        Ok(tz) => at.with_timezone(&tz).fixed_offset(),
        Err(_) => at.with_timezone(&chrono::Local).fixed_offset(),
    }
}

/// Source `s` reads calendar `c` (one of its provider's): an ics source by feed name, the others
/// by account and calendar.
pub fn selected(s: &Source, c: &eventkit::Calendar) -> bool {
    if s.kind == SourceKind::Ics {
        return s.feeds.is_empty() || s.feeds.iter().any(|f| f == &c.id);
    }
    if s.accounts.is_empty() && s.calendars.is_empty() {
        return true;
    }
    let same = |a: &str, b: &str| a.trim().eq_ignore_ascii_case(b.trim());
    s.accounts
        .iter()
        .any(|a| same(a, &c.account) || a == &c.account_id)
        || s.calendars.iter().any(|x| x == &c.id || same(x, &c.title))
}

// ---- events ----------------------------------------------------------------------------------

pub(crate) fn from_ics(source: &str, v: &Value) -> Option<Event> {
    let t = |k: &str| {
        v.get(k)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    let at = |k: &str| {
        DateTime::parse_from_rfc3339(&t(k))
            .ok()
            .map(|d| d.with_timezone(&Utc))
    };
    let start = at("start")?;
    let attendees = v
        .get("attendees")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .map(|p| rules::Person {
                    name: p["name"].as_str().unwrap_or_default().to_string(),
                    email: p["email"].as_str().unwrap_or_default().to_string(),
                    status: p["status"].as_str().unwrap_or_default().to_string(),
                    me: false,
                })
                .collect()
        })
        .unwrap_or_default();
    Some(Event {
        source: source.to_string(),
        kind: SourceKind::Ics.as_str().to_string(),
        id: format!("{}@{}", t("uid"), start.timestamp()),
        uid: t("uid"),
        title: t("summary"),
        end: at("end").unwrap_or(start),
        start,
        all_day: v["all_day"].as_bool().unwrap_or(false),
        calendar: t("feed"),
        calendar_id: t("feed"),
        account: String::new(),
        location: t("location"),
        notes: t("description"),
        url: t("url"),
        attendees,
        declined: false,
        cancelled: t("status") == "CANCELLED",
        start_date: v
            .get("start_date")
            .and_then(Value::as_str)
            .map(str::to_string),
    })
}

/// One source's events in `[from, to)`: its provider's calendars, the selected ones read.
fn source_events(
    engine: &Engine,
    s: &Source,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    errors: &mut Vec<Value>,
) -> Result<Vec<Event>> {
    let p = engine.calendar_provider(s.kind);
    if p.networked() {
        provider::check_network(engine, s)?;
    }
    let chosen: Vec<eventkit::Calendar> = p
        .calendars(engine, s)?
        .into_iter()
        .filter(|c| selected(s, c))
        .collect();
    if chosen.is_empty() {
        return Ok(vec![]);
    }
    let cfg = engine.snapshot();
    let mut out = p.events(engine, s, &chosen, from, to, errors)?;
    for e in &mut out {
        e.source = s.name.clone();
        e.kind = s.kind.as_str().to_string();
        if e.all_day && e.start_date.is_none() {
            e.start_date = Some(
                on_clock(&cfg.config, e.start)
                    .format("%Y-%m-%d")
                    .to_string(),
            );
        }
    }
    Ok(out)
}

/// The same meeting seen through two sources (an account in both the OS and a google source):
/// the same iCalUID (case-insensitive) and start. The first one, from the earlier source in
/// `calendar.sources`, stays.
pub fn dedupe(events: &mut Vec<Event>) -> usize {
    let mut seen = std::collections::HashSet::new();
    let before = events.len();
    events.retain(|e| {
        let uid = e.uid.trim();
        uid.is_empty() || seen.insert((uid.to_ascii_lowercase(), e.start))
    });
    before - events.len()
}

/// Every enabled source's events overlapping `[from, to)` (or one source's, enabled or not),
/// deduplicated by iCalUID, sorted by start, and the per-source errors (a failing source does
/// not hide the others).
pub fn gather(
    engine: &Engine,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    only: Option<&str>,
) -> Result<(Vec<Event>, Vec<Value>)> {
    let mut sources = load(engine)?;
    match only {
        Some(n) => {
            let one = find_mut(&mut sources, n)?.clone();
            sources = vec![one];
        }
        None => sources.retain(|s| s.enabled),
    }
    let mut events = Vec::new();
    let mut errors = Vec::new();
    for s in &sources {
        let mut errs = Vec::new();
        match source_events(engine, s, from, to, &mut errs) {
            Ok(mut e) => events.append(&mut e),
            Err(e) => errors.push(json!({"source": s.name, "error": err_json(&e)})),
        }
        for e in errs {
            let mut e = e;
            if e.get("source").is_none() {
                e["source"] = json!(s.name);
            }
            errors.push(e);
        }
    }
    dedupe(&mut events);
    events.sort_by(|a, b| {
        a.start
            .cmp(&b.start)
            .then_with(|| a.source.cmp(&b.source))
            .then_with(|| a.id.cmp(&b.id))
    });
    Ok((events, errors))
}

// ---- actions ---------------------------------------------------------------------------------

pub fn dispatch(engine: &Engine, a: &Action) -> Result<Value> {
    match a {
        Action::CalendarSourcesList {} => sources_list(engine),
        Action::CalendarSourceAdd {
            name,
            kind,
            enabled,
            feeds,
            accounts,
            calendars,
            url,
            username,
            password,
            client_id,
            client_secret,
            tenant,
        } => {
            let kind = SourceKind::parse(kind).ok_or_else(|| {
                let all: Vec<&str> = SourceKind::ALL.iter().map(|k| k.as_str()).collect();
                ConfigError::new(
                    "invalid_argument",
                    format!("kind '{kind}' is not one of {}", all.join(", ")),
                )
            })?;
            let text = |v: &Option<String>| v.as_deref().unwrap_or_default().trim().to_string();
            source_add(
                engine,
                Source {
                    name: name.clone(),
                    kind,
                    enabled: enabled.unwrap_or(true),
                    feeds: strings("feeds", feeds.as_ref())?,
                    accounts: strings("accounts", accounts.as_ref())?,
                    calendars: strings("calendars", calendars.as_ref())?,
                    url: text(url),
                    username: text(username),
                    password: text(password),
                    client_id: text(client_id),
                    client_secret: text(client_secret),
                    tenant: text(tenant),
                },
            )
        }
        Action::CalendarSourceRemove { name } => source_remove(engine, name),
        Action::CalendarSourceEnable { name, enabled } => {
            source_enable(engine, name, enabled.unwrap_or(true))
        }
        Action::CalendarSelect {
            source,
            account,
            calendar,
            selected,
        } => select(
            engine,
            source,
            account.as_deref(),
            calendar.as_deref(),
            selected.unwrap_or(true),
        ),
        Action::CalendarAccess { request } => access(engine, request.unwrap_or(false)),
        Action::CalendarEvents { from, to, source } => {
            events(engine, from.as_deref(), to.as_deref(), source.as_deref())
        }
        Action::CalendarMatch { at, to } => match_now(engine, at.as_deref(), to.as_deref()),
        Action::CalendarConnect { source, open } => connect(engine, source, open.unwrap_or(false)),
        Action::CalendarDisconnect { source } => disconnect(engine, source),
        _ => Err(ConfigError::new("bad_request", "not a calendar action")),
    }
}

/// A list param: one string or an array of strings.
fn strings(name: &str, v: Option<&Value>) -> Result<Vec<String>> {
    let bad = || {
        ConfigError::new(
            "invalid_argument",
            format!("{name} must be a string or an array of strings"),
        )
    };
    match v {
        None | Some(Value::Null) => Ok(vec![]),
        Some(Value::String(s)) if s.trim().is_empty() => Ok(vec![]),
        Some(Value::String(s)) => Ok(vec![s.clone()]),
        Some(Value::Array(a)) => a
            .iter()
            .map(|x| x.as_str().map(str::to_string).ok_or_else(bad))
            .collect(),
        Some(_) => Err(bad()),
    }
}

/// The grant, asked for (the one system prompt) only when still undetermined.
fn ensure_access(engine: &Engine) -> Value {
    let store = engine.event_store();
    if !store.supported() {
        return json!({"authorization": Access::Unsupported.as_str(), "requested": false});
    }
    let now = store.authorization();
    if now != Access::NotDetermined {
        return json!({"authorization": now.as_str(), "requested": false});
    }
    let wait = engine
        .snapshot()
        .config
        .calendar
        .access_timeout
        .as_duration();
    match store.request_access(wait) {
        Ok(a) => json!({"authorization": a.as_str(), "requested": true}),
        Err(e) => json!({"authorization": store.authorization().as_str(), "requested": true,
                         "error": err_json(&e)}),
    }
}

fn access(engine: &Engine, request: bool) -> Result<Value> {
    let store = engine.event_store();
    if request {
        if !store.supported() {
            return Err(store.request_access(std::time::Duration::ZERO).unwrap_err());
        }
        return Ok(ensure_access(engine));
    }
    Ok(
        json!({"supported": store.supported(), "authorization": store.authorization().as_str(),
              "requested": false}),
    )
}

fn source_json(s: &Source) -> Value {
    serde_json::to_value(s).expect("source serializes")
}

fn source_add(engine: &Engine, src: Source) -> Result<Value> {
    let (name, kind, enabled) = (src.name.clone(), src.kind, src.enabled);
    let _g = SOURCE_LIST_LOCK.lock().expect("source list lock");
    let mut sources = load(engine)?;
    if sources.iter().any(|s| s.name == name) {
        return Err(ConfigError::new(
            "source_exists",
            format!("a calendar source named '{name}' already exists"),
        ));
    }
    sources.push(src.clone());
    // parse_sources re-checks the name and the kind's fields before anything is written.
    parse_sources(&serde_json::to_string(&sources).expect("serialize"))
        .map_err(|e| ConfigError::new("invalid_argument", e.message))?;
    save(engine, &sources, "calendar-source-add")?;
    let mut out = json!({"source": source_json(&src), "sources": sources.len()});
    if uses_eventkit(kind) && enabled {
        out["access"] = ensure_access(engine);
    }
    if kind.oauth() {
        out["next"] = json!(format!("calendar-connect source={name}"));
    }
    Ok(out)
}

fn source_remove(engine: &Engine, name: &str) -> Result<Value> {
    let _g = SOURCE_LIST_LOCK.lock().expect("source list lock");
    let mut sources = load(engine)?;
    find_mut(&mut sources, name)?;
    sources.retain(|s| s.name != name);
    save(engine, &sources, "calendar-source-remove")?;
    Ok(json!({"removed": name, "sources": sources.len()}))
}

fn source_enable(engine: &Engine, name: &str, enabled: bool) -> Result<Value> {
    let _g = SOURCE_LIST_LOCK.lock().expect("source list lock");
    let mut sources = load(engine)?;
    let s = find_mut(&mut sources, name)?;
    s.enabled = enabled;
    let src = s.clone();
    save(engine, &sources, "calendar-source-enable")?;
    let mut out = json!({"source": source_json(&src)});
    if uses_eventkit(src.kind) && enabled {
        out["access"] = ensure_access(engine);
    }
    Ok(out)
}

/// Tick (or untick) one calendar, or every calendar of one account, in a source, as a Settings
/// checkbox does. The selection is rewritten in its shortest form: whole accounts by
/// title, the rest by calendar id, and both lists empty when everything is ticked. Unticking the
/// last calendar disables the source; ticking one in a disabled source enables it with just that.
fn select(
    engine: &Engine,
    source: &str,
    account: Option<&str>,
    calendar: Option<&str>,
    on: bool,
) -> Result<Value> {
    if account.is_none() && calendar.is_none() {
        return Err(ConfigError::new(
            "invalid_argument",
            "name an account or a calendar to select",
        ));
    }
    let _g = SOURCE_LIST_LOCK.lock().expect("source list lock");
    let mut sources = load(engine)?;
    let s = find_mut(&mut sources, source)?;
    if s.kind == SourceKind::Ics {
        return Err(ConfigError::new(
            "invalid_argument",
            format!("'{source}' is an ics source; select its feeds with `feeds`"),
        ));
    }
    let p = engine.calendar_provider(s.kind);
    if p.networked() {
        provider::check_network(engine, s)?;
    }
    let all = p.calendars(engine, s)?;
    let same = |a: &str, b: &str| a.trim().eq_ignore_ascii_case(b.trim());
    let in_account =
        |c: &eventkit::Calendar| account.is_none_or(|x| same(x, &c.account) || x == c.account_id);
    let targets: Vec<&eventkit::Calendar> = all
        .iter()
        .filter(|c| in_account(c))
        .filter(|c| calendar.is_none_or(|x| x == c.id || same(x, &c.title)))
        .collect();
    if targets.is_empty() {
        return Err(ConfigError::new(
            "invalid_argument",
            format!(
                "no calendar matches account {account:?} calendar {calendar:?} (see calendar-sources-list)"
            ),
        ));
    }
    let was_enabled = s.enabled;
    let mut set: Vec<&str> = if s.enabled {
        all.iter()
            .filter(|c| selected(s, c))
            .map(|c| c.id.as_str())
            .collect()
    } else {
        vec![]
    };
    for t in &targets {
        if on && !set.contains(&t.id.as_str()) {
            set.push(&t.id);
        } else if !on {
            set.retain(|id| *id != t.id);
        }
    }
    s.enabled = !set.is_empty();
    s.accounts.clear();
    s.calendars.clear();
    if set.len() < all.len() {
        let mut accounts: Vec<(&str, &str)> = all
            .iter()
            .map(|c| (c.account_id.as_str(), c.account.as_str()))
            .collect();
        accounts.sort();
        accounts.dedup();
        for (id, title) in accounts {
            let mine: Vec<&eventkit::Calendar> =
                all.iter().filter(|c| c.account_id == id).collect();
            if mine.iter().all(|c| set.contains(&c.id.as_str())) {
                s.accounts.push(title.to_string());
            }
        }
        for c in &all {
            let whole = s.accounts.iter().any(|a| a == &c.account);
            if set.contains(&c.id.as_str()) && !whole {
                s.calendars.push(c.id.clone());
            }
        }
    }
    let src = s.clone();
    save(engine, &sources, "calendar-select")?;
    let mut out =
        json!({"source": source_json(&src), "selected": set.len(), "calendars": all.len()});
    if src.enabled && !was_enabled && uses_eventkit(src.kind) {
        out["access"] = ensure_access(engine);
    }
    Ok(out)
}

fn sources_list(engine: &Engine) -> Result<Value> {
    let sources = load(engine)?;
    let store = engine.event_store();
    let access = store.authorization();
    let feeds = crate::ics::dispatch(engine, &Action::IcsList {})
        .ok()
        .and_then(|v| v["feeds"].as_array().cloned())
        .unwrap_or_default();
    let rows: Vec<Value> = sources
        .iter()
        .map(|s| {
            let mut v = source_json(s);
            v["enabled"] = json!(s.enabled);
            if s.kind == SourceKind::Ics {
                v["available"] = json!(feeds
                    .iter()
                    .map(|f| {
                        let n = f["name"].as_str().unwrap_or_default();
                        let on = f["enabled"].as_bool().unwrap_or(true)
                            && (s.feeds.is_empty() || s.feeds.iter().any(|x| x == n));
                        json!({"name": n, "enabled": f["enabled"], "selected": on})
                    })
                    .collect::<Vec<_>>());
                return v;
            }
            if uses_eventkit(s.kind) {
                v["authorization"] = json!(access.as_str());
            }
            if s.kind.oauth() {
                let t = oauth::load(engine, s).ok().flatten();
                v["connected"] = json!(t.as_ref().is_some_and(|t| !t.refresh_token.is_empty()));
                v["account"] = json!(t.map(|t| t.account).unwrap_or_default());
            }
            let p = engine.calendar_provider(s.kind);
            let got = if p.networked() {
                provider::check_network(engine, s).and_then(|_| p.calendars(engine, s))
            } else {
                p.calendars(engine, s)
            };
            match got {
                Ok(cals) => v["accounts"] = accounts_json(s, &cals),
                Err(e) => {
                    v["accounts"] = json!([]);
                    v["error"] = err_json(&e);
                }
            }
            v
        })
        .collect();
    Ok(json!({
        "sources": rows,
        "kinds": SourceKind::ALL.iter().map(|k| k.as_str()).collect::<Vec<_>>(),
        "eventkit": {"supported": store.supported(), "authorization": access.as_str()},
    }))
}

/// `calendar-connect`: sign an OAuth source in; discover a caldav server's calendars; ask for the
/// Calendars grant of an os-local / eventkit source on a Mac.
fn connect(engine: &Engine, name: &str, open: bool) -> Result<Value> {
    let sources = load(engine)?;
    let s = sources.iter().find(|s| s.name == name).ok_or_else(|| {
        ConfigError::new(
            "source_not_found",
            format!("no calendar source named '{name}'"),
        )
    })?;
    if s.kind.oauth() {
        return oauth::connect(engine, s, open);
    }
    let mut out = json!({"source": s.name, "kind": s.kind.as_str()});
    if uses_eventkit(s.kind) {
        out["access"] = ensure_access(engine);
    }
    let p = engine.calendar_provider(s.kind);
    if p.networked() {
        provider::check_network(engine, s)?;
    }
    let cals = p.calendars(engine, s)?;
    out["accounts"] = accounts_json(s, &cals);
    out["calendars"] = json!(cals.len());
    Ok(out)
}

fn disconnect(engine: &Engine, name: &str) -> Result<Value> {
    let sources = load(engine)?;
    let s = sources.iter().find(|s| s.name == name).ok_or_else(|| {
        ConfigError::new(
            "source_not_found",
            format!("no calendar source named '{name}'"),
        )
    })?;
    if !s.kind.oauth() {
        return Err(ConfigError::new(
            "invalid_argument",
            format!(
                "'{name}' is a {} source; only google and microsoft sources sign in",
                s.kind.as_str()
            ),
        ));
    }
    Ok(json!({"source": name, "forgotten": oauth::forget(engine, s)?}))
}

/// The calendars grouped by account, each ticked when source `s` reads it.
fn accounts_json(s: &Source, cals: &[eventkit::Calendar]) -> Value {
    let mut ids: Vec<(&str, &str, &str)> = cals
        .iter()
        .map(|c| {
            (
                c.account.as_str(),
                c.account_id.as_str(),
                c.account_kind.as_str(),
            )
        })
        .collect();
    ids.sort();
    ids.dedup();
    let rows: Vec<Value> = ids
        .into_iter()
        .map(|(title, id, kind)| {
            let mine: Vec<Value> = cals
                .iter()
                .filter(|c| c.account_id == id)
                .map(|c| {
                    let mut v = c.to_json();
                    v["selected"] = json!(s.enabled && selected(s, c));
                    v
                })
                .collect();
            let all = mine.iter().all(|c| c["selected"] == true);
            json!({"title": title, "id": id, "kind": kind, "selected": all, "calendars": mine})
        })
        .collect();
    Value::Array(rows)
}

fn window(
    engine: &Engine,
    from: Option<&str>,
    to: Option<&str>,
) -> Result<(DateTime<Utc>, DateTime<Utc>)> {
    let c = engine.snapshot();
    let tz = c
        .config
        .ics
        .default_tz
        .trim()
        .parse::<Tz>()
        .unwrap_or(Tz::UTC);
    let from = match from {
        Some(v) => crate::ics::parse_instant("from", v, tz)?,
        None => Utc::now(),
    };
    let to = match to {
        Some(v) => crate::ics::parse_instant("to", v, tz)?,
        None => from + Duration::days(i64::from(c.config.ics.window_days)),
    };
    if to < from {
        return Err(ConfigError::new(
            "invalid_argument",
            "'to' is before 'from'",
        ));
    }
    Ok((from, to))
}

fn events(
    engine: &Engine,
    from: Option<&str>,
    to: Option<&str>,
    source: Option<&str>,
) -> Result<Value> {
    let (from, to) = window(engine, from, to)?;
    let (evs, errors) = gather(engine, from, to, source)?;
    let r = Rules::from_config(&engine.snapshot().config.calendar);
    let rows: Vec<Value> = evs
        .iter()
        .map(|e| {
            let mut v = e.to_json(&r);
            // The ics-events names the Calendar view reads.
            v["summary"] = json!(e.title);
            v["feed"] = json!(if e.calendar.is_empty() {
                &e.source
            } else {
                &e.calendar
            });
            v
        })
        .collect();
    Ok(json!({"from": stamp(from), "to": stamp(to), "events": rows, "errors": errors}))
}

// ---- titling sessions ------------------------------------------------------------------------

/// What a match puts in `session.json`, and what the caller reports.
#[derive(Debug, Default)]
pub struct Titled {
    /// The automatic title; None leaves the session untitled.
    pub title: Option<String>,
    pub fields: Map<String, Value>,
    pub report: Value,
}

fn event_meta(e: &Event, r: &Rules) -> Map<String, Value> {
    let mut m = Map::new();
    m.insert(
        "calendarEvent".into(),
        json!({
            "id": e.id, "source": e.source, "kind": e.kind, "title": e.title,
            "calendar": e.calendar, "calendarId": e.calendar_id, "account": e.account,
            "start": stamp(e.start), "end": stamp(e.end), "location": e.location,
            "url": e.url, "videoUrl": rules::video_link(e, &r.video_hosts),
        }),
    );
    m.insert(
        "attendees".into(),
        json!(e
            .attendees
            .iter()
            .map(rules::Person::to_json)
            .collect::<Vec<_>>()),
    );
    m.insert("participants".into(), json!(rules::participants(e, r)));
    m
}

/// Match `[from, to]` and build the title and metadata.
fn titled(engine: &Engine, from: DateTime<Utc>, to: DateTime<Utc>) -> Titled {
    let snap = engine.snapshot();
    let c = &snap.config;
    let r = Rules::from_config(&c.calendar);
    let at = on_clock(c, from);
    let (events, errors) = match gather(engine, from, to + r.lead + Duration::seconds(1), None) {
        Ok(x) => x,
        Err(e) => (vec![], vec![json!({"error": err_json(&e)})]),
    };
    let m = rules::pick(&events, from, to, &r);
    let mut out = Titled::default();
    let picked = m.picked.map(|i| &events[i]);
    let mut title = picked
        .map(|e| rules::render(&c.calendar.title_template, Some(e), at))
        .unwrap_or_default();
    if title.is_empty() {
        title = rules::render(&c.calendar.title_fallback, None, at);
    }
    if let Some(e) = picked {
        out.fields = event_meta(e, &r);
    }
    out.report = json!({
        "event": picked.map(|e| e.to_json(&r)),
        "title": title,
        "fallback": picked.is_none(),
        "candidates": m.candidates.iter().map(|(i, why)| {
            json!({"id": events[*i].id, "source": events[*i].source, "title": events[*i].title,
                   "verdict": why})
        }).collect::<Vec<_>>(),
        "errors": errors,
    });
    out.title = Some(title).filter(|t| !t.is_empty());
    out
}

/// `calendar-match`: what a recording over `[at, to]` (default: starting now) would be titled.
fn match_now(engine: &Engine, at: Option<&str>, to: Option<&str>) -> Result<Value> {
    let c = engine.snapshot();
    let tz = c
        .config
        .ics
        .default_tz
        .trim()
        .parse::<Tz>()
        .unwrap_or(Tz::UTC);
    let from = match at {
        Some(v) => crate::ics::parse_instant("at", v, tz)?,
        None => Utc::now(),
    };
    let to = match to {
        Some(v) => crate::ics::parse_instant("to", v, tz)?,
        None => from,
    };
    if to < from {
        return Err(ConfigError::new("invalid_argument", "'to' is before 'at'"));
    }
    let mut v = titled(engine, from, to).report;
    v["at"] = json!(stamp(from));
    v["to"] = json!(stamp(to));
    Ok(v)
}

/// At `record-start`: the event overlapping `now`. `given` is the title passed in; with one the
/// event is still recorded but the title is the user's.
pub fn for_start(engine: &Engine, now: DateTime<Utc>, given: Option<&str>) -> Titled {
    if !engine.snapshot().config.calendar.title_from_event {
        return Titled {
            report: json!({"state": "off"}),
            ..Titled::default()
        };
    }
    let mut t = titled(engine, now, now);
    match given {
        Some(g) => {
            t.title = Some(g.to_string());
        }
        None => {
            t.fields.insert(
                "autoTitle".into(),
                json!(t.title.clone().unwrap_or_default()),
            );
        }
    }
    t
}

/// At `record-stop`: a session that matched no event at its start is matched over its whole span;
/// its title follows only if nobody set one by hand. Returns what changed, if anything.
pub fn on_saved(engine: &Engine, id: &str) -> Option<Value> {
    let c = engine.snapshot().config.calendar.clone();
    if !c.title_from_event || !c.retitle_on_save {
        return None;
    }
    let store = engine.store();
    let meta = store.get(id).ok()?["metadata"].clone();
    if meta.get("calendarEvent").is_some_and(Value::is_object) {
        return None;
    }
    let at = |k: &str| {
        meta.get(k)
            .and_then(Value::as_str)
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|d| d.with_timezone(&Utc))
    };
    let from = at("startedAt")?;
    let to = at("endedAt").unwrap_or_else(Utc::now).max(from);
    let t = titled(engine, from, to);
    if t.fields.is_empty() {
        return None;
    }
    let mut fields = t.fields;
    let now = meta
        .get("title")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let auto = meta.get("autoTitle").and_then(Value::as_str);
    if auto == Some(now) {
        let title = t.title.unwrap_or_default();
        fields.insert("title".into(), json!(title));
        fields.insert("autoTitle".into(), json!(title));
    }
    store.merge_meta(id, &fields).ok()?;
    Some(t.report)
}

/// The engine's EventKit store, swappable for tests.
pub struct StoreSlot(pub Mutex<std::sync::Arc<dyn EventStore>>);

impl Default for StoreSlot {
    fn default() -> Self {
        StoreSlot(Mutex::new(std::sync::Arc::from(eventkit::system())))
    }
}
