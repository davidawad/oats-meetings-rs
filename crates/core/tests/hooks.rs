//! Configurable hooks (dot-4uzgol.89): every hook event fires with the session in the hook's
//! environment and the event on stdin; a hook past its timeout is killed with its children; a
//! failing, missing or slow hook never touches the session; the hook actions edit `[[hooks]]` in
//! the user file. Hooks here are `sh` scripts, so this suite is unix-only.
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use oats_audio::fake::FakeBackend;
use oats_core::action::Action;
use oats_core::control::engine::{Engine, EngineOpts};
use oats_core::hooks;
use oats_core::stt::{Asr, Segment};
use serde_json::{json, Value};

/// Says "tone" for any chunk with sound in it.
struct ToneAsr;

impl Asr for ToneAsr {
    fn name(&self) -> &'static str {
        "tone-asr"
    }

    fn transcribe(&self, pcm: &[f32], _language: &str) -> oats_core::config::Result<Vec<Segment>> {
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
    root: PathBuf,
    cfg: PathBuf,
    /// Where the hooks write what they saw.
    out: PathBuf,
    engine: Arc<Engine>,
}

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
            "# my hooks\n[sessions]\nroot = {:?}\n\n[stt.local]\nauto_download = false\n\n[stt.live]\nchunk = \"1s\"\n\n{toml}",
            rec.to_str().unwrap()
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
        root,
        cfg,
        out,
        engine,
    }
}

/// Writes every OATS_* variable to `<out>/<event>.env` (one per line), stdin to `<event>.json` and
/// the OATS_EVENT_FILE to `<event>.file.json`, each replaced whole (two runs of one event may
/// overlap).
const SPY: &str = r#"t="$1/$OATS_EVENT.$$"; printf '%s\n' "$OATS_EVENT" "$OATS_SESSION_ID" "$OATS_SESSION_DIR" "$OATS_TITLE" "$OATS_AUDIO" "$OATS_TRANSCRIPT" "$OATS_HOOK" "$PWD" "$OATS_RUN_ID" "$OATS_SEQ" "$OATS_EVENT_FILE" > "$t.env"; cp "$OATS_EVENT_FILE" "$t.file"; cat > "$t.json"; mv "$t.file" "$1/$OATS_EVENT.file.json"; mv "$t.json" "$1/$OATS_EVENT.json"; mv "$t.env" "$1/$OATS_EVENT.env""#;

fn spy_hooks(out: &Path) -> String {
    hooks::names()
        .iter()
        .map(|e| {
            // A phrase hook needs phrases (dot-4uzgol.111.6): one no tone is heard to say.
            let m = if *e == "transcript.phrase" {
                "[hooks.match]\nphrases = [\"oats spy never said\"]\n"
            } else {
                ""
            };
            format!(
                "[[hooks]]\nname = \"spy-{e}\"\nevent = \"{e}\"\nshell = true\ncommand = [{SPY:?}, {:?}]\n{m}\n",
                out.to_str().unwrap()
            )
        })
        .collect()
}

fn wait_until(what: &str, mut cond: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while !cond() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// What the spy saw for `event`: (env lines, stdin JSON).
fn seen(out: &Path, event: &str) -> (Vec<String>, Value) {
    let env = out.join(format!("{event}.env"));
    wait_until(&format!("the {event} hook"), || env.exists());
    let lines = std::fs::read_to_string(&env)
        .unwrap()
        .lines()
        .map(str::to_string)
        .collect();
    let stdin = std::fs::read_to_string(out.join(format!("{event}.json"))).unwrap();
    (lines, serde_json::from_str(&stdin).unwrap())
}

/// The envelope a spy saw holds to the event's schema, the OATS_EVENT_FILE held the same JSON
/// and OATS_RUN_ID / OATS_SEQ are the envelope's. Two runs of one event may overlap, so the spy's
/// three files are read again until they are one run's.
fn check_envelope(out: &Path, event: &str) {
    let mut got = (vec![], Value::Null);
    wait_until(&format!("one {event} run's spy files"), || {
        let (env, stdin) = seen(out, event);
        let file = std::fs::read_to_string(out.join(format!("{event}.file.json")))
            .ok()
            .and_then(|s| serde_json::from_str::<Value>(&s).ok());
        let same = env.get(8).map(String::as_str) == stdin["run_id"].as_str()
            && file.as_ref() == Some(&stdin);
        got = (env, stdin);
        same
    });
    let (env, stdin) = got;
    let spec = hooks::events::find(event).unwrap();
    hooks::events::validate(&hooks::events::envelope_schema(Some(spec)), &stdin)
        .unwrap_or_else(|e| panic!("{event}: {e}\n{stdin}"));
    assert_eq!(stdin["schema"], "oats.hook-event/1");
    assert_eq!(stdin["event"], event);
    assert_eq!(env[9], stdin["seq"].to_string(), "{event} OATS_SEQ");
    assert!(
        env[10].ends_with(".json"),
        "{event} OATS_EVENT_FILE {}",
        env[10]
    );
}

fn record(r: &Rig, id: &str, title: &str) -> Value {
    let e = &r.engine;
    e.dispatch(&Action::RecordStart {
        session: Some(id.into()),
        title: Some(title.into()),
    })
    .unwrap();
    std::thread::sleep(Duration::from_millis(300));
    e.dispatch(&Action::RecordPause {}).unwrap();
    e.dispatch(&Action::RecordPause {}).unwrap();
    std::thread::sleep(Duration::from_millis(1500));
    e.dispatch(&Action::RecordStop { by: None }).unwrap()
}

#[test]
fn each_recording_event_fires_its_hook_with_the_session_in_env_and_stdin() {
    let r = rig("");
    let out = r.out.clone();
    std::fs::write(
        &r.cfg,
        std::fs::read_to_string(&r.cfg).unwrap() + &spy_hooks(&out),
    )
    .unwrap();
    r.engine.reload("test").unwrap();
    assert_eq!(r.engine.snapshot().config.hooks.len(), hooks::names().len());
    let stop = record(&r, "hooked", "Hooked call");
    assert_eq!(stop["transcription"]["final"], "ok", "{stop}");
    let dir = r.root.join("rec").join("hooked");
    let dir = dir.to_str().unwrap();
    for event in [
        "record.start",
        "record.pause",
        "record.stop",
        "transcript.final",
        "session.saved",
    ] {
        let (env, stdin) = seen(&out, event);
        assert_eq!(env[0], event, "OATS_EVENT");
        assert_eq!(env[1], "hooked", "{event} OATS_SESSION_ID");
        assert_eq!(env[2], dir, "{event} OATS_SESSION_DIR");
        assert_eq!(env[3], "Hooked call", "{event} OATS_TITLE");
        assert_eq!(env[6], format!("spy-{event}"), "OATS_HOOK");
        // cwd defaults to the session folder.
        assert_eq!(
            std::fs::canonicalize(&env[7]).unwrap(),
            std::fs::canonicalize(dir).unwrap(),
            "{event} cwd"
        );
        check_envelope(&out, event);
        assert_eq!(stdin["hook"], format!("spy-{event}"));
        assert!(
            stdin["dedup"]
                .as_str()
                .unwrap()
                .starts_with(&format!("hooked:{event}")),
            "{stdin}"
        );
        assert_eq!(stdin["calendar"], Value::Null, "{event}: no calendar event");
        assert_eq!(stdin["session"]["id"], "hooked");
        assert_eq!(stdin["session"]["title"], "Hooked call");
        if event != "record.start" {
            assert!(
                env[4].ends_with("audio.wav"),
                "{event} OATS_AUDIO {}",
                env[4]
            );
        }
        if event == "record.stop" {
            assert_eq!(stdin["data"]["session"], "hooked");
            assert_eq!(stdin["data"]["transcription"]["final"], "ok");
        }
        if matches!(event, "record.stop" | "transcript.final") {
            assert!(
                env[5].ends_with("transcript.final.jsonl"),
                "{event} OATS_TRANSCRIPT {}",
                env[5]
            );
        }
    }
    let (_, pause) = seen(&out, "record.pause");
    assert!(pause["data"]["paused"].is_boolean(), "{pause}");
    // seq is monotonic within the session: start, pause, stop (one message: transcript.final
    // shares its seq), then the save.
    let seq = |e: &str| seen(&out, e).1["seq"].as_u64().unwrap();
    assert!(seq("record.start") < seq("record.pause"));
    assert!(seq("record.pause") < seq("record.stop"));
    assert_eq!(seq("record.stop"), seq("transcript.final"));
    assert!(seq("record.stop") < seq("session.saved"));
    // A recording that matched a calendar event: the engine publishes the match in the
    // recording-started / -stopped payload (calendar.event), which is what calendar.matched follows.
    r.engine.bus.publish(
        "recording-stopped",
        json!({"session": "hooked", "calendar": {"event": {"title": "Standup", "id": "e1"}}}),
    );
    let (env, stdin) = seen(&out, "calendar.matched");
    assert_eq!(env[1], "hooked");
    assert_eq!(stdin["data"]["calendar"]["event"]["title"], "Standup");
    check_envelope(&out, "calendar.matched");
    assert_eq!(stdin["dedup"], "hooked:calendar.matched:e1");
    assert_eq!(stdin["calendar"]["title"], "Standup", "{stdin}");
    // Auto-start and auto-stop, as the engine publishes them.
    r.engine.bus.publish(
        "auto-started",
        json!({"session": "hooked", "source": "speaker", "after": "10s",
               "message": "Recording started: speaker audio for 10s"}),
    );
    r.engine.bus.publish(
        "auto-stopped",
        json!({"session": "hooked", "silence_ms": 300_000, "after": "5m", "cut_ms": 1000,
               "trimmed_ms": 0, "duration_ms": 1000, "message": "Recording stopped after 5m of silence"}),
    );
    for event in ["record.auto_start", "record.auto_stop"] {
        check_envelope(&out, event);
        assert_eq!(seen(&out, event).0[1], "hooked");
    }
    // A re-transcription saves the session as transcribed: transcript.final again.
    std::fs::remove_file(out.join("transcript.final.env")).unwrap();
    r.engine
        .dispatch(&Action::SessionTranscribe {
            session: "hooked".into(),
            overwrite: Some(true),
        })
        .unwrap();
    let (env, stdin) = seen(&out, "transcript.final");
    assert_eq!(env[1], "hooked");
    assert_eq!(stdin["data"]["change"], "transcribed");
    check_envelope(&out, "transcript.final");
    // Every run is over: their event files are gone.
    wait_until("the event files removed", || {
        hooks::names().iter().all(|e| {
            std::fs::read_to_string(out.join(format!("{e}.env")))
                .map(|s| s.lines().nth(10).is_some_and(|f| !Path::new(f).exists()))
                .unwrap_or(true)
        })
    });
    // Every run is listed with its exit code.
    let list = r.engine.dispatch(&Action::HookList {}).unwrap();
    let runs = list["runs"].as_array().unwrap();
    assert!(
        runs.iter().all(|x| x["ok"] == true && x["exit_code"] == 0),
        "{list}"
    );
    assert!(list["hooks"][0]["last_run"]["ok"] == true, "{list}");
}

#[test]
fn notes_generated_fires_after_notes_are_written() {
    // A local mock LLM (ollama wire format) answering every request with notes.
    let server = Arc::new(tiny_http::Server::http("127.0.0.1:0").unwrap());
    let base = format!("http://{}", server.server_addr().to_ip().unwrap());
    let sv = Arc::clone(&server);
    std::thread::spawn(move || {
        for mut req in sv.incoming_requests() {
            let mut body = String::new();
            let _ = req.as_reader().read_to_string(&mut body);
            let reply = json!({"message": {"role": "assistant", "content": "## Summary\nok"}, "done": true});
            let _ = req.respond(tiny_http::Response::from_string(reply.to_string()));
        }
    });
    let r = rig(&format!(
        "[llm]\nbackend = \"ollama\"\nbase_url = \"{base}\"\nmodel = \"m\"\n\n[notes]\nenforce_headings = false\n\n"
    ));
    let out = r.out.clone();
    r.engine
        .dispatch(&Action::HookAdd {
            name: "notes".into(),
            event: "notes.generated".into(),
            command: json!([SPY, out.to_str().unwrap()]),
            shell: Some(true),
            timeout: None,
            blocking: None,
            enabled: None,
            cwd: None,
            r#match: None,
        })
        .unwrap();
    record(&r, "noted", "Noted");
    r.engine
        .dispatch(&Action::TemplateSave {
            name: "Plain".into(),
            system_prompt: "Write a summary.".into(),
            icon: None,
            previous: None,
            overwrite: None,
        })
        .unwrap();
    // A dry run sends nothing and writes nothing: no hook.
    r.engine
        .dispatch(&Action::NotesGenerate {
            session: "noted".into(),
            template: "Plain".into(),
            dry_run: Some(true),
        })
        .unwrap();
    std::thread::sleep(Duration::from_millis(300));
    assert!(!out.join("notes.generated.env").exists());
    r.engine
        .dispatch(&Action::NotesGenerate {
            session: "noted".into(),
            template: "Plain".into(),
            dry_run: None,
        })
        .unwrap();
    let (env, stdin) = seen(&out, "notes.generated");
    check_envelope(&out, "notes.generated");
    assert_eq!(env[1], "noted");
    assert_eq!(env[3], "Noted");
    assert_eq!(stdin["data"]["template"], "Plain");
}

#[test]
fn a_hook_past_its_timeout_is_killed_with_its_children() {
    let r = rig("");
    let late = r.out.join("late");
    let script = format!("sleep 30; touch {:?}", late.to_str().unwrap());
    r.engine
        .dispatch(&Action::HookAdd {
            name: "slow".into(),
            event: "record.stop".into(),
            command: json!(script),
            shell: Some(true),
            timeout: Some("300ms".into()),
            blocking: None,
            enabled: None,
            cwd: None,
            r#match: None,
        })
        .unwrap();
    let t = Instant::now();
    let run = r
        .engine
        .dispatch(&Action::HookTest {
            hook: "slow".into(),
            session: None,
            event: None,
        })
        .unwrap();
    assert!(t.elapsed() < Duration::from_secs(10), "{:?}", t.elapsed());
    assert_eq!(run["timed_out"], true, "{run}");
    assert_eq!(run["ok"], false);
    assert!(
        run["error"]
            .as_str()
            .unwrap()
            .contains("timed out after 300ms"),
        "{run}"
    );
    assert_eq!(run["test"], true);
    std::thread::sleep(Duration::from_millis(500));
    assert!(!late.exists(), "the shell's sleep was killed too");
    // Outside app.hook_timeout's range is refused.
    let e = r
        .engine
        .dispatch(&Action::HookEdit {
            hook: "slow".into(),
            name: None,
            event: None,
            command: None,
            shell: None,
            timeout: Some("2h".into()),
            blocking: None,
            enabled: None,
            cwd: None,
            r#match: None,
        })
        .unwrap_err();
    assert_eq!(e.code, "invalid_value", "{e}");
}

#[test]
fn failing_missing_and_slow_hooks_never_touch_the_session() {
    let r = rig(
        "[[hooks]]\nevent = \"record.start\"\ncommand = [\"/nonexistent/oats-hook\"]\n\n\
         [[hooks]]\nevent = \"record.stop\"\nshell = true\ncommand = [\"echo boom >&2; exit 3\"]\n\n\
         [[hooks]]\nname = \"slow\"\nevent = \"record.stop\"\ncommand = [\"sleep\", \"5\"]\n\n",
    );
    let events = r.engine.bus.subscribe(vec!["hook-finished".into()]);
    r.engine
        .dispatch(&Action::RecordStart {
            session: Some("safe".into()),
            title: None,
        })
        .unwrap();
    std::thread::sleep(Duration::from_millis(1200));
    let t = Instant::now();
    let stop = r.engine.dispatch(&Action::RecordStop { by: None }).unwrap();
    // The 5 s hook runs in the background: stop does not wait for it.
    assert!(t.elapsed() < Duration::from_secs(4), "{:?}", t.elapsed());
    assert_eq!(stop["session"], "safe");
    let s = r
        .engine
        .dispatch(&Action::SessionGet {
            session: "safe".into(),
        })
        .unwrap();
    assert_eq!(s["state"], "saved", "{s}");
    assert!(s["utterance_count"].as_u64().unwrap() > 0, "{s}");
    // Both failures are reported on the bus and kept with their output.
    let mut finished = vec![];
    while finished.len() < 2 {
        let line = events.recv_timeout(Duration::from_secs(20)).unwrap();
        finished.push(serde_json::from_str::<Value>(&line).unwrap()["data"].clone());
    }
    let list = r.engine.dispatch(&Action::HookList {}).unwrap();
    let missing = &list["hooks"][0]["last_run"];
    assert_eq!(missing["ok"], false, "{list}");
    assert!(
        missing["error"].as_str().unwrap().contains("cannot run"),
        "{missing}"
    );
    let boom = &list["hooks"][1]["last_run"];
    assert_eq!(boom["exit_code"], 3, "{list}");
    assert_eq!(boom["stderr"], "boom\n");
    assert!(finished.iter().all(|f| f["ok"] == false), "{finished:?}");
    // The engine still records: a second session starts and stops.
    r.engine
        .dispatch(&Action::RecordStart {
            session: Some("again".into()),
            title: None,
        })
        .unwrap();
    r.engine.dispatch(&Action::RecordStop { by: None }).unwrap();
}

#[test]
fn hook_actions_edit_the_hooks_tables_in_the_user_file() {
    let r = rig("");
    let add = |name: &str, event: &str| {
        r.engine.dispatch(&Action::HookAdd {
            name: name.into(),
            event: event.into(),
            command: json!(["notify-send", "done"]),
            shell: None,
            timeout: Some("10s".into()),
            blocking: None,
            enabled: None,
            cwd: None,
            r#match: None,
        })
    };
    let v = add("ping", "record.stop").unwrap();
    assert_eq!(v["name"], "ping");
    assert_eq!(v["timeout"], "10s");
    add("other", "session.saved").unwrap();
    let text = std::fs::read_to_string(&r.cfg).unwrap();
    assert!(text.starts_with("# my hooks\n"), "{text}");
    assert!(text.contains("[[hooks]]\nname = \"ping\""), "{text}");
    assert!(
        text.contains("command = [\"notify-send\", \"done\"]"),
        "{text}"
    );
    // An unknown event, a duplicate name and an empty command are refused before the file.
    assert_eq!(
        add("bad", "record.explode").unwrap_err().code,
        "invalid_value"
    );
    assert_eq!(
        add("ping", "record.stop").unwrap_err().code,
        "invalid_value"
    );
    assert_eq!(std::fs::read_to_string(&r.cfg).unwrap(), text);
    let off = r
        .engine
        .dispatch(&Action::HookEnable {
            hook: "ping".into(),
            enabled: Some(false),
        })
        .unwrap();
    assert_eq!(off["enabled"], false);
    r.engine
        .dispatch(&Action::HookEdit {
            hook: "2".into(),
            name: Some("renamed".into()),
            event: Some("record.start".into()),
            command: Some(json!("echo hi")),
            shell: None,
            timeout: Some(String::new()),
            blocking: None,
            enabled: None,
            cwd: Some("/tmp".into()),
            r#match: None,
        })
        .unwrap();
    let hooks = r.engine.snapshot().config.hooks.clone();
    assert_eq!(hooks.len(), 2);
    assert!(!hooks[0].enabled);
    assert_eq!(hooks[1].name, "renamed");
    assert_eq!(hooks[1].command, vec!["echo", "hi"]);
    assert_eq!(hooks[1].timeout, None);
    // A disabled hook does not fire, but hook-test still runs it.
    r.engine
        .dispatch(&Action::HookRemove {
            hook: "renamed".into(),
        })
        .unwrap();
    r.engine
        .dispatch(&Action::HookRemove {
            hook: "ping".into(),
        })
        .unwrap();
    let text = std::fs::read_to_string(&r.cfg).unwrap();
    assert!(!text.contains("[[hooks]]"), "{text}");
    assert!(r.engine.snapshot().config.hooks.is_empty());
    let e = r
        .engine
        .dispatch(&Action::HookTest {
            hook: "ping".into(),
            session: None,
            event: None,
        })
        .unwrap_err();
    assert_eq!(e.code, "not_found");
}

#[test]
fn a_disabled_hook_stays_quiet_and_hook_test_runs_it_for_a_session() {
    let r = rig("");
    let out = r.out.clone();
    std::fs::write(
        &r.cfg,
        std::fs::read_to_string(&r.cfg).unwrap()
            + &format!(
                "[[hooks]]\nname = \"quiet\"\nevent = \"record.stop\"\nenabled = false\nshell = true\ncommand = [{SPY:?}, {:?}]\n",
                out.to_str().unwrap()
            ),
    )
    .unwrap();
    r.engine.reload("test").unwrap();
    record(&r, "q1", "Quiet");
    std::thread::sleep(Duration::from_millis(500));
    assert!(!out.join("record.stop.env").exists(), "disabled hook ran");
    let run = r
        .engine
        .dispatch(&Action::HookTest {
            hook: "quiet".into(),
            session: Some("q1".into()),
            event: None,
        })
        .unwrap();
    assert_eq!(run["ok"], true, "{run}");
    let (env, stdin) = seen(&out, "record.stop");
    assert_eq!(env[1], "q1");
    assert_eq!(env[3], "Quiet");
    assert_eq!(stdin["data"]["test"], true);
    // An unknown session is refused.
    let e = r
        .engine
        .dispatch(&Action::HookTest {
            hook: "quiet".into(),
            session: Some("nope".into()),
            event: None,
        })
        .unwrap_err();
    assert_eq!(e.code, "not_found", "{e}");
}

/// The raw log as `session-events` returns it: (seq, topic, data) per line.
fn session_events(
    r: &Rig,
    session: Option<&str>,
    topic: Option<&str>,
) -> Vec<(u64, String, Value)> {
    let v = r
        .engine
        .dispatch(&Action::SessionEvents {
            session: session.map(str::to_string),
            topic: topic.map(str::to_string),
            from: None,
            to: None,
        })
        .unwrap();
    assert_eq!(v["skipped"], 0, "{v}");
    v["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| {
            (
                e["seq"].as_u64().unwrap(),
                e["topic"].as_str().unwrap().to_string(),
                e["data"].clone(),
            )
        })
        .collect()
}

#[test]
fn a_recording_logs_every_message_naming_it_in_seq_order_and_session_events_reads_it() {
    let r = rig("");
    let out = r.out.clone();
    std::fs::write(
        &r.cfg,
        std::fs::read_to_string(&r.cfg).unwrap()
            + &format!(
                "[[hooks]]\nname = \"spy-record.stop\"\nevent = \"record.stop\"\nshell = true\ncommand = [{SPY:?}, {:?}]\n",
                out.to_str().unwrap()
            ),
    )
    .unwrap();
    r.engine.reload("test").unwrap();
    let e = &r.engine;
    e.dispatch(&Action::RecordStart {
        session: Some("logged".into()),
        title: Some("Logged call".into()),
    })
    .unwrap();
    // A live transcript segment, as the live worker publishes it, and a message naming no session.
    e.bus.publish(
        "segment",
        json!({"session": "logged", "kind": "live", "speaker": "you", "text": "pricing", "start": 0.0, "end": 1.0}),
    );
    e.bus.publish("setting-changed", json!({"key": "ui.theme"}));
    e.dispatch(&Action::RecordPause {}).unwrap();
    e.dispatch(&Action::RecordPause {}).unwrap();
    std::thread::sleep(Duration::from_millis(1500));
    e.dispatch(&Action::RecordStop { by: None }).unwrap();
    let (_, stop) = seen(&out, "record.stop");
    // The hook's own hook-finished names the session too: wait for it to be logged.
    wait_until("hook-finished in the log", || {
        session_events(&r, Some("logged"), Some("hook-finished")).len() == 1
    });
    let log = session_events(&r, Some("logged"), None);
    assert!(
        r.root.join("rec/logged/events.jsonl").is_file(),
        "the log is a file of the session"
    );
    // Ordered and gap-free from 1.
    let seqs: Vec<u64> = log.iter().map(|l| l.0).collect();
    assert_eq!(seqs, (1..=log.len() as u64).collect::<Vec<_>>(), "{log:?}");
    let topics: Vec<&str> = log.iter().map(|l| l.1.as_str()).collect();
    assert_eq!(topics[0], "recording-started", "{topics:?}");
    assert!(
        log.iter()
            .any(|l| l.1 == "segment" && l.2["text"] == "pricing"),
        "the live segment is logged: {log:?}"
    );
    for t in [
        "recording-paused",
        "recording-stopped",
        "session-saved",
        "hook-finished",
    ] {
        assert!(topics.contains(&t), "{t} missing: {topics:?}");
    }
    assert!(!topics.contains(&"setting-changed"), "{topics:?}");
    // seq is the hook envelope's: record.stop stands for the logged recording-stopped.
    let stopped = log.iter().find(|l| l.1 == "recording-stopped").unwrap();
    assert_eq!(stop["seq"], stopped.0, "{stop}");
    assert_eq!(stop["data"], stopped.2);
    // Filters; the newest session is the default.
    assert_eq!(session_events(&r, None, None).len(), log.len());
    let segs = session_events(&r, None, Some("segment"));
    assert!(!segs.is_empty() && segs.iter().all(|l| l.1 == "segment"));
    let v = e
        .dispatch(&Action::SessionEvents {
            session: Some("logged".into()),
            topic: None,
            from: Some(2),
            to: Some(3),
        })
        .unwrap();
    assert_eq!(v["session"], "logged");
    let got: Vec<u64> = v["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["seq"].as_u64().unwrap())
        .collect();
    assert_eq!(got, vec![2, 3]);
    // hook-test is no bus message: it takes the last seq and leaves no gap.
    let last = log.len() as u64;
    let run = e
        .dispatch(&Action::HookTest {
            hook: "spy-record.stop".into(),
            session: Some("logged".into()),
            event: None,
        })
        .unwrap();
    assert_eq!(run["event_seq"], last, "{run}");
    let after = session_events(&r, Some("logged"), None);
    let seqs: Vec<u64> = after.iter().map(|l| l.0).collect();
    assert_eq!(seqs, (1..=after.len() as u64).collect::<Vec<_>>());
    // An unknown session is refused.
    let err = e
        .dispatch(&Action::SessionEvents {
            session: Some("nope".into()),
            topic: None,
            from: None,
            to: None,
        })
        .unwrap_err();
    assert_eq!(err.code, "not_found", "{err}");
}

#[test]
fn bad_hooks_tables_veto_the_config() {
    let r = rig("");
    let before = std::fs::read_to_string(&r.cfg).unwrap();
    for bad in [
        "[[hooks]]\nevent = \"nope\"\ncommand = [\"true\"]\n",
        "[[hooks]]\nevent = \"record.stop\"\ncommand = []\n",
        "[[hooks]]\nevent = \"record.stop\"\ncommand = [\"true\"]\nbogus = 1\n",
        "[[hooks]]\nevent = \"record.stop\"\ncommand = [\"true\"]\ntimeout = \"1ms\"\n",
        "hooks = \"x\"\n",
    ] {
        std::fs::write(&r.cfg, format!("{bad}\n{before}")).unwrap();
        let e = r.engine.reload("test").unwrap_err();
        assert!(
            matches!(e.code, "invalid_value" | "type_mismatch"),
            "{bad}: {e}"
        );
        assert!(r.engine.snapshot().config.hooks.is_empty());
    }
}

#[test]
fn a_hook_fires_at_most_once_per_dedup_key() {
    let r = rig("");
    let out = r.out.clone();
    let log = |name: &str, event: &str| Action::HookAdd {
        name: name.into(),
        event: event.into(),
        command: json!([
            // One file per run (runs of one hook overlap): <hook>.<run id>.json.
            r#"cat > "$1/$OATS_HOOK.$OATS_RUN_ID.tmp"; mv "$1/$OATS_HOOK.$OATS_RUN_ID.tmp" "$1/$OATS_HOOK.$OATS_RUN_ID.json""#,
            out.to_str().unwrap()
        ]),
        shell: Some(true),
        timeout: None,
        blocking: None,
        enabled: None,
        cwd: None,
        r#match: None,
    };
    r.engine
        .dispatch(&log("cal-a", "calendar.matched"))
        .unwrap();
    r.engine
        .dispatch(&log("cal-b", "calendar.matched"))
        .unwrap();
    r.engine.dispatch(&log("stop", "record.stop")).unwrap();
    r.engine.dispatch(&log("pause", "record.pause")).unwrap();
    let e1 = json!({"session": "d1", "calendar": {"event": {"id": "e1", "title": "Standup"}}});
    // The match at start, at stop and a stop seen twice are one calendar.matched and one stop.
    r.engine.bus.publish("recording-started", e1.clone());
    r.engine.bus.publish("recording-stopped", e1.clone());
    r.engine.bus.publish("recording-stopped", e1.clone());
    // Another event, and another session, are new keys; every pause is its own occurrence.
    r.engine.bus.publish(
        "recording-stopped",
        json!({"session": "d1", "calendar": {"event": {"id": "e2"}}}),
    );
    r.engine
        .bus
        .publish("recording-stopped", json!({"session": "d2"}));
    for _ in 0..2 {
        r.engine.bus.publish(
            "recording-paused",
            json!({"session": "d2", "paused": true, "duration_ms": 5}),
        );
    }
    let lines = |hook: &str| -> Vec<Value> {
        std::fs::read_dir(&out)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with(&format!("{hook}.")) && n.ends_with(".json"))
            .map(|n| serde_json::from_str(&std::fs::read_to_string(out.join(n)).unwrap()).unwrap())
            .collect()
    };
    let total = || {
        r.engine.dispatch(&Action::HookList {}).unwrap()["runs"]
            .as_array()
            .unwrap()
            .len()
    };
    wait_until("the hook runs", || total() >= 8);
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(total(), 8, "{:?}", r.engine.dispatch(&Action::HookList {}));
    for hook in ["cal-a", "cal-b"] {
        let mut keys: Vec<String> = lines(hook)
            .iter()
            .map(|e| e["dedup"].as_str().unwrap().to_string())
            .collect();
        keys.sort();
        assert_eq!(
            keys,
            ["d1:calendar.matched:e1", "d1:calendar.matched:e2"],
            "{hook}"
        );
    }
    let mut stops: Vec<String> = lines("stop")
        .iter()
        .map(|e| e["dedup"].as_str().unwrap().to_string())
        .collect();
    stops.sort();
    assert_eq!(stops, ["d1:record.stop", "d2:record.stop"]);
    let pauses = lines("pause");
    assert_eq!(pauses.len(), 2);
    assert_ne!(pauses[0]["dedup"], pauses[1]["dedup"]);
    // hook-test is outside dedup: it runs every time.
    for _ in 0..2 {
        let run = r
            .engine
            .dispatch(&Action::HookTest {
                hook: "stop".into(),
                session: None,
                event: None,
            })
            .unwrap();
        assert_eq!(run["ok"], true, "{run}");
    }
}

#[test]
fn hook_events_is_the_table_and_the_committed_file() {
    let r = rig("");
    let v = r
        .engine
        .dispatch(&Action::HookEvents { event: None })
        .unwrap();
    assert_eq!(v, hooks::events::table_json());
    let committed: Value = serde_json::from_str(
        &std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../config/hook-events.json"
        ))
        .unwrap(),
    )
    .unwrap();
    assert_eq!(
        v, committed,
        "config/hook-events.json drifted: run config generate"
    );
    let names: Vec<&str> = v["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, hooks::names());
    for e in v["events"].as_array().unwrap() {
        let spec = hooks::events::find(e["name"].as_str().unwrap()).unwrap();
        hooks::events::validate(&hooks::events::envelope_schema(Some(spec)), &e["example"])
            .unwrap_or_else(|err| panic!("{}: {err}", e["name"]));
    }
    let one = r
        .engine
        .dispatch(&Action::HookEvents {
            event: Some("record.stop".into()),
        })
        .unwrap();
    assert_eq!(one["events"].as_array().unwrap().len(), 1);
    assert_eq!(one["events"][0]["name"], "record.stop");
    let e = r
        .engine
        .dispatch(&Action::HookEvents {
            event: Some("record.explode".into()),
        })
        .unwrap_err();
    assert_eq!(e.code, "not_found");
    // hook-list's event picker and the hook-add / hook-edit docs come from the table too.
    let list = r.engine.dispatch(&Action::HookList {}).unwrap();
    assert_eq!(list["events"], json!(hooks::names()));
    assert_eq!(list["event_docs"].as_array().unwrap().len(), names.len());
    let manifest = oats_core::action::manifest_json();
    for verb in ["hook-add", "hook-edit"] {
        let doc = manifest["actions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|a| a["name"] == verb)
            .unwrap()["doc"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(!doc.contains("{hook-events}"), "{doc}");
        assert!(doc.contains(&hooks::names().join(", ")), "{verb}: {doc}");
    }
    // The [[hooks]] schema's event enum is the table.
    assert_eq!(
        hooks::item_schema()["properties"]["event"]["enum"],
        json!(hooks::names())
    );
}
