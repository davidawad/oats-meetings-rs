//! One trait for every calendar provider (dot-4uzgol.100): a provider lists the calendars a source
//! can read, each with the account holding it, and the events of the chosen ones in a window.
//! `calendar.sources` names a provider by `kind`; the checkboxes (`calendar-select`) and
//! `calendar-sources-list` work the same over all of them.
//!
//! - `os-local`: this computer's accounts, no network: macOS EventKit (whatever Internet Accounts
//!   holds), Linux Evolution Data Server and KDE Akonadi (`local`), Windows Appointments
//!   (`windows`).
//! - `google`: Google Calendar API v3 (`google`), OAuth installed-app flow (`oauth`).
//! - `microsoft`: Microsoft Graph `calendarView` (`graph`), OAuth.
//! - `caldav`: any CalDAV server, found through `.well-known/caldav` (`caldav`).
//! - `ics`: the cached `ics.feeds` (URLs and local files).
//! - `eventkit`: the macOS Calendar app (the os-local provider of a Mac, by its old name).
//! - `command`: a program the user names, run on each read, printing ICS or JSON (`command`).
//!
//! Tests swap any provider for a fake with `Engine::set_calendar_provider`.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Utc};
use serde_json::{json, Value};

use super::eventkit::{Access, Calendar};
use super::rules::Event;
use super::{Source, SourceKind};
use crate::action::Action;
use crate::config::{ConfigError, Result};
use crate::control::engine::Engine;

pub trait Provider: Send + Sync {
    /// It reads the network, so `network.allow` gates it.
    fn networked(&self) -> bool;
    /// Every calendar source `s` can read, each with its account.
    fn calendars(&self, engine: &Engine, s: &Source) -> Result<Vec<Calendar>>;
    /// The occurrences overlapping `[from, to)` in `cals` (a subset of `calendars`). `source` and
    /// `kind` are filled by the caller. A calendar that fails alone is reported in `errors` and
    /// the others still read.
    fn events(
        &self,
        engine: &Engine,
        s: &Source,
        cals: &[Calendar],
        from: DateTime<Utc>,
        to: DateTime<Utc>,
        errors: &mut Vec<Value>,
    ) -> Result<Vec<Event>>;
}

/// The engine's providers: the built-in one per kind unless a test put another in; and the
/// command sources' last runs.
#[derive(Default)]
pub struct ProviderSlot(
    Mutex<BTreeMap<&'static str, Arc<dyn Provider>>>,
    super::command::Runs,
);

impl ProviderSlot {
    pub fn get(&self, kind: SourceKind) -> Arc<dyn Provider> {
        if let Some(p) = self.0.lock().expect("providers").get(kind.as_str()) {
            return Arc::clone(p);
        }
        builtin(kind)
    }

    pub fn set(&self, kind: SourceKind, p: Arc<dyn Provider>) {
        self.0.lock().expect("providers").insert(kind.as_str(), p);
    }

    pub fn command_runs(&self) -> &super::command::Runs {
        &self.1
    }
}

fn builtin(kind: SourceKind) -> Arc<dyn Provider> {
    match kind {
        SourceKind::Ics => Arc::new(IcsProvider),
        SourceKind::Eventkit => Arc::new(EventKitProvider),
        SourceKind::OsLocal => os_local(),
        SourceKind::Google => Arc::new(super::google::Google),
        SourceKind::Microsoft => Arc::new(super::graph::Graph),
        SourceKind::Caldav => Arc::new(super::caldav::CalDav),
        SourceKind::Command => Arc::new(super::command::CommandProvider),
    }
}

/// This platform's os-local provider.
pub fn os_local() -> Arc<dyn Provider> {
    #[cfg(target_os = "macos")]
    {
        Arc::new(EventKitProvider)
    }
    #[cfg(windows)]
    {
        Arc::new(super::windows::Appointments)
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    {
        Arc::new(super::local::Local::from_env())
    }
}

/// The source's kind reads the macOS Calendars store, so turning it on asks for the grant.
pub fn uses_eventkit(kind: SourceKind) -> bool {
    kind == SourceKind::Eventkit || (kind == SourceKind::OsLocal && cfg!(target_os = "macos"))
}

// ---- ics -------------------------------------------------------------------------------------

/// The cached `ics.feeds`, one calendar per feed under one "ICS feeds" account.
pub struct IcsProvider;

impl Provider for IcsProvider {
    fn networked(&self) -> bool {
        false // the feeds are fetched (behind network.allow) by the ics refresher, read from cache
    }

    fn calendars(&self, engine: &Engine, _: &Source) -> Result<Vec<Calendar>> {
        let v = crate::ics::dispatch(engine, &Action::IcsList {})?;
        Ok(v["feeds"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|f| f["name"].as_str())
            .map(|n| Calendar {
                id: n.to_string(),
                title: n.to_string(),
                account: "ICS feeds".into(),
                account_id: "ics".into(),
                account_kind: "ics".into(),
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
        let v = crate::ics::dispatch(
            engine,
            &Action::IcsEvents {
                from: Some(super::stamp(from)),
                to: Some(super::stamp(to)),
                feed: None,
            },
        )?;
        let wanted = |feed: &str| cals.iter().any(|c| c.id == feed);
        errors.extend(
            v["errors"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|e| wanted(e["name"].as_str().unwrap_or_default()))
                .map(|e| json!({"source": s.name, "feed": e["name"], "error": e["error"]})),
        );
        Ok(v["events"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|e| wanted(e["feed"].as_str().unwrap_or_default()))
            .filter_map(|e| super::from_ics(&s.name, e))
            .collect())
    }
}

// ---- eventkit --------------------------------------------------------------------------------

pub fn no_access(a: Access) -> ConfigError {
    ConfigError::new(
        "calendar_access",
        format!(
            "Calendars access is {}: enable an eventkit / os-local source or run `calendar-access request=true`",
            a.as_str()
        ),
    )
}

/// The engine's EventKit store (macOS Calendar): every account Internet Accounts syncs.
pub struct EventKitProvider;

impl Provider for EventKitProvider {
    fn networked(&self) -> bool {
        false
    }

    fn calendars(&self, engine: &Engine, _: &Source) -> Result<Vec<Calendar>> {
        let store = engine.event_store();
        if !store.supported() {
            return store.calendars();
        }
        let a = store.authorization();
        if !a.readable() {
            return Err(no_access(a));
        }
        store.calendars()
    }

    fn events(
        &self,
        engine: &Engine,
        _: &Source,
        cals: &[Calendar],
        from: DateTime<Utc>,
        to: DateTime<Utc>,
        _: &mut Vec<Value>,
    ) -> Result<Vec<Event>> {
        let ids: Vec<String> = cals.iter().map(|c| c.id.clone()).collect();
        engine.event_store().events(from, to, &ids)
    }
}

// ---- shared helpers for the providers --------------------------------------------------------

/// ICS text (one VCALENDAR) expanded into the events of `[from, to)`, read with the `ics.*`
/// parse options; each event is filed under calendar `cal`.
pub fn ics_text_events(
    engine: &Engine,
    text: &str,
    cal: &Calendar,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
) -> Result<Vec<Event>> {
    let c = engine.snapshot();
    let settings = crate::ics::Settings::from_config(&c.config.ics)?;
    let parsed = crate::ics::parse::parse_feed(text, &settings.parse)?;
    let rows = crate::ics::parse::expand(
        &cal.title,
        &parsed,
        &settings.parse,
        settings.max_occ,
        from,
        to,
    )?;
    Ok(rows
        .iter()
        .filter_map(|v| super::from_ics("", v))
        .map(|mut e| {
            e.calendar = cal.title.clone();
            e.calendar_id = cal.id.clone();
            e.account = cal.account.clone();
            e
        })
        .collect())
}

/// Percent-encode everything but RFC 3986's unreserved characters (a path segment or a query
/// value).
pub fn enc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

pub fn err_json(e: &ConfigError) -> Value {
    json!({"code": e.code, "message": e.message})
}

/// `network_denied` unless `network.allow`.
pub fn check_network(engine: &Engine, s: &Source) -> Result<()> {
    if engine.snapshot().config.network.allow {
        Ok(())
    } else {
        Err(ConfigError::new(
            "network_denied",
            format!(
                "calendar source '{}' ({}) needs network.allow = true",
                s.name,
                s.kind.as_str()
            ),
        ))
    }
}

/// The blocking HTTP client the providers share: `calendar.timeout` for the whole request, no
/// status-as-error (the callers map statuses), WebDAV methods allowed, no automatic redirects
/// (CalDAV follows them itself so a PROPFIND stays a PROPFIND).
pub fn agent(engine: &Engine) -> ureq::Agent {
    let timeout = engine.snapshot().config.calendar.timeout.as_duration();
    let builder = ureq::Agent::config_builder()
        .timeout_global(Some(timeout))
        .http_status_as_error(false)
        .allow_non_standard_methods(true)
        .max_redirects(0);
    #[cfg(windows)]
    let builder = builder.tls_config(
        ureq::tls::TlsConfig::builder()
            .provider(ureq::tls::TlsProvider::NativeTls)
            .build(),
    );
    builder.build().into()
}

pub fn unreachable(url: &str, e: ureq::Error) -> ConfigError {
    match e {
        ureq::Error::Timeout(_) => ConfigError::new("fetch_timeout", format!("{url}: {e}")),
        other => ConfigError::new("fetch_failed", format!("{url}: {other}")),
    }
}

/// The body of a reply, as text; at most `ics.max_bytes`.
pub fn body_text(
    engine: &Engine,
    url: &str,
    resp: &mut ureq::http::Response<ureq::Body>,
) -> Result<String> {
    let max = engine.snapshot().config.ics.max_bytes;
    let bytes = resp
        .body_mut()
        .with_config()
        .limit(max)
        .read_to_vec()
        .map_err(|e| unreachable(url, e))?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// An HTTP status outside 2xx as an error, with the start of the reply.
pub fn status_error(engine: &Engine, url: &str, status: u16, body: &str) -> ConfigError {
    let n = engine.snapshot().config.network.error_snippet as usize;
    let snippet: String = body.chars().take(n).collect();
    let code = match status {
        401 | 403 => "calendar_auth",
        404 => "calendar_not_found",
        _ => "http_status",
    };
    ConfigError::new(code, format!("{url}: HTTP {status}: {snippet}"))
}

/// GET `url` with a bearer token (and extra headers) and parse the JSON reply.
pub fn get_json(
    engine: &Engine,
    url: &str,
    query: &[(&str, String)],
    bearer: &str,
    headers: &[(&str, &str)],
) -> Result<Value> {
    let mut req = agent(engine)
        .get(url)
        .header("Authorization", format!("Bearer {bearer}"))
        .header("Accept", "application/json");
    for (k, v) in headers {
        req = req.header(*k, *v);
    }
    for (k, v) in query {
        req = req.query(*k, v);
    }
    let mut resp = req.call().map_err(|e| unreachable(url, e))?;
    let status = resp.status().as_u16();
    let text = body_text(engine, url, &mut resp)?;
    if !(200..300).contains(&status) {
        return Err(status_error(engine, url, status, &text));
    }
    serde_json::from_str(&text)
        .map_err(|e| ConfigError::new("backend_bad_response", format!("{url}: not JSON: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_encoding_keeps_unreserved_only() {
        assert_eq!(enc("me@davidaw.ad"), "me%40davidaw.ad");
        assert_eq!(
            enc("en.usa#holiday@group.v.calendar.google.com"),
            "en.usa%23holiday%40group.v.calendar.google.com"
        );
        assert_eq!(enc("a b/c~_-."), "a%20b%2Fc~_-.");
    }
}
