//! The os-local provider off macOS and Windows: the calendar accounts the Linux desktop already
//! holds, read from their own files with no network and no permission prompt.
//!
//! - Evolution Data Server (GNOME Calendar, Evolution, and every GNOME Online Accounts account
//!   with Calendar on): the sources are keyfiles in `$XDG_CONFIG_HOME/evolution/sources`. A
//!   `[Calendar]` source is a calendar; its parent `[Collection]` source is the account (a GOA
//!   Google / Microsoft / Nextcloud account, or "On This Computer"). A `local` calendar's events
//!   are `$XDG_DATA_HOME/evolution/calendar/<uid>/calendar.ics`; a synced one's are EDS's offline
//!   cache, `$XDG_CACHE_HOME/evolution/calendar/<uid>/cache.db` (SQLite, read only).
//! - KDE Akonadi (KOrganizer, Merkuro): the iCal file and iCal folder resources
//!   (`$XDG_CONFIG_HOME/akonadi_ical_resource_*rc`, `akonadi_icaldir_resource_*rc`). Akonadi's
//!   DAV accounts keep their events in its database; add those as a caldav source.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde_json::{json, Value};

use super::eventkit::Calendar;
use super::provider::{err_json, ics_text_events, Provider};
use super::rules::Event;
use super::Source;
use crate::config::{ConfigError, Result};
use crate::control::engine::Engine;

/// Where the desktop keeps its files (the XDG base directories).
#[derive(Clone, Debug)]
pub struct Local {
    pub config: PathBuf,
    pub data: PathBuf,
    pub cache: PathBuf,
}

/// A keyfile (`.source`, `*rc`): group -> key -> value. Localized keys (`Name[de]`) are dropped.
pub fn keyfile(text: &str) -> BTreeMap<String, BTreeMap<String, String>> {
    let mut out: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
    let mut group = String::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if let Some(g) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            group = g.to_string();
            out.entry(group.clone()).or_default();
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            let k = k.trim();
            if !k.contains('[') {
                out.entry(group.clone())
                    .or_default()
                    .insert(k.to_string(), v.trim().to_string());
            }
        }
    }
    out
}

/// Where one calendar's events are.
#[derive(Clone, Debug, PartialEq)]
enum Store {
    Ics(PathBuf),
    IcsDir(PathBuf),
    EdsCache(PathBuf),
}

impl Local {
    pub fn from_env() -> Local {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_default();
        let xdg = |var: &str, dflt: &str| {
            std::env::var_os(var)
                .map(PathBuf::from)
                .filter(|p| p.is_absolute())
                .unwrap_or_else(|| home.join(dflt))
        };
        Local {
            config: xdg("XDG_CONFIG_HOME", ".config"),
            data: xdg("XDG_DATA_HOME", ".local/share"),
            cache: xdg("XDG_CACHE_HOME", ".cache"),
        }
    }

    /// Every calendar found, with where its events are.
    fn scan(&self) -> Vec<(Calendar, Store)> {
        let mut out = self.eds();
        out.extend(self.akonadi());
        out
    }

    fn eds(&self) -> Vec<(Calendar, Store)> {
        let dir = self.config.join("evolution/sources");
        let mut sources: BTreeMap<String, BTreeMap<String, BTreeMap<String, String>>> =
            BTreeMap::new();
        for e in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let p = e.path();
            if p.extension().and_then(|x| x.to_str()) != Some("source") {
                continue;
            }
            let (Some(uid), Ok(text)) = (
                p.file_stem().and_then(|s| s.to_str()).map(str::to_string),
                std::fs::read_to_string(&p),
            ) else {
                continue;
            };
            sources.insert(uid, keyfile(&text));
        }
        // EDS's built-in "Personal" calendar has no .source file until it is edited.
        let system = self.data.join("evolution/calendar/system/calendar.ics");
        if !sources.contains_key("system-calendar") && system.exists() {
            let mut k = BTreeMap::new();
            k.insert(
                "Data Source".to_string(),
                BTreeMap::from([
                    ("DisplayName".to_string(), "Personal".to_string()),
                    ("Parent".to_string(), "local-stub".to_string()),
                ]),
            );
            k.insert(
                "Calendar".to_string(),
                BTreeMap::from([("BackendName".to_string(), "local".to_string())]),
            );
            sources.insert("system-calendar".into(), k);
        }
        let get = |k: &BTreeMap<String, BTreeMap<String, String>>, g: &str, key: &str| {
            k.get(g)
                .and_then(|g| g.get(key))
                .cloned()
                .unwrap_or_default()
        };
        let enabled = |k: &BTreeMap<String, BTreeMap<String, String>>| {
            !get(k, "Data Source", "Enabled").eq_ignore_ascii_case("false")
        };
        let mut out = Vec::new();
        for (uid, k) in &sources {
            if !k.contains_key("Calendar") || !enabled(k) {
                continue;
            }
            let backend = get(k, "Calendar", "BackendName");
            let parent = get(k, "Data Source", "Parent");
            let (account, account_kind) = match sources.get(&parent) {
                Some(p) if p.contains_key("Collection") => {
                    if !enabled(p) {
                        continue;
                    }
                    let name = [
                        get(p, "Collection", "Identity"),
                        get(p, "Authentication", "User"),
                        get(p, "Data Source", "DisplayName"),
                    ]
                    .into_iter()
                    .find(|n| !n.is_empty())
                    .unwrap_or_else(|| parent.clone());
                    let kind = get(p, "Collection", "BackendName");
                    (
                        name,
                        if kind.is_empty() {
                            "collection".into()
                        } else {
                            kind
                        },
                    )
                }
                Some(p) => (get(p, "Data Source", "DisplayName"), backend.clone()),
                None => match parent.as_str() {
                    "local-stub" | "" => ("On This Computer".into(), "local".into()),
                    "webcal-stub" => ("On The Web".into(), "webcal".into()),
                    "caldav-stub" => ("CalDAV".into(), "caldav".into()),
                    "google-stub" => ("Google".into(), "google".into()),
                    "weather-stub" => ("Weather".into(), "weather".into()),
                    "birthdays" | "contacts-stub" => ("Contacts".into(), "contacts".into()),
                    other => (other.to_string(), backend.clone()),
                },
            };
            let store = if backend == "local" {
                let folder = if uid == "system-calendar" {
                    "system"
                } else {
                    uid
                };
                Store::Ics(
                    self.data
                        .join("evolution/calendar")
                        .join(folder)
                        .join("calendar.ics"),
                )
            } else {
                Store::EdsCache(
                    self.cache
                        .join("evolution/calendar")
                        .join(uid)
                        .join("cache.db"),
                )
            };
            let title = get(k, "Data Source", "DisplayName");
            out.push((
                Calendar {
                    id: uid.clone(),
                    title: if title.is_empty() { uid.clone() } else { title },
                    account,
                    account_id: if parent.is_empty() {
                        "local-stub".into()
                    } else {
                        parent
                    },
                    account_kind: format!("eds-{account_kind}"),
                },
                store,
            ));
        }
        out
    }

    fn akonadi(&self) -> Vec<(Calendar, Store)> {
        let mut out = Vec::new();
        for e in std::fs::read_dir(&self.config)
            .into_iter()
            .flatten()
            .flatten()
        {
            let name = e.file_name().to_string_lossy().to_string();
            let (dir, id) = if let Some(id) = name
                .strip_suffix("rc")
                .filter(|n| n.starts_with("akonadi_ical_resource_"))
            {
                (false, id.to_string())
            } else if let Some(id) = name
                .strip_suffix("rc")
                .filter(|n| n.starts_with("akonadi_icaldir_resource_"))
            {
                (true, id.to_string())
            } else {
                continue;
            };
            let Ok(text) = std::fs::read_to_string(e.path()) else {
                continue;
            };
            let k = keyfile(&text);
            let Some(path) = k
                .get("General")
                .and_then(|g| g.get("Path"))
                .filter(|p| !p.is_empty())
            else {
                continue;
            };
            let path = PathBuf::from(path.strip_prefix("file://").unwrap_or(path));
            let agent = std::fs::read_to_string(
                self.config
                    .join("akonadi")
                    .join(format!("agent_config_{id}")),
            )
            .ok()
            .map(|t| keyfile(&t))
            .and_then(|k| k.get("Agent").and_then(|a| a.get("Name")).cloned())
            .filter(|n| !n.is_empty());
            let title = agent.unwrap_or_else(|| {
                path.file_stem()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_else(|| id.clone())
            });
            out.push((
                Calendar {
                    id: id.clone(),
                    title,
                    account: "KDE (Akonadi)".into(),
                    account_id: "akonadi".into(),
                    account_kind: if dir {
                        "akonadi-icaldir".into()
                    } else {
                        "akonadi-ical".into()
                    },
                },
                if dir {
                    Store::IcsDir(path)
                } else {
                    Store::Ics(path)
                },
            ));
        }
        out.sort_by(|a, b| a.0.id.cmp(&b.0.id));
        out
    }
}

fn read(path: &Path) -> Result<String> {
    std::fs::read_to_string(path)
        .map_err(|e| ConfigError::new("fetch_failed", format!("{}: {e}", path.display())))
}

/// Bare components (EDS stores VEVENTs and VTIMEZONEs on their own) as one VCALENDAR.
pub fn wrap(components: &[String]) -> String {
    let cals: Vec<String> = components
        .iter()
        .map(|c| {
            if c.trim_start()
                .to_ascii_uppercase()
                .starts_with("BEGIN:VCALENDAR")
            {
                c.clone()
            } else {
                format!("BEGIN:VCALENDAR\r\n{}\r\nEND:VCALENDAR\r\n", c.trim())
            }
        })
        .collect();
    crate::ics::fetch::merge_calendars(&cals)
}

/// The VEVENTs and VTIMEZONEs of an EDS offline cache (`ECacheObjects.ECacheOBJ`, `timezones.zone`).
#[cfg(not(any(target_os = "macos", windows)))]
pub fn eds_cache(path: &Path) -> Result<String> {
    use rusqlite::{Connection, OpenFlags};
    let fail =
        |e: rusqlite::Error| ConfigError::new("fetch_failed", format!("{}: {e}", path.display()));
    // Never synced (or a contacts-backed Birthdays calendar): nothing to read yet, not an error.
    if !path.exists() {
        return Ok(wrap(&[]));
    }
    let db = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(fail)?;
    let mut parts = Vec::new();
    let has = |t: &str| {
        db.query_row(
            "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
            [t],
            |r| r.get::<_, i64>(0),
        )
        .map(|n| n > 0)
        .unwrap_or(false)
    };
    if has("timezones") {
        let mut q = db.prepare("SELECT zone FROM timezones").map_err(fail)?;
        let rows = q.query_map([], |r| r.get::<_, String>(0)).map_err(fail)?;
        parts.extend(rows.flatten());
    }
    let mut q = db
        .prepare("SELECT ECacheOBJ FROM ECacheObjects")
        .map_err(fail)?;
    let rows = q.query_map([], |r| r.get::<_, String>(0)).map_err(fail)?;
    parts.extend(rows.flatten());
    Ok(wrap(&parts))
}

#[cfg(any(target_os = "macos", windows))]
pub fn eds_cache(path: &Path) -> Result<String> {
    Err(ConfigError::new(
        "unsupported",
        format!(
            "{}: Evolution caches are read on Linux only",
            path.display()
        ),
    ))
}

fn text_of(store: &Store) -> Result<String> {
    match store {
        Store::Ics(p) => read(p),
        Store::EdsCache(p) => eds_cache(p),
        Store::IcsDir(d) => {
            let mut files: Vec<PathBuf> = std::fs::read_dir(d)
                .map_err(|e| ConfigError::new("fetch_failed", format!("{}: {e}", d.display())))?
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("ics"))
                .collect();
            files.sort();
            let cals: Vec<String> = files.iter().filter_map(|p| read(p).ok()).collect();
            Ok(crate::ics::fetch::merge_calendars(&cals))
        }
    }
}

impl Provider for Local {
    fn networked(&self) -> bool {
        false
    }

    fn calendars(&self, _: &Engine, _: &Source) -> Result<Vec<Calendar>> {
        Ok(self.scan().into_iter().map(|(c, _)| c).collect())
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
        let all = self.scan();
        let mut out = Vec::new();
        for cal in cals {
            let Some((_, store)) = all.iter().find(|(c, _)| c.id == cal.id) else {
                continue;
            };
            match text_of(store).and_then(|t| ics_text_events(engine, &t, cal, from, to)) {
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

    #[test]
    fn keyfiles_keep_groups_and_drop_localized_keys() {
        let k = keyfile("# c\n[Data Source]\nDisplayName=Work\nDisplayName[de]=Arbeit\nParent=abc\n\n[Calendar]\nBackendName=caldav\n");
        assert_eq!(k["Data Source"]["DisplayName"], "Work");
        assert_eq!(k["Data Source"].len(), 2);
        assert_eq!(k["Calendar"]["BackendName"], "caldav");
    }

    #[test]
    fn bare_components_wrap_into_one_calendar() {
        let w = wrap(&[
            "BEGIN:VEVENT\r\nUID:a\r\nEND:VEVENT".into(),
            "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:b\r\nEND:VEVENT\r\nEND:VCALENDAR".into(),
        ]);
        assert_eq!(w.matches("BEGIN:VCALENDAR").count(), 1);
        assert_eq!(w.matches("BEGIN:VEVENT").count(), 2);
    }
}
