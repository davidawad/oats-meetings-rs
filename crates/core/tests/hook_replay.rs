//! dot-4uzgol.111.8 (docs/design/hooks.md L4, L5): replay and the ledger. Recorded raw logs in
//! `tests/fixtures/hook-replay/` (a calendar-matched meeting with phrases, a manual recording
//! that auto-stops, a deferred-transcription session) are fed through `hooks::replay::simulate`,
//! asserting the exact ordered verdict list with its reasons, and every fire's envelope holds to
//! its schema. Then the engine: a recording writes its decisions and runs to
//! `<session>/hook-runs.jsonl`, `hook-runs` filters it, `hook-simulate` replays a session (or a
//! fixture file) without running anything, and with `run = true` runs the fires and ledgers them
//! tagged simulated.

use std::path::PathBuf;

use oats_core::config::typed::Dur;
use oats_core::hooks::replay::{self, Base, Opts, Step};
use oats_core::hooks::{events, Hook};
use serde_json::{json, Value};

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/hook-replay")
        .join(name)
}

fn lines(name: &str) -> Vec<Value> {
    let (lines, skipped) = replay::parse_log(&std::fs::read_to_string(fixture(name)).unwrap());
    assert_eq!(skipped, 0, "{name}");
    lines
}

fn hooks(list: Value) -> Vec<(String, Hook)> {
    list.as_array()
        .unwrap()
        .iter()
        .map(|v| {
            let mut v = v.clone();
            v["command"] = json!(["true"]);
            let h: Hook = serde_json::from_value(v).unwrap();
            (h.name.clone(), h)
        })
        .collect()
}

const OPTS: Opts = Opts {
    cooldown: Dur(30_000),
    before_end: Dur(300_000),
};

/// (seq, event, hook, fire, reason) per verdict.
fn verdicts(steps: &[Step]) -> Vec<(u64, String, String, bool, String)> {
    steps
        .iter()
        .map(|s| {
            (
                s.seq,
                s.event.clone(),
                s.hook.clone(),
                s.fire,
                s.reason.clone(),
            )
        })
        .collect()
}

fn v(
    seq: u64,
    event: &str,
    hook: &str,
    fire: bool,
    reason: &str,
) -> (u64, String, String, bool, String) {
    (seq, event.into(), hook.into(), fire, reason.into())
}

/// Every fire carries an envelope that holds to its event's schema, names its hook and has the
/// step's seq and dedup; a skip carries none.
fn check_envelopes(steps: &[Step]) {
    for s in steps {
        match (&s.envelope, s.fire) {
            (Some(env), true) => {
                let spec = events::find(&s.event).unwrap();
                events::validate(&events::envelope_schema(Some(spec)), env)
                    .unwrap_or_else(|e| panic!("{} {}: {e}: {env}", s.seq, s.hook));
                assert_eq!(env["hook"], s.hook);
                assert_eq!(env["seq"], s.seq);
                assert_eq!(env["dedup"], s.dedup);
                assert_eq!(env["run_id"], replay::run_id(s.seq, &s.hook));
            }
            (None, false) => {}
            (env, fire) => panic!("{} {}: fire {fire} with envelope {env:?}", s.seq, s.hook),
        }
    }
}

fn standup_hooks() -> Vec<(String, Hook)> {
    hooks(json!([
        {"name": "matched", "event": "calendar.matched"},
        {"name": "pricing", "event": "transcript.phrase",
         "match": {"phrases": ["pricing"], "speaker": "remote", "cooldown": "2m"}},
        {"name": "five", "event": "calendar.ending"},
        {"name": "wrap", "event": "calendar.ending", "match": {"before_end": "2m"}},
        {"name": "over", "event": "calendar.ended"},
        {"name": "standup-sync", "event": "record.stop", "match": {"title": "Standup*"}},
        {"name": "acme-only", "event": "record.stop", "match": {"attendees": ["*@acme.com"]}},
        {"name": "off", "event": "record.stop", "enabled": false},
        {"name": "final", "event": "transcript.final"},
        {"name": "saved", "event": "session.saved", "match": {"calendar": false}},
    ]))
}

#[test]
fn a_calendar_matched_meeting_with_phrases_replays_to_the_exact_fire_list() {
    let lines = lines("calendar-standup.events.jsonl");
    let base = Base::from_log(&lines);
    assert_eq!(base.session["id"], "2026-11-13-standup");
    assert_eq!(base.session["title"], "Standup");
    assert_eq!(base.calendar["attendees"], json!(["ana@acme.com", "Bo"]));
    let steps = replay::simulate(&standup_hooks(), &lines, &base, OPTS);
    assert_eq!(
        verdicts(&steps),
        [
            v(1, "calendar.matched", "matched", true, "no match filter: fires on every calendar.matched"),
            v(3, "transcript.phrase", "pricing", true, r#"match.speaker = remote: "remote_1" is remote; match.phrases: "pricing" said in "Can we talk about pricing?""#),
            v(5, "transcript.phrase", "pricing", false, r#"match.speaker = remote: "you" is you"#),
            v(6, "transcript.phrase", "pricing", false, r#"match.cooldown: fired at 1:01 (recording time), within 2m of 1:30 (match.speaker = remote: "remote_1" is remote; match.phrases: "pricing" said in "So the pricing, then.")"#),
            v(7, "calendar.ending", "five", true, "no match filter: fires on every calendar.ending"),
            v(8, "calendar.ending", "wrap", true, "match.before_end = 2m: 1m59s left before the scheduled end"),
            v(9, "calendar.ended", "over", true, "no match filter: fires on every calendar.ended"),
            v(10, "transcript.phrase", "pricing", true, r#"match.speaker = remote: "remote_1" is remote; match.phrases: "pricing" said in "Pricing is final.""#),
            v(11, "transcript.phrase", "pricing", false, r#"already fired for 2026-11-13-standup:transcript.phrase:remote_1@61200:pricing (match.speaker = remote: "remote_2" is remote; match.phrases: "pricing" said in "Can we talk about pricing")"#),
            v(12, "record.stop", "standup-sync", true, r#"match.title: "Standup*" matched "Standup""#),
            v(12, "record.stop", "acme-only", true, r#"match.attendees: "*@acme.com" matched "ana@acme.com""#),
            v(12, "record.stop", "off", false, "the hook is off (enabled = false)"),
            v(12, "transcript.final", "final", true, "no match filter: fires on every transcript.final"),
            v(12, "calendar.matched", "matched", false, "already fired for 2026-11-13-standup:calendar.matched:e1 (no match filter: fires on every calendar.matched)"),
            v(13, "session.saved", "saved", false, r#"match.calendar = false: the session matched calendar event "Standup""#),
        ]
    );
    check_envelopes(&steps);
    let dedups: Vec<&str> = steps
        .iter()
        .filter(|s| s.fire)
        .map(|s| s.dedup.as_str())
        .collect();
    assert_eq!(
        dedups,
        [
            "2026-11-13-standup:calendar.matched:e1",
            "2026-11-13-standup:transcript.phrase:remote_1@61200:pricing",
            "2026-11-13-standup:calendar.ending:e1",
            "2026-11-13-standup:calendar.ending:e1",
            "2026-11-13-standup:calendar.ended:e1",
            "2026-11-13-standup:transcript.phrase:remote_1@920000:pricing",
            "2026-11-13-standup:record.stop",
            "2026-11-13-standup:record.stop",
            "2026-11-13-standup:transcript.final:record",
        ]
    );
    let phrase = steps
        .iter()
        .find(|s| s.fire && s.hook == "pricing")
        .unwrap();
    let env = phrase.envelope.as_ref().unwrap();
    assert_eq!(env["data"]["live"], true);
    assert_eq!(env["calendar"]["title"], "Standup");
    assert_eq!(env["session"]["title"], "Standup");
    // Pure: the same log replays to the same list.
    assert_eq!(
        replay::simulate(&standup_hooks(), &lines, &base, OPTS),
        steps
    );
}

#[test]
fn a_manual_recording_that_auto_stops_replays_to_the_exact_fire_list() {
    let lines = lines("manual-auto-stop.events.jsonl");
    let base = Base::from_log(&lines);
    assert_eq!(base.calendar, Value::Null);
    let hs = hooks(json!([
        {"name": "start", "event": "record.start"},
        {"name": "pause", "event": "record.pause"},
        {"name": "steps", "event": "transcript.phrase", "match": {"phrases": ["next steps"]}},
        {"name": "prep", "event": "record.before_stop", "blocking": true},
        {"name": "stop", "event": "record.stop"},
        {"name": "standup-only", "event": "record.stop", "match": {"title": "Standup*"}},
        {"name": "cal-only", "event": "record.stop", "match": {"calendar": true}},
        {"name": "auto", "event": "record.auto_stop"},
        {"name": "notes", "event": "notes.generated", "match": {"title": "1:1*"}},
    ]));
    let steps = replay::simulate(&hs, &lines, &base, OPTS);
    assert_eq!(
        verdicts(&steps),
        [
            v(
                1,
                "record.start",
                "start",
                true,
                "no match filter: fires on every record.start"
            ),
            v(
                2,
                "transcript.phrase",
                "steps",
                true,
                r#"match.phrases: "next steps" said in "Let's agree on next steps.""#
            ),
            v(
                3,
                "record.pause",
                "pause",
                true,
                "no match filter: fires on every record.pause"
            ),
            v(
                4,
                "record.pause",
                "pause",
                true,
                "no match filter: fires on every record.pause"
            ),
            v(
                5,
                "transcript.phrase",
                "steps",
                false,
                r#"match.cooldown: fired at 0:05 (recording time), within 30s of 0:20 (match.phrases: "next steps" said in "Next steps: I send the doc.")"#
            ),
            v(
                6,
                "transcript.phrase",
                "steps",
                true,
                r#"match.phrases: "next steps" said in "Next steps are clear.""#
            ),
            v(
                8,
                "record.before_stop",
                "prep",
                true,
                "no match filter: fires on every record.before_stop"
            ),
            v(
                9,
                "transcript.phrase",
                "steps",
                false,
                r#"already fired for 2026-11-12-manual:transcript.phrase:you@5000:next steps (match.phrases: "next steps" said in "Let's agree on next steps.")"#
            ),
            v(
                10,
                "record.stop",
                "stop",
                true,
                "no match filter: fires on every record.stop"
            ),
            v(
                10,
                "record.stop",
                "standup-only",
                false,
                r#"match.title: "Standup*" did not match "1:1 Bob""#
            ),
            v(
                10,
                "record.stop",
                "cal-only",
                false,
                "match.calendar = true: the session matched no calendar event"
            ),
            v(
                12,
                "record.auto_stop",
                "auto",
                true,
                "no match filter: fires on every record.auto_stop"
            ),
            v(
                14,
                "notes.generated",
                "notes",
                true,
                r#"match.title: "1:1*" matched "1:1 Bob""#
            ),
        ]
    );
    check_envelopes(&steps);
    let dedup = |seq: u64, hook: &str| {
        steps
            .iter()
            .find(|s| s.seq == seq && s.hook == hook)
            .unwrap()
            .dedup
            .clone()
    };
    assert_eq!(
        dedup(1, "start"),
        "2026-11-12-manual:record.start:2026-11-12T10:00:00Z"
    );
    assert_eq!(dedup(3, "pause"), "2026-11-12-manual:record.pause:3");
    assert_eq!(dedup(4, "pause"), "2026-11-12-manual:record.pause:4");
    assert_eq!(dedup(8, "prep"), "2026-11-12-manual:record.before_stop:8");
    assert_eq!(dedup(14, "notes"), "2026-11-12-manual:notes.generated:14");
    let prep = steps.iter().find(|s| s.hook == "prep").unwrap();
    assert_eq!(prep.envelope.as_ref().unwrap()["data"]["reason"], "auto");
}

#[test]
fn a_deferred_session_replays_its_phrases_from_the_final_pass() {
    let lines = lines("deferred.events.jsonl");
    let base = Base::from_log(&lines);
    let hs = hooks(json!([
        {"name": "before", "event": "record.before_stop"},
        {"name": "stop", "event": "record.stop"},
        {"name": "pricing", "event": "transcript.phrase", "match": {"phrases": ["pricing"]}},
        {"name": "final", "event": "transcript.final"},
        {"name": "saved", "event": "session.saved"},
    ]));
    let steps = replay::simulate(&hs, &lines, &base, OPTS);
    assert_eq!(
        verdicts(&steps),
        [
            v(
                2,
                "record.before_stop",
                "before",
                true,
                "no match filter: fires on every record.before_stop"
            ),
            // The stop's final pass is still running: no transcript.final from it.
            v(
                3,
                "record.stop",
                "stop",
                true,
                "no match filter: fires on every record.stop"
            ),
            v(
                4,
                "session.saved",
                "saved",
                true,
                "no match filter: fires on every session.saved"
            ),
            v(
                6,
                "transcript.phrase",
                "pricing",
                true,
                r#"match.phrases: "pricing" said in "What about pricing?""#
            ),
            v(
                7,
                "transcript.phrase",
                "pricing",
                true,
                r#"match.phrases: "pricing" said in "Pricing is fine.""#
            ),
            v(
                8,
                "transcript.final",
                "final",
                true,
                "no match filter: fires on every transcript.final"
            ),
            v(
                8,
                "session.saved",
                "saved",
                true,
                "no match filter: fires on every session.saved"
            ),
        ]
    );
    check_envelopes(&steps);
    for s in steps.iter().filter(|s| s.event == "transcript.phrase") {
        let env = s.envelope.as_ref().unwrap();
        assert_eq!(
            env["data"]["live"], false,
            "the final pass is no live speech"
        );
        assert_eq!(env["data"]["kind"], "final");
    }
    let fin = steps.iter().find(|s| s.hook == "final").unwrap();
    assert_eq!(fin.dedup, "2026-11-14-deferred:transcript.final:8");
}

/// A replay keeps its own dedup: the same log twice over (the second copy's seqs after the
/// first's, as a log continued after a restart) fires the once-per-session events once and the
/// spoken phrases once.
#[test]
fn dedup_holds_across_a_replay_of_the_same_log() {
    let once = lines("calendar-standup.events.jsonl");
    let n = once.len() as u64;
    let mut twice = once.clone();
    twice.extend(once.iter().map(|l| {
        let mut l = l.clone();
        l["seq"] = json!(l["seq"].as_u64().unwrap() + n);
        l
    }));
    let base = Base::from_log(&once);
    let fires = |ls: &[Value]| {
        replay::simulate(&standup_hooks(), ls, &base, OPTS)
            .into_iter()
            .filter(|s| s.fire)
            .map(|s| (s.hook, s.dedup))
            .collect::<Vec<_>>()
    };
    assert_eq!(fires(&twice), fires(&once));
}

#[cfg(unix)]
mod engine {
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use oats_audio::fake::FakeBackend;
    use oats_core::action::Action;
    use oats_core::control::engine::{Engine, EngineOpts};
    use serde_json::{json, Value};

    struct Rig {
        _dir: tempfile::TempDir,
        root: PathBuf,
        engine: Arc<Engine>,
    }

    fn rig(toml: &str) -> Rig {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let cfg = root.join("config.toml");
        std::fs::write(
            &cfg,
            format!(
                "[sessions]\nroot = {:?}\n\n[stt.local]\nauto_download = false\n\n[audio]\ntranscribe = \"off\"\n\n{toml}",
                root.join("rec").to_str().unwrap()
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

    /// A hook that appends its event and run id to `<out>/<hook>.log`.
    fn spy(name: &str, event: &str, out: &Path, extra: &str) -> String {
        format!(
            "[[hooks]]\nname = \"{name}\"\nevent = \"{event}\"\nshell = true\ncommand = [\"printf '%s %s\\\\n' \\\"$OATS_EVENT\\\" \\\"$OATS_RUN_ID\\\" >> \\\"$1/$OATS_HOOK.log\\\"; echo said-$OATS_HOOK\", {:?}]\n{extra}\n",
            out.to_str().unwrap()
        )
    }

    fn runs_of(r: &Rig, args: Value) -> Value {
        let mut a = json!({"action": "hook-runs"});
        a.as_object_mut()
            .unwrap()
            .extend(args.as_object().unwrap().clone());
        r.engine
            .dispatch(&serde_json::from_value::<Action>(a).unwrap())
            .unwrap()
    }

    fn simulate(r: &Rig, args: Value) -> oats_core::config::Result<Value> {
        let mut a = json!({"action": "hook-simulate"});
        a.as_object_mut()
            .unwrap()
            .extend(args.as_object().unwrap().clone());
        r.engine
            .dispatch(&serde_json::from_value::<Action>(a).unwrap())
    }

    fn lines_of(p: &Path) -> usize {
        std::fs::read_to_string(p)
            .map(|t| t.lines().count())
            .unwrap_or(0)
    }

    #[test]
    fn a_recording_ledgers_its_decisions_and_runs_and_hook_simulate_replays_it() {
        let tmp = tempfile::tempdir().unwrap();
        let out = tmp.path().to_path_buf();
        let hooks = spy("stopped", "record.stop", &out, "")
            + &spy(
                "standup",
                "record.stop",
                &out,
                "[hooks.match]\ntitle = \"Standup*\"\n",
            )
            + &spy("quiet", "record.start", &out, "enabled = false\n");
        let r = rig(&hooks);
        let e = &r.engine;
        e.dispatch(&Action::RecordStart {
            session: Some("led".into()),
            title: Some("1:1 Bob".into()),
        })
        .unwrap();
        std::thread::sleep(Duration::from_millis(300));
        e.dispatch(&Action::RecordStop { by: None }).unwrap();
        let ledger = r.root.join("rec/led/hook-runs.jsonl");
        // record.start's quiet skip, record.stop's two verdicts, the one run.
        wait_until("the run in the ledger", || {
            runs_of(&r, json!({"session": "led", "outcome": "ok"}))["count"] == 1
        });
        let all = runs_of(&r, json!({"session": "led"}));
        assert_eq!(all["skipped"], 0);
        let rows = all["rows"].as_array().unwrap();
        let got: Vec<(String, String, String)> = rows
            .iter()
            .map(|r| {
                (
                    r["kind"].as_str().unwrap().into(),
                    r["hook"].as_str().unwrap().into(),
                    r["outcome"].as_str().unwrap().into(),
                )
            })
            .collect();
        let want = |k: &str, h: &str, o: &str| (k.to_string(), h.to_string(), o.to_string());
        for row in [
            want("verdict", "quiet", "skip"),
            want("verdict", "stopped", "fire"),
            want("verdict", "standup", "skip"),
            want("run", "stopped", "ok"),
        ] {
            assert!(got.contains(&row), "{row:?} not in {got:?}");
        }
        assert_eq!(got.len(), 4, "{got:?}");
        let run = rows.iter().find(|r| r["kind"] == "run").unwrap();
        assert_eq!(run["exit"], 0);
        assert_eq!(run["timed_out"], false);
        assert_eq!(run["stdout_tail"], "said-stopped\n");
        assert_eq!(run["simulated"], false);
        assert_eq!(run["reason"], "no match filter: fires on every record.stop");
        assert!(run["run_id"].as_str().unwrap().ends_with("-stopped"));
        let skip = rows.iter().find(|r| r["hook"] == "standup").unwrap();
        assert_eq!(
            skip["reason"],
            r#"match.title: "Standup*" did not match "1:1 Bob""#
        );
        // Filters.
        assert_eq!(runs_of(&r, json!({"hook": "quiet"}))["count"], 1);
        assert_eq!(runs_of(&r, json!({"event": "record.stop"}))["count"], 3);
        assert_eq!(runs_of(&r, json!({"outcome": "skip"}))["count"], 2);
        assert_eq!(runs_of(&r, json!({"outcome": "failed"}))["count"], 0);
        assert_eq!(runs_of(&r, json!({"limit": 1}))["count"], 1);
        let bad = r
            .engine
            .dispatch(&Action::HookRuns {
                session: None,
                hook: None,
                event: None,
                outcome: Some("maybe".into()),
                limit: None,
            })
            .unwrap_err();
        assert_eq!(bad.code, "invalid_value", "{bad}");

        // A dry replay of the session: the same verdicts, an envelope per fire, nothing run or
        // written.
        let before = lines_of(&ledger);
        let log_before = lines_of(&out.join("stopped.log"));
        let dry = simulate(&r, json!({"session": "led"})).unwrap();
        assert_eq!(dry["run"], false);
        let decided: Vec<(String, bool)> = dry["decisions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|d| (d["hook"].as_str().unwrap().into(), d["fire"] == true))
            .collect();
        assert_eq!(
            decided,
            [
                ("quiet".to_string(), false),
                ("stopped".to_string(), true),
                ("standup".to_string(), false)
            ]
        );
        let fires = dry["fires"].as_array().unwrap();
        assert_eq!(fires.len(), 1);
        let env = &fires[0]["envelope"];
        assert_eq!(env["session"]["id"], "led");
        assert_eq!(env["session"]["title"], "1:1 Bob");
        assert!(env["session"]["dir"].as_str().unwrap().ends_with("led"));
        assert!(fires[0].get("run").is_none());
        assert_eq!(lines_of(&ledger), before, "a dry replay writes nothing");
        assert_eq!(lines_of(&out.join("stopped.log")), log_before);
        // One hook, judged as if it were on.
        let one = simulate(
            &r,
            json!({"session": "led", "hook": "quiet", "enable": true}),
        )
        .unwrap();
        assert_eq!(one["hooks"], json!(["quiet"]));
        assert_eq!(one["fires"][0]["hook"], "quiet");
        assert_eq!(one["fires"][0]["event"], "record.start");

        // run = true: the fire runs (as hook-test does) and the ledger gets it, tagged simulated.
        let ran = simulate(&r, json!({"session": "led", "run": true})).unwrap();
        let run = &ran["fires"][0]["run"];
        assert_eq!(run["ok"], true, "{ran}");
        assert_eq!(run["simulated"], true);
        assert_eq!(lines_of(&out.join("stopped.log")), log_before + 1);
        let sim = runs_of(&r, json!({"session": "led"}))["rows"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|r| r["simulated"] == true)
            .map(|r| {
                (
                    r["kind"].as_str().unwrap().to_string(),
                    r["hook"].as_str().unwrap().to_string(),
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(sim.len(), 4, "three verdicts and the run: {sim:?}");
        assert!(sim.contains(&("run".into(), "stopped".into())));
    }

    /// A fixture file replays through the engine's verb as through the pure function, for a
    /// session the store does not know; a missing file is refused.
    #[test]
    fn hook_simulate_replays_a_given_file() {
        let r = rig(&format!(
            "{}{}",
            spy("matched", "calendar.matched", Path::new("/nonexistent"), ""),
            spy(
                "pricing",
                "transcript.phrase",
                Path::new("/nonexistent"),
                "[hooks.match]\nphrases = [\"pricing\"]\nspeaker = \"remote\"\ncooldown = \"2m\"\n"
            )
        ));
        let file = super::fixture("calendar-standup.events.jsonl");
        let v = simulate(&r, json!({"file": file.to_str().unwrap()})).unwrap();
        assert_eq!(v["session"], "2026-11-13-standup");
        assert_eq!(v["lines"], 14);
        let got: Vec<(u64, String, bool)> = v["decisions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|d| {
                (
                    d["seq"].as_u64().unwrap(),
                    d["hook"].as_str().unwrap().into(),
                    d["fire"] == true,
                )
            })
            .collect();
        let w = |s: u64, h: &str, f: bool| (s, h.to_string(), f);
        assert_eq!(
            got,
            [
                w(1, "matched", true),
                w(3, "pricing", true),
                w(5, "pricing", false),
                w(6, "pricing", false),
                w(10, "pricing", true),
                w(11, "pricing", false),
                w(12, "matched", false),
            ]
        );
        assert!(v["fires"]
            .as_array()
            .unwrap()
            .iter()
            .all(|f| f["envelope"]["calendar"]["title"] == "Standup"));
        let e = simulate(&r, json!({"file": "/nonexistent/events.jsonl"})).unwrap_err();
        assert_eq!(e.code, "not_found", "{e}");
        let e = simulate(&r, json!({"session": "nope"})).unwrap_err();
        assert_eq!(e.code, "not_found", "{e}");
    }
}
