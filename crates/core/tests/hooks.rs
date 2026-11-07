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
            "# my hooks\n[sessions]\nroot = {:?}\n\n[stt.live]\nchunk = \"1s\"\n\n{toml}",
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

/// Writes every OATS_* variable to `<out>/<event>.env` (one per line) and stdin to `<event>.json`,
/// each replaced whole (two runs of one event may overlap).
const SPY: &str = r#"t="$1/$OATS_EVENT.$$"; printf '%s\n' "$OATS_EVENT" "$OATS_SESSION_ID" "$OATS_SESSION_DIR" "$OATS_TITLE" "$OATS_AUDIO" "$OATS_TRANSCRIPT" "$OATS_HOOK" "$PWD" > "$t.env"; cat > "$t.json"; mv "$t.json" "$1/$OATS_EVENT.json"; mv "$t.env" "$1/$OATS_EVENT.env""#;

fn spy_hooks(out: &Path) -> String {
    hooks::EVENTS
        .iter()
        .map(|e| {
            format!(
                "[[hooks]]\nname = \"spy-{e}\"\nevent = \"{e}\"\nshell = true\ncommand = [{SPY:?}, {:?}]\n\n",
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
    assert_eq!(r.engine.snapshot().config.hooks.len(), hooks::EVENTS.len());
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
        assert_eq!(stdin["event"], event);
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
    // A recording that matched a calendar event: the engine publishes the match in the
    // recording-started / -stopped payload (calendar.event), which is what calendar.matched follows.
    r.engine.bus.publish(
        "recording-stopped",
        json!({"session": "hooked", "calendar": {"event": {"title": "Standup", "id": "e1"}}}),
    );
    let (env, stdin) = seen(&out, "calendar.matched");
    assert_eq!(env[1], "hooked");
    assert_eq!(stdin["data"]["calendar"]["event"]["title"], "Standup");
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
            enabled: None,
            cwd: None,
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
            enabled: None,
            cwd: None,
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
            enabled: None,
            cwd: None,
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
            enabled: None,
            cwd: None,
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
            enabled: None,
            cwd: Some("/tmp".into()),
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
