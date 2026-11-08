//! The command calendar source (dot-4uzgol.105): a tiny fixture script run through the source end
//! to end (JSON and ICS output, env and cwd, a recording titled from it, dedupe against another
//! source), and every way a run fails (nonzero exit, timeout, too much output, unreadable output)
//! as a source error that keeps the last good events. Synthetic events only.
#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::{SecondsFormat, Utc};
use oats_audio::fake::FakeBackend;
use oats_core::action::Action;
use oats_core::control::engine::{Engine, EngineOpts};
use serde_json::{json, Value};

struct Rig {
    dir: tempfile::TempDir,
    engine: Arc<Engine>,
}

fn rig() -> Rig {
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
    engine.set_audio_factory(Box::new(|_| Ok(Box::new(FakeBackend::default()))));
    for (k, v) in [
        ("audio.transcribe", json!("off")),
        ("ui.tz", json!("UTC")),
        ("ics.default_tz", json!("UTC")),
        ("calendar.sources", json!("[]")),
    ] {
        engine.set(k, &v, "test").unwrap();
    }
    Rig { dir, engine }
}

fn run(r: &Rig, a: Value) -> oats_core::config::Result<Value> {
    let a: Action = serde_json::from_value(a).unwrap();
    r.engine.dispatch(&a)
}

fn ok(r: &Rig, a: Value) -> Value {
    run(r, a.clone()).unwrap_or_else(|e| panic!("{a}: {e}"))
}

/// An executable `sh` script in the rig's folder.
fn script(r: &Rig, name: &str, body: &str) -> PathBuf {
    let p = r.dir.path().join(name);
    std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    p
}

fn path(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

fn at(mins: i64) -> String {
    (Utc::now() + chrono::Duration::minutes(mins)).to_rfc3339_opts(SecondsFormat::Secs, true)
}

/// What a calendar CLI printing Google Calendar API events would print: one meeting now, one
/// cancelled, one tomorrow.
fn google_json() -> String {
    json!([
        {"id": "g-now", "iCalUID": "now@example.com", "summary": "Design review",
         "start": {"dateTime": at(-5)}, "end": {"dateTime": at(25)}, "status": "confirmed",
         "location": "Room 4", "attendees": [{"email": "ana@example.com"}, {"email": "bo@example.com"}]},
        {"id": "g-gone", "iCalUID": "gone@example.com", "summary": "Cancelled sync",
         "start": {"dateTime": at(-5)}, "end": {"dateTime": at(25)}, "status": "cancelled"},
        {"id": "g-later", "iCalUID": "later@example.com", "summary": "Tomorrow",
         "start": {"dateTime": at(24 * 60)}, "end": {"dateTime": at(24 * 60 + 30)}}
    ])
    .to_string()
}

fn events(r: &Rig) -> Value {
    ok(
        r,
        json!({"action": "calendar-events", "from": at(-60), "to": at(48 * 60)}),
    )
}

fn titles(v: &Value) -> Vec<String> {
    v["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["title"].as_str().unwrap().to_string())
        .collect()
}

fn listed(r: &Rig, name: &str) -> Value {
    ok(r, json!({"action": "calendar-sources-list"}))["sources"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["name"] == name)
        .cloned()
        .unwrap()
}

#[test]
fn a_script_printing_google_events_titles_the_recording() {
    let r = rig();
    std::fs::write(r.dir.path().join("events.json"), google_json()).unwrap();
    let feed = script(&r, "feed.sh", r#"cat "$EVENTS_FILE""#);
    let v = ok(
        &r,
        json!({"action": "calendar-source-add", "name": "mine", "kind": "command",
               "argv": [path(&feed)], "env": {"EVENTS_FILE": path(&r.dir.path().join("events.json"))}}),
    );
    assert_eq!(v["source"]["kind"], "command");
    assert_eq!(v["source"]["argv"][0], path(&feed));
    // Not run yet: the list says so and runs nothing.
    let s = listed(&r, "mine");
    assert!(s["last_run"].is_null(), "{s}");
    assert_eq!(s["accounts"][0]["calendars"][0]["title"], "mine");

    let v = events(&r);
    assert_eq!(titles(&v), ["Design review", "Tomorrow"], "{v}");
    assert_eq!(v["errors"], json!([]));
    let e = &v["events"][0];
    assert_eq!(e["source"], "mine");
    assert_eq!(e["kind"], "command");
    assert_eq!(e["uid"], "now@example.com");

    let s = listed(&r, "mine");
    assert_eq!(s["last_run"]["ok"], true, "{s}");
    assert!(s.get("error").is_none());

    let m = ok(&r, json!({"action": "calendar-match"}));
    assert_eq!(m["title"], "Design review", "{m}");
    let v = ok(&r, json!({"action": "record-start", "session": "s1"}));
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(v["title"], "Design review");
    ok(&r, json!({"action": "record-stop"}));
    let m = ok(&r, json!({"action": "session-get", "session": "s1"}))["metadata"].clone();
    assert_eq!(m["title"], "Design review");
    assert_eq!(m["calendarEvent"]["source"], "mine");
    assert_eq!(m["calendarEvent"]["kind"], "command");
    assert_eq!(m["calendarEvent"]["location"], "Room 4");

    // calendar-connect is a test run.
    let c = ok(&r, json!({"action": "calendar-connect", "source": "mine"}));
    assert_eq!(c["events"], 2, "{c}");
}

#[test]
fn ics_output_cwd_and_dedupe_with_another_source() {
    let r = rig();
    let now = Utc::now();
    let stamp = |m: i64| {
        (now + chrono::Duration::minutes(m))
            .format("%Y%m%dT%H%M%SZ")
            .to_string()
    };
    let ics = format!(
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//test//EN\r\nBEGIN:VEVENT\r\nUID:shared@example.com\r\nDTSTAMP:{s}\r\nDTSTART:{s}\r\nDTEND:{e}\r\nSUMMARY:Shared meeting\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n",
        s = stamp(10),
        e = stamp(40)
    );
    std::fs::write(r.dir.path().join("cal.ics"), ics).unwrap();
    let start = (now + chrono::Duration::minutes(10))
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string();
    std::fs::write(
        r.dir.path().join("plain.json"),
        json!({"events": [{"title": "Shared meeting (copy)", "start": start, "uid": "SHARED@example.com"},
                          {"title": "Only here", "start": at(60), "end": at(90)}]})
        .to_string(),
    )
    .unwrap();
    // `cat` reads relative to cwd: no shell, no script.
    ok(
        &r,
        json!({"action": "calendar-source-add", "name": "ics-cmd", "kind": "command",
               "argv": ["cat", "cal.ics"], "cwd": path(r.dir.path()), "format": "ics"}),
    );
    ok(
        &r,
        json!({"action": "calendar-source-add", "name": "json-cmd", "kind": "command",
               "argv": "cat plain.json", "cwd": path(r.dir.path())}),
    );
    let v = events(&r);
    assert_eq!(titles(&v), ["Shared meeting", "Only here"], "{v}");
    assert_eq!(v["events"][0]["source"], "ics-cmd");
}

#[test]
fn a_failing_run_is_a_source_error_and_the_last_good_events_stay() {
    let r = rig();
    let good = r.dir.path().join("events.json");
    std::fs::write(&good, google_json()).unwrap();
    let mode = r.dir.path().join("mode");
    std::fs::write(&mode, "ok").unwrap();
    let feed = script(
        &r,
        "flaky.sh",
        &format!(
            r#"case "$(cat {m})" in
  ok) cat {g} ;;
  fail) echo "calendar API said no" >&2; exit 3 ;;
  garbage) echo "<html>login</html>" ;;
  slow) sleep 30 ;;
esac"#,
            m = path(&mode),
            g = path(&good)
        ),
    );
    ok(
        &r,
        json!({"action": "calendar-source-add", "name": "flaky", "kind": "command",
               "argv": [path(&feed)], "timeout": "1s"}),
    );
    assert_eq!(titles(&events(&r)), ["Design review", "Tomorrow"]);

    for (m, code) in [
        ("fail", "command_failed"),
        ("garbage", "command_output"),
        ("slow", "command_timeout"),
    ] {
        std::fs::write(&mode, m).unwrap();
        let t0 = Instant::now();
        let v = events(&r);
        assert!(
            t0.elapsed() < Duration::from_secs(10),
            "{m} took {:?}",
            t0.elapsed()
        );
        assert_eq!(titles(&v), ["Design review", "Tomorrow"], "{m}: {v}");
        assert_eq!(v["errors"][0]["source"], "flaky");
        assert_eq!(v["errors"][0]["error"]["code"], code, "{m}: {v}");
        assert_eq!(v["errors"][0]["kept_last_good"], true);
        let s = listed(&r, "flaky");
        assert_eq!(s["error"]["code"], code, "{s}");
        assert_eq!(s["last_run"]["ok"], false);
        assert_eq!(s["last_run"]["kept_last_good"], true);
        if m == "fail" {
            let msg = s["error"]["message"].as_str().unwrap();
            assert!(
                msg.contains("code 3") && msg.contains("calendar API said no"),
                "{msg}"
            );
        }
    }

    // Back to good: the error clears.
    std::fs::write(&mode, "ok").unwrap();
    assert_eq!(events(&r)["errors"], json!([]));
    assert!(listed(&r, "flaky").get("error").is_none());
}

#[test]
fn without_a_good_run_a_failure_shows_and_nothing_crashes() {
    let r = rig();
    let fail = script(&r, "fail.sh", "echo boom >&2; exit 1");
    ok(
        &r,
        json!({"action": "calendar-source-add", "name": "bad", "kind": "command",
               "argv": [path(&fail)]}),
    );
    ok(
        &r,
        json!({"action": "calendar-source-add", "name": "missing", "kind": "command",
               "argv": ["/nonexistent/oats-calendar-tool"]}),
    );
    let v = events(&r);
    assert_eq!(v["events"], json!([]));
    let codes: Vec<(&str, &str)> = v["errors"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| {
            (
                e["source"].as_str().unwrap(),
                e["error"]["code"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        codes,
        [("bad", "command_failed"), ("missing", "command_failed")]
    );
    assert_eq!(listed(&r, "bad")["error"]["code"], "command_failed");
    let e = run(&r, json!({"action": "calendar-connect", "source": "bad"})).unwrap_err();
    assert_eq!(e.code, "command_failed");
    // A recording still starts, titled by the fallback.
    let v = ok(&r, json!({"action": "record-start", "session": "s1"}));
    std::thread::sleep(Duration::from_millis(50));
    assert!(v["title"].is_string());
    ok(&r, json!({"action": "record-stop"}));
}

#[test]
fn output_past_the_cap_is_refused() {
    let r = rig();
    r.engine
        .set("calendar.command_max_bytes", &json!(1024), "test")
        .unwrap();
    let big = script(
        &r,
        "big.sh",
        "i=0; while [ $i -lt 400 ]; do echo '[{}, {}, {}]'; i=$((i+1)); done",
    );
    ok(
        &r,
        json!({"action": "calendar-source-add", "name": "big", "kind": "command",
               "argv": [path(&big)]}),
    );
    let v = events(&r);
    assert_eq!(v["errors"][0]["error"]["code"], "command_output", "{v}");
}

#[test]
fn fields_are_checked_and_a_source_can_be_edited() {
    let r = rig();
    for (a, why) in [
        (
            json!({"action": "calendar-source-add", "name": "c", "kind": "command"}),
            "needs argv",
        ),
        (
            json!({"action": "calendar-source-add", "name": "c", "kind": "command", "argv": ["x"], "format": "xml"}),
            "format",
        ),
        (
            json!({"action": "calendar-source-add", "name": "c", "kind": "command", "argv": ["x"], "timeout": "2h"}),
            "timeout",
        ),
        (
            json!({"action": "calendar-source-add", "name": "c", "kind": "ics", "argv": ["x"]}),
            "argv is for command",
        ),
    ] {
        let e = run(&r, a.clone()).unwrap_err();
        assert_eq!(e.code, "invalid_argument", "{a}");
        assert!(e.message.contains(why), "{a}: {}", e.message);
    }
    let e = r
        .engine
        .set(
            "calendar.sources",
            &json!(r#"[{"name":"c","kind":"command","argv":[]}]"#),
            "test",
        )
        .unwrap_err();
    assert_eq!(e.code, "invalid_value");

    ok(
        &r,
        json!({"action": "calendar-source-add", "name": "c", "kind": "command",
               "argv": ["my-calendar", "--json"], "env": ["TOKEN_FILE=/tmp/t", "DAYS=2"], "timeout": "5s"}),
    );
    let v = ok(
        &r,
        json!({"action": "calendar-source-edit", "name": "c", "argv": ["other-tool", "--ics"],
               "format": "ics", "timeout": "", "rename": "d"}),
    );
    assert_eq!(v["source"]["name"], "d");
    assert_eq!(v["source"]["argv"], json!(["other-tool", "--ics"]));
    assert_eq!(v["source"]["format"], "ics");
    assert!(v["source"].get("timeout").is_none());
    assert_eq!(
        v["source"]["env"],
        json!({"DAYS": "2", "TOKEN_FILE": "/tmp/t"})
    );
    let saved: Value = serde_json::from_str(&r.engine.snapshot().config.calendar.sources).unwrap();
    assert_eq!(saved[0]["name"], "d");
    assert_eq!(saved[0]["argv"][0], "other-tool");
    let e = run(
        &r,
        json!({"action": "calendar-source-edit", "name": "d", "argv": []}),
    )
    .unwrap_err();
    assert_eq!(e.code, "invalid_argument");
    let kinds = ok(&r, json!({"action": "calendar-sources-list"}))["kinds"].clone();
    assert!(kinds.as_array().unwrap().contains(&json!("command")));
}
