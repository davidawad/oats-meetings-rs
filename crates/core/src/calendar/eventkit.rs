//! The macOS Calendar app's store (EventKit) behind a trait, so the engine and its tests never need
//! a Mac: `system()` is the real store on macOS and `Unsupported` elsewhere; tests inject a fake
//! with `Engine::set_event_store`.
//!
//! Permission: EventKit needs the Calendars TCC grant. Nothing here asks for it except
//! `request_access`, which the engine calls only on an explicit act (enabling an eventkit source,
//! or `calendar-access request=true`); listing and matching read `authorization()` and stay quiet.

use std::time::Duration;

use chrono::{DateTime, Utc};
use serde_json::{json, Value};

use super::rules::Event;
use crate::config::Result;

/// The Calendars grant, as EventKit reports it (`unsupported` off macOS).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    NotDetermined,
    Restricted,
    Denied,
    FullAccess,
    WriteOnly,
    Unsupported,
}

impl Access {
    pub fn as_str(self) -> &'static str {
        match self {
            Access::NotDetermined => "not_determined",
            Access::Restricted => "restricted",
            Access::Denied => "denied",
            Access::FullAccess => "full_access",
            Access::WriteOnly => "write_only",
            Access::Unsupported => "unsupported",
        }
    }

    /// Events can be read.
    pub fn readable(self) -> bool {
        self == Access::FullAccess
    }
}

/// One calendar and the account (EKSource) holding it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Calendar {
    pub id: String,
    pub title: String,
    pub account: String,
    pub account_id: String,
    /// `local`, `exchange`, `caldav`, `mobileme`, `subscribed`, `birthdays` or `unknown`.
    pub account_kind: String,
}

impl Calendar {
    pub fn to_json(&self) -> Value {
        json!({"id": self.id, "title": self.title, "account": self.account,
               "account_id": self.account_id, "account_kind": self.account_kind})
    }
}

pub trait EventStore: Send + Sync {
    /// This build can reach EventKit at all.
    fn supported(&self) -> bool;
    /// The current grant; never prompts.
    fn authorization(&self) -> Access;
    /// Ask the OS for full Calendars access (the one system prompt, shown only while the grant is
    /// undetermined); waits up to `timeout` for the answer.
    fn request_access(&self, timeout: Duration) -> Result<Access>;
    /// Every event calendar with its account. Empty without access.
    fn calendars(&self) -> Result<Vec<Calendar>>;
    /// Occurrences overlapping `[from, to)` in the calendars with these ids. `source` and `kind`
    /// are left for the caller to fill.
    fn events(&self, from: DateTime<Utc>, to: DateTime<Utc>, ids: &[String]) -> Result<Vec<Event>>;
}

/// Off macOS: no EventKit.
pub struct Unsupported;

fn unsupported() -> crate::config::ConfigError {
    crate::config::ConfigError::new(
        "unsupported",
        "eventkit calendars are macOS only (use an ics source here)",
    )
}

impl EventStore for Unsupported {
    fn supported(&self) -> bool {
        false
    }
    fn authorization(&self) -> Access {
        Access::Unsupported
    }
    fn request_access(&self, _: Duration) -> Result<Access> {
        Err(unsupported())
    }
    fn calendars(&self) -> Result<Vec<Calendar>> {
        Err(unsupported())
    }
    fn events(&self, _: DateTime<Utc>, _: DateTime<Utc>, _: &[String]) -> Result<Vec<Event>> {
        Err(unsupported())
    }
}

/// The platform store.
pub fn system() -> Box<dyn EventStore> {
    #[cfg(target_os = "macos")]
    {
        Box::new(mac::Kit)
    }
    #[cfg(not(target_os = "macos"))]
    {
        Box::new(Unsupported)
    }
}

#[cfg(target_os = "macos")]
mod mac {
    use std::sync::mpsc;
    use std::time::Duration;

    use block2::RcBlock;
    use chrono::{DateTime, Utc};
    use objc2::rc::Retained;
    use objc2::runtime::{Bool, NSObjectProtocol};
    use objc2::sel;
    use objc2_event_kit::{
        EKAuthorizationStatus, EKCalendar, EKEntityType, EKEventStatus, EKEventStore,
        EKParticipant, EKParticipantStatus, EKSourceType,
    };
    use objc2_foundation::{NSArray, NSDate, NSError, NSString};

    use super::{Access, Calendar, EventStore};
    use crate::calendar::rules::{Event, Person};
    use crate::config::{ConfigError, Result};

    pub struct Kit;

    fn text(s: &NSString) -> String {
        s.to_string()
    }

    fn date(secs: i64) -> Retained<NSDate> {
        NSDate::dateWithTimeIntervalSince1970(secs as f64)
    }

    fn instant(d: &NSDate) -> DateTime<Utc> {
        let secs = d.timeIntervalSince1970();
        DateTime::<Utc>::from_timestamp(secs.floor() as i64, 0).unwrap_or_default()
    }

    fn status() -> Access {
        let s = unsafe { EKEventStore::authorizationStatusForEntityType(EKEntityType::Event) };
        match s {
            EKAuthorizationStatus::NotDetermined => Access::NotDetermined,
            EKAuthorizationStatus::Restricted => Access::Restricted,
            EKAuthorizationStatus::Denied => Access::Denied,
            EKAuthorizationStatus::FullAccess => Access::FullAccess,
            EKAuthorizationStatus::WriteOnly => Access::WriteOnly,
            _ => Access::Denied,
        }
    }

    fn kind_of(t: EKSourceType) -> &'static str {
        match t {
            EKSourceType::Local => "local",
            EKSourceType::Exchange => "exchange",
            EKSourceType::CalDAV => "caldav",
            EKSourceType::MobileMe => "mobileme",
            EKSourceType::Subscribed => "subscribed",
            EKSourceType::Birthdays => "birthdays",
            _ => "unknown",
        }
    }

    fn calendar_of(c: &EKCalendar) -> Calendar {
        let src = unsafe { c.source() };
        Calendar {
            id: text(&*unsafe { c.calendarIdentifier() }),
            title: text(&*unsafe { c.title() }),
            account: src
                .as_ref()
                .map(|s| text(&*unsafe { s.title() }))
                .unwrap_or_default(),
            account_id: src
                .as_ref()
                .map(|s| text(&*unsafe { s.sourceIdentifier() }))
                .unwrap_or_default(),
            account_kind: src
                .as_ref()
                .map_or("unknown", |s| kind_of(unsafe { s.sourceType() }))
                .to_string(),
        }
    }

    fn person(p: &EKParticipant) -> Person {
        let url = text(&unsafe { p.URL() }.absoluteString().unwrap_or_default());
        let email = url
            .strip_prefix("mailto:")
            .unwrap_or(&url)
            .trim()
            .to_string();
        let status = match unsafe { p.participantStatus() } {
            EKParticipantStatus::Accepted => "ACCEPTED",
            EKParticipantStatus::Declined => "DECLINED",
            EKParticipantStatus::Tentative => "TENTATIVE",
            EKParticipantStatus::Pending => "NEEDS-ACTION",
            EKParticipantStatus::Delegated => "DELEGATED",
            EKParticipantStatus::Completed => "COMPLETED",
            EKParticipantStatus::InProcess => "IN-PROCESS",
            _ => "",
        };
        Person {
            name: unsafe { p.name() }.map(|n| text(&n)).unwrap_or_default(),
            email,
            status: status.to_string(),
            me: unsafe { p.isCurrentUser() },
        }
    }

    fn store() -> Retained<EKEventStore> {
        unsafe { EKEventStore::new() }
    }

    impl EventStore for Kit {
        fn supported(&self) -> bool {
            true
        }

        fn authorization(&self) -> Access {
            status()
        }

        fn request_access(&self, timeout: Duration) -> Result<Access> {
            if status() != Access::NotDetermined {
                return Ok(status());
            }
            let st = store();
            let (tx, rx) = mpsc::channel::<std::result::Result<bool, String>>();
            let done = RcBlock::new(move |granted: Bool, err: *mut NSError| {
                let r = if err.is_null() {
                    Ok(granted.as_bool())
                } else {
                    Err(unsafe { &*err }.localizedDescription().to_string())
                };
                let _ = tx.send(r);
            });
            // macOS 14 split the grant into full and write-only; 13 has the one entity-type ask.
            let full = st.respondsToSelector(sel!(requestFullAccessToEventsWithCompletion:));
            unsafe {
                if full {
                    st.requestFullAccessToEventsWithCompletion(RcBlock::as_ptr(&done));
                } else {
                    #[allow(deprecated)]
                    st.requestAccessToEntityType_completion(
                        EKEntityType::Event,
                        RcBlock::as_ptr(&done),
                    );
                }
            }
            match rx.recv_timeout(timeout) {
                Ok(Ok(_)) => Ok(status()),
                Ok(Err(e)) => Err(ConfigError::new("calendar_access_failed", e)),
                Err(_) => Err(ConfigError::new(
                    "calendar_access_timeout",
                    format!("no answer to the Calendars access request within {timeout:?} (calendar.access_timeout)"),
                )),
            }
        }

        fn calendars(&self) -> Result<Vec<Calendar>> {
            if !status().readable() {
                return Ok(vec![]);
            }
            let st = store();
            let all = unsafe { st.calendarsForEntityType(EKEntityType::Event) };
            Ok(all.iter().map(|c| calendar_of(&c)).collect())
        }

        fn events(
            &self,
            from: DateTime<Utc>,
            to: DateTime<Utc>,
            ids: &[String],
        ) -> Result<Vec<Event>> {
            if !status().readable() || ids.is_empty() {
                return Ok(vec![]);
            }
            let st = store();
            let chosen: Vec<Retained<EKCalendar>> =
                unsafe { st.calendarsForEntityType(EKEntityType::Event) }
                    .iter()
                    .filter(|c| ids.contains(&text(&*unsafe { c.calendarIdentifier() })))
                    .collect();
            if chosen.is_empty() {
                return Ok(vec![]);
            }
            let cals = NSArray::from_retained_slice(&chosen);
            let pred = unsafe {
                st.predicateForEventsWithStartDate_endDate_calendars(
                    &date(from.timestamp()),
                    &date(to.timestamp()),
                    Some(&cals),
                )
            };
            let found = unsafe { st.eventsMatchingPredicate(&pred) };
            let mut out = Vec::new();
            for e in found.iter() {
                let cal = unsafe { e.calendar() }
                    .map(|c| calendar_of(&c))
                    .unwrap_or_default();
                let attendees: Vec<Person> = unsafe { e.attendees() }
                    .map(|a| a.iter().map(|p| person(&p)).collect())
                    .unwrap_or_default();
                let start = instant(&*unsafe { e.startDate() });
                let id = unsafe { e.eventIdentifier() }
                    .map(|s| text(&s))
                    .unwrap_or_else(|| text(&*unsafe { e.calendarItemIdentifier() }));
                out.push(Event {
                    // One id per occurrence: a recurring event shares its identifier.
                    id: format!("{id}@{}", start.timestamp()),
                    title: text(&*unsafe { e.title() }),
                    start,
                    end: instant(&*unsafe { e.endDate() }),
                    all_day: unsafe { e.isAllDay() },
                    calendar: cal.title,
                    calendar_id: cal.id,
                    account: cal.account,
                    location: unsafe { e.location() }
                        .map(|s| text(&s))
                        .unwrap_or_default(),
                    notes: unsafe { e.notes() }.map(|s| text(&s)).unwrap_or_default(),
                    url: unsafe { e.URL() }
                        .and_then(|u| u.absoluteString())
                        .map(|s| text(&s))
                        .unwrap_or_default(),
                    declined: attendees.iter().any(|p| p.me && p.status == "DECLINED"),
                    attendees,
                    cancelled: unsafe { e.status() } == EKEventStatus::Canceled,
                    ..Event::default()
                });
            }
            Ok(out)
        }
    }
}
