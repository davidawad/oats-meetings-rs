//! Configurable speaker labels (dot-4uzgol.86): transcripts keep the stable ids (`you`,
//! `remote_N`, `room_N`); the `[speakers]` table and the session's `speakerNames` label them when
//! they are shown or exported. Editing the config relabels old sessions without rewriting them.

use std::path::Path;
use std::sync::Arc;

use oats_core::action::Action;
use oats_core::control::engine::{Engine, EngineOpts};
use oats_core::sessions::Store;
use oats_core::stt::{utterance_json, Utt};
use serde_json::{json, Value};

fn engine(dir: &Path, toml: &str) -> Arc<Engine> {
    let cfg = dir.join("config.toml");
    std::fs::write(&cfg, toml).unwrap();
    Engine::new(EngineOpts {
        user_config: cfg,
        pack: None,
        os: "linux".into(),
        arch: "x86_64".into(),
        machine: "default".into(),
        sessions_dir: dir.join("sessions"),
        secrets: None,
    })
    .map_err(|e| e.to_string())
    .unwrap()
}

/// A finished session `id` under `dir/sessions` whose final transcript says `speakers` in turn.
fn session(dir: &Path, id: &str, speakers: &[&str]) {
    let s = Store::new(dir.join("sessions"));
    s.create(id, "2026-11-06T15:00:00Z", "", "test").unwrap();
    let started = chrono::DateTime::parse_from_rfc3339("2026-11-06T15:00:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc);
    for (i, sp) in speakers.iter().enumerate() {
        let at = i as f64 * 2.0;
        let u = Utt {
            speaker: sp.to_string(),
            text: format!("{sp} line {i}"),
            start: at,
            end: at + 1.0,
        };
        s.append_utterance(id, "final", &utterance_json(&u, started))
            .unwrap();
    }
    s.finish(id, "2026-11-06T15:01:00Z").unwrap();
}

fn transcript_file(dir: &Path, id: &str) -> Vec<u8> {
    std::fs::read(dir.join("sessions").join(id).join("transcript.final.jsonl")).unwrap()
}

fn labels(e: &Engine, id: &str) -> Value {
    e.dispatch(&Action::TranscriptGet { session: id.into() })
        .unwrap()["speakers"]
        .clone()
}

fn act(v: Value) -> Action {
    serde_json::from_value(v).unwrap()
}

#[test]
fn editing_the_config_relabels_a_saved_session_without_rewriting_it() {
    let d = tempfile::tempdir().unwrap();
    session(d.path(), "meet", &["you", "remote_1", "remote_2", "you"]);
    session(d.path(), "pair", &["you", "remote_1"]);
    let before = transcript_file(d.path(), "meet");
    let e = engine(d.path(), "");
    assert_eq!(
        labels(&e, "meet"),
        json!({"you": "You", "remote_1": "Speaker 1", "remote_2": "Speaker 2"})
    );
    // One remote voice: the single label.
    assert_eq!(
        labels(&e, "pair"),
        json!({"you": "You", "remote_1": "Them"})
    );

    for (k, v) in [
        ("speakers.mic.label", "David"),
        ("speakers.system.template", "Guest {n}"),
        ("speakers.system.label_single", "Caller"),
    ] {
        e.set(k, &json!(v), "test").unwrap();
    }
    assert_eq!(
        labels(&e, "meet"),
        json!({"you": "David", "remote_1": "Guest 1", "remote_2": "Guest 2"})
    );
    assert_eq!(
        labels(&e, "pair"),
        json!({"you": "David", "remote_1": "Caller"})
    );
    // A per-session name beats the template, in that session only.
    e.dispatch(&act(json!({"action": "session-speaker-name", "session": "meet", "speaker": "remote_2", "name": "Dana"})))
        .unwrap();
    assert_eq!(labels(&e, "meet")["remote_2"], "Dana");
    assert_eq!(labels(&e, "meet")["remote_1"], "Guest 1");
    // Back to the defaults: back to the old labels. The transcript never changed.
    for (k, v) in [
        ("speakers.mic.label", "You"),
        ("speakers.system.template", "Speaker {n}"),
        ("speakers.system.label_single", "Them"),
    ] {
        e.set(k, &json!(v), "test").unwrap();
    }
    assert_eq!(labels(&e, "meet")["remote_1"], "Speaker 1");
    assert_eq!(transcript_file(d.path(), "meet"), before);
    let t = e
        .dispatch(&Action::TranscriptGet {
            session: "meet".into(),
        })
        .unwrap();
    assert_eq!(t["utterances"][2]["speaker"], "remote_2");
    assert_eq!(t["speaker_names"], json!({"remote_2": "Dana"}));
}

#[test]
fn templates_must_contain_n_and_labels_must_not_be_blank() {
    let d = tempfile::tempdir().unwrap();
    let e = engine(d.path(), "");
    for (k, bad) in [
        ("speakers.system.template", "Speaker"),
        ("speakers.mic.template", "Room n"),
        ("speakers.mic.label", " "),
        ("speakers.system.label_single", ""),
    ] {
        let err = e.set(k, &json!(bad), "test").unwrap_err();
        assert_eq!(err.code, "invalid_value", "{k}={bad:?}: {err}");
        assert!(err.message.contains(k), "{k}: {err}");
        // The running config is kept and still checks out.
        assert_eq!(
            e.dispatch(&act(json!({"action": "config-check"}))).unwrap()["valid"],
            true
        );
    }
    e.set("speakers.system.template", &json!("{n}. caller"), "test")
        .unwrap();

    // A config file with a bad template does not check (nor load).
    let bad = tempfile::tempdir().unwrap();
    let cfg = bad.path().join("config.toml");
    std::fs::write(&cfg, "[speakers.mic]\ntemplate = \"Room\"\n").unwrap();
    let err = Engine::new(EngineOpts {
        user_config: cfg,
        pack: None,
        os: "linux".into(),
        arch: "x86_64".into(),
        machine: "default".into(),
        sessions_dir: bad.path().join("sessions"),
        secrets: None,
    })
    .err()
    .expect("a template without {n} is refused");
    assert!(err.message.contains("must contain {n}"), "{err}");

    // The retired id keys say what replaced them.
    let err = e
        .set("stt.speaker_them", &json!("caller"), "test")
        .unwrap_err();
    assert_eq!(err.code, "unknown_key");
    assert!(
        err.message.contains("speakers.system.label_single"),
        "{err}"
    );
}

fn export(e: &Engine, d: &Path, id: &str) -> String {
    let out = d.join("out");
    let v = e
        .dispatch(&act(
            json!({"action": "session-export", "session": id, "dir": out}),
        ))
        .unwrap();
    std::fs::read_to_string(v["path"].as_str().unwrap()).unwrap()
}

#[test]
fn the_export_maps_speakers_as_openoats_does_or_by_label() {
    let d = tempfile::tempdir().unwrap();
    session(
        d.path(),
        "room",
        &["room_1", "room_2", "remote_1", "remote_2"],
    );
    let e = engine(d.path(), "[speakers.mic]\nlabel = \"David\"\n");
    e.dispatch(&act(json!({"action": "session-speaker-name", "session": "room", "speaker": "remote_2", "name": "Dana"})))
        .unwrap();
    // openoats (the default): the mic side is You, everything else Them, unless named.
    let md = export(&e, d.path(), "room");
    assert!(
        md.contains("participants:\n  - You\n  - Them\n  - Dana\n"),
        "{md}"
    );
    assert!(md.contains("**You:** room_1 line 0"), "{md}");
    assert!(md.contains("**You:** room_2 line 1"), "{md}");
    assert!(md.contains("**Them:** remote_1 line 2"), "{md}");
    assert!(md.contains("**Dana:** remote_2 line 3"), "{md}");
    // labels: what the app shows.
    e.set("speakers.export", &json!("labels"), "test").unwrap();
    let md = export(&e, d.path(), "room");
    assert!(
        md.contains("participants:\n  - Room 1\n  - Room 2\n  - Speaker 1\n  - Dana\n"),
        "{md}"
    );
    assert!(md.contains("**Room 2:** room_2 line 1"), "{md}");
    assert!(md.contains("**Speaker 1:** remote_1 line 2"), "{md}");
    // A single mic voice and a single remote one are the plain labels.
    session(d.path(), "pair", &["you", "remote_1"]);
    let md = export(&e, d.path(), "pair");
    assert!(md.contains("**David:** you line 0"), "{md}");
    assert!(md.contains("**Them:** remote_1 line 1"), "{md}");
}
