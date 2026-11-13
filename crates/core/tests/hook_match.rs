//! dot-4uzgol.111.4 (docs/design/hooks.md L2): `[hooks.match]` filters a hook by session title,
//! attendees, calendar source and calendar match. `hooks::matches` gives a verdict with a reason
//! both ways; the config check refuses a bad glob or regex naming the hook; a match survives a
//! write to the user file and a re-read; the bus tap publishes every decision as `hook-decided`
//! and keeps it for `hook-list`; `hook-explain` shows every hook's verdict without running one.

use oats_core::hooks::{self, Hook, Match, Verdict};
use serde_json::{json, Value};

fn hook(m: Value) -> Hook {
    serde_json::from_value(
        json!({"name": "h", "event": "record.stop", "command": ["true"], "match": m}),
    )
    .unwrap()
}

/// As much of an envelope as `matches` reads.
fn envelope(event: &str, title: &str, calendar: Value) -> Value {
    json!({"event": event, "session": {"title": title}, "calendar": calendar})
}

fn standup() -> Value {
    json!({"title": "Standup", "start": "2026-11-13T15:00:00Z", "end": "2026-11-13T15:15:00Z",
           "attendees": ["ana@acme.com", "Bo"], "source": "work"})
}

#[test]
fn verdicts_and_their_reasons() {
    let none = Value::Null;
    let cases: Vec<(Value, &str, Value, bool, &str)> = vec![
        (
            json!({}),
            "x",
            none.clone(),
            true,
            "no match filter: fires on every record.stop",
        ),
        (
            json!({"title": "Standup*"}),
            "Standup with Ana",
            none.clone(),
            true,
            r#"match.title: "Standup*" matched "Standup with Ana""#,
        ),
        (
            json!({"title": "Standup*"}),
            "1:1 Bob",
            none.clone(),
            false,
            r#"match.title: "Standup*" did not match "1:1 Bob""#,
        ),
        // A glob is case-insensitive and covers the whole title.
        (
            json!({"title": "standup"}),
            "STANDUP",
            none.clone(),
            true,
            r#"match.title: "standup" matched "STANDUP""#,
        ),
        (
            json!({"title": "standup"}),
            "Standup 2",
            none.clone(),
            false,
            r#"match.title: "standup" did not match "Standup 2""#,
        ),
        (
            json!({"title": "St?ndup"}),
            "Standup",
            none.clone(),
            true,
            r#"match.title: "St?ndup" matched "Standup""#,
        ),
        (
            json!({"title": "[!x]*"}),
            "xyz",
            none.clone(),
            false,
            r#"match.title: "[!x]*" did not match "xyz""#,
        ),
        (
            json!({"title": "[a-c]ase (1)"}),
            "Base (1)",
            none.clone(),
            true,
            r#"match.title: "[a-c]ase (1)" matched "Base (1)""#,
        ),
        // A regex is as written: unanchored, case-sensitive unless it says (?i).
        (
            json!({"title": "re:^1:1 "}),
            "1:1 Bob",
            none.clone(),
            true,
            r#"match.title: "re:^1:1 " matched "1:1 Bob""#,
        ),
        (
            json!({"title": "re:retro"}),
            "Sprint Retro",
            none.clone(),
            false,
            r#"match.title: "re:retro" did not match "Sprint Retro""#,
        ),
        (
            json!({"title": "re:(?i)retro"}),
            "Sprint Retro",
            none.clone(),
            true,
            r#"match.title: "re:(?i)retro" matched "Sprint Retro""#,
        ),
        // The calendar event's title counts too.
        (
            json!({"title": "Standup*"}),
            "Daily",
            standup(),
            true,
            r#"match.title: "Standup*" matched "Standup""#,
        ),
        (
            json!({"title": "Retro"}),
            "Daily",
            standup(),
            false,
            r#"match.title: "Retro" did not match "Daily" or "Standup""#,
        ),
        (
            json!({"title": "Standup*"}),
            "",
            none.clone(),
            false,
            r#"match.title: "Standup*": the session has no title"#,
        ),
        (
            json!({"attendees": ["*@acme.com"]}),
            "x",
            standup(),
            true,
            r#"match.attendees: "*@acme.com" matched "ana@acme.com""#,
        ),
        (
            json!({"attendees": ["*@other.org", "bo"]}),
            "x",
            standup(),
            true,
            r#"match.attendees: "bo" matched "Bo""#,
        ),
        (
            json!({"attendees": ["*@other.org", "carl"]}),
            "x",
            standup(),
            false,
            r#"match.attendees: none of "*@other.org", "carl" matched "ana@acme.com", "Bo""#,
        ),
        (
            json!({"attendees": ["*"]}),
            "x",
            none.clone(),
            false,
            r#"match.attendees: "*": the session has no attendees"#,
        ),
        (
            json!({"source": "work"}),
            "x",
            standup(),
            true,
            r#"match.source: "work" matched calendar source "work""#,
        ),
        (
            json!({"source": "home"}),
            "x",
            standup(),
            false,
            r#"match.source: "home" did not match calendar source "work""#,
        ),
        (
            json!({"source": "work"}),
            "x",
            none.clone(),
            false,
            r#"match.source: "work": the session matched no calendar event"#,
        ),
        (
            json!({"calendar": true}),
            "x",
            none.clone(),
            false,
            "match.calendar = true: the session matched no calendar event",
        ),
        (
            json!({"calendar": true}),
            "x",
            standup(),
            true,
            r#"match.calendar = true: the session matched calendar event "Standup""#,
        ),
        (
            json!({"calendar": false}),
            "x",
            standup(),
            false,
            r#"match.calendar = false: the session matched calendar event "Standup""#,
        ),
        (
            json!({"calendar": false}),
            "x",
            none.clone(),
            true,
            "match.calendar = false: the session matched no calendar event",
        ),
        // All present fields must hold: every one that held is the reason to fire...
        (
            json!({"title": "Standup*", "attendees": ["*@acme.com"], "source": "work", "calendar": true}),
            "Standup",
            standup(),
            true,
            r#"match.calendar = true: the session matched calendar event "Standup"; match.title: "Standup*" matched "Standup"; match.attendees: "*@acme.com" matched "ana@acme.com"; match.source: "work" matched calendar source "work""#,
        ),
        // ...and every one that failed the reason not to.
        (
            json!({"title": "Retro", "attendees": ["*@acme.com"], "source": "home"}),
            "Standup",
            standup(),
            false,
            r#"match.title: "Retro" did not match "Standup"; match.source: "home" did not match calendar source "work""#,
        ),
        // The phrase and calendar-end fields belong to events of their own.
        (
            json!({"phrases": ["pricing"], "speaker": "remote", "cooldown": "2m"}),
            "x",
            none.clone(),
            true,
            "nothing in match filters record.stop: fires on every one",
        ),
    ];
    for (m, title, cal, fire, reason) in cases {
        let v = hooks::matches(
            &hook(m.clone()),
            &envelope("record.stop", title, cal.clone()),
        );
        assert_eq!(
            v,
            Verdict {
                fire,
                reason: reason.to_string()
            },
            "match {m} on title {title:?}, calendar {cal}"
        );
    }
    // The event and the on/off switch come first.
    let h = hook(json!({"title": "Standup*"}));
    assert_eq!(
        hooks::matches(&h, &envelope("session.saved", "Standup", Value::Null)),
        Verdict {
            fire: false,
            reason: "listens for record.stop, not session.saved".into()
        }
    );
    let off = Hook {
        enabled: false,
        ..h
    };
    assert_eq!(
        hooks::matches(&off, &envelope("record.stop", "Standup", Value::Null)),
        Verdict {
            fire: false,
            reason: "the hook is off (enabled = false)".into()
        }
    );
}

/// The verdict on a real envelope: `matches` reads `session.title` and `calendar` from it.
#[test]
fn a_full_envelope_is_judged_like_its_parts() {
    let spec = hooks::events::find("record.stop").unwrap();
    let env = hooks::events::example(spec);
    let v = hooks::matches(
        &hook(json!({"title": "standup", "attendees": ["*@acme.com"]})),
        &env,
    );
    assert_eq!(
        v.reason,
        r#"match.title: "standup" matched "Standup"; match.attendees: "*@acme.com" matched "ana@acme.com""#
    );
    assert!(v.fire);
}

#[test]
fn bad_patterns_and_unknown_match_keys_are_refused_with_the_hook_name() {
    let one = |m: Value| {
        hooks::parse(
            &json!([{"name": "bad", "event": "record.stop", "command": ["true"], "match": m}]),
        )
    };
    for (m, field, what) in [
        (json!({"title": "re:("}), "match.title", "bad regex"),
        (json!({"title": "[abc"}), "match.title", "bad glob"),
        (json!({"source": "re:a{2,1}"}), "match.source", "bad regex"),
        (
            json!({"attendees": ["*@acme.com", "re:[z-a]"]}),
            "match.attendees",
            "bad regex",
        ),
        (
            json!({"phrases": ["pricing", "re:(?P<"]}),
            "match.phrases",
            "bad regex",
        ),
    ] {
        let e = one(m.clone()).unwrap_err();
        assert_eq!(e.code, "invalid_value", "{m}: {e}");
        assert!(
            e.message.contains(&format!("hook bad: {field}: {what}")),
            "{m}: {}",
            e.message
        );
    }
    let e = one(json!({"speaker": "them"})).unwrap_err();
    assert!(
        e.message.contains("hook bad: match.speaker \"them\""),
        "{}",
        e.message
    );
    for m in [
        json!({"titel": "x"}),
        json!({"title": 3}),
        json!({"cooldown": "soon"}),
    ] {
        let e = one(m.clone()).unwrap_err();
        assert_eq!(e.code, "type_mismatch", "{m}: {e}");
    }
    // The reserved fields parse now.
    let ok = one(
        json!({"phrases": ["pricing", "re:\\bdiscount(s)?\\b"], "speaker": "remote",
                        "cooldown": "2m", "before_end": "5m"}),
    )
    .unwrap();
    assert_eq!(ok[0].filter.phrases, ["pricing", "re:\\bdiscount(s)?\\b"]);
    assert_eq!(ok[0].filter.speaker, "remote");
    assert_eq!(ok[0].filter.cooldown.map(|d| d.0), Some(120_000));
    assert_eq!(ok[0].filter.before_end.map(|d| d.0), Some(300_000));
    // No match, or an empty one, is the same hook, and is not written.
    let bare =
        hooks::parse(&json!([{"event": "record.stop", "command": ["true"], "match": {}}])).unwrap();
    assert_eq!(bare[0].filter, Match::default());
    assert!(serde_json::to_value(&bare[0])
        .unwrap()
        .get("match")
        .is_none());
    // schema.json describes the table.
    let schema = hooks::item_schema();
    assert_eq!(schema["properties"]["match"], hooks::match_schema());
    assert_eq!(schema["properties"]["match"]["additionalProperties"], false);
}

#[cfg(unix)]
mod engine {
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use oats_audio::fake::FakeBackend;
    use oats_core::action::Action;
    use oats_core::control::engine::{Engine, EngineOpts};
    use oats_core::stt::{Asr, Segment};
    use serde_json::{json, Value};

    struct ToneAsr;

    impl Asr for ToneAsr {
        fn name(&self) -> &'static str {
            "tone-asr"
        }

        fn transcribe(
            &self,
            pcm: &[f32],
            _language: &str,
        ) -> oats_core::config::Result<Vec<Segment>> {
            Ok(if pcm.iter().any(|s| s.abs() > 0.1) {
                vec![Segment {
                    start: 0.0,
                    end: pcm.len() as f64 / 16000.0,
                    text: "tone".into(),
                }]
            } else {
                vec![]
            })
        }
    }

    struct Rig {
        _dir: tempfile::TempDir,
        cfg: PathBuf,
        out: PathBuf,
        engine: Arc<Engine>,
    }

    fn rig(toml: &str) -> Rig {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let cfg = root.join("config.toml");
        let out = root.join("out");
        std::fs::create_dir_all(&out).unwrap();
        std::fs::write(
            &cfg,
            format!(
                "# my hooks\n[sessions]\nroot = {:?}\n\n[stt.local]\nauto_download = false\n\n[stt.live]\nchunk = \"1s\"\n\n{toml}",
                root.join("rec").to_str().unwrap()
            ),
        )
        .unwrap();
        let engine = Engine::new(EngineOpts {
            user_config: cfg.clone(),
            pack: None,
            os: "linux".into(),
            arch: "x86_64".into(),
            machine: "default".into(),
            sessions_dir: root.join("sessions"),
            secrets: None,
        })
        .unwrap();
        engine.set_audio_factory(Box::new(|_| Ok(Box::new(FakeBackend::default()))));
        engine.set_asr_factory(Box::new(|_| Ok(Arc::new(ToneAsr))));
        Rig {
            _dir: dir,
            cfg,
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

    /// A hook on `event` that leaves `<out>/<hook>.<session>` behind when it runs.
    fn add(r: &Rig, name: &str, event: &str, m: Option<Value>) -> oats_core::config::Result<Value> {
        r.engine.dispatch(&Action::HookAdd {
            name: name.into(),
            event: event.into(),
            command: json!([
                "sh",
                "-c",
                r#"touch "$1/$OATS_HOOK.$OATS_SESSION_ID""#,
                "hook",
                r.out.to_str().unwrap()
            ]),
            shell: None,
            timeout: None,
            blocking: None,
            enabled: None,
            cwd: None,
            r#match: m,
        })
    }

    fn edit_match(r: &Rig, hook: &str, m: Value) -> oats_core::config::Result<Value> {
        r.engine.dispatch(&Action::HookEdit {
            hook: hook.into(),
            name: None,
            event: None,
            command: None,
            shell: None,
            timeout: None,
            blocking: None,
            enabled: None,
            cwd: None,
            r#match: Some(m),
        })
    }

    #[test]
    fn a_match_round_trips_through_the_user_file() {
        let r = rig("");
        let m = json!({"title": "Standup*", "attendees": ["*@acme.com", "re:^bo@"], "source": "work", "calendar": true});
        let v = add(&r, "standup", "record.stop", Some(m.clone())).unwrap();
        assert_eq!(v["match"], m);
        add(&r, "plain", "record.stop", None).unwrap();
        let text = std::fs::read_to_string(&r.cfg).unwrap();
        assert!(text.starts_with("# my hooks\n"), "{text}");
        let (a, t, b) = (
            text.find("name = \"standup\"").unwrap(),
            text.find("[hooks.match]").unwrap(),
            text.find("name = \"plain\"").unwrap(),
        );
        assert!(a < t && t < b, "the match sits under its own hook: {text}");
        assert!(text.contains("title = \"Standup*\""), "{text}");
        // Re-read from the file: the same hooks.
        let before = r.engine.snapshot().config.hooks.clone();
        r.engine.reload("test").unwrap();
        let after = r.engine.snapshot().config.hooks.clone();
        assert_eq!(after, before);
        assert_eq!(serde_json::to_value(&after[0].filter).unwrap(), m);
        assert!(after[1].filter.is_empty());
        // hook-edit's match replaces the whole table; {} clears it.
        edit_match(&r, "standup", json!({"calendar": false})).unwrap();
        r.engine.reload("test").unwrap();
        let h = &r.engine.snapshot().config.hooks[0];
        assert_eq!(
            serde_json::to_value(&h.filter).unwrap(),
            json!({"calendar": false})
        );
        edit_match(&r, "standup", json!({})).unwrap();
        let text = std::fs::read_to_string(&r.cfg).unwrap();
        assert!(!text.contains("[hooks.match]"), "{text}");
        // A bad pattern is refused with the hook's name, and the file stays as it was.
        let e = edit_match(&r, "plain", json!({"title": "re:("})).unwrap_err();
        assert_eq!(e.code, "invalid_value");
        assert!(
            e.message.contains("hook plain: match.title: bad regex"),
            "{e}"
        );
        let e = add(&r, "typo", "record.stop", Some(json!({"titel": "x"}))).unwrap_err();
        assert!(e.message.contains("titel"), "{e}");
        assert_eq!(std::fs::read_to_string(&r.cfg).unwrap(), text);
        // So is one written into the file by hand.
        std::fs::write(
            &r.cfg,
            format!("{text}\n[[hooks]]\nname = \"hand\"\nevent = \"record.stop\"\ncommand = [\"true\"]\n[hooks.match]\nattendees = [\"[x\"]\n"),
        )
        .unwrap();
        let e = r.engine.reload("test").unwrap_err();
        assert!(
            e.message.contains("hook hand: match.attendees: bad glob"),
            "{e}"
        );
        assert_eq!(r.engine.snapshot().config.hooks.len(), 2);
    }

    #[test]
    fn every_decision_is_published_and_kept_and_only_matches_run() {
        let r = rig("");
        add(
            &r,
            "standup",
            "record.stop",
            Some(json!({"title": "Standup*"})),
        )
        .unwrap();
        add(&r, "any", "record.stop", None).unwrap();
        add(&r, "off", "record.stop", None).unwrap();
        r.engine
            .dispatch(&Action::HookEnable {
                hook: "off".into(),
                enabled: Some(false),
            })
            .unwrap();
        add(
            &r,
            "cal",
            "record.stop",
            Some(json!({"calendar": true, "source": "work"})),
        )
        .unwrap();
        add(
            &r,
            "saved",
            "session.saved",
            Some(json!({"title": "Standup*"})),
        )
        .unwrap();
        let rx = r.engine.bus.subscribe(vec!["hook-decided".into()]);
        let s1 = json!({"session": "s1", "title": "Standup daily",
                        "calendar": {"event": {"id": "e1", "title": "Standup daily", "source": "work"}}});
        r.engine.bus.publish("recording-stopped", s1.clone());
        r.engine.bus.publish(
            "recording-stopped",
            json!({"session": "s2", "title": "1:1 Bob"}),
        );
        // Seen again: one of the two fires, the other says it already did.
        r.engine.bus.publish("recording-stopped", s1);
        let decisions = || -> Vec<Value> {
            r.engine.dispatch(&Action::HookList {}).unwrap()["decisions"]
                .as_array()
                .unwrap()
                .clone()
        };
        wait_until("12 decisions", || decisions().len() >= 12);
        let runs = || {
            r.engine.dispatch(&Action::HookList {}).unwrap()["runs"]
                .as_array()
                .unwrap()
                .len()
        };
        wait_until("4 runs", || runs() >= 4);
        std::thread::sleep(Duration::from_millis(300));
        let all = decisions();
        assert_eq!(all.len(), 12, "{all:?}");
        assert_eq!(runs(), 4);
        let of = |hook: &str, session: &str| -> Vec<(bool, String)> {
            all.iter()
                .filter(|d| d["hook"] == hook && d["session"] == session)
                .map(|d| (d["fire"] == true, d["reason"].as_str().unwrap().to_string()))
                .collect()
        };
        let mut s1 = of("standup", "s1");
        s1.sort();
        assert_eq!(
            s1,
            [
                (false, r#"already fired for s1:record.stop (match.title: "Standup*" matched "Standup daily")"#.into()),
                (true, r#"match.title: "Standup*" matched "Standup daily""#.into()),
            ]
        );
        assert_eq!(
            of("standup", "s2"),
            [(
                false,
                r#"match.title: "Standup*" did not match "1:1 Bob""#.into()
            )]
        );
        assert_eq!(
            of("any", "s2"),
            [(true, "no match filter: fires on every record.stop".into())]
        );
        assert!(of("off", "s1")
            .iter()
            .chain(&of("off", "s2"))
            .all(|(f, why)| !f && why == "the hook is off (enabled = false)"));
        assert_eq!(of("cal", "s1").iter().filter(|(f, _)| *f).count(), 1);
        assert_eq!(
            of("cal", "s2"),
            [(false, r#"match.calendar = true: the session matched no calendar event; match.source: "work": the session matched no calendar event"#.into())]
        );
        // A hook on another event is not asked.
        assert!(all.iter().all(|d| d["hook"] != "saved"));
        for (hook, session, ran) in [
            ("standup", "s1", true),
            ("standup", "s2", false),
            ("any", "s1", true),
            ("any", "s2", true),
            ("off", "s1", false),
            ("cal", "s1", true),
            ("cal", "s2", false),
        ] {
            assert_eq!(
                r.out.join(format!("{hook}.{session}")).exists(),
                ran,
                "{hook} on {session}"
            );
        }
        // Each decision went out as hook-decided, with the envelope's seq and dedup.
        let mut published = vec![];
        while let Ok(l) = rx.recv_timeout(Duration::from_millis(300)) {
            let v: Value = serde_json::from_str(&l).unwrap();
            assert_eq!(v["event"], "hook-decided");
            published.push(v["data"].clone());
        }
        assert_eq!(published.len(), 12, "{published:?}");
        assert!(published.iter().all(|d| d["event"] == "record.stop"
            && d["dedup"].as_str().unwrap().ends_with(":record.stop")
            && d["event_seq"].as_u64().unwrap() > 0
            && d["reason"].is_string()));
        // hook-list shows each hook's last decision on its row.
        let list = r.engine.dispatch(&Action::HookList {}).unwrap();
        let row = list["hooks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|h| h["name"] == "standup")
            .unwrap()
            .clone();
        assert_eq!(row["match"], json!({"title": "Standup*"}));
        assert!(
            row["last_decision"]["reason"]
                .as_str()
                .unwrap()
                .contains("match.title"),
            "{row}"
        );
    }

    #[test]
    fn hook_explain_gives_every_hooks_verdict_and_runs_nothing() {
        let r = rig("");
        add(
            &r,
            "standup",
            "record.stop",
            Some(json!({"title": "*standup*"})),
        )
        .unwrap();
        add(
            &r,
            "one-on-one",
            "record.stop",
            Some(json!({"title": "re:^1:1"})),
        )
        .unwrap();
        add(&r, "cal", "record.stop", Some(json!({"calendar": true}))).unwrap();
        add(&r, "noted", "notes.generated", None).unwrap();
        let e = &r.engine;
        for (id, title) in [("older", "1:1 Bob"), ("newer", "Weekly Standup")] {
            e.dispatch(&Action::RecordStart {
                session: Some(id.into()),
                title: Some(title.into()),
            })
            .unwrap();
            std::thread::sleep(Duration::from_millis(300));
            e.dispatch(&Action::RecordStop { by: None }).unwrap();
            // started_at has whole seconds: start the next session in a later second, or the two
            // tie and "newest" is ambiguous (dot-4uzgol.111.9: flaked on main).
            std::thread::sleep(Duration::from_millis(1100));
        }
        // Let the stops' own hooks finish, then forget them: explain must add none.
        std::thread::sleep(Duration::from_millis(1000));
        let runs = |e: &Engine| {
            e.dispatch(&Action::HookList {}).unwrap()["runs"]
                .as_array()
                .unwrap()
                .len()
        };
        let decided = |e: &Engine| {
            e.dispatch(&Action::HookList {}).unwrap()["decisions"]
                .as_array()
                .unwrap()
                .len()
        };
        let (n_runs, n_decided) = (runs(e), decided(e));
        let rx = e
            .bus
            .subscribe(vec!["hook-decided".into(), "hook-finished".into()]);
        let v = e
            .dispatch(&Action::HookExplain {
                session: None,
                event: "record.stop".into(),
                utterance: None,
            })
            .unwrap();
        assert_eq!(v["session"], "newer", "the newest session by default: {v}");
        assert_eq!(v["event"], "record.stop");
        assert_eq!(v["title"], "Weekly Standup");
        let verdicts: Vec<(String, bool, String)> = v["hooks"]
            .as_array()
            .unwrap()
            .iter()
            .map(|h| {
                (
                    h["name"].as_str().unwrap().into(),
                    h["fire"] == true,
                    h["reason"].as_str().unwrap().into(),
                )
            })
            .collect();
        assert_eq!(
            verdicts,
            [
                (
                    "standup".to_string(),
                    true,
                    r#"match.title: "*standup*" matched "Weekly Standup""#.to_string()
                ),
                (
                    "one-on-one".into(),
                    false,
                    r#"match.title: "re:^1:1" did not match "Weekly Standup""#.into()
                ),
                (
                    "cal".into(),
                    false,
                    "match.calendar = true: the session matched no calendar event".into()
                ),
                (
                    "noted".into(),
                    false,
                    "listens for notes.generated, not record.stop".into()
                ),
            ]
        );
        let older = e
            .dispatch(&Action::HookExplain {
                session: Some("older".into()),
                event: "record.stop".into(),
                utterance: None,
            })
            .unwrap();
        assert_eq!(older["hooks"][0]["fire"], false);
        assert_eq!(older["hooks"][1]["fire"], true, "{older}");
        assert_eq!(
            older["hooks"][1]["reason"],
            r#"match.title: "re:^1:1" matched "1:1 Bob""#
        );
        // Nothing ran and nothing was decided.
        assert_eq!((runs(e), decided(e)), (n_runs, n_decided));
        assert!(rx.recv_timeout(Duration::from_millis(300)).is_err());
        // An unknown event or session is refused.
        let bad = e
            .dispatch(&Action::HookExplain {
                session: None,
                event: "record.explode".into(),
                utterance: None,
            })
            .unwrap_err();
        assert_eq!(bad.code, "not_found");
        assert!(e
            .dispatch(&Action::HookExplain {
                session: Some("nope".into()),
                event: "record.stop".into(),
                utterance: None,
            })
            .is_err());
    }
}
