//! dot-4uzgol.111.7 (docs/design/hooks.md): `record.before_start` / `record.before_stop` fire
//! before the recorder acts, with `data.reason`; a `blocking = true` hook holds the action up to
//! its timeout and then the action goes on whatever happened (fail open on a timeout, a crash, a
//! missing program, a non-zero exit); blocking hooks run concurrently; `blocking` anywhere but a
//! `*.before_*` event is refused. `calendar.ending` / `calendar.ended` fire on a calendar-matched
//! recording, each once (the timer itself is tested with a fake clock in `hooks::ending`).
//! Hooks here are `sh` scripts, so this suite is unix-only.
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use oats_audio::fake::FakeBackend;
use oats_core::action::Action;
use oats_core::control::engine::{Engine, EngineOpts};
use oats_core::hooks;
use serde_json::{json, Value};

struct Rig {
    _dir: tempfile::TempDir,
    root: PathBuf,
    out: PathBuf,
    engine: Arc<Engine>,
}

/// An engine whose recordings write to `<root>/rec` with no transcription, and `toml` (hooks
/// whose `{out}` is replaced by the folder they write to) in its config.
fn rig(toml: &str) -> Rig {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let cfg = root.join("config.toml");
    let out = root.join("out");
    std::fs::create_dir_all(&out).unwrap();
    let rec = root.join("rec");
    std::fs::write(
        &cfg,
        format!(
            "[sessions]\nroot = {:?}\n\n[audio]\ntranscribe = \"off\"\n\n[record]\nauto_check = \"20ms\"\n\n{}",
            rec.to_str().unwrap(),
            toml.replace("{out}", out.to_str().unwrap())
        ),
    )
    .unwrap();
    let engine = Engine::new(EngineOpts {
        user_config: cfg,
        pack: None,
        os: "linux".into(),
        arch: "x86_64".into(),
        machine: "default".into(),
        sessions_dir: root.join("sessions"),
        secrets: None,
    })
    .unwrap();
    engine.set_audio_factory(Box::new(|_| Ok(Box::new(FakeBackend::default()))));
    Rig {
        _dir: dir,
        root,
        out,
        engine,
    }
}

fn wait_until(what: &str, mut cond: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while !cond() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn start(r: &Rig, id: &str) -> Value {
    r.engine
        .dispatch(&Action::RecordStart {
            session: Some(id.into()),
            title: Some("Weekly".into()),
        })
        .unwrap()
}

fn stop(r: &Rig) -> Value {
    r.engine.dispatch(&Action::RecordStop { by: None }).unwrap()
}

/// The envelope a hook wrote with `cat > {out}/<name>.json`.
fn envelope(out: &Path, name: &str) -> Value {
    let p = out.join(format!("{name}.json"));
    wait_until(&format!("hook {name}"), || {
        std::fs::read_to_string(&p)
            .ok()
            .is_some_and(|s| serde_json::from_str::<Value>(&s).is_ok())
    });
    serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap()
}

/// A shell hook that saves its envelope as `{out}/<name>.json` (whole, by rename) and then runs
/// `then`; `extra` is more of its table (last, so it may open `[hooks.match]`).
fn spy(name: &str, event: &str, extra: &str, then: &str) -> String {
    format!(
        "[[hooks]]\nname = \"{name}\"\nevent = \"{event}\"\nshell = true\ncommand = [\"cat > {{out}}/{name}.tmp; mv {{out}}/{name}.tmp {{out}}/{name}.json; {then}\"]\n{extra}\n\n"
    )
}

#[test]
fn before_start_and_before_stop_fire_before_the_recorder_acts_with_the_reason() {
    let r = rig(&format!(
        "{}{}",
        spy("pre", "record.before_start", "", "true"),
        spy("pre-stop", "record.before_stop", "", "true"),
    ));
    let events = r.engine.bus.subscribe(vec!["recording-started".into()]);
    start(&r, "s1");
    let pre = envelope(&r.out, "pre");
    let spec = hooks::events::find("record.before_start").unwrap();
    hooks::events::validate(&hooks::events::envelope_schema(Some(spec)), &pre)
        .unwrap_or_else(|e| panic!("{e}\n{pre}"));
    assert_eq!(pre["data"]["reason"], "manual", "{pre}");
    assert_eq!(pre["data"]["requested"], "s1", "{pre}");
    assert_eq!(pre["session"]["title"], "Weekly", "{pre}");
    // No session yet: the envelope names none.
    assert_eq!(pre["session"]["id"], Value::Null, "{pre}");
    assert!(events.recv_timeout(Duration::from_secs(10)).is_ok());
    let stopped = stop(&r);
    // A hook that does not block is not waited for, so nothing is reported.
    assert!(stopped.get("before_hooks").is_none(), "{stopped}");
    let pre = envelope(&r.out, "pre-stop");
    let spec = hooks::events::find("record.before_stop").unwrap();
    hooks::events::validate(&hooks::events::envelope_schema(Some(spec)), &pre)
        .unwrap_or_else(|e| panic!("{e}\n{pre}"));
    assert_eq!(pre["data"]["reason"], "manual");
    assert_eq!(pre["data"]["by"], "api");
    assert_eq!(pre["session"]["id"], "s1");
    // The raw log of the session holds before_stop under its event name, before the stop.
    let log = r
        .engine
        .dispatch(&Action::SessionEvents {
            session: Some("s1".into()),
            topic: None,
            from: None,
            to: None,
        })
        .unwrap();
    let topics: Vec<&str> = log["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["topic"].as_str().unwrap())
        .collect();
    let at = |t: &str| topics.iter().position(|x| *x == t).unwrap();
    assert!(
        at("record.before_stop") < at("recording-stopped"),
        "{topics:?}"
    );
    assert_eq!(log["events"][0]["seq"], 1, "the log starts at 1: {log}");
}

#[test]
fn a_blocking_hook_holds_the_start_until_it_ends() {
    let r = rig(&spy(
        "slow-ok",
        "record.before_start",
        "blocking = true\ntimeout = \"10s\"",
        "sleep 0.4; touch {out}/done",
    ));
    let t = Instant::now();
    let v = start(&r, "held");
    assert!(r.out.join("done").exists(), "the start waited for the hook");
    assert!(t.elapsed() < Duration::from_secs(8), "{:?}", t.elapsed());
    let w = &v["before_hooks"][0];
    assert_eq!(w["hook"], "slow-ok", "{v}");
    assert_eq!(w["finished"], true, "{v}");
    assert_eq!(w["ok"], true, "{v}");
    assert_eq!(w["exit_code"], 0, "{v}");
    assert!(w["waited_ms"].as_u64().unwrap() >= 400, "{v}");
    assert_eq!(v["session"], "held");
    stop(&r);
}

#[test]
fn a_blocking_hook_past_its_timeout_fails_open() {
    let r = rig(&spy(
        "hang",
        "record.before_start",
        "blocking = true\ntimeout = \"300ms\"",
        "sleep 30",
    ));
    let t = Instant::now();
    let v = start(&r, "open");
    assert!(t.elapsed() < Duration::from_secs(5), "{:?}", t.elapsed());
    assert_eq!(v["session"], "open", "the recording started anyway: {v}");
    let w = &v["before_hooks"][0];
    assert_eq!(w["ok"], false, "{v}");
    assert!(
        w["timed_out"] == true || w["finished"] == false,
        "a timeout: {v}"
    );
    assert!(w["waited_ms"].as_u64().unwrap() >= 300, "{v}");
    assert_eq!(stop(&r)["session"], "open");
}

#[test]
fn a_blocking_hook_that_crashes_fails_or_exits_non_zero_never_stops_the_action() {
    let r = rig(&format!(
        "{}{}{}",
        spy(
            "crash",
            "record.before_stop",
            "blocking = true\ntimeout = \"5s\"",
            "kill -9 $$"
        ),
        spy(
            "refuse",
            "record.before_stop",
            "blocking = true\ntimeout = \"5s\"",
            "exit 3"
        ),
        "[[hooks]]\nname = \"missing\"\nevent = \"record.before_stop\"\nblocking = true\ncommand = [\"/nonexistent/oats-hook\"]\n\n",
    ));
    start(&r, "kept");
    std::thread::sleep(Duration::from_millis(300));
    let t = Instant::now();
    let v = stop(&r);
    assert!(t.elapsed() < Duration::from_secs(4), "{:?}", t.elapsed());
    assert_eq!(v["session"], "kept", "the stop went on: {v}");
    let by = |n: &str| {
        v["before_hooks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|w| w["hook"] == n)
            .cloned()
            .unwrap_or_else(|| panic!("{n}: {v}"))
    };
    let crash = by("crash");
    assert_eq!(crash["ok"], false, "{crash}");
    assert!(
        crash["error"].as_str().unwrap().contains("signal 9"),
        "{crash}"
    );
    let refuse = by("refuse");
    // A non-zero exit is logged and returned, never enforced.
    assert_eq!(refuse["exit_code"], 3, "{refuse}");
    assert_eq!(refuse["finished"], true);
    let missing = by("missing");
    assert!(
        missing["error"].as_str().unwrap().contains("cannot run"),
        "{missing}"
    );
    let s = r
        .engine
        .dispatch(&Action::SessionGet {
            session: "kept".into(),
        })
        .unwrap();
    assert_eq!(s["state"], "saved", "{s}");
    // The recorder is free: another recording starts.
    start(&r, "next");
    stop(&r);
}

#[test]
fn blocking_hooks_run_concurrently_and_the_wait_is_the_longest() {
    let hook = |n: &str| {
        spy(
            n,
            "record.before_start",
            "blocking = true\ntimeout = \"10s\"",
            "sleep 0.8",
        )
    };
    let r = rig(&format!("{}{}{}", hook("a"), hook("b"), hook("c")));
    let t = Instant::now();
    let v = start(&r, "together");
    let took = t.elapsed();
    assert_eq!(v["before_hooks"].as_array().unwrap().len(), 3, "{v}");
    assert!(took >= Duration::from_millis(800), "{took:?}");
    // One after another would be 2.4s.
    assert!(took < Duration::from_millis(2000), "{took:?}");
    stop(&r);
}

#[test]
fn blocking_is_refused_on_any_event_but_a_before_one() {
    let r = rig("");
    for event in hooks::names() {
        let add = r.engine.dispatch(&Action::HookAdd {
            name: format!("b-{event}"),
            event: event.to_string(),
            command: json!(["true"]),
            shell: None,
            timeout: None,
            blocking: Some(true),
            enabled: None,
            cwd: None,
            r#match: (event == "transcript.phrase").then(|| json!({"phrases": ["x"]})),
        });
        if hooks::events::may_block(event) {
            add.unwrap_or_else(|e| panic!("{event}: {e}"));
        } else {
            let e = add.unwrap_err();
            assert_eq!(e.code, "invalid_value", "{event}: {e}");
            assert!(
                e.message.contains(&format!("hook b-{event}: blocking = true only on record.before_start or record.before_stop")),
                "{event}: {e}"
            );
        }
    }
    assert_eq!(
        hooks::events::blocking_events(),
        ["record.before_start", "record.before_stop"]
    );
    // The config file is checked the same way.
    let bad =
        hooks::parse(&json!([{"event": "record.stop", "command": ["true"], "blocking": true}]));
    assert!(bad.unwrap_err().message.contains("blocking"));
    let ok = hooks::parse(
        &json!([{"event": "record.before_stop", "command": ["true"], "blocking": true}]),
    )
    .unwrap();
    assert!(ok[0].blocking);
    // before_end is checked against app.hook_before_end's range.
    let e = hooks::parse(&json!([{"event": "calendar.ending", "command": ["true"],
                                  "match": {"before_end": "2h"}}]))
    .unwrap_err();
    assert!(e.message.contains("match.before_end must be from"), "{e}");
}

/// Write a calendar event ending `in_ms` from now into the recording's session.json, as a
/// calendar match at start would.
fn match_calendar(r: &Rig, id: &str, in_ms: i64) {
    let p = r.root.join("rec").join(id).join("session.json");
    wait_until("session.json", || p.exists());
    let mut meta: Value = serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
    let end = chrono::Utc::now() + chrono::Duration::milliseconds(in_ms);
    meta["calendarEvent"] = json!({
        "id": "ev-1", "source": "work", "title": "Weekly",
        "start": (end - chrono::Duration::minutes(30)).to_rfc3339(),
        "end": end.to_rfc3339(),
    });
    std::fs::write(&p, meta.to_string()).unwrap();
}

#[test]
fn calendar_ending_and_ended_fire_once_each_on_a_matched_recording() {
    let r = rig(&format!(
        "{}{}{}",
        spy(
            "soon",
            "calendar.ending",
            "[hooks.match]\nbefore_end = \"1s\"",
            "true"
        ),
        spy(
            "late",
            "calendar.ending",
            "[hooks.match]\nbefore_end = \"5m\"",
            "true"
        ),
        spy("over", "calendar.ended", "", "true"),
    ));
    let decided = r.engine.bus.subscribe(vec!["hook-decided".into()]);
    start(&r, "cal");
    match_calendar(&r, "cal", 1800);
    // The 5m lead is due at once, the 1s one about 0.8s later.
    let late = envelope(&r.out, "late");
    let soon = envelope(&r.out, "soon");
    for e in [&late, &soon] {
        let spec = hooks::events::find("calendar.ending").unwrap();
        hooks::events::validate(&hooks::events::envelope_schema(Some(spec)), e)
            .unwrap_or_else(|x| panic!("{x}\n{e}"));
        assert_eq!(e["calendar"]["title"], "Weekly", "{e}");
        assert_eq!(e["dedup"], "cal:calendar.ending:ev-1", "{e}");
    }
    assert!(late["data"]["remaining_ms"].as_u64().unwrap() <= 1800);
    assert!(
        soon["data"]["remaining_ms"].as_u64().unwrap() <= 1000,
        "{soon}"
    );
    let over = envelope(&r.out, "over");
    let spec = hooks::events::find("calendar.ended").unwrap();
    hooks::events::validate(&hooks::events::envelope_schema(Some(spec)), &over)
        .unwrap_or_else(|x| panic!("{x}\n{over}"));
    assert_eq!(over["dedup"], "cal:calendar.ended:ev-1");
    assert!(over["data"]["overrun_ms"].is_u64(), "{over}");
    // The recording keeps going past the end: nothing fires again.
    std::thread::sleep(Duration::from_millis(600));
    stop(&r);
    let mut fired: Vec<String> = vec![];
    while let Ok(line) = decided.recv_timeout(Duration::from_millis(300)) {
        let d = serde_json::from_str::<Value>(&line).unwrap()["data"].clone();
        if d["fire"] == true {
            fired.push(d["hook"].as_str().unwrap().to_string());
        }
    }
    fired.sort();
    assert_eq!(fired, ["late", "over", "soon"], "each hook fired once");
}

#[test]
fn an_unmatched_recording_says_nothing_about_the_calendar() {
    let r = rig(&format!(
        "{}{}",
        spy("soon", "calendar.ending", "", "true"),
        spy("over", "calendar.ended", "", "true"),
    ));
    start(&r, "plain");
    std::thread::sleep(Duration::from_millis(500));
    stop(&r);
    std::thread::sleep(Duration::from_millis(200));
    assert!(!r.out.join("soon.json").exists());
    assert!(!r.out.join("over.json").exists());
}

#[test]
fn before_end_explains_itself() {
    let h: hooks::Hook = serde_json::from_value(json!({"name": "h", "event": "calendar.ending",
        "command": ["true"], "match": {"before_end": "2m"}}))
    .unwrap();
    let env = |left: u64| {
        json!({"event": "calendar.ending", "session": {"title": "Weekly"}, "calendar": null,
               "data": {"remaining_ms": left}})
    };
    let v = hooks::matches(&h, &env(119_000));
    assert!(v.fire, "{v:?}");
    assert_eq!(
        v.reason,
        "match.before_end = 2m: 1m59s left before the scheduled end"
    );
    let v = hooks::matches(&h, &env(299_000));
    assert!(!v.fire);
    assert_eq!(
        v.reason,
        "match.before_end = 2m: 4m59s left before the scheduled end"
    );
}
