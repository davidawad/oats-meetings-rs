//! The os-local provider on Windows: `Windows.ApplicationModel.Appointments`, the store the
//! Calendar app and the accounts in Settings > Accounts (Outlook.com, Microsoft 365, Google,
//! iCloud) sync into. Read only (`AllCalendarsReadOnly`); where the store is not available to this
//! process (no package identity on some builds, or the Calendar privacy switch is off) the
//! request fails and the source reports `calendar_access` with the reason.

use chrono::{DateTime, Utc};
use serde_json::Value;
use windows::core::HSTRING;
use windows::ApplicationModel::Appointments::{
    AppointmentManager, AppointmentParticipantResponse, AppointmentProperties, AppointmentStore,
    AppointmentStoreAccessType, FindAppointmentsOptions,
};
use windows::Foundation::{DateTime as WinTime, TimeSpan};

use super::eventkit::Calendar;
use super::provider::Provider;
use super::rules::{Event, Person};
use super::Source;
use crate::config::{ConfigError, Result};
use crate::control::engine::Engine;

pub struct Appointments;

fn fail(e: windows::core::Error) -> ConfigError {
    ConfigError::new(
        "calendar_access",
        format!("Windows appointments store: {e}"),
    )
}

fn store() -> Result<AppointmentStore> {
    AppointmentManager::RequestStoreAsync(AppointmentStoreAccessType::AllCalendarsReadOnly)
        .and_then(|op| op.join())
        .map_err(fail)
}

/// Windows file time (100 ns ticks since 1601-01-01) to and from an instant.
fn ticks_per_sec() -> i64 {
    10_000_000
}

fn epoch_gap_secs() -> i64 {
    11_644_473_600
}

fn to_win(d: DateTime<Utc>) -> WinTime {
    WinTime {
        UniversalTime: (d.timestamp() + epoch_gap_secs()) * ticks_per_sec()
            + i64::from(d.timestamp_subsec_nanos()) / 100,
    }
}

fn from_win(t: WinTime) -> DateTime<Utc> {
    let secs = t.UniversalTime.div_euclid(ticks_per_sec()) - epoch_gap_secs();
    let nanos = (t.UniversalTime.rem_euclid(ticks_per_sec()) * 100) as u32;
    DateTime::from_timestamp(secs, nanos).unwrap_or_default()
}

fn response(r: AppointmentParticipantResponse) -> String {
    match r {
        AppointmentParticipantResponse::Accepted => "ACCEPTED".into(),
        AppointmentParticipantResponse::Declined => "DECLINED".into(),
        AppointmentParticipantResponse::Tentative => "TENTATIVE".into(),
        AppointmentParticipantResponse::None => "NEEDS-ACTION".into(),
        _ => String::new(),
    }
}

impl Provider for Appointments {
    fn networked(&self) -> bool {
        false
    }

    fn calendars(&self, _: &Engine, _: &Source) -> Result<Vec<Calendar>> {
        let st = store()?;
        let cals = st
            .FindAppointmentCalendarsAsync()
            .and_then(|op| op.join())
            .map_err(fail)?;
        let mut out = Vec::new();
        for c in cals {
            let s =
                |r: windows::core::Result<HSTRING>| r.map(|h| h.to_string()).unwrap_or_default();
            let account = s(c.SourceDisplayName());
            out.push(Calendar {
                id: s(c.LocalId()),
                title: s(c.DisplayName()),
                account_id: {
                    let id = s(c.UserDataAccountId());
                    if id.is_empty() {
                        account.clone()
                    } else {
                        id
                    }
                },
                account,
                account_kind: "windows".into(),
            });
        }
        Ok(out)
    }

    fn events(
        &self,
        _: &Engine,
        _: &Source,
        cals: &[Calendar],
        from: DateTime<Utc>,
        to: DateTime<Utc>,
        _: &mut Vec<Value>,
    ) -> Result<Vec<Event>> {
        let st = store()?;
        let opts = FindAppointmentsOptions::new().map_err(fail)?;
        let ids = opts.CalendarIds().map_err(fail)?;
        for c in cals {
            ids.Append(&HSTRING::from(c.id.as_str())).map_err(fail)?;
        }
        let props = opts.FetchProperties().map_err(fail)?;
        for p in [
            AppointmentProperties::Subject(),
            AppointmentProperties::Location(),
            AppointmentProperties::StartTime(),
            AppointmentProperties::Duration(),
            AppointmentProperties::AllDay(),
            AppointmentProperties::Details(),
            AppointmentProperties::OnlineMeetingLink(),
            AppointmentProperties::Uri(),
            AppointmentProperties::Invitees(),
            AppointmentProperties::IsCanceledMeeting(),
            AppointmentProperties::UserResponse(),
        ] {
            props.Append(&p.map_err(fail)?).map_err(fail)?;
        }
        let range = TimeSpan {
            Duration: (to - from).num_seconds().max(0) * ticks_per_sec(),
        };
        let found = st
            .FindAppointmentsAsyncWithOptions(to_win(from), range, &opts)
            .and_then(|op| op.join())
            .map_err(fail)?;
        let mut out = Vec::new();
        for a in found {
            let s =
                |r: windows::core::Result<HSTRING>| r.map(|h| h.to_string()).unwrap_or_default();
            let start = a.StartTime().map(from_win).unwrap_or(from);
            let len = a.Duration().map(|d| d.Duration).unwrap_or_default();
            let cal_id = s(a.CalendarId());
            let cal = cals
                .iter()
                .find(|c| c.id == cal_id)
                .cloned()
                .unwrap_or_default();
            let attendees: Vec<Person> = a
                .Invitees()
                .map(|v| {
                    v.into_iter()
                        .map(|p| Person {
                            name: s(p.DisplayName()),
                            email: s(p.Address()),
                            status: p.Response().map(response).unwrap_or_default(),
                            me: false,
                        })
                        .collect()
                })
                .unwrap_or_default();
            let link = s(a.OnlineMeetingLink());
            let uri = a
                .Uri()
                .ok()
                .and_then(|u| u.AbsoluteUri().ok())
                .map(|h| h.to_string())
                .unwrap_or_default();
            out.push(Event {
                id: format!("{}@{}", s(a.LocalId()), start.timestamp()),
                uid: s(a.RoamingId()),
                title: s(a.Subject()),
                start,
                end: start + chrono::Duration::seconds(len / ticks_per_sec()),
                all_day: a.AllDay().unwrap_or(false),
                calendar: cal.title,
                calendar_id: cal_id,
                account: cal.account,
                location: s(a.Location()),
                notes: s(a.Details()),
                url: if link.is_empty() { uri } else { link },
                attendees,
                declined: a.UserResponse().ok() == Some(AppointmentParticipantResponse::Declined),
                cancelled: a.IsCanceledMeeting().unwrap_or(false),
                ..Event::default()
            });
        }
        Ok(out)
    }
}
