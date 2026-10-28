//! ICS subscriptions through the engine: feed list, cache, gating by network.allow, HTTP against a
//! local server (never the internet), refresh schedule, the ics-updated event, and every ics.*
//! choice key exercised for every value.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use oats_core::action::Action;
use oats_core::config::ConfigError;
use oats_core::control::client::Client;
use oats_core::control::engine::{Engine, EngineOpts};
use oats_core::control::server;
use serde_json::{json, Map, Value};

// ---- local HTTP server ---------------------------------------------------------------------

struct Resp {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
    delay: Duration,
}

impl Resp {
    fn ok(body: &str) -> Resp {
        Resp { status: 200, headers: vec![("Content-Type".into(), "text/calendar".into())], body: body.as_bytes().to_vec(), delay: Duration::ZERO }
    }
    fn status(code: u16) -> Resp {
        Resp { status: code, headers: vec![], body: vec![], delay: Duration::ZERO }
    }
}

type Handler = Arc<dyn Fn(&str, &str) -> Resp + Send + Sync>;

struct Http {
    port: u16,
    stop: Arc<AtomicBool>,
    seen: Arc<Mutex<Vec<String>>>,
}

impl Http {
    fn start(h: Handler) -> Http {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        l.set_nonblocking(true).unwrap();
        let port = l.local_addr().unwrap().port();
        let stop = Arc::new(AtomicBool::new(false));
        let seen = Arc::new(Mutex::new(Vec::new()));
        let (stop2, seen2) = (Arc::clone(&stop), Arc::clone(&seen));
        thread::spawn(move || {
            while !stop2.load(Ordering::SeqCst) {
                match l.accept() {
                    Ok((mut c, _)) => {
                        let (h, seen) = (Arc::clone(&h), Arc::clone(&seen2));
                        thread::spawn(move || {
                            let _ = c.set_nonblocking(false);
                            let _ = c.set_read_timeout(Some(Duration::from_secs(2)));
                            let mut buf = Vec::new();
                            let mut tmp = [0u8; 1024];
                            loop {
                                match c.read(&mut tmp) {
                                    Ok(0) | Err(_) => return,
                                    Ok(n) => {
                                        buf.extend_from_slice(&tmp[..n]);
                                        if buf.windows(4).any(|w| w == b"\r\n\r\n") {
                                            break;
                                        }
                                    }
                                }
                            }
                            let req = String::from_utf8_lossy(&buf).to_string();
                            let path = req.split_whitespace().nth(1).unwrap_or("/").to_string();
                            seen.lock().unwrap().push(req.clone());
                            let r = h(&path, &req.to_ascii_lowercase());
                            thread::sleep(r.delay);
                            let mut head = format!(
                                "HTTP/1.1 {} X\r\nContent-Length: {}\r\nConnection: close\r\n",
                                r.status,
                                r.body.len()
                            );
                            for (k, v) in &r.headers {
                                head.push_str(&format!("{k}: {v}\r\n"));
                            }
                            head.push_str("\r\n");
                            let _ = c.write_all(head.as_bytes());
                            let _ = c.write_all(&r.body);
                            let _ = c.flush();
                        });
                    }
                    Err(_) => thread::sleep(Duration::from_millis(10)),
                }
            }
        });
        Http { port, stop, seen }
    }

    fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{path}", self.port)
    }
    fn hits(&self) -> usize {
        self.seen.lock().unwrap().len()
    }
}

impl Drop for Http {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

fn fixture_path(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/ics").join(name)
}

fn fixture(name: &str) -> String {
    std::fs::read_to_string(fixture_path(name)).unwrap()
}

fn file_url(name: &str) -> String {
    format!("file://{}", fixture_path(name).display())
}

// ---- engine rig ----------------------------------------------------------------------------

struct Rig {
    dir: tempfile::TempDir,
    engine: Arc<Engine>,
}

impl Rig {
    fn new() -> Rig {
        Self::with_config("")
    }

    /// Network on, short timeout: for tests that talk to the local server.
    fn net() -> Rig {
        Self::with_config("[network]\nallow = true\n[ics]\ntimeout_secs = \"2\"\n")
    }

    fn with_config(toml: &str) -> Rig {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join("cfg").join("config.toml");
        std::fs::create_dir_all(cfg.parent().unwrap()).unwrap();
        std::fs::write(&cfg, toml).unwrap();
        let engine = Engine::new(EngineOpts {
            user_config: cfg,
            pack: None,
            os: "linux".into(),
            arch: "x86_64".into(),
            machine: "default".into(),
            sessions_dir: dir.path().join("data").join("sessions"),
            secrets: None,
        })
        .unwrap();
        Rig { dir, engine }
    }

    fn set(&self, key: &str, v: Value) {
        self.engine.set(key, &v, "test").unwrap();
    }

    fn add(&self, name: &str, url: &str) -> Result<Value, ConfigError> {
        self.engine.dispatch(&Action::IcsAdd {
            name: name.into(),
            url: url.into(),
            enabled: None,
            refresh_secs: None,
            header: None,
        })
    }

    fn add_full(&self, name: &str, url: &str, enabled: Option<bool>, refresh: Option<u64>, header: Option<&str>) -> Result<Value, ConfigError> {
        self.engine.dispatch(&Action::IcsAdd {
            name: name.into(),
            url: url.into(),
            enabled,
            refresh_secs: refresh,
            header: header.map(str::to_string),
        })
    }

    fn refresh(&self, name: Option<&str>) -> Result<Value, ConfigError> {
        self.engine.dispatch(&Action::IcsRefresh { name: name.map(str::to_string) })
    }

    fn events(&self, from: &str, to: &str) -> Value {
        self.engine
            .dispatch(&Action::IcsEvents { from: Some(from.into()), to: Some(to.into()), feed: None })
            .unwrap()
    }

    fn list(&self) -> Value {
        self.engine.dispatch(&Action::IcsList {}).unwrap()
    }

    fn sub(&self) -> Receiver<String> {
        self.engine.bus.subscribe(vec!["ics-updated".into()])
    }
}

fn code<T: std::fmt::Debug>(r: Result<T, ConfigError>) -> &'static str {
    r.unwrap_err().code
}

fn starts(v: &Value) -> Vec<String> {
    v["events"].as_array().unwrap().iter().map(|e| e["start"].as_str().unwrap().to_string()).collect()
}

// ---- feed list -----------------------------------------------------------------------------

#[test]
fn add_list_remove_and_persist_in_the_user_config() {
    let r = Rig::new();
    assert_eq!(r.list()["feeds"].as_array().unwrap().len(), 0);
    let added = r.add_full("work", &file_url("weekly-ny.ics"), None, Some(60), Some("Authorization: Bearer s3cret")).unwrap();
    assert_eq!(added["feed"]["name"], "work");
    assert_eq!(added["feed"]["has_header"], true);
    assert!(!added.to_string().contains("s3cret"), "the header value is never echoed");
    r.add("home", "https://example.invalid/home.ics").unwrap();
    let l = r.list();
    let names: Vec<&str> = l["feeds"].as_array().unwrap().iter().map(|f| f["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["work", "home"]);
    assert_eq!(l["feeds"][0]["scheme"], "file");
    assert_eq!(l["feeds"][0]["effective_refresh_secs"], 60);
    assert_eq!(l["feeds"][1]["effective_refresh_secs"], 900);
    assert_eq!(l["feeds"][1]["scheme"], "http");
    // The list is a config key: it survives an engine restart over the same user file.
    let user_cfg = r.dir.path().join("cfg").join("config.toml");
    let text = std::fs::read_to_string(&user_cfg).unwrap();
    assert!(text.contains("[ics]") && text.contains("feeds"), "{text}");
    r.engine.reload("test").unwrap();
    assert_eq!(r.list()["feeds"].as_array().unwrap().len(), 2);
    r.engine.dispatch(&Action::IcsRemove { name: "work".into() }).unwrap();
    assert_eq!(r.list()["feeds"].as_array().unwrap().len(), 1);
}

#[test]
fn add_and_remove_validate_their_arguments() {
    let r = Rig::new();
    r.add("a", "https://example.invalid/a.ics").unwrap();
    assert_eq!(code(r.add("a", "https://example.invalid/b.ics")), "feed_exists");
    assert_eq!(code(r.add("../x", "https://example.invalid/a.ics")), "invalid_argument");
    assert_eq!(code(r.add("", "https://example.invalid/a.ics")), "invalid_argument");
    assert_eq!(code(r.add("b", "ftp://example.invalid/a.ics")), "invalid_argument");
    assert_eq!(code(r.add("b", "not a url")), "invalid_argument");
    assert_eq!(code(r.add_full("b", "https://example.invalid/a.ics", None, None, Some("no colon"))), "invalid_argument");
    assert_eq!(code(r.engine.dispatch(&Action::IcsRemove { name: "zzz".into() })), "feed_not_found");
    assert_eq!(code(r.refresh(Some("zzz"))), "feed_not_found");
    assert_eq!(r.list()["feeds"].as_array().unwrap().len(), 1);
}

#[test]
fn every_url_scheme_is_accepted() {
    let r = Rig::new();
    for (i, u) in ["http://h/a.ics", "https://h/a.ics", "webcal://h/a.ics", "webcals://h/a.ics", "file:///tmp/a.ics"].iter().enumerate() {
        r.add(&format!("f{i}"), u).unwrap();
    }
    assert_eq!(r.list()["feeds"].as_array().unwrap().len(), 5);
}

// ---- file feeds, cache, event ---------------------------------------------------------------

#[test]
fn file_feed_refresh_caches_and_serves_expanded_events() {
    let r = Rig::new(); // network.allow is false: file:// is not a network fetch
    let rx = r.sub();
    r.add("ny", &file_url("weekly-ny.ics")).unwrap();
    let first = r.refresh(Some("ny")).unwrap();
    assert_eq!(first["feeds"][0]["ok"], true);
    assert_eq!(first["feeds"][0]["changed"], true);
    assert_eq!(first["feeds"][0]["events"], 2);
    let ev: Value = serde_json::from_str(&rx.recv_timeout(Duration::from_secs(2)).unwrap()).unwrap();
    assert_eq!(ev["event"], "ics-updated");
    assert_eq!(ev["data"]["feed"], "ny");
    assert_eq!(ev["data"]["events"], 2);
    let second = r.refresh(Some("ny")).unwrap();
    assert_eq!(second["feeds"][0]["changed"], false);
    assert!(rx.try_recv().is_err(), "an unchanged feed publishes nothing");
    let v = r.events("2026-03-01T00:00:00Z", "2026-05-01T00:00:00Z");
    assert_eq!(v["events"].as_array().unwrap().len(), 7);
    assert_eq!(starts(&v)[0], "2026-03-02T14:00:00Z");
    assert_eq!(v["events"][0]["feed"], "ny");
    assert!(r.dir.path().join("data/ics/ny.ics").exists());
    // The cache outlives the source: delete nothing, just stop refreshing and read again.
    let l = r.list();
    assert_eq!(l["feeds"][0]["cached"], true);
    assert_eq!(l["feeds"][0]["events"], 2);
    assert!(l["feeds"][0]["fetched_at"].as_str().unwrap().ends_with('Z'));
}

#[test]
fn removing_a_feed_drops_its_cache_and_announces_it() {
    let r = Rig::new();
    r.add("ny", &file_url("weekly-ny.ics")).unwrap();
    r.refresh(None).unwrap();
    let rx = r.sub();
    r.engine.dispatch(&Action::IcsRemove { name: "ny".into() }).unwrap();
    assert!(!r.dir.path().join("data/ics/ny.ics").exists());
    let ev: Value = serde_json::from_str(&rx.recv_timeout(Duration::from_secs(2)).unwrap()).unwrap();
    assert_eq!(ev["data"]["removed"], true);
    assert_eq!(r.events("2026-03-01T00:00:00Z", "2026-05-01T00:00:00Z")["events"].as_array().unwrap().len(), 0);
}

#[test]
fn several_feeds_merge_sorted_and_can_be_filtered() {
    let r = Rig::new();
    r.add("ny", &file_url("weekly-ny.ics")).unwrap();
    r.add("zones", &file_url("zones-misc.ics")).unwrap();
    r.refresh(None).unwrap();
    let v = r.events("2026-03-01T00:00:00Z", "2026-08-01T00:00:00Z");
    let s = starts(&v);
    let mut sorted = s.clone();
    sorted.sort();
    assert_eq!(s, sorted);
    assert_eq!(s.len(), 7 + 3);
    let one = r
        .engine
        .dispatch(&Action::IcsEvents { from: Some("2026-03-01".into()), to: Some("2026-08-01".into()), feed: Some("zones".into()) })
        .unwrap();
    assert_eq!(one["events"].as_array().unwrap().len(), 3);
    assert_eq!(
        code(r.engine.dispatch(&Action::IcsEvents { from: None, to: None, feed: Some("nope".into()) })),
        "feed_not_found"
    );
}

#[test]
fn events_window_arguments() {
    let r = Rig::new();
    r.add("ny", &file_url("weekly-ny.ics")).unwrap();
    r.refresh(None).unwrap();
    let e = |from: Option<&str>, to: Option<&str>| {
        r.engine.dispatch(&Action::IcsEvents { from: from.map(str::to_string), to: to.map(str::to_string), feed: None })
    };
    assert_eq!(code(e(Some("garbage"), None)), "invalid_argument");
    assert_eq!(code(e(Some("2026-04-01"), Some("2026-03-01"))), "invalid_argument");
    // Date-only bounds are midnight in ics.default_tz.
    let a = e(Some("2026-03-02"), Some("2026-03-03")).unwrap();
    assert_eq!(starts(&a), ["2026-03-02T14:00:00Z"]);
    assert_eq!(a["from"], "2026-03-02T00:00:00Z");
    // No `to`: from + ics.window_days.
    assert_eq!(e(Some("2026-03-01T00:00:00Z"), None).unwrap()["to"], "2026-03-08T00:00:00Z");
    r.set("ics.window_days", json!("30"));
    let w = e(Some("2026-03-01T00:00:00Z"), None).unwrap();
    assert_eq!(w["to"], "2026-03-31T00:00:00Z");
    assert_eq!(w["events"].as_array().unwrap().len(), 4); // Mar 2, 9, 23 (moved), 30
    r.set("ics.window_days", json!("1"));
    assert_eq!(e(Some("2026-03-02T00:00:00Z"), None).unwrap()["events"].as_array().unwrap().len(), 1);
    // No `from`: starts now, so the 2026 fixture series is behind us or ahead: either way no panic.
    e(None, None).unwrap();
}

// ---- gating --------------------------------------------------------------------------------

#[test]
fn network_denied_for_http_webcal_and_webcals_without_touching_the_network() {
    let h = Http::start(Arc::new(|_, _| Resp::ok(&fixture("weekly-ny.ics"))));
    let r = Rig::new(); // network.allow defaults to false
    r.add("h", &h.url("/a.ics")).unwrap();
    r.add("w", &format!("webcal://127.0.0.1:{}/a.ics", h.port)).unwrap();
    r.add("s", &format!("webcals://127.0.0.1:{}/a.ics", h.port)).unwrap();
    for n in ["h", "w", "s"] {
        assert_eq!(code(r.refresh(Some(n))), "network_denied", "{n}");
    }
    let all = r.refresh(None).unwrap();
    assert_eq!(all["failed"], 3);
    for f in all["feeds"].as_array().unwrap() {
        assert_eq!(f["error"]["code"], "network_denied");
    }
    assert_eq!(h.hits(), 0);
    // Allowing it at runtime takes effect on the next call.
    r.set("network.allow", json!(true));
    assert_eq!(r.refresh(Some("h")).unwrap()["feeds"][0]["ok"], true);
    assert_eq!(h.hits(), 1);
    r.set("network.allow", json!(false));
    assert_eq!(code(r.refresh(Some("h"))), "network_denied");
}

#[test]
fn allow_file_gates_file_feeds_both_ways() {
    for allow in [true, false] {
        let r = Rig::with_config(&format!("[ics]\nallow_file = {allow}\n"));
        r.add("f", &file_url("lf-only.ics")).unwrap();
        let res = r.refresh(Some("f"));
        if allow {
            assert_eq!(res.unwrap()["feeds"][0]["events"], 1);
        } else {
            assert_eq!(code(res), "file_denied");
        }
    }
}

#[test]
fn master_switch_disables_refresh_but_cached_events_stay_readable() {
    for enabled in [true, false] {
        let r = Rig::new();
        r.add("f", &file_url("lf-only.ics")).unwrap();
        r.refresh(None).unwrap();
        r.set("ics.enabled", json!(enabled));
        let res = r.refresh(None);
        if enabled {
            res.unwrap();
        } else {
            assert_eq!(code(res), "ics_disabled");
            assert_eq!(r.list()["enabled"], false);
        }
        assert_eq!(r.events("2026-08-01T00:00:00Z", "2026-08-02T00:00:00Z")["events"].as_array().unwrap().len(), 1);
    }
}

#[test]
fn a_disabled_feed_is_not_refreshed_or_listed_in_events() {
    let r = Rig::new();
    r.add_full("off", &file_url("lf-only.ics"), Some(false), None, None).unwrap();
    r.add("on", &file_url("zones-misc.ics")).unwrap();
    assert_eq!(code(r.refresh(Some("off"))), "feed_disabled");
    let all = r.refresh(None).unwrap();
    assert_eq!(all["feeds"].as_array().unwrap().len(), 1);
    assert_eq!(all["feeds"][0]["name"], "on");
    let v = r.events("2026-07-01T00:00:00Z", "2026-09-01T00:00:00Z");
    assert_eq!(v["events"].as_array().unwrap().len(), 3);
}

// ---- HTTP ----------------------------------------------------------------------------------

#[test]
fn http_fetch_sends_the_header_and_parses_the_body() {
    let body = fixture("weekly-ny.ics");
    let h = Http::start(Arc::new(move |_, _| Resp::ok(&body)));
    let r = Rig::net();
    r.add_full("n", &h.url("/cal.ics"), None, None, Some("Authorization: Bearer s3cret")).unwrap();
    let res = r.refresh(Some("n")).unwrap();
    assert_eq!(res["feeds"][0]["events"], 2);
    let req = h.seen.lock().unwrap()[0].to_ascii_lowercase();
    assert!(req.contains("get /cal.ics"), "{req}");
    assert!(req.contains("authorization: bearer s3cret"), "{req}");
    assert!(req.contains("accept: text/calendar"), "{req}");
    assert_eq!(r.events("2026-03-01", "2026-05-01")["events"].as_array().unwrap().len(), 7);
}

#[test]
fn webcal_scheme_choice_http_and_https() {
    let body = fixture("lf-only.ics");
    let h = Http::start(Arc::new(move |_, _| Resp::ok(&body)));
    for scheme in ["http", "https"] {
        let r = Rig::with_config(&format!("[network]\nallow = true\n[ics]\nwebcal_scheme = \"{scheme}\"\ntimeout_secs = \"1\"\n"));
        r.add("w", &format!("webcal://127.0.0.1:{}/a.ics", h.port)).unwrap();
        let res = r.refresh(Some("w"));
        if scheme == "http" {
            assert_eq!(res.unwrap()["feeds"][0]["events"], 1);
        } else {
            // A TLS handshake against a plain-HTTP port cannot succeed.
            let c = code(res);
            assert!(c == "fetch_failed" || c == "fetch_timeout", "{c}");
        }
    }
}

#[test]
fn conditional_get_uses_the_etag() {
    let state = Arc::new(Mutex::new(("\"v1\"".to_string(), fixture("lf-only.ics"))));
    let s2 = Arc::clone(&state);
    let h = Http::start(Arc::new(move |_, req| {
        let (etag, body) = s2.lock().unwrap().clone();
        if req.contains(&format!("if-none-match: {}", etag.to_ascii_lowercase())) {
            return Resp { status: 304, headers: vec![("ETag".into(), etag)], body: vec![], delay: Duration::ZERO };
        }
        let mut r = Resp::ok(&body);
        r.headers.push(("ETag".into(), etag));
        r
    }));
    let r = Rig::net();
    let rx = r.sub();
    r.add("e", &h.url("/e.ics")).unwrap();
    assert_eq!(r.refresh(Some("e")).unwrap()["feeds"][0]["changed"], true);
    let again = r.refresh(Some("e")).unwrap();
    assert_eq!(again["feeds"][0]["not_modified"], true);
    assert_eq!(again["feeds"][0]["changed"], false);
    assert_eq!(h.hits(), 2);
    rx.recv_timeout(Duration::from_secs(2)).unwrap();
    assert!(rx.try_recv().is_err());
    *state.lock().unwrap() = ("\"v2\"".to_string(), fixture("lf-only.ics").replace("LF line endings", "Changed"));
    let third = r.refresh(Some("e")).unwrap();
    assert_eq!(third["feeds"][0]["changed"], true);
    rx.recv_timeout(Duration::from_secs(2)).unwrap();
    assert_eq!(r.events("2026-08-01", "2026-08-02")["events"][0]["summary"], "Changed");
}

#[test]
fn http_failures_have_stable_codes() {
    let big = "BEGIN:VCALENDAR\r\n".to_string() + &"X-PAD:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\r\n".repeat(50) + "END:VCALENDAR\r\n";
    let (good, html) = (fixture("lf-only.ics"), fixture("not-a-calendar.html"));
    let h = Http::start(Arc::new(move |path, _| match path {
        "/404" => Resp::status(404),
        "/500" => Resp::status(500),
        "/html" => Resp::ok(&html),
        "/big" => Resp::ok(&big),
        "/slow" => Resp { delay: Duration::from_secs(4), ..Resp::ok(&good) },
        _ => Resp::ok(&good),
    }));
    let r = Rig::with_config("[network]\nallow = true\n[ics]\ntimeout_secs = \"1\"\nmax_bytes = \"500\"\n");
    for (path, want) in [
        ("/404", "http_status"),
        ("/500", "http_status"),
        ("/html", "ics_not_calendar"),
        ("/big", "feed_too_large"),
        ("/slow", "fetch_timeout"),
    ] {
        r.add(&path[1..], &h.url(path)).unwrap();
        assert_eq!(code(r.refresh(Some(&path[1..]))), want, "{path}");
    }
    // Nothing was cached for any of them.
    assert!(!r.dir.path().join("data/ics/404.ics").exists());
    // Connection refused.
    let dead = {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    r.add("dead", &format!("http://127.0.0.1:{dead}/a.ics")).unwrap();
    assert_eq!(code(r.refresh(Some("dead"))), "fetch_failed");
    // The failure is recorded for the list view.
    let l = r.list();
    let row = l["feeds"].as_array().unwrap().iter().find(|f| f["name"] == "404").unwrap();
    assert_eq!(row["error"]["code"], "http_status");
}

#[test]
fn stale_on_error_serves_the_last_good_cache_or_fails_loudly() {
    for stale in [true, false] {
        let up = Arc::new(AtomicBool::new(true));
        let (u2, body) = (Arc::clone(&up), fixture("lf-only.ics"));
        let h = Http::start(Arc::new(move |_, _| if u2.load(Ordering::SeqCst) { Resp::ok(&body) } else { Resp::status(503) }));
        let r = Rig::with_config(&format!("[network]\nallow = true\n[ics]\nstale_on_error = {stale}\ntimeout_secs = \"2\"\n"));
        r.add("s", &h.url("/s.ics")).unwrap();
        r.refresh(Some("s")).unwrap();
        up.store(false, Ordering::SeqCst);
        let res = r.refresh(Some("s"));
        if stale {
            let v = res.unwrap();
            assert_eq!(v["feeds"][0]["stale"], true);
            assert_eq!(v["feeds"][0]["error"]["code"], "http_status");
        } else {
            assert_eq!(code(res), "http_status");
        }
        // Either way the cached copy still answers.
        assert_eq!(r.events("2026-08-01", "2026-08-02")["events"].as_array().unwrap().len(), 1);
    }
}

#[test]
fn a_malformed_refresh_keeps_the_previous_good_cache() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("live.ics");
    std::fs::copy(fixture_path("lf-only.ics"), &path).unwrap();
    let r = Rig::with_config("[ics]\nstale_on_error = false\n");
    r.add("live", &format!("file://{}", path.display())).unwrap();
    r.refresh(Some("live")).unwrap();
    std::fs::write(&path, "<html>login</html>").unwrap();
    assert_eq!(code(r.refresh(Some("live"))), "ics_not_calendar");
    assert_eq!(r.events("2026-08-01", "2026-08-02")["events"].as_array().unwrap().len(), 1);
    assert_eq!(r.list()["feeds"][0]["error"]["code"], "ics_not_calendar");
    std::fs::remove_file(&path).unwrap();
    assert_eq!(code(r.refresh(Some("live"))), "fetch_failed");
}

// ---- parse-policy keys, every value ---------------------------------------------------------

#[test]
fn on_bad_event_error_and_skip() {
    for mode in ["error", "skip"] {
        let r = Rig::with_config(&format!("[ics]\non_bad_event = \"{mode}\"\n"));
        r.add("m", &file_url("mixed.ics")).unwrap();
        let res = r.refresh(Some("m"));
        if mode == "error" {
            assert_eq!(code(res), "ics_bad_event");
        } else {
            let v = res.unwrap();
            assert_eq!(v["feeds"][0]["events"], 1);
            assert_eq!(v["feeds"][0]["skipped"], 1);
        }
    }
}

#[test]
fn unknown_tz_error_and_default_tz() {
    for mode in ["error", "default_tz"] {
        let r = Rig::with_config(&format!("[ics]\nunknown_tz = \"{mode}\"\ndefault_tz = \"Asia/Tokyo\"\n"));
        r.add("t", &file_url("bad-tz.ics")).unwrap();
        let res = r.refresh(Some("t"));
        if mode == "error" {
            assert_eq!(code(res), "ics_bad_tz");
        } else {
            res.unwrap();
            assert_eq!(starts(&r.events("2026-07-14", "2026-07-16")), ["2026-07-15T01:00:00Z"]);
        }
    }
}

#[test]
fn cancelled_and_all_day_choices_apply_at_query_time() {
    let r = Rig::new();
    r.add("m", &file_url("misc.ics")).unwrap();
    r.add("a", &file_url("all-day.ics")).unwrap();
    r.refresh(None).unwrap();
    let count = |r: &Rig, uid: &str, from: &str, to: &str| {
        r.events(from, to)["events"].as_array().unwrap().iter().filter(|e| e["uid"] == uid).count()
    };
    for mode in ["hide", "show"] {
        r.set("ics.cancelled", json!(mode));
        assert_eq!(count(&r, "cx@fixture", "2026-07-22", "2026-07-23"), usize::from(mode == "show"), "{mode}");
    }
    for mode in ["include", "exclude"] {
        r.set("ics.all_day", json!(mode));
        assert_eq!(count(&r, "conf@fixture", "2026-03-01", "2026-04-01"), usize::from(mode == "include"), "{mode}");
    }
}

#[test]
fn default_tz_choice_moves_floating_and_all_day_bounds() {
    let r = Rig::new();
    r.add("z", &file_url("zones-misc.ics")).unwrap();
    r.add("a", &file_url("all-day.ics")).unwrap();
    r.refresh(None).unwrap();
    for (tz, floating, midnight) in [
        ("UTC", "2026-07-15T08:00:00Z", "2026-06-05T00:00:00Z"),
        ("Europe/Berlin", "2026-07-15T06:00:00Z", "2026-06-04T22:00:00Z"),
        ("America/Los_Angeles", "2026-07-15T15:00:00Z", "2026-06-05T07:00:00Z"),
    ] {
        r.set("ics.default_tz", json!(tz));
        let v = r.events("2026-06-04T00:00:00Z", "2026-07-16T00:00:00Z");
        let f = v["events"].as_array().unwrap().iter().find(|e| e["summary"] == "Floating").unwrap();
        assert_eq!(f["start"], floating, "{tz}");
        let b = v["events"].as_array().unwrap().iter().find(|e| e["summary"] == "Birthday").unwrap();
        assert_eq!(b["start"], midnight, "{tz}");
    }
}

#[test]
fn max_occurrences_choice() {
    let r = Rig::new();
    r.add("l", &file_url("london-bst-end.ics")).unwrap();
    r.refresh(None).unwrap();
    for (cap, want) in [("2", 2), ("500", 6)] {
        r.set("ics.max_occurrences", json!(cap));
        assert_eq!(r.events("2026-10-01", "2026-12-01")["events"].as_array().unwrap().len(), want, "{cap}");
    }
}

#[test]
fn a_bad_ics_value_vetoes_the_change_and_keeps_the_running_config() {
    let r = Rig::new();
    r.add("keep", "https://example.invalid/a.ics").unwrap();
    for (key, bad) in [
        ("ics.timeout_secs", "soon"),
        ("ics.max_bytes", "-1"),
        ("ics.default_refresh_secs", "1.5"),
        ("ics.window_days", ""),
        ("ics.default_tz", "Mars/Base"),
        ("ics.feeds", "not json"),
        ("ics.feeds", r#"[{"name":"x","url":"ftp://h/a.ics"}]"#),
        ("ics.feeds", r#"[{"name":"x","url":"https://h/a.ics","header":"nocolon"}]"#),
        ("ics.feeds", r#"[{"name":"../x","url":"https://h/a.ics"}]"#),
    ] {
        let e = r.engine.set(key, &json!(bad), "test").unwrap_err();
        assert!(e.message.starts_with("ics:"), "{key}={bad}: {e}");
        assert!(e.code == "invalid_value" || e.code == "invalid_argument", "{key}: {}", e.code);
        assert_eq!(r.list()["feeds"].as_array().unwrap().len(), 1, "{key}");
        r.engine.dispatch(&Action::ConfigCheck {}).unwrap();
    }
}

#[test]
fn a_reload_that_drops_or_repoints_a_feed_discards_its_cache_and_announces_it() {
    let r = Rig::new();
    r.add("gone", &file_url("lf-only.ics")).unwrap();
    r.add("moved", &file_url("zones-misc.ics")).unwrap();
    r.add("kept", &file_url("all-day.ics")).unwrap();
    r.refresh(None).unwrap();
    let rx = r.sub();
    let feeds = json!([
        {"name": "moved", "url": file_url("lf-only.ics")},
        {"name": "kept", "url": file_url("all-day.ics")}
    ])
    .to_string();
    let text = format!("[ics]\nfeeds = '{feeds}'\n");
    std::fs::write(r.dir.path().join("cfg/config.toml"), text).unwrap();
    r.engine.reload("test").unwrap();
    let mut seen = Vec::new();
    while let Ok(l) = rx.recv_timeout(Duration::from_millis(500)) {
        let v: Value = serde_json::from_str(&l).unwrap();
        seen.push((v["data"]["feed"].as_str().unwrap().to_string(), v["data"]["removed"].as_bool().unwrap()));
    }
    seen.sort();
    assert_eq!(seen, [("gone".to_string(), true), ("moved".to_string(), false)]);
    assert!(!r.dir.path().join("data/ics/gone.ics").exists());
    assert!(!r.dir.path().join("data/ics/moved.ics").exists());
    assert!(r.dir.path().join("data/ics/kept.ics").exists());
    // A reload with an invalid feed URL is vetoed whole; the running list stays.
    std::fs::write(r.dir.path().join("cfg/config.toml"), "[ics]\nfeeds = '[{\"name\":\"z\",\"url\":\"gopher://x\"}]'\n").unwrap();
    assert_eq!(code(r.engine.reload("test")), "invalid_argument");
    assert_eq!(r.list()["feeds"].as_array().unwrap().len(), 2);
}

// ---- schedule ------------------------------------------------------------------------------

#[test]
fn tick_honours_each_feeds_interval() {
    let r = Rig::new();
    r.add_full("a", &file_url("lf-only.ics"), None, Some(60), None).unwrap();
    r.add_full("b", &file_url("zones-misc.ics"), None, Some(300), None).unwrap();
    assert_eq!(oats_core::ics::tick(&r.engine, 1_000), ["a", "b"]);
    assert!(oats_core::ics::tick(&r.engine, 1_030).is_empty());
    assert_eq!(oats_core::ics::tick(&r.engine, 1_060), ["a"]);
    assert_eq!(oats_core::ics::tick(&r.engine, 1_300), ["a", "b"]);
    // ics-list judges "due" against the real clock, long after the fake t=1300 attempts.
    assert_eq!(r.list()["feeds"][0]["due"], true);
}

#[test]
fn tick_default_interval_zero_means_manual_only() {
    for (secs, want) in [("0", 0usize), ("900", 1)] {
        let r = Rig::with_config(&format!("[ics]\ndefault_refresh_secs = \"{secs}\"\n"));
        r.add("a", &file_url("lf-only.ics")).unwrap();
        assert_eq!(oats_core::ics::tick(&r.engine, 5_000).len(), want, "{secs}");
    }
}

#[test]
fn tick_skips_disabled_feeds_policy_denials_and_the_master_switch() {
    let h = Http::start(Arc::new(|_, _| Resp::ok(&fixture("lf-only.ics"))));
    let r = Rig::with_config("[ics]\nallow_file = false\n");
    r.add_full("off", &file_url("lf-only.ics"), Some(false), Some(1), None).unwrap();
    r.add_full("file", &file_url("lf-only.ics"), None, Some(1), None).unwrap();
    r.add_full("net", &h.url("/a.ics"), None, Some(1), None).unwrap();
    assert!(oats_core::ics::tick(&r.engine, 1_000).is_empty());
    assert_eq!(h.hits(), 0);
    r.set("network.allow", json!(true));
    r.set("ics.allow_file", json!(true));
    assert_eq!(oats_core::ics::tick(&r.engine, 2_000), ["file", "net"]);
    r.set("ics.enabled", json!(false));
    assert!(oats_core::ics::tick(&r.engine, 9_000).is_empty());
}

#[test]
fn a_failing_feed_backs_off_for_its_interval() {
    let h = Http::start(Arc::new(|_, _| Resp::status(500)));
    let r = Rig::net();
    r.add_full("bad", &h.url("/a.ics"), None, Some(60), None).unwrap();
    assert_eq!(oats_core::ics::tick(&r.engine, 1_000), ["bad"]);
    assert!(oats_core::ics::tick(&r.engine, 1_010).is_empty());
    assert_eq!(h.hits(), 1);
    assert_eq!(oats_core::ics::tick(&r.engine, 1_060), ["bad"]);
    assert_eq!(h.hits(), 2);
}

// ---- over the control socket ----------------------------------------------------------------

fn p(pairs: &[(&str, Value)]) -> Map<String, Value> {
    pairs.iter().map(|(k, v)| (k.to_string(), v.clone())).collect()
}

#[test]
fn socket_actions_and_scheduler_driven_ics_updated() {
    let r = Rig::with_config("[config]\nhot_reload = false\n");
    let rt = r.dir.path().join("rt");
    let handle = server::serve_with_poll(Arc::clone(&r.engine), &rt, Duration::from_millis(40)).unwrap();
    let mut sub = Client::connect(&rt).unwrap();
    assert_eq!(sub.call("subscribe", p(&[("topics", json!(["ics-updated"]))])).unwrap()["ok"], true);
    let mut c = Client::connect(&rt).unwrap();
    let add = c
        .call("ics-add", p(&[("name", json!("auto")), ("url", json!(file_url("lf-only.ics"))), ("refresh_secs", json!(1))]))
        .unwrap();
    assert_eq!(add["ok"], true, "{add}");
    // No ics-refresh call: the scheduler thread fetches it.
    let (tx, rx) = std::sync::mpsc::channel();
    thread::spawn(move || {
        tx.send(sub.next_event()).ok();
    });
    let ev = rx.recv_timeout(Duration::from_secs(10)).expect("scheduled refresh").unwrap();
    assert_eq!(ev["event"], "ics-updated");
    assert_eq!(ev["data"]["feed"], "auto");
    let listed = c.call("ics-list", Map::new()).unwrap();
    assert_eq!(listed["data"]["feeds"][0]["cached"], true);
    let evs = c.call("ics-events", p(&[("from", json!("2026-08-01")), ("to", json!("2026-08-02"))])).unwrap();
    assert_eq!(evs["data"]["events"][0]["summary"], "LF line endings");
    let bad = c.call("ics-refresh", p(&[("name", json!("missing"))])).unwrap();
    assert_eq!(bad["error"]["code"], "feed_not_found");
    let rm = c.call("ics-remove", p(&[("name", json!("auto"))])).unwrap();
    assert_eq!(rm["ok"], true);
    // An unknown topic is still refused while ics-updated is accepted.
    let mut c2 = Client::connect(&rt).unwrap();
    assert_eq!(c2.call("subscribe", p(&[("topics", json!(["ics-nope"]))])).unwrap()["ok"], false);
    drop(handle);
}
