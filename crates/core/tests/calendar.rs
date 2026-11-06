//! Calendar sources and session titles (dot-4uzgol.88): the event matching rules, the title
//! template, eventkit / ics source selection, and record-start / record-stop titling a session,
//! with EventKit mocked behind `EventStore` (the real store is proven live on a Mac).

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{DateTime, TimeZone, Utc};
use oats_audio::fake::FakeBackend;
use oats_core::action::Action;
use oats_core::calendar::eventkit::{Access, Calendar, EventStore};
use oats_core::calendar::rules::{self, Event, Person, Rules};
use oats_core::calendar::{parse_sources, selected, Source, SourceKind};
use oats_core::control::engine::{Engine, EngineOpts};
use serde_json::{json, Value};

fn t(s: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
}

fn rules() -> Rules {
    let c = oats_core::config::typed::Calendar {
        access_timeout: oats_core::config::typed::Dur(1000),
        ignore_all_day: true,
        ignore_cancelled: true,
        ignore_declined: true,
        match_lead: oats_core::config::typed::Dur(5 * 60_000),
        me: "me@davidaw.ad".into(),
        prefer_attendees: true,
        prefer_video: true,
        retitle_on_save: true,
        sources: "[]".into(),
        title_fallback: String::new(),
        title_from_event: true,
        title_template: "{event.title}".into(),
        video_hosts: "zoom.us,meet.google.com".into(),
    };
    Rules::from_config(&c)
}

fn ev(id: &str, title: &str, start: &str, end: &str) -> Event {
    Event {
        source: "mac".into(),
        kind: "eventkit".into(),
        id: id.into(),
        title: title.into(),
        start: t(start),
        end: t(end),
        calendar: "Work".into(),
        calendar_id: "cal-work".into(),
        account: "me@davidaw.ad".into(),
        ..Event::default()
    }
}

fn person(name: &str, email: &str, status: &str) -> Person {
    Person {
        name: name.into(),
        email: email.into(),
        status: status.into(),
        me: false,
    }
}

fn picked<'a>(evs: &'a [Event], from: &str, to: &str, r: &Rules) -> Option<&'a str> {
    rules::pick(evs, t(from), t(to), r)
        .picked
        .map(|i| evs[i].id.as_str())
}

// ---- matching rules ---------------------------------------------------------------------------

#[test]
fn an_event_matches_from_lead_before_its_start_until_its_end() {
    let r = rules();
    let evs = [ev(
        "a",
        "Sync",
        "2026-11-06T16:00:00Z",
        "2026-11-06T16:30:00Z",
    )];
    let at = |s: &str| picked(&evs, s, s, &r);
    assert_eq!(at("2026-11-06T15:54:59Z"), None, "more than the lead early");
    assert_eq!(
        at("2026-11-06T15:55:00Z"),
        Some("a"),
        "exactly the lead early"
    );
    assert_eq!(at("2026-11-06T16:01:11Z"), Some("a"));
    assert_eq!(at("2026-11-06T16:29:59Z"), Some("a"));
    assert_eq!(at("2026-11-06T16:30:00Z"), None, "the end is exclusive");
    let mut r0 = r.clone();
    r0.lead = chrono::Duration::zero();
    assert_eq!(
        picked(&evs, "2026-11-06T15:59:00Z", "2026-11-06T15:59:00Z", &r0),
        None
    );
}

#[test]
fn all_day_declined_and_cancelled_events_are_ignored_by_their_keys() {
    let mut all_day = ev(
        "d",
        "Holiday",
        "2026-11-06T00:00:00Z",
        "2026-11-07T00:00:00Z",
    );
    all_day.all_day = true;
    let mut declined = ev(
        "x",
        "Declined",
        "2026-11-06T16:00:00Z",
        "2026-11-06T17:00:00Z",
    );
    declined.attendees = vec![person("David", "ME@davidaw.ad", "DECLINED")];
    let mut ek_declined = ev(
        "y",
        "Declined too",
        "2026-11-06T16:00:00Z",
        "2026-11-06T17:00:00Z",
    );
    ek_declined.declined = true;
    let mut cancelled = ev("c", "Off", "2026-11-06T16:00:00Z", "2026-11-06T17:00:00Z");
    cancelled.cancelled = true;
    let evs = [all_day, declined, ek_declined, cancelled];
    let now = "2026-11-06T16:10:00Z";
    let m = rules::pick(&evs, t(now), t(now), &rules());
    assert_eq!(m.picked, None);
    assert_eq!(
        m.candidates,
        vec![
            (0, "all_day"),
            (1, "declined"),
            (2, "declined"),
            (3, "cancelled")
        ]
    );
    let mut r = rules();
    r.ignore_all_day = false;
    r.ignore_declined = false;
    r.ignore_cancelled = false;
    // With nothing ignored, the shortest-overlap tie breaks on the start nearest now, then id.
    assert!(rules::pick(&evs, t(now), t(now), &r).picked.is_some());
    r.ignore_declined = true;
    let m = rules::pick(&evs, t(now), t(now), &r);
    assert_eq!(m.candidates[1], (1, "declined"));
}

#[test]
fn video_links_then_attendees_then_overlap_then_nearest_start_rank_the_pick() {
    let r = rules();
    let focus = ev(
        "focus",
        "Focus",
        "2026-11-06T16:00:00Z",
        "2026-11-06T18:00:00Z",
    );
    let mut people = ev(
        "people",
        "1:1",
        "2026-11-06T16:00:00Z",
        "2026-11-06T16:30:00Z",
    );
    people.attendees = vec![person("Ana", "ana@x.io", "ACCEPTED")];
    let mut zoom = ev(
        "zoom",
        "Zoom",
        "2026-11-06T16:00:00Z",
        "2026-11-06T16:30:00Z",
    );
    zoom.notes = "Join: <https://acme.zoom.us/j/123?pwd=x>.".into();
    let now = "2026-11-06T16:05:00Z";
    let all = [focus.clone(), people.clone(), zoom.clone()];
    assert_eq!(picked(&all, now, now, &r), Some("zoom"));
    let mut no_video = r.clone();
    no_video.prefer_video = false;
    assert_eq!(picked(&all, now, now, &no_video), Some("people"));
    let mut neither = no_video.clone();
    neither.prefer_attendees = false;
    // At a start instant every overlap is zero: the start nearest now, then the id, decide.
    assert_eq!(picked(&all, now, now, &neither), Some("focus"));
    // Over a span the longest overlap wins.
    let late = ev(
        "late",
        "Late",
        "2026-11-06T16:50:00Z",
        "2026-11-06T18:00:00Z",
    );
    let short = ev(
        "short",
        "Short",
        "2026-11-06T16:00:00Z",
        "2026-11-06T16:20:00Z",
    );
    assert_eq!(
        picked(
            &[short, late],
            "2026-11-06T16:00:00Z",
            "2026-11-06T17:30:00Z",
            &neither
        ),
        Some("late")
    );
    // Only you as attendee is not "attendees".
    // (`plain` starts nearer now, so it wins only if `solo` does not count as having attendees.)
    let mut solo = ev(
        "solo",
        "Solo",
        "2026-11-06T16:00:00Z",
        "2026-11-06T16:30:00Z",
    );
    solo.attendees = vec![person("Me", "me@davidaw.ad", "ACCEPTED")];
    let plain = ev(
        "plain",
        "Plain",
        "2026-11-06T16:04:00Z",
        "2026-11-06T16:30:00Z",
    );
    assert_eq!(picked(&[solo, plain], now, now, &no_video), Some("plain"));
}

#[test]
fn video_link_hosts_and_participants() {
    let hosts = vec!["zoom.us".to_string(), "meet.google.com".to_string()];
    let mut e = ev("a", "A", "2026-11-06T16:00:00Z", "2026-11-06T16:30:00Z");
    assert_eq!(rules::video_link(&e, &hosts), None);
    e.location = "Room 4, https://notzoom.us/x".into();
    assert_eq!(
        rules::video_link(&e, &hosts),
        None,
        "a suffix is not a subdomain"
    );
    e.url = "https://meet.google.com/abc-defg-hij".into();
    assert_eq!(
        rules::video_link(&e, &hosts).as_deref(),
        Some("https://meet.google.com/abc-defg-hij")
    );
    e.url.clear();
    e.notes = "dial in (https://us02web.zoom.us/j/9),".into();
    assert_eq!(
        rules::video_link(&e, &hosts).as_deref(),
        Some("https://us02web.zoom.us/j/9")
    );
    let r = rules();
    let mut me = person("David", "me@davidaw.ad", "ACCEPTED");
    e.attendees = vec![
        me.clone(),
        person("Ana", "ana@x.io", "ACCEPTED"),
        person("", "bo@x.io", "NEEDS-ACTION"),
        person("Ana", "ana@x.io", "ACCEPTED"),
    ];
    assert_eq!(rules::participants(&e, &r), ["Ana", "bo@x.io"]);
    me.email = "other@x.io".into();
    me.me = true;
    e.attendees = vec![me];
    assert!(
        rules::participants(&e, &r).is_empty(),
        "EventKit's current user is you"
    );
}

// ---- title template ---------------------------------------------------------------------------

#[test]
fn the_title_template_fills_event_and_clock_variables() {
    let mut e = ev(
        "a",
        "Acme sync",
        "2026-11-06T16:00:00Z",
        "2026-11-06T16:30:00Z",
    );
    e.location = "Room 4".into();
    let at = Utc
        .with_ymd_and_hms(2026, 11, 6, 16, 1, 11)
        .unwrap()
        .with_timezone(&chrono::FixedOffset::west_opt(5 * 3600).unwrap());
    let r = |tpl: &str| rules::render(tpl, Some(&e), at);
    assert_eq!(r("{event.title}"), "Acme sync");
    assert_eq!(
        r("{event.title} ({event.calendar}, {event.account})"),
        "Acme sync (Work, me@davidaw.ad)"
    );
    assert_eq!(
        r("{date} {event.start} {event.title}"),
        "2026-11-06 11:00 Acme sync"
    );
    assert_eq!(
        r("{weekday} {time} @ {event.location}"),
        "Friday 11:01 @ Room 4"
    );
    assert_eq!(r("{{literal}} {event.title}"), "{literal} Acme sync");
    e.location.clear();
    assert_eq!(
        rules::render("{event.title}  {event.location}", Some(&e), at),
        "Acme sync",
        "no gap left"
    );
    assert_eq!(
        rules::render("Meeting {date}", None, at),
        "Meeting 2026-11-06"
    );
    assert_eq!(rules::render("{event.title}", None, at), "");
}

#[test]
fn templates_are_checked_for_known_variables() {
    let ok = |t: &str, ev: bool| rules::check_template("calendar.title_template", t, ev);
    assert!(ok("{event.title}", true).is_ok());
    assert!(ok("Call {date} {time}", false).is_ok());
    assert!(ok("plain", false).is_ok());
    for (bad, ev) in [
        ("{event.title}", false),
        ("{event.nope}", true),
        ("{title", true),
        ("x }", true),
    ] {
        let e = ok(bad, ev).unwrap_err();
        assert_eq!(e.code, "invalid_value", "{bad}");
        assert!(
            e.message.contains("calendar.title_template"),
            "{}",
            e.message
        );
    }
}

// ---- source selection -------------------------------------------------------------------------

fn cal(id: &str, title: &str, account: &str) -> Calendar {
    Calendar {
        id: id.into(),
        title: title.into(),
        account: account.into(),
        account_id: format!("src-{account}"),
        account_kind: "caldav".into(),
    }
}

#[test]
fn sources_parse_with_one_account_or_many_and_check_each_kind() {
    let s = parse_sources(
        r#"[{"name":"mac","kind":"eventkit","account":"me@davidaw.ad"},
            {"name":"ics","kind":"ics","feeds":"work","enabled":false}]"#,
    )
    .unwrap();
    assert_eq!(s[0].kind, SourceKind::Eventkit);
    assert_eq!(s[0].accounts, ["me@davidaw.ad"]);
    assert!(s[0].enabled);
    assert_eq!(s[1].feeds, ["work"]);
    assert!(!s[1].enabled);
    for bad in [
        r#"[{"name":"a","kind":"caldav"}]"#,
        r#"[{"name":"a","kind":"ics"},{"name":"a","kind":"eventkit"}]"#,
        r#"[{"name":"../a","kind":"ics"}]"#,
        r#"[{"name":"a","kind":"ics","calendars":["Work"]}]"#,
        r#"[{"name":"a","kind":"eventkit","feeds":["x"]}]"#,
        r#"[{"name":"a","kind":"eventkit","colour":"red"}]"#,
        "not json",
    ] {
        assert_eq!(
            parse_sources(bad).unwrap_err().code,
            "invalid_value",
            "{bad}"
        );
    }
    assert!(parse_sources("").unwrap().is_empty());
}

#[test]
fn an_eventkit_source_reads_whole_accounts_and_single_calendars() {
    let src = |accounts: &[&str], calendars: &[&str]| Source {
        name: "mac".into(),
        kind: SourceKind::Eventkit,
        enabled: true,
        feeds: vec![],
        accounts: accounts.iter().map(|s| s.to_string()).collect(),
        calendars: calendars.iter().map(|s| s.to_string()).collect(),
    };
    let work = cal("w1", "Work", "me@davidaw.ad");
    let home = cal("h1", "Home", "iCloud");
    let bday = cal("b1", "Birthdays", "Other");
    let all = src(&[], &[]);
    assert!([&work, &home, &bday].iter().all(|c| selected(&all, c)));
    let mine = src(&["ME@davidaw.ad"], &[]);
    assert!(selected(&mine, &work));
    assert!(!selected(&mine, &home));
    let by_id = src(&["src-iCloud"], &[]);
    assert!(selected(&by_id, &home));
    let one = src(&[], &["home"]);
    assert!(selected(&one, &home) && !selected(&one, &work));
    let both = src(&["me@davidaw.ad"], &["b1"]);
    assert!(selected(&both, &work) && selected(&both, &bday) && !selected(&both, &home));
}

// ---- engine, with a fake EventKit -------------------------------------------------------------

#[derive(Default)]
struct FakeKit {
    access: Mutex<Option<Access>>,
    requests: AtomicUsize,
    grant: bool,
    calendars: Vec<Calendar>,
    events: Mutex<Vec<Event>>,
}

impl FakeKit {
    fn new(access: Access, grant: bool) -> Arc<FakeKit> {
        Arc::new(FakeKit {
            access: Mutex::new(Some(access)),
            grant,
            calendars: vec![
                cal("w1", "Work", "me@davidaw.ad"),
                cal("w2", "Team", "me@davidaw.ad"),
                cal("h1", "Home", "iCloud"),
            ],
            ..FakeKit::default()
        })
    }
    fn now(&self) -> Access {
        self.access.lock().unwrap().unwrap_or(Access::NotDetermined)
    }
}

impl EventStore for FakeKit {
    fn supported(&self) -> bool {
        true
    }
    fn authorization(&self) -> Access {
        self.now()
    }
    fn request_access(&self, _: Duration) -> oats_core::config::Result<Access> {
        self.requests.fetch_add(1, Ordering::SeqCst);
        let to = if self.grant {
            Access::FullAccess
        } else {
            Access::Denied
        };
        *self.access.lock().unwrap() = Some(to);
        Ok(to)
    }
    fn calendars(&self) -> oats_core::config::Result<Vec<Calendar>> {
        Ok(if self.now().readable() {
            self.calendars.clone()
        } else {
            vec![]
        })
    }
    fn events(
        &self,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
        ids: &[String],
    ) -> oats_core::config::Result<Vec<Event>> {
        Ok(self
            .events
            .lock()
            .unwrap()
            .iter()
            .filter(|e| ids.contains(&e.calendar_id) && e.start < to && e.end > from)
            .cloned()
            .collect())
    }
}

struct Rig {
    dir: tempfile::TempDir,
    engine: Arc<Engine>,
    kit: Arc<FakeKit>,
}

fn rig(kit: Arc<FakeKit>) -> Rig {
    let dir = tempfile::tempdir().unwrap();
    let engine = Engine::new(EngineOpts {
        user_config: dir.path().join("config.toml"),
        pack: None,
        os: "linux".into(),
        arch: "x86_64".into(),
        machine: "default".into(),
        sessions_dir: dir.path().join("sessions"),
        secrets: None,
    })
    .unwrap();
    engine.set_event_store(kit.clone());
    engine.set_audio_factory(Box::new(|_| Ok(Box::new(FakeBackend::default()))));
    engine
        .set("audio.transcribe", &json!("off"), "test")
        .unwrap();
    engine.set("ui.tz", &json!("UTC"), "test").unwrap();
    Rig { dir, engine, kit }
}

fn run(r: &Rig, a: Value) -> oats_core::config::Result<Value> {
    let a: Action = serde_json::from_value(a).unwrap();
    r.engine.dispatch(&a)
}

fn ok(r: &Rig, a: Value) -> Value {
    run(r, a.clone()).unwrap_or_else(|e| panic!("{a}: {e}"))
}

/// An eventkit event on calendar `cal_id` from now - `ago` minutes for `len` minutes.
fn live(id: &str, title: &str, cal_id: &str, ago: i64, len: i64) -> Event {
    let start = Utc::now() - chrono::Duration::minutes(ago);
    Event {
        id: id.into(),
        title: title.into(),
        start,
        end: start + chrono::Duration::minutes(len),
        calendar: "Work".into(),
        calendar_id: cal_id.into(),
        account: "me@davidaw.ad".into(),
        ..Event::default()
    }
}

#[test]
fn listing_shows_accounts_and_calendars_and_never_asks_for_access() {
    let r = rig(FakeKit::new(Access::NotDetermined, true));
    r.engine
        .set(
            "calendar.sources",
            &json!(r#"[{"name":"mac","kind":"eventkit","account":"me@davidaw.ad"}]"#),
            "test",
        )
        .unwrap();
    let v = ok(&r, json!({"action": "calendar-sources-list"}));
    assert_eq!(v["eventkit"]["authorization"], "not_determined");
    assert_eq!(v["sources"][0]["error"]["code"], "calendar_access");
    assert_eq!(v["sources"][0]["accounts"], json!([]));
    // Matching and recording read the grant but never ask for it either.
    ok(&r, json!({"action": "calendar-match"}));
    ok(&r, json!({"action": "calendar-access"}));
    assert_eq!(
        r.kit.requests.load(Ordering::SeqCst),
        0,
        "no surprise prompt"
    );
    *r.kit.access.lock().unwrap() = Some(Access::FullAccess);
    let v = ok(&r, json!({"action": "calendar-sources-list"}));
    let accounts = v["sources"][0]["accounts"].as_array().unwrap();
    assert_eq!(accounts.len(), 2);
    let me = accounts
        .iter()
        .find(|a| a["title"] == "me@davidaw.ad")
        .unwrap();
    assert_eq!(me["selected"], true);
    assert_eq!(me["calendars"].as_array().unwrap().len(), 2);
    let icloud = accounts.iter().find(|a| a["title"] == "iCloud").unwrap();
    assert_eq!(icloud["selected"], false);
    assert_eq!(icloud["calendars"][0]["selected"], false);
    assert!(v["sources"][0].get("error").is_none());
    assert_eq!(r.kit.requests.load(Ordering::SeqCst), 0);
}

#[test]
fn enabling_an_eventkit_source_asks_for_access_once() {
    let r = rig(FakeKit::new(Access::NotDetermined, true));
    let v = ok(
        &r,
        json!({"action": "calendar-source-add", "name": "mac", "kind": "eventkit",
               "accounts": "me@davidaw.ad", "enabled": false}),
    );
    assert!(v.get("access").is_none(), "a disabled source does not ask");
    assert_eq!(r.kit.requests.load(Ordering::SeqCst), 0);
    let v = ok(
        &r,
        json!({"action": "calendar-source-enable", "name": "mac"}),
    );
    assert_eq!(
        v["access"],
        json!({"authorization": "full_access", "requested": true})
    );
    assert_eq!(r.kit.requests.load(Ordering::SeqCst), 1);
    ok(
        &r,
        json!({"action": "calendar-source-enable", "name": "mac", "enabled": false}),
    );
    let v = ok(
        &r,
        json!({"action": "calendar-source-enable", "name": "mac"}),
    );
    assert_eq!(
        v["access"]["requested"], false,
        "decided grants are not asked again"
    );
    assert_eq!(r.kit.requests.load(Ordering::SeqCst), 1);
    let sources = r.engine.snapshot().config.calendar.sources.clone();
    assert!(
        sources.contains(r#""accounts":["me@davidaw.ad"]"#),
        "{sources}"
    );
    // Errors are structured.
    let dup = json!({"action": "calendar-source-add", "name": "mac", "kind": "eventkit"});
    assert_eq!(run(&r, dup).unwrap_err().code, "source_exists");
    let bad = json!({"action": "calendar-source-add", "name": "x", "kind": "caldav"});
    assert_eq!(run(&r, bad).unwrap_err().code, "invalid_argument");
    let mixed =
        json!({"action": "calendar-source-add", "name": "x", "kind": "ics", "calendars": ["Work"]});
    assert_eq!(run(&r, mixed).unwrap_err().code, "invalid_argument");
    ok(
        &r,
        json!({"action": "calendar-source-remove", "name": "mac"}),
    );
    let gone = json!({"action": "calendar-source-remove", "name": "mac"});
    assert_eq!(run(&r, gone).unwrap_err().code, "source_not_found");
}

#[test]
fn a_denied_request_is_reported_and_the_source_reads_nothing() {
    let r = rig(FakeKit::new(Access::NotDetermined, false));
    let v = ok(
        &r,
        json!({"action": "calendar-source-add", "name": "mac", "kind": "eventkit"}),
    );
    assert_eq!(v["access"]["authorization"], "denied");
    let v = ok(&r, json!({"action": "calendar-events"}));
    assert!(v["errors"]
        .as_array()
        .unwrap()
        .iter()
        .any(|e| e["source"] == "mac" && e["error"]["code"] == "calendar_access"));
}

fn selection(r: &Rig) -> Source {
    let s = r.engine.snapshot().config.calendar.sources.clone();
    parse_sources(&s)
        .unwrap()
        .into_iter()
        .find(|s| s.name == "mac")
        .unwrap()
}

#[test]
fn checkboxes_tick_and_untick_calendars_and_accounts_in_the_shortest_form() {
    let r = rig(FakeKit::new(Access::FullAccess, true));
    ok(
        &r,
        json!({"action": "calendar-source-add", "name": "mac", "kind": "eventkit"}),
    );
    let sel = |a: Value| ok(&r, a);
    // Everything is ticked; untick Home: the whole me@ account remains.
    sel(json!({"action": "calendar-select", "source": "mac", "calendar": "h1", "selected": false}));
    let s = selection(&r);
    assert_eq!(
        (s.accounts.clone(), s.calendars.clone()),
        (vec!["me@davidaw.ad".to_string()], vec![])
    );
    // Untick Team: just Work, by id.
    sel(
        json!({"action": "calendar-select", "source": "mac", "calendar": "Team", "selected": false}),
    );
    let s = selection(&r);
    assert_eq!(
        (s.accounts.clone(), s.calendars.clone()),
        (vec![], vec!["w1".to_string()])
    );
    // Tick the iCloud account.
    sel(json!({"action": "calendar-select", "source": "mac", "account": "iCloud"}));
    let s = selection(&r);
    assert_eq!(s.accounts, ["iCloud"]);
    assert_eq!(s.calendars, ["w1"]);
    // Tick Team: everything again, the empty form.
    let v = sel(json!({"action": "calendar-select", "source": "mac", "calendar": "w2"}));
    assert_eq!(v["selected"], 3);
    let s = selection(&r);
    assert!(s.accounts.is_empty() && s.calendars.is_empty() && s.enabled);
    // Untick every account: the source turns off; ticking one turns it on with just that.
    sel(
        json!({"action": "calendar-select", "source": "mac", "account": "me@davidaw.ad", "selected": false}),
    );
    sel(
        json!({"action": "calendar-select", "source": "mac", "account": "iCloud", "selected": false}),
    );
    assert!(!selection(&r).enabled);
    sel(json!({"action": "calendar-select", "source": "mac", "calendar": "Home"}));
    let s = selection(&r);
    assert!(s.enabled);
    assert_eq!(s.accounts, ["iCloud"]);
    let none = json!({"action": "calendar-select", "source": "mac", "calendar": "Nope"});
    assert_eq!(run(&r, none).unwrap_err().code, "invalid_argument");
    let ics = json!({"action": "calendar-select", "source": "ics", "calendar": "Work"});
    assert_eq!(run(&r, ics).unwrap_err().code, "invalid_argument");
}

const ICS: &str = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//oats//test//EN\r\n\
BEGIN:VEVENT\r\nUID:standup@test\r\nDTSTAMP:20260101T000000Z\r\nDTSTART:20260101T000000Z\r\n\
DTEND:20260101T235959Z\r\nRRULE:FREQ=DAILY\r\nSUMMARY:All-day-ish standup\r\n\
URL:https://meet.google.com/abc-defg-hij\r\n\
ATTENDEE;CN=Ana Lee;PARTSTAT=ACCEPTED:mailto:ana@x.io\r\n\
ATTENDEE;CN=David;PARTSTAT=ACCEPTED:mailto:me@davidaw.ad\r\n\
END:VEVENT\r\nEND:VCALENDAR\r\n";

fn add_ics(r: &Rig) {
    let path = r.dir.path().join("team.ics");
    std::fs::write(&path, ICS).unwrap();
    ok(
        r,
        json!({"action": "ics-add", "name": "team",
               "url": format!("file://{}", path.to_str().unwrap())}),
    );
    ok(r, json!({"action": "ics-refresh", "name": "team"}));
}

#[test]
fn events_merge_ics_and_eventkit_and_an_ics_source_filters_feeds() {
    let r = rig(FakeKit::new(Access::FullAccess, true));
    add_ics(&r);
    r.engine
        .set("calendar.me", &json!("me@davidaw.ad"), "test")
        .unwrap();
    ok(
        &r,
        json!({"action": "calendar-source-add", "name": "mac", "kind": "eventkit", "calendars": ["Work"]}),
    );
    r.kit.events.lock().unwrap().extend([
        live("ek1", "Board", "w1", 10, 60),
        live("ek2", "Not read", "h1", 10, 60),
    ]);
    let v = ok(&r, json!({"action": "calendar-events"}));
    let titles: Vec<&str> = v["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["title"].as_str().unwrap())
        .collect();
    assert!(titles.contains(&"Board"), "{titles:?}");
    assert!(titles.contains(&"All-day-ish standup"), "{titles:?}");
    assert!(!titles.contains(&"Not read"), "{titles:?}");
    let standup = v["events"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["source"] == "ics")
        .unwrap();
    assert_eq!(standup["video_url"], "https://meet.google.com/abc-defg-hij");
    assert_eq!(standup["attendees"][0]["name"], "Ana Lee");
    assert_eq!(standup["feed"], "team");
    // An ics source naming another feed reads none of these.
    r.engine
        .set(
            "calendar.sources",
            &json!(r#"[{"name":"ics","kind":"ics","feeds":["other"]}]"#),
            "test",
        )
        .unwrap();
    let v = ok(&r, json!({"action": "calendar-events"}));
    assert_eq!(v["events"], json!([]));
    // One source on its own.
    r.engine
        .set(
            "calendar.sources",
            &json!(
                r#"[{"name":"ics","kind":"ics"},{"name":"mac","kind":"eventkit","enabled":false}]"#
            ),
            "test",
        )
        .unwrap();
    let v = ok(&r, json!({"action": "calendar-events", "source": "mac"}));
    assert_eq!(
        v["events"].as_array().unwrap().len(),
        2,
        "a named source is read even when off"
    );
}

#[test]
fn a_bad_calendar_value_vetoes_the_reload() {
    let r = rig(FakeKit::new(Access::FullAccess, true));
    for (k, v) in [
        ("calendar.sources", "nope"),
        ("calendar.sources", r#"[{"name":"a","kind":"caldav"}]"#),
        ("calendar.title_template", "{event.nope}"),
        ("calendar.title_fallback", "{event.title}"),
    ] {
        let e = r.engine.set(k, &json!(v), "test").unwrap_err();
        assert_eq!(e.code, "invalid_value", "{k} = {v}");
    }
    assert_eq!(
        r.engine.snapshot().config.calendar.title_template,
        "{event.title}"
    );
}

fn meta(r: &Rig, id: &str) -> Value {
    ok(r, json!({"action": "session-get", "session": id}))["metadata"].clone()
}

fn record(r: &Rig, a: Value) -> Value {
    let v = ok(r, a);
    std::thread::sleep(Duration::from_millis(50));
    v
}

#[test]
fn record_start_titles_the_session_from_the_overlapping_event() {
    let r = rig(FakeKit::new(Access::FullAccess, true));
    ok(
        &r,
        json!({"action": "calendar-source-add", "name": "mac", "kind": "eventkit", "account": "me@davidaw.ad"}),
    );
    let mut sync = live("ek-sync", "Acme sync", "w1", 1, 30);
    sync.notes = "https://acme.zoom.us/j/1".into();
    sync.attendees = vec![
        Person {
            name: "David".into(),
            email: "me@davidaw.ad".into(),
            status: "ACCEPTED".into(),
            me: true,
        },
        person("Ana Lee", "ana@x.io", "ACCEPTED"),
    ];
    let other = live("ek-focus", "Focus block", "w2", 30, 120);
    r.kit.events.lock().unwrap().extend([sync, other]);
    let m = ok(&r, json!({"action": "calendar-match"}));
    assert_eq!(m["title"], "Acme sync");
    assert_eq!(m["candidates"].as_array().unwrap().len(), 2);
    let v = record(&r, json!({"action": "record-start", "session": "s1"}));
    assert_eq!(v["title"], "Acme sync");
    assert_eq!(v["calendar"]["event"]["id"], "ek-sync");
    let m = meta(&r, "s1");
    assert_eq!(m["title"], "Acme sync");
    assert_eq!(m["autoTitle"], "Acme sync");
    assert_eq!(m["calendarEvent"]["id"], "ek-sync");
    assert_eq!(m["calendarEvent"]["account"], "me@davidaw.ad");
    assert_eq!(m["calendarEvent"]["videoUrl"], "https://acme.zoom.us/j/1");
    assert_eq!(m["participants"], json!(["Ana Lee"]));
    assert_eq!(m["attendees"].as_array().unwrap().len(), 2);
    // Renamed by hand while recording: the stop keeps the user's title.
    ok(
        &r,
        json!({"action": "session-rename", "session": "s1", "name": "Mine"}),
    );
    ok(&r, json!({"action": "record-stop"}));
    assert_eq!(meta(&r, "s1")["title"], "Mine");
    // A template, and an explicit title that wins but still records the event.
    r.engine
        .set(
            "calendar.title_template",
            &json!("{event.title} ({event.calendar})"),
            "test",
        )
        .unwrap();
    let v = record(&r, json!({"action": "record-start", "session": "s2"}));
    assert_eq!(v["title"], "Acme sync (Work)");
    ok(&r, json!({"action": "record-stop"}));
    record(
        &r,
        json!({"action": "record-start", "session": "s3", "title": "Given"}),
    );
    ok(&r, json!({"action": "record-stop"}));
    let m = meta(&r, "s3");
    assert_eq!(m["title"], "Given");
    assert_eq!(m["calendarEvent"]["id"], "ek-sync");
    assert!(m.get("autoTitle").is_none());
    // Off: no title, no event.
    r.engine
        .set("calendar.title_from_event", &json!(false), "test")
        .unwrap();
    record(&r, json!({"action": "record-start", "session": "s4"}));
    ok(&r, json!({"action": "record-stop"}));
    let m = meta(&r, "s4");
    assert_eq!(m["title"], "");
    assert!(m.get("calendarEvent").is_none());
}

#[test]
fn no_event_falls_back_and_the_stop_rematches_over_the_whole_recording() {
    let r = rig(FakeKit::new(Access::FullAccess, true));
    ok(
        &r,
        json!({"action": "calendar-source-add", "name": "mac", "kind": "eventkit"}),
    );
    r.engine
        .set("calendar.title_fallback", &json!("Meeting {date}"), "test")
        .unwrap();
    let v = record(&r, json!({"action": "record-start", "session": "f1"}));
    let today = Utc::now().format("%Y-%m-%d").to_string();
    assert_eq!(v["title"], format!("Meeting {today}"));
    assert_eq!(v["calendar"]["fallback"], true);
    // An event that starts later than the lead but inside the recording, say a meeting the user
    // started recording early for.
    r.engine
        .set("calendar.match_lead", &json!("0s"), "test")
        .unwrap();
    let mut late = live("ek-late", "Late standup", "w1", 0, 30);
    late.start = Utc::now() + chrono::Duration::milliseconds(100);
    r.kit.events.lock().unwrap().push(late);
    // session.json stamps whole seconds: run past the next one so endedAt is after the start.
    std::thread::sleep(Duration::from_millis(1400));
    let v = ok(&r, json!({"action": "record-stop"}));
    assert_eq!(v["calendar"]["event"]["id"], "ek-late");
    let m = meta(&r, "f1");
    assert_eq!(m["title"], "Late standup");
    assert_eq!(m["calendarEvent"]["id"], "ek-late");
    // retitle_on_save off: the fallback stays.
    r.engine
        .set("calendar.retitle_on_save", &json!(false), "test")
        .unwrap();
    r.engine
        .set("calendar.match_lead", &json!("5m"), "test")
        .unwrap();
    r.kit.events.lock().unwrap().clear();
    record(&r, json!({"action": "record-start", "session": "f2"}));
    let mut later = live("ek-later", "Later", "w1", 0, 30);
    later.start = Utc::now() + chrono::Duration::minutes(10);
    r.kit.events.lock().unwrap().push(later);
    ok(&r, json!({"action": "record-stop"}));
    assert_eq!(meta(&r, "f2")["title"], format!("Meeting {today}"));
}

#[test]
fn an_ics_event_titles_a_recording_with_its_attendees() {
    let r = rig(Arc::new(FakeKit::default()));
    r.engine
        .set("calendar.me", &json!("me@davidaw.ad"), "test")
        .unwrap();
    add_ics(&r);
    let v = record(&r, json!({"action": "record-start", "session": "i1"}));
    assert_eq!(v["title"], "All-day-ish standup");
    ok(&r, json!({"action": "record-stop"}));
    let m = meta(&r, "i1");
    assert_eq!(m["calendarEvent"]["source"], "ics");
    assert_eq!(m["calendarEvent"]["calendar"], "team");
    assert_eq!(m["participants"], json!(["Ana Lee"]));
    // Declined (you are an attendee in calendar.me): untitled.
    let path = r.dir.path().join("team.ics");
    std::fs::write(
        &path,
        ICS.replace("CN=David;PARTSTAT=ACCEPTED", "CN=David;PARTSTAT=DECLINED"),
    )
    .unwrap();
    ok(&r, json!({"action": "ics-refresh", "name": "team"}));
    let m = ok(&r, json!({"action": "calendar-match"}));
    assert_eq!(m["event"], Value::Null);
    assert_eq!(m["candidates"][0]["verdict"], "declined");
}
