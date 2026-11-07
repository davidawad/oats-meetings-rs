//! Calendar providers (dot-4uzgol.100): one provider trait over os-local, google, microsoft,
//! caldav and ics, proven with fakes; Google Calendar and Microsoft Graph request shaping against
//! recorded reply fixtures (no real account); the OAuth loopback + PKCE flow against a fake token
//! endpoint; CalDAV discovery against a fake server and, when `radicale` is installed, a real
//! local Radicale; and the Linux os-local reader over a fake Evolution / Akonadi home.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{DateTime, Utc};
use oats_audio::fake::FakeBackend;
use oats_core::action::Action;
use oats_core::calendar::eventkit::Calendar;
use oats_core::calendar::provider::Provider;
use oats_core::calendar::rules::Event;
use oats_core::calendar::{parse_sources, Source, SourceKind};
use oats_core::control::engine::{Engine, EngineOpts};
use oats_core::secrets::{MemStore, SecretStore};
use serde_json::{json, Value};

// ---- rig -------------------------------------------------------------------------------------

struct Rig {
    _dir: tempfile::TempDir,
    engine: Arc<Engine>,
    secrets: Arc<MemStore>,
}

fn rig() -> Rig {
    let dir = tempfile::tempdir().unwrap();
    let secrets = Arc::new(MemStore::default());
    let engine = Engine::new(EngineOpts {
        user_config: dir.path().join("config.toml"),
        pack: None,
        os: "linux".into(),
        arch: "x86_64".into(),
        machine: "default".into(),
        sessions_dir: dir.path().join("sessions"),
        secrets: Some(secrets.clone() as Arc<dyn SecretStore>),
    })
    .unwrap();
    engine.set_audio_factory(Box::new(|_| Ok(Box::new(FakeBackend::default()))));
    for (k, v) in [
        ("audio.transcribe", json!("off")),
        ("ui.tz", json!("UTC")),
        ("ics.default_tz", json!("UTC")),
        ("calendar.sources", json!("[]")),
    ] {
        engine.set(k, &v, "test").unwrap();
    }
    Rig {
        _dir: dir,
        engine,
        secrets,
    }
}

fn run(r: &Rig, a: Value) -> oats_core::config::Result<Value> {
    let a: Action = serde_json::from_value(a).unwrap();
    r.engine.dispatch(&a)
}

fn ok(r: &Rig, a: Value) -> Value {
    run(r, a.clone()).unwrap_or_else(|e| panic!("{a}: {e}"))
}

fn t(s: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
}

fn cal(id: &str, title: &str, account: &str, kind: &str) -> Calendar {
    Calendar {
        id: id.into(),
        title: title.into(),
        account: account.into(),
        account_id: format!("acct-{account}"),
        account_kind: kind.into(),
    }
}

fn ev(id: &str, uid: &str, title: &str, cal_id: &str, start: DateTime<Utc>, mins: i64) -> Event {
    Event {
        id: id.into(),
        uid: uid.into(),
        title: title.into(),
        start,
        end: start + chrono::Duration::minutes(mins),
        calendar_id: cal_id.into(),
        ..Event::default()
    }
}

/// A provider with fixed calendars and events that counts its calls.
struct Fake {
    networked: bool,
    cals: Vec<Calendar>,
    events: Mutex<Vec<Event>>,
    calls: Mutex<Vec<String>>,
}

impl Fake {
    fn new(networked: bool, cals: Vec<Calendar>, events: Vec<Event>) -> Arc<Fake> {
        Arc::new(Fake {
            networked,
            cals,
            events: Mutex::new(events),
            calls: Mutex::new(vec![]),
        })
    }
}

impl Provider for Fake {
    fn networked(&self) -> bool {
        self.networked
    }
    fn calendars(&self, _: &Engine, s: &Source) -> oats_core::config::Result<Vec<Calendar>> {
        self.calls
            .lock()
            .unwrap()
            .push(format!("calendars {}", s.name));
        Ok(self.cals.clone())
    }
    fn events(
        &self,
        _: &Engine,
        s: &Source,
        cals: &[Calendar],
        from: DateTime<Utc>,
        to: DateTime<Utc>,
        _: &mut Vec<Value>,
    ) -> oats_core::config::Result<Vec<Event>> {
        self.calls
            .lock()
            .unwrap()
            .push(format!("events {}", s.name));
        Ok(self
            .events
            .lock()
            .unwrap()
            .iter()
            .filter(|e| cals.iter().any(|c| c.id == e.calendar_id))
            .filter(|e| e.start < to && e.end > from)
            .cloned()
            .collect())
    }
}

fn fakes(r: &Rig) -> (Arc<Fake>, Arc<Fake>) {
    let local = Fake::new(
        false,
        vec![
            cal("l-work", "Work", "me@davidaw.ad", "eds-google"),
            cal("l-home", "Personal", "On This Computer", "eds-local"),
        ],
        vec![],
    );
    let google = Fake::new(
        true,
        vec![
            cal("me@davidaw.ad", "me@davidaw.ad", "me@davidaw.ad", "google"),
            cal("team", "Team", "me@davidaw.ad", "google"),
        ],
        vec![],
    );
    r.engine
        .set_calendar_provider(SourceKind::OsLocal, local.clone());
    r.engine
        .set_calendar_provider(SourceKind::Google, google.clone());
    ok(
        r,
        json!({"action": "calendar-source-add", "name": "local", "kind": "os-local"}),
    );
    ok(
        r,
        json!({"action": "calendar-source-add", "name": "g", "kind": "google", "client_id": "cid"}),
    );
    (local, google)
}

// ---- the trait, with fakes -------------------------------------------------------------------

#[test]
fn the_default_source_is_os_local_and_every_kind_parses() {
    let def = oats_core::config::keys::KEYS
        .iter()
        .find(|k| k.path == "calendar.sources")
        .unwrap();
    let oats_core::config::keys::Def::S(text) = def.default else {
        panic!()
    };
    let s = parse_sources(text).unwrap();
    assert_eq!(s[0].kind, SourceKind::OsLocal);
    assert!(s[0].enabled);
    for k in SourceKind::ALL {
        assert_eq!(SourceKind::parse(k.as_str()), Some(k));
    }
}

#[test]
fn source_fields_belong_to_their_kinds_and_credentials_are_secret_refs() {
    let good = r#"[
        {"name":"local","kind":"os-local","accounts":"me@davidaw.ad"},
        {"name":"g","kind":"google","client_id":"x.apps.googleusercontent.com","client_secret":"secret:google/g","calendars":["Team"]},
        {"name":"m","kind":"microsoft","client_id":"0000-1111","tenant":"organizations"},
        {"name":"fm","kind":"caldav","url":"https://caldav.fastmail.com/","username":"me@davidaw.ad","password":"secret:caldav/fm"},
        {"name":"feeds","kind":"ics","feeds":["work"]}
    ]"#;
    assert_eq!(parse_sources(good).unwrap().len(), 5);
    for (bad, why) in [
        (r#"[{"name":"c","kind":"caldav"}]"#, "needs url"),
        (
            r#"[{"name":"c","kind":"caldav","url":"https://x/","password":"hunter2"}]"#,
            "secret reference",
        ),
        (
            r#"[{"name":"g","kind":"google","client_secret":"plain"}]"#,
            "secret reference",
        ),
        (
            r#"[{"name":"g","kind":"google","feeds":["a"]}]"#,
            "feeds is for ics",
        ),
        (
            r#"[{"name":"g","kind":"google","tenant":"x"}]"#,
            "tenant is for microsoft",
        ),
        (
            r#"[{"name":"l","kind":"os-local","url":"https://x/"}]"#,
            "url is for caldav",
        ),
        (
            r#"[{"name":"i","kind":"ics","calendars":["a"]}]"#,
            "calendars is for",
        ),
        (r#"[{"name":"x","kind":"exchange"}]"#, "not a JSON array"),
    ] {
        let e = parse_sources(bad).unwrap_err();
        assert_eq!(e.code, "invalid_value", "{bad}");
        assert!(e.message.contains(why), "{bad}: {}", e.message);
    }
}

#[test]
fn every_provider_lists_accounts_and_calendars_and_the_checkboxes_work_on_any() {
    let r = rig();
    r.engine.set("network.allow", &json!(true), "test").unwrap();
    let (_local, google) = fakes(&r);
    let l = ok(&r, json!({"action": "calendar-sources-list"}));
    assert!(l["kinds"]
        .as_array()
        .unwrap()
        .iter()
        .any(|k| k == "microsoft"));
    let local = &l["sources"][0];
    assert_eq!(local["kind"], "os-local");
    assert_eq!(local["accounts"].as_array().unwrap().len(), 2);
    let g = &l["sources"][1];
    assert_eq!(g["kind"], "google");
    assert_eq!(g["connected"], false);
    assert_eq!(g["accounts"][0]["title"], "me@davidaw.ad");
    assert_eq!(g["accounts"][0]["calendars"][1]["selected"], true);
    // Untick Team on the google source, as a Settings checkbox does.
    let v = ok(
        &r,
        json!({"action": "calendar-select", "source": "g", "calendar": "Team", "selected": false}),
    );
    assert_eq!(v["selected"], 1);
    let srcs = parse_sources(&r.engine.snapshot().config.calendar.sources).unwrap();
    assert_eq!(srcs[1].calendars, vec!["me@davidaw.ad".to_string()]);
    assert!(google
        .calls
        .lock()
        .unwrap()
        .iter()
        .all(|c| c.starts_with("calendars")));
    // ics sources still take feeds.
    ok(
        &r,
        json!({"action": "calendar-source-add", "name": "feeds", "kind": "ics"}),
    );
    let e = run(
        &r,
        json!({"action": "calendar-select", "source": "feeds", "calendar": "x"}),
    )
    .unwrap_err();
    assert_eq!(e.code, "invalid_argument");
}

#[test]
fn network_providers_obey_network_allow_and_os_local_needs_none() {
    let r = rig();
    let (local, google) = fakes(&r);
    let now = t("2026-11-09T15:00:00Z");
    local
        .events
        .lock()
        .unwrap()
        .push(ev("a", "uid-a", "Local sync", "l-work", now, 30));
    google
        .events
        .lock()
        .unwrap()
        .push(ev("b", "uid-b", "Google only", "team", now, 30));
    let v = ok(
        &r,
        json!({"action": "calendar-events", "from": "2026-11-09T14:00:00Z", "to": "2026-11-09T18:00:00Z"}),
    );
    let titles: Vec<&str> = v["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["title"].as_str().unwrap())
        .collect();
    assert_eq!(titles, vec!["Local sync"]);
    assert_eq!(v["errors"][0]["source"], "g");
    assert_eq!(v["errors"][0]["error"]["code"], "network_denied");
    assert!(
        google.calls.lock().unwrap().is_empty(),
        "denied before any call"
    );
    let l = ok(&r, json!({"action": "calendar-sources-list"}));
    assert_eq!(l["sources"][1]["error"]["code"], "network_denied");
    r.engine.set("network.allow", &json!(true), "test").unwrap();
    let v = ok(
        &r,
        json!({"action": "calendar-events", "from": "2026-11-09T14:00:00Z", "to": "2026-11-09T18:00:00Z"}),
    );
    assert_eq!(v["events"].as_array().unwrap().len(), 2);
    assert_eq!(v["events"][0]["kind"], "google");
    assert_eq!(v["events"][1]["kind"], "os-local");
}

#[test]
fn the_same_meeting_through_two_providers_is_kept_once_by_ical_uid() {
    let r = rig();
    r.engine.set("network.allow", &json!(true), "test").unwrap();
    let (local, google) = fakes(&r);
    let at = t("2026-11-09T15:00:00Z");
    let next_week = t("2026-11-16T15:00:00Z");
    local.events.lock().unwrap().extend([
        ev(
            "ek-1",
            "Sync-123@google.com",
            "Weekly sync",
            "l-work",
            at,
            30,
        ),
        ev(
            "ek-2",
            "sync-123@google.com",
            "Weekly sync",
            "l-work",
            next_week,
            30,
        ),
    ]);
    google.events.lock().unwrap().extend([
        // The same occurrence (the UID compared without case) and a different one.
        ev(
            "g-1",
            "sync-123@google.com",
            "Weekly sync",
            "me@davidaw.ad",
            at,
            30,
        ),
        ev("g-2", "other@google.com", "1:1", "me@davidaw.ad", at, 30),
        ev("g-3", "", "No uid", "me@davidaw.ad", at, 30),
    ]);
    let v = ok(
        &r,
        json!({"action": "calendar-events", "from": "2026-11-09T00:00:00Z", "to": "2026-11-20T00:00:00Z"}),
    );
    let rows: Vec<(String, String)> = v["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| {
            (
                e["id"].as_str().unwrap().to_string(),
                e["source"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    assert_eq!(
        rows,
        vec![
            ("g-2".into(), "g".into()),
            ("g-3".into(), "g".into()),
            ("ek-1".into(), "local".into()),
            ("ek-2".into(), "local".into()),
        ],
        "the earlier source (local) keeps the shared occurrence; other occurrences stay"
    );
    let mut evs = vec![ev("x", "U", "a", "c", at, 1), ev("y", "u", "b", "c", at, 1)];
    assert_eq!(oats_core::calendar::dedupe(&mut evs), 1);
    assert_eq!(evs[0].id, "x");
}

#[test]
fn a_recording_gets_its_event_title_from_an_os_local_account() {
    let r = rig();
    let (local, _) = fakes(&r);
    let start = Utc::now() - chrono::Duration::minutes(2);
    let mut e = ev(
        "eds-1",
        "standup@x",
        "Standup with Ana",
        "l-work",
        start,
        30,
    );
    e.account = "me@davidaw.ad".into();
    local.events.lock().unwrap().push(e);
    let v = ok(&r, json!({"action": "record-start", "session": "s1"}));
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(v["title"], "Standup with Ana");
    ok(&r, json!({"action": "record-stop"}));
    let m = ok(&r, json!({"action": "session-get", "session": "s1"}))["metadata"].clone();
    assert_eq!(m["title"], "Standup with Ana");
    assert_eq!(m["calendarEvent"]["source"], "local");
    assert_eq!(m["calendarEvent"]["kind"], "os-local");
}

// ---- a fake HTTP server that records what it was asked ---------------------------------------

#[derive(Clone, Debug)]
struct Seen {
    method: String,
    url: String,
    headers: Vec<(String, String)>,
    body: String,
}

impl Seen {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
    fn path(&self) -> &str {
        self.url.split('?').next().unwrap()
    }
    fn query(&self, k: &str) -> Option<String> {
        let q = self.url.split_once('?')?.1;
        q.split('&').find_map(|kv| {
            let (a, b) = kv.split_once('=')?;
            (decode(a) == k).then(|| decode(b))
        })
    }
}

fn decode(s: &str) -> String {
    let b = s.replace('+', " ");
    let by = b.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < by.len() {
        if by[i] == b'%' && i + 2 < by.len() {
            out.push(u8::from_str_radix(&b[i + 1..i + 3], 16).unwrap());
            i += 3;
        } else {
            out.push(by[i]);
            i += 1;
        }
    }
    String::from_utf8(out).unwrap()
}

type Handler = dyn Fn(&Seen, &str) -> (u16, Vec<(&'static str, String)>, String) + Send + Sync;

struct Server {
    base: String,
    seen: Arc<Mutex<Vec<Seen>>>,
}

fn serve(handler: Box<Handler>) -> Server {
    let server = Arc::new(tiny_http::Server::http("127.0.0.1:0").unwrap());
    let base = format!("http://{}", server.server_addr().to_ip().unwrap());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let (s2, b2, srv) = (seen.clone(), base.clone(), server.clone());
    std::thread::spawn(move || {
        for mut rq in srv.incoming_requests() {
            let mut body = String::new();
            let _ = rq.as_reader().read_to_string(&mut body);
            let req = Seen {
                method: rq.method().to_string(),
                url: rq.url().to_string(),
                headers: rq
                    .headers()
                    .iter()
                    .map(|h| (h.field.to_string(), h.value.to_string()))
                    .collect(),
                body,
            };
            s2.lock().unwrap().push(req.clone());
            let (status, headers, text) = handler(&req, &b2);
            let mut resp = tiny_http::Response::from_string(text).with_status_code(status);
            for (k, v) in headers {
                resp = resp.with_header(tiny_http::Header::from_bytes(k, v.as_bytes()).unwrap());
            }
            let _ = rq.respond(resp);
        }
    });
    Server { base, seen }
}

fn fixture(name: &str) -> String {
    std::fs::read_to_string(format!(
        "{}/tests/fixtures/calendar/{name}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap()
}

fn json_reply(text: String) -> (u16, Vec<(&'static str, String)>, String) {
    (200, vec![("Content-Type", "application/json".into())], text)
}

/// Tokens as `calendar-connect` leaves them, valid for an hour.
fn signed_in(r: &Rig, source: &str, access: &str, account: &str) {
    r.secrets
        .set(
            &format!("calendar/{source}"),
            &json!({"access_token": access, "refresh_token": "rt-1",
                    "expires_at": Utc::now().timestamp() + 3600, "account": account})
            .to_string(),
        )
        .unwrap();
}

// ---- Google Calendar API, against recorded replies -------------------------------------------

#[test]
fn google_requests_are_shaped_as_the_api_wants_and_replies_map_to_events() {
    let srv = serve(Box::new(|rq, _| match rq.path() {
        "/calendar/v3/users/me/calendarList" => json_reply(fixture("google_calendar_list.json")),
        "/calendar/v3/calendars/me%40davidaw.ad/events" => {
            if rq.query("pageToken").as_deref() == Some("CigKGjBzNXNkNnVvN2k3") {
                json_reply(fixture("google_events_page2.json"))
            } else {
                json_reply(fixture("google_events_page1.json"))
            }
        }
        _ => (404, vec![], "{}".into()),
    }));
    let r = rig();
    for (k, v) in [
        ("network.allow", json!(true)),
        (
            "calendar.google_api",
            json!(format!("{}/calendar/v3", srv.base)),
        ),
        ("calendar.page_size", json!(50)),
    ] {
        r.engine.set(k, &v, "test").unwrap();
    }
    ok(
        &r,
        json!({"action": "calendar-source-add", "name": "g", "kind": "google",
               "client_id": "cid.apps.googleusercontent.com", "calendars": ["me@davidaw.ad"]}),
    );
    // Not signed in yet: the source says so and how to fix it.
    let l = ok(&r, json!({"action": "calendar-sources-list"}));
    assert_eq!(l["sources"][0]["error"]["code"], "calendar_not_connected");
    signed_in(&r, "g", "ya29.at-1", "me@davidaw.ad");
    let l = ok(&r, json!({"action": "calendar-sources-list"}));
    let g = &l["sources"][0];
    assert_eq!(g["connected"], true);
    assert_eq!(g["account"], "me@davidaw.ad");
    let cals = g["accounts"][0]["calendars"].as_array().unwrap();
    let titles: Vec<&str> = cals.iter().map(|c| c["title"].as_str().unwrap()).collect();
    assert_eq!(
        titles,
        vec!["me@davidaw.ad", "Holidays in Germany", "Team (shared)"]
    );
    assert_eq!(cals[2]["selected"], false);
    let v = ok(
        &r,
        json!({"action": "calendar-events", "from": "2026-11-09T00:00:00Z", "to": "2026-11-12T00:00:00Z"}),
    );
    // Request shaping.
    let seen = srv.seen.lock().unwrap().clone();
    let list = seen
        .iter()
        .find(|s| s.path().ends_with("/calendarList"))
        .unwrap();
    assert_eq!(list.method, "GET");
    assert_eq!(list.header("Authorization"), Some("Bearer ya29.at-1"));
    assert_eq!(list.query("minAccessRole").as_deref(), Some("reader"));
    let evq: Vec<&Seen> = seen
        .iter()
        .filter(|s| s.path().ends_with("/events"))
        .collect();
    assert_eq!(evq.len(), 2, "two pages, only the selected calendar");
    for q in &evq {
        assert_eq!(q.header("Authorization"), Some("Bearer ya29.at-1"));
        assert_eq!(q.query("singleEvents").as_deref(), Some("true"));
        assert_eq!(q.query("orderBy").as_deref(), Some("startTime"));
        assert_eq!(q.query("timeMin").as_deref(), Some("2026-11-09T00:00:00Z"));
        assert_eq!(q.query("timeMax").as_deref(), Some("2026-11-12T00:00:00Z"));
        assert_eq!(q.query("maxResults").as_deref(), Some("50"));
    }
    assert_eq!(evq[0].query("pageToken"), None);
    assert_eq!(
        evq[1].query("pageToken").as_deref(),
        Some("CigKGjBzNXNkNnVvN2k3")
    );
    // Mapping.
    let evs = v["events"].as_array().unwrap();
    assert_eq!(evs.len(), 4, "{v}");
    let sync = &evs[0];
    assert_eq!(sync["title"], "Weekly sync");
    assert_eq!(sync["kind"], "google");
    assert_eq!(sync["uid"], "0s5sd6uo7i7mbq2j7rdq6h3tq8@google.com");
    assert_eq!(sync["start"], "2026-11-09T15:00:00Z");
    assert_eq!(sync["end"], "2026-11-09T15:30:00Z");
    assert_eq!(sync["video_url"], "https://meet.google.com/abc-defg-hij");
    assert_eq!(sync["account"], "me@davidaw.ad");
    assert_eq!(
        sync["attendees"].as_array().unwrap().len(),
        2,
        "rooms dropped"
    );
    assert_eq!(sync["attendees"][0]["me"], true);
    assert_eq!(sync["attendees"][1]["status"], "NEEDS-ACTION");
    let offsite = evs.iter().find(|e| e["title"] == "Offsite").unwrap();
    assert_eq!(offsite["all_day"], true);
    assert_eq!(offsite["start"], "2026-11-10T00:00:00Z");
    let pitch = evs.iter().find(|e| e["title"] == "Vendor pitch").unwrap();
    assert_eq!(pitch["attendees"][1]["status"], "DECLINED");
    // The matching rules apply as for any source: the declined pitch never titles a recording.
    let m = ok(
        &r,
        json!({"action": "calendar-match", "at": "2026-11-10T09:05:00Z"}),
    );
    assert!(m["event"].is_null(), "{m}");
    let verdict = |m: &Value, title: &str| {
        m["candidates"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["title"] == title)
            .map(|c| c["verdict"].clone())
    };
    assert_eq!(verdict(&m, "Vendor pitch"), Some(json!("declined")));
    assert_eq!(verdict(&m, "Offsite"), Some(json!("all_day")));
    let m = ok(
        &r,
        json!({"action": "calendar-match", "at": "2026-11-09T15:01:00Z"}),
    );
    assert_eq!(m["title"], "Weekly sync");
}

// ---- Microsoft Graph, against recorded replies -----------------------------------------------

#[test]
fn graph_requests_are_shaped_as_the_api_wants_and_replies_map_to_events() {
    let srv = serve(Box::new(|rq, base| match rq.path() {
        "/v1.0/me/calendars" => json_reply(fixture("graph_calendars.json")),
        "/v1.0/me/calendars/AAMkAGI2TGuLAAA%3D/calendarView" => {
            if rq.query("$skip").is_some() {
                json_reply(fixture("graph_view_page2.json"))
            } else {
                json_reply(
                    fixture("graph_view_page1.json").replace("{base}", &format!("{base}/v1.0")),
                )
            }
        }
        _ => (404, vec![], "{}".into()),
    }));
    let r = rig();
    for (k, v) in [
        ("network.allow", json!(true)),
        (
            "calendar.microsoft_api",
            json!(format!("{}/v1.0", srv.base)),
        ),
        ("calendar.page_size", json!(2)),
    ] {
        r.engine.set(k, &v, "test").unwrap();
    }
    ok(
        &r,
        json!({"action": "calendar-source-add", "name": "m365", "kind": "microsoft",
               "client_id": "00000000-aaaa", "calendars": "Calendar"}),
    );
    signed_in(&r, "m365", "eyJ0.at-1", "me@davidaw.ad");
    let v = ok(
        &r,
        json!({"action": "calendar-events", "from": "2026-11-09T00:00:00Z", "to": "2026-11-12T00:00:00Z"}),
    );
    let seen = srv.seen.lock().unwrap().clone();
    let cals = seen
        .iter()
        .find(|s| s.path() == "/v1.0/me/calendars")
        .unwrap();
    assert_eq!(cals.header("Authorization"), Some("Bearer eyJ0.at-1"));
    assert_eq!(cals.query("$select").as_deref(), Some("id,name,owner"));
    let views: Vec<&Seen> = seen
        .iter()
        .filter(|s| s.path().ends_with("/calendarView"))
        .collect();
    assert_eq!(views.len(), 2, "the nextLink is followed");
    assert_eq!(
        views[0].query("startDateTime").as_deref(),
        Some("2026-11-09T00:00:00Z")
    );
    assert_eq!(
        views[0].query("endDateTime").as_deref(),
        Some("2026-11-12T00:00:00Z")
    );
    assert_eq!(views[0].query("$top").as_deref(), Some("2"));
    assert!(views[0].query("$select").unwrap().contains("iCalUId"));
    for q in &views {
        assert_eq!(q.header("Prefer"), Some("outlook.timezone=\"UTC\""));
        assert_eq!(q.header("Authorization"), Some("Bearer eyJ0.at-1"));
    }
    assert_eq!(views[1].query("$skip").as_deref(), Some("2"));
    let evs = v["events"].as_array().unwrap();
    assert_eq!(evs.len(), 3, "{v}");
    let q = &evs[0];
    assert_eq!(q["title"], "Quarterly planning");
    assert_eq!(q["kind"], "microsoft");
    assert_eq!(q["start"], "2026-11-09T14:00:00Z");
    assert_eq!(q["calendar"], "Calendar");
    assert_eq!(q["account"], "me@davidaw.ad");
    assert!(q["video_url"]
        .as_str()
        .unwrap()
        .starts_with("https://teams.microsoft.com/"));
    assert_eq!(q["attendees"].as_array().unwrap().len(), 1);
    assert_eq!(q["attendees"][0]["status"], "ACCEPTED");
    let m = ok(
        &r,
        json!({"action": "calendar-match", "at": "2026-11-10T09:01:00Z"}),
    );
    assert!(m["event"].is_null());
    assert_eq!(m["candidates"][0]["verdict"], "declined");
    let hol = evs
        .iter()
        .find(|e| e["title"] == "Company holiday")
        .unwrap();
    assert_eq!(hol["all_day"], true);
}

// ---- OAuth: loopback redirect + PKCE, refresh ------------------------------------------------

fn http_get(url: &str) -> (u16, String) {
    let mut resp = ureq::get(url).call().unwrap();
    let status = resp.status().as_u16();
    (status, resp.body_mut().read_to_string().unwrap_or_default())
}

fn form(body: &str) -> Vec<(String, String)> {
    body.split('&')
        .filter_map(|kv| kv.split_once('='))
        .map(|(k, v)| {
            let s = Seen {
                method: String::new(),
                url: format!("/?x={v}"),
                headers: vec![],
                body: String::new(),
            };
            (k.to_string(), s.query("x").unwrap())
        })
        .collect()
}

fn wait_event(rx: &std::sync::mpsc::Receiver<String>) -> Value {
    let line = rx
        .recv_timeout(Duration::from_secs(10))
        .expect("a bus event");
    serde_json::from_str(&line).unwrap()
}

#[test]
fn google_connect_runs_the_loopback_pkce_flow_and_refreshes_tokens() {
    let refreshes = Arc::new(Mutex::new(0));
    let rf = refreshes.clone();
    let srv = serve(Box::new(move |rq, _| match rq.path() {
        "/token" => {
            let f = form(&rq.body);
            let get = |k: &str| f.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());
            assert_eq!(get("client_id").as_deref(), Some("cid.apps"));
            assert_eq!(get("client_secret").as_deref(), Some("GOCSPX-test"));
            match get("grant_type").as_deref() {
                Some("authorization_code") => {
                    assert_eq!(get("code").as_deref(), Some("4/0AbC-code"));
                    assert!(get("redirect_uri")
                        .unwrap()
                        .starts_with("http://127.0.0.1:"));
                    // The verifier's S256 is checked by the test below against the URL.
                    json_reply(
                        json!({"access_token": "at-1", "expires_in": 3599, "refresh_token": "rt-1",
                               "scope": "https://www.googleapis.com/auth/calendar.readonly",
                               "token_type": "Bearer", "verifier": get("code_verifier")})
                        .to_string(),
                    )
                }
                Some("refresh_token") => {
                    assert_eq!(get("refresh_token").as_deref(), Some("rt-1"));
                    *rf.lock().unwrap() += 1;
                    json_reply(json!({"access_token": "at-2", "expires_in": 3599}).to_string())
                }
                g => panic!("grant {g:?}"),
            }
        }
        "/api/users/me/calendarList/primary" => {
            json_reply(json!({"id": "me@davidaw.ad", "primary": true}).to_string())
        }
        "/api/users/me/calendarList" => json_reply(fixture("google_calendar_list.json")),
        _ => (404, vec![], "{}".into()),
    }));
    let r = rig();
    for (k, v) in [
        ("network.allow", json!(true)),
        (
            "calendar.google_auth_url",
            json!(format!("{}/auth", srv.base)),
        ),
        (
            "calendar.google_token_url",
            json!(format!("{}/token", srv.base)),
        ),
        ("calendar.google_api", json!(format!("{}/api", srv.base))),
        ("calendar.google_client_id", json!("cid.apps")),
    ] {
        r.engine.set(k, &v, "test").unwrap();
    }
    r.secrets.set("google/g", "GOCSPX-test").unwrap();
    let added = ok(
        &r,
        json!({"action": "calendar-source-add", "name": "g", "kind": "google", "client_secret": "secret:google/g"}),
    );
    assert_eq!(added["next"], "calendar-connect source=g");
    // Denied without the network.
    r.engine
        .set("network.allow", &json!(false), "test")
        .unwrap();
    let e = run(&r, json!({"action": "calendar-connect", "source": "g"})).unwrap_err();
    assert_eq!(e.code, "network_denied");
    r.engine.set("network.allow", &json!(true), "test").unwrap();
    let rx = r.engine.bus.subscribe(vec![
        "calendar-connected".into(),
        "calendar-connect-failed".into(),
    ]);
    let v = ok(&r, json!({"action": "calendar-connect", "source": "g"}));
    let url = v["url"].as_str().unwrap();
    assert!(url.starts_with(&format!("{}/auth?", srv.base)), "{url}");
    let auth = Seen {
        method: String::new(),
        url: url.to_string(),
        headers: vec![],
        body: String::new(),
    };
    assert_eq!(auth.query("response_type").as_deref(), Some("code"));
    assert_eq!(auth.query("client_id").as_deref(), Some("cid.apps"));
    assert_eq!(auth.query("code_challenge_method").as_deref(), Some("S256"));
    assert_eq!(auth.query("access_type").as_deref(), Some("offline"));
    assert_eq!(
        auth.query("scope").as_deref(),
        Some("https://www.googleapis.com/auth/calendar.readonly")
    );
    let redirect = auth.query("redirect_uri").unwrap();
    assert_eq!(redirect, v["redirect_uri"]);
    let state = auth.query("state").unwrap();
    let challenge = auth.query("code_challenge").unwrap();
    // A favicon request first, then the browser's redirect.
    let (st, _) = {
        let resp = ureq::get(&format!("{redirect}favicon.ico"))
            .config()
            .http_status_as_error(false)
            .build()
            .call()
            .unwrap();
        (resp.status().as_u16(), ())
    };
    assert_eq!(st, 404);
    let (st, page) = http_get(&format!(
        "{redirect}?state={state}&code=4%2F0AbC-code&scope=x"
    ));
    assert_eq!(st, 200);
    assert!(page.contains("Signed in"));
    let ev = wait_event(&rx);
    assert_eq!(ev["event"], "calendar-connected", "{ev}");
    assert_eq!(ev["data"]["account"], "me@davidaw.ad");
    // PKCE: the verifier sent to the token endpoint hashes to the URL's challenge.
    let seen = srv.seen.lock().unwrap().clone();
    let tok = seen.iter().find(|s| s.path() == "/token").unwrap();
    let f = form(&tok.body);
    let verifier = &f.iter().find(|(k, _)| k == "code_verifier").unwrap().1;
    assert_eq!(oats_core::calendar::oauth::challenge(verifier), challenge);
    // Stored in the app's store (here the test's), never returned.
    let stored: Value =
        serde_json::from_str(&r.secrets.get("calendar/g").unwrap().unwrap()).unwrap();
    assert_eq!(stored["refresh_token"], "rt-1");
    assert_eq!(stored["account"], "me@davidaw.ad");
    assert!(!v.to_string().contains("rt-1"));
    let l = ok(&r, json!({"action": "calendar-sources-list"}));
    assert_eq!(l["sources"][0]["connected"], true);
    assert!(!l.to_string().contains("at-1"));
    // An expired access token is refreshed once and stored.
    let mut t = stored.clone();
    t["expires_at"] = json!(Utc::now().timestamp() - 10);
    r.secrets.set("calendar/g", &t.to_string()).unwrap();
    ok(&r, json!({"action": "calendar-sources-list"}));
    ok(&r, json!({"action": "calendar-sources-list"}));
    assert_eq!(*refreshes.lock().unwrap(), 1);
    let stored: Value =
        serde_json::from_str(&r.secrets.get("calendar/g").unwrap().unwrap()).unwrap();
    assert_eq!(stored["access_token"], "at-2");
    assert_eq!(
        stored["refresh_token"], "rt-1",
        "kept when the reply has none"
    );
    let last = srv.seen.lock().unwrap().last().unwrap().clone();
    assert_eq!(last.header("Authorization"), Some("Bearer at-2"));
    // A forged redirect fails the flow.
    let v = ok(&r, json!({"action": "calendar-connect", "source": "g"}));
    let redirect = v["redirect_uri"].as_str().unwrap().to_string();
    let resp = ureq::get(&format!("{redirect}?state=forged&code=x"))
        .config()
        .http_status_as_error(false)
        .build()
        .call()
        .unwrap();
    assert_eq!(resp.status().as_u16(), 400);
    let ev = wait_event(&rx);
    assert_eq!(ev["event"], "calendar-connect-failed");
    assert_eq!(ev["data"]["error"]["code"], "calendar_auth");
    // Sign out.
    let v = ok(&r, json!({"action": "calendar-disconnect", "source": "g"}));
    assert_eq!(v["forgotten"], true);
    assert!(r.secrets.get("calendar/g").unwrap().is_none());
}

#[test]
fn microsoft_signs_in_through_its_tenant_and_needs_a_client_id() {
    let r = rig();
    r.engine.set("network.allow", &json!(true), "test").unwrap();
    ok(
        &r,
        json!({"action": "calendar-source-add", "name": "m", "kind": "microsoft"}),
    );
    let e = run(&r, json!({"action": "calendar-connect", "source": "m"})).unwrap_err();
    assert_eq!(e.code, "invalid_argument");
    assert!(e.message.contains("calendar.microsoft_client_id"));
    ok(
        &r,
        json!({"action": "calendar-source-add", "name": "work", "kind": "microsoft",
               "client_id": "abc", "tenant": "contoso.onmicrosoft.com"}),
    );
    let v = ok(&r, json!({"action": "calendar-connect", "source": "work"}));
    let url = v["url"].as_str().unwrap();
    assert!(
        url.starts_with(
            "https://login.microsoftonline.com/contoso.onmicrosoft.com/oauth2/v2.0/authorize?"
        ),
        "{url}"
    );
    assert!(url.contains("scope=offline_access%20User.Read%20Calendars.Read"));
    assert!(url.contains("code_challenge_method=S256"));
    // os-local / ics sources do not sign in.
    ok(
        &r,
        json!({"action": "calendar-source-add", "name": "l", "kind": "os-local"}),
    );
    assert_eq!(
        run(&r, json!({"action": "calendar-disconnect", "source": "l"}))
            .unwrap_err()
            .code,
        "invalid_argument"
    );
}

// ---- CalDAV ----------------------------------------------------------------------------------

fn multistatus(responses: &str) -> (u16, Vec<(&'static str, String)>, String) {
    (
        207,
        vec![("Content-Type", "application/xml; charset=utf-8".into())],
        format!(
            r#"<?xml version="1.0" encoding="utf-8"?><d:multistatus xmlns:d="DAV:" xmlns:cal="urn:ietf:params:xml:ns:caldav">{responses}</d:multistatus>"#
        ),
    )
}

const EVENT: &str = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//t//t//EN\r\nBEGIN:VEVENT\r\nUID:dav-1@fastmail\r\nDTSTAMP:20261101T000000Z\r\nDTSTART:20261109T150000Z\r\nDTEND:20261109T153000Z\r\nSUMMARY:Board call\r\nRRULE:FREQ=DAILY;COUNT=3\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";

#[test]
fn caldav_discovers_the_principal_home_and_calendars_then_reports_events() {
    let srv = serve(Box::new(|rq, base| {
        let auth_ok = rq.header("Authorization") == Some("Basic bWVAZGF2aWRhdy5hZDphcHAtcHc=");
        if !auth_ok {
            return (
                401,
                vec![("WWW-Authenticate", "Basic realm=\"dav\"".into())],
                String::new(),
            );
        }
        match (rq.method.as_str(), rq.path()) {
            // The server root names no principal; the well-known URL redirects to one that does.
            ("PROPFIND", "/") => multistatus("<d:response><d:href>/</d:href><d:propstat><d:prop><d:resourcetype><d:collection/></d:resourcetype></d:prop></d:propstat></d:response>"),
            ("PROPFIND", "/.well-known/caldav") => (301, vec![("Location", format!("{base}/dav/"))], String::new()),
            ("PROPFIND", "/dav/") => multistatus("<d:response><d:href>/dav/</d:href><d:propstat><d:prop><d:current-user-principal><d:href>/dav/principals/me/</d:href></d:current-user-principal></d:prop></d:propstat></d:response>"),
            ("PROPFIND", "/dav/principals/me/") => multistatus("<d:response><d:href>/dav/principals/me/</d:href><d:propstat><d:prop><cal:calendar-home-set><d:href>/dav/calendars/me/</d:href></cal:calendar-home-set></d:prop></d:propstat></d:response>"),
            ("PROPFIND", "/dav/calendars/me/") => {
                assert_eq!(rq.header("Depth"), Some("1"));
                multistatus(r#"<d:response><d:href>/dav/calendars/me/</d:href><d:propstat><d:prop><d:resourcetype><d:collection/></d:resourcetype></d:prop></d:propstat></d:response>
<d:response><d:href>/dav/calendars/me/work/</d:href><d:propstat><d:prop><d:resourcetype><d:collection/><cal:calendar/></d:resourcetype><d:displayname>Work</d:displayname><cal:supported-calendar-component-set><cal:comp name="VEVENT"/></cal:supported-calendar-component-set></d:prop></d:propstat></d:response>
<d:response><d:href>/dav/calendars/me/tasks/</d:href><d:propstat><d:prop><d:resourcetype><d:collection/><cal:calendar/></d:resourcetype><d:displayname>Tasks</d:displayname><cal:supported-calendar-component-set><cal:comp name="VTODO"/></cal:supported-calendar-component-set></d:prop></d:propstat></d:response>"#)
            }
            ("REPORT", "/dav/calendars/me/work/") => {
                assert!(rq.body.contains(r#"<C:time-range start="20261109T000000Z" end="20261112T000000Z"/>"#), "{}", rq.body);
                multistatus(&format!("<d:response><d:href>/dav/calendars/me/work/a.ics</d:href><d:propstat><d:prop><d:getetag>\"1\"</d:getetag><cal:calendar-data><![CDATA[{EVENT}]]></cal:calendar-data></d:prop></d:propstat></d:response>"))
            }
            _ => (404, vec![], String::new()),
        }
    }));
    let r = rig();
    r.secrets.set("caldav/fm", "app-pw").unwrap();
    ok(
        &r,
        json!({"action": "calendar-source-add", "name": "fm", "kind": "caldav", "url": format!("{}/", srv.base),
               "username": "me@davidaw.ad", "password": "secret:caldav/fm"}),
    );
    let e = run(&r, json!({"action": "calendar-connect", "source": "fm"})).unwrap_err();
    assert_eq!(e.code, "network_denied");
    r.engine.set("network.allow", &json!(true), "test").unwrap();
    let v = ok(&r, json!({"action": "calendar-connect", "source": "fm"}));
    assert_eq!(v["calendars"], 1, "{v}");
    let acct = &v["accounts"][0];
    assert_eq!(acct["title"], "me@davidaw.ad");
    assert_eq!(acct["kind"], "caldav");
    assert_eq!(acct["calendars"][0]["title"], "Work");
    assert_eq!(
        acct["calendars"][0]["id"],
        format!("{}/dav/calendars/me/work/", srv.base)
    );
    let v = ok(
        &r,
        json!({"action": "calendar-events", "from": "2026-11-09T00:00:00Z", "to": "2026-11-12T00:00:00Z"}),
    );
    let evs = v["events"].as_array().unwrap();
    assert_eq!(evs.len(), 3, "the RRULE expands: {v}");
    assert_eq!(evs[0]["title"], "Board call");
    assert_eq!(evs[0]["kind"], "caldav");
    assert_eq!(evs[0]["uid"], "dav-1@fastmail");
    assert_eq!(evs[0]["calendar"], "Work");
    assert_eq!(evs[0]["account"], "me@davidaw.ad");
    // A wrong password is calendar_auth.
    r.secrets.set("caldav/fm", "nope").unwrap();
    let v = ok(
        &r,
        json!({"action": "calendar-events", "from": "2026-11-09T00:00:00Z", "to": "2026-11-12T00:00:00Z"}),
    );
    assert_eq!(v["errors"][0]["error"]["code"], "calendar_auth", "{v}");
}

/// A real Radicale on a free loopback port, when `radicale` is on PATH (CI installs it):
/// discovery from the server root and an event read back.
#[test]
fn caldav_against_a_local_radicale() {
    let Some(bin) = which("radicale") else {
        eprintln!("radicale not installed: skipped");
        return;
    };
    let d = tempfile::tempdir().unwrap();
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let cfg = d.path().join("config");
    std::fs::write(
        &cfg,
        format!(
            "[server]\nhosts = 127.0.0.1:{port}\n[auth]\ntype = none\n[storage]\nfilesystem_folder = {}\n[logging]\nlevel = warning\n",
            d.path().join("collections").display()
        ),
    )
    .unwrap();
    let mut child = std::process::Command::new(bin)
        .arg("--config")
        .arg(&cfg)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let base = format!("http://127.0.0.1:{port}");
    let up = (0..100).any(|_| {
        std::thread::sleep(Duration::from_millis(100));
        std::net::TcpStream::connect(("127.0.0.1", port)).is_ok()
    });
    assert!(up, "radicale did not come up");
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .allow_non_standard_methods(true)
        .build()
        .into();
    let call = |method: &str, path: &str, body: &str, ctype: &str| {
        let req = ureq::http::Request::builder()
            .method(method)
            .uri(format!("{base}{path}"))
            .header("Content-Type", ctype)
            .header("Authorization", "Basic ZGF2aWQ6eA==")
            .body(body.to_string())
            .unwrap();
        agent.run(req).unwrap().status().as_u16()
    };
    let mk = r#"<?xml version="1.0" encoding="utf-8"?><C:mkcalendar xmlns:D="DAV:" xmlns:C="urn:ietf:params:xml:ns:caldav"><D:set><D:prop><D:displayname>Meetings</D:displayname></D:prop></D:set></C:mkcalendar>"#;
    let st = call("MKCALENDAR", "/david/meetings/", mk, "application/xml");
    assert!(st == 201 || st == 200, "MKCALENDAR {st}");
    let st = call("PUT", "/david/meetings/dav-1.ics", EVENT, "text/calendar");
    assert!(st == 201 || st == 204, "PUT {st}");
    let r = rig();
    r.engine.set("network.allow", &json!(true), "test").unwrap();
    r.secrets.set("caldav/rad", "x").unwrap();
    ok(
        &r,
        json!({"action": "calendar-source-add", "name": "rad", "kind": "caldav", "url": format!("{base}/"),
               "username": "david", "password": "secret:caldav/rad"}),
    );
    let res = (|| {
        let v = ok(&r, json!({"action": "calendar-connect", "source": "rad"}));
        assert_eq!(v["accounts"][0]["calendars"][0]["title"], "Meetings", "{v}");
        let v = ok(
            &r,
            json!({"action": "calendar-events", "from": "2026-11-09T00:00:00Z", "to": "2026-11-12T00:00:00Z"}),
        );
        assert_eq!(v["events"].as_array().unwrap().len(), 3, "{v}");
        assert_eq!(v["events"][0]["title"], "Board call");
        let m = ok(
            &r,
            json!({"action": "calendar-match", "at": "2026-11-10T15:02:00Z"}),
        );
        assert_eq!(m["title"], "Board call");
    })();
    let _ = child.kill();
    let _ = child.wait();
    res
}

fn which(name: &str) -> Option<std::path::PathBuf> {
    std::env::var_os("PATH").and_then(|p| {
        std::env::split_paths(&p)
            .map(|d| d.join(name))
            .find(|f| f.is_file())
    })
}

// ---- os-local on Linux: Evolution Data Server and KDE Akonadi --------------------------------

#[cfg(target_os = "linux")]
#[test]
fn linux_os_local_reads_evolution_and_akonadi_accounts_with_no_network() {
    use oats_core::calendar::local::Local;
    let d = tempfile::tempdir().unwrap();
    let home = Local {
        config: d.path().join("config"),
        data: d.path().join("data"),
        cache: d.path().join("cache"),
    };
    let w = |p: std::path::PathBuf, text: &str| {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    };
    let src = home.config.join("evolution/sources");
    // A GNOME Online Accounts Google account with one synced calendar.
    w(src.join("goa-acct.source"), "[Data Source]\nDisplayName=me@davidaw.ad\nEnabled=true\nParent=\n\n[Collection]\nBackendName=google\nIdentity=me@davidaw.ad\n\n[GNOME Online Accounts]\nAccountId=account_1\n");
    w(src.join("cal-work.source"), "[Data Source]\nDisplayName=Work\nEnabled=true\nParent=goa-acct\n\n[Calendar]\nBackendName=caldav\n");
    w(src.join("cal-off.source"), "[Data Source]\nDisplayName=Hidden\nEnabled=false\nParent=goa-acct\n\n[Calendar]\nBackendName=caldav\n");
    w(src.join("addressbook.source"), "[Data Source]\nDisplayName=Contacts\nParent=local-stub\n\n[Address Book]\nBackendName=local\n");
    // The built-in Personal calendar, never edited, so no .source file.
    w(
        home.data.join("evolution/calendar/system/calendar.ics"),
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:home-1\r\nDTSTART:20261109T180000Z\r\nDTEND:20261109T190000Z\r\nSUMMARY:Dentist\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n",
    );
    // EDS's offline cache of the synced calendar.
    let db = home.cache.join("evolution/calendar/cal-work/cache.db");
    std::fs::create_dir_all(db.parent().unwrap()).unwrap();
    let c = rusqlite::Connection::open(&db).unwrap();
    c.execute_batch(
        "CREATE TABLE ECacheObjects (ECacheUID TEXT PRIMARY KEY, ECacheREV TEXT, ECacheOBJ TEXT, ECacheState INTEGER);
         CREATE TABLE timezones (tzid TEXT PRIMARY KEY, zone TEXT, refs INTEGER);",
    )
    .unwrap();
    c.execute(
        "INSERT INTO timezones VALUES ('Europe/Berlin', ?1, 1)",
        ["BEGIN:VTIMEZONE\r\nTZID:Europe/Berlin\r\nBEGIN:STANDARD\r\nDTSTART:19701025T030000\r\nTZOFFSETFROM:+0200\r\nTZOFFSETTO:+0100\r\nEND:STANDARD\r\nEND:VTIMEZONE"],
    )
    .unwrap();
    c.execute(
        "INSERT INTO ECacheObjects VALUES ('sync-123@google.com', '1', ?1, 0)",
        ["BEGIN:VEVENT\r\nUID:sync-123@google.com\r\nDTSTART;TZID=Europe/Berlin:20261109T160000\r\nDTEND;TZID=Europe/Berlin:20261109T163000\r\nSUMMARY:Weekly sync\r\nURL:https://meet.google.com/abc-defg-hij\r\nEND:VEVENT"],
    )
    .unwrap();
    drop(c);
    // A KDE iCal file resource.
    let kde = d.path().join("korganizer/std.ics");
    w(kde.clone(), "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:kde-1\r\nDTSTART:20261110T080000Z\r\nDTEND:20261110T083000Z\r\nSUMMARY:KDE standup\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n");
    w(
        home.config.join("akonadi_ical_resource_0rc"),
        &format!("[General]\nPath={}\nReadOnly=false\n", kde.display()),
    );
    w(
        home.config
            .join("akonadi/agent_config_akonadi_ical_resource_0"),
        "[Agent]\nName=Personal (KDE)\n",
    );

    let r = rig();
    r.engine
        .set_calendar_provider(SourceKind::OsLocal, Arc::new(home));
    ok(
        &r,
        json!({"action": "calendar-source-add", "name": "local", "kind": "os-local"}),
    );
    let l = ok(&r, json!({"action": "calendar-sources-list"}));
    let accts = l["sources"][0]["accounts"].as_array().unwrap().clone();
    let names: Vec<(&str, &str)> = accts
        .iter()
        .map(|a| (a["title"].as_str().unwrap(), a["kind"].as_str().unwrap()))
        .collect();
    assert_eq!(
        names,
        vec![
            ("KDE (Akonadi)", "akonadi-ical"),
            ("On This Computer", "eds-local"),
            ("me@davidaw.ad", "eds-google"),
        ],
        "{l}"
    );
    let google = &accts[2];
    assert_eq!(
        google["calendars"].as_array().unwrap().len(),
        1,
        "disabled calendar left out"
    );
    assert_eq!(google["calendars"][0]["title"], "Work");
    // network.allow stays off: os-local needs none.
    assert_eq!(r.engine.snapshot().config.network.allow, false);
    let v = ok(
        &r,
        json!({"action": "calendar-events", "from": "2026-11-09T00:00:00Z", "to": "2026-11-11T00:00:00Z"}),
    );
    assert!(v["errors"].as_array().unwrap().is_empty(), "{v}");
    let got: Vec<(&str, &str, &str)> = v["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| {
            (
                e["title"].as_str().unwrap(),
                e["start"].as_str().unwrap(),
                e["account"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        got,
        vec![
            ("Weekly sync", "2026-11-09T15:00:00Z", "me@davidaw.ad"),
            ("Dentist", "2026-11-09T18:00:00Z", "On This Computer"),
            ("KDE standup", "2026-11-10T08:00:00Z", "KDE (Akonadi)"),
        ]
    );
    // Untick the KDE account: the rest still read.
    ok(
        &r,
        json!({"action": "calendar-select", "source": "local", "account": "akonadi", "selected": false}),
    );
    let v = ok(
        &r,
        json!({"action": "calendar-events", "from": "2026-11-09T00:00:00Z", "to": "2026-11-11T00:00:00Z"}),
    );
    assert_eq!(v["events"].as_array().unwrap().len(), 2);
    let m = ok(
        &r,
        json!({"action": "calendar-match", "at": "2026-11-09T15:03:00Z"}),
    );
    assert_eq!(m["title"], "Weekly sync");
    assert_eq!(
        m["event"]["video_url"],
        "https://meet.google.com/abc-defg-hij"
    );
}
