//! OpenOats compatibility (dot-4uzgol.85): golden round trips against data shaped as OpenOats
//! (yazinsai/OpenOats b4353cd7) writes it, under tests/fixtures/openoats-golden (synthetic, see its
//! README). oats-rs must read OpenOats' sessions and templates, write them back so OpenOats still
//! decodes them, and produce OpenOats' markdown export byte for byte.
//!
//! A private OpenOats sessions folder (real meetings, never committed) is checked the same way when
//! `OATS_OPENOATS_PRIVATE` names it, or it sits at tests/fixtures/private/openoats (gitignored).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use oats_core::action::Action;
use oats_core::control::engine::{Engine, EngineOpts};
use oats_core::openoats;
use oats_core::sessions::Store;
use oats_core::stt::{utterance_json, Utt};
use oats_core::templates;
use serde_json::{json, Map, Value};

const ID: &str = "session_2026-11-02_14-30-05";

fn golden() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/openoats-golden")
}

fn golden_session() -> PathBuf {
    golden().join("sessions").join(ID)
}

fn ny() -> chrono_tz::Tz {
    "America/New_York".parse().unwrap()
}

fn json_of(p: &Path) -> Value {
    serde_json::from_slice(&std::fs::read(p).unwrap()).unwrap()
}

fn jsonl(p: &Path) -> Vec<Value> {
    std::fs::read_to_string(p)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

/// Every file under `dir` by relative path, session.json excluded.
fn files(dir: &Path) -> Vec<(String, Vec<u8>)> {
    fn walk(d: &Path, pre: &str, out: &mut Vec<(String, Vec<u8>)>) {
        for e in std::fs::read_dir(d).unwrap().flatten() {
            let n = format!("{pre}{}", e.file_name().to_string_lossy());
            if e.path().is_dir() {
                walk(&e.path(), &format!("{n}/"), out);
            } else if n != "session.json" {
                out.push((n, std::fs::read(e.path()).unwrap()));
            }
        }
    }
    let mut v = vec![];
    walk(dir, "", &mut v);
    v.sort();
    v
}

fn without_x_oats(mut v: Value) -> Value {
    v.as_object_mut().unwrap().remove("x_oats");
    v
}

/// What OpenOats' decoders need of a session folder: session.json decodes, every transcript line
/// decodes, notes.md has a notes.meta.json that decodes.
fn assert_openoats_reads(dir: &Path) {
    let meta = json_of(&dir.join("session.json"));
    assert_eq!(openoats::decodes_meta(&meta), Ok(()), "{}", dir.display());
    for f in ["transcript.live.jsonl", "transcript.final.jsonl"] {
        if dir.join(f).exists() {
            for r in jsonl(&dir.join(f)) {
                assert_eq!(openoats::decodes_record(&r), Ok(()), "{f}");
            }
        }
    }
    if dir.join("notes.md").exists() {
        assert!(
            openoats::notes_meta_ok(&json_of(&dir.join("notes.meta.json"))),
            "{}",
            dir.display()
        );
    }
}

#[test]
fn the_golden_sample_is_what_openoats_decodes() {
    assert_openoats_reads(&golden_session());
}

#[test]
fn import_reads_an_openoats_session_and_keeps_every_file() {
    let d = tempfile::tempdir().unwrap();
    let s = Store::new(d.path());
    let r = s.import(&golden().join("sessions"), false).unwrap();
    assert_eq!(r["imported"], json!([ID]));

    let g = s.get(ID).unwrap();
    assert_eq!(g["title"], "Q4 \"Roadmap\" review");
    assert_eq!(g["started_at"], "2026-11-02T19:30:05Z");
    assert_eq!(g["transcript_source"], "live");
    assert_eq!(g["utterance_count"], 5);
    assert_eq!(g["tags"], json!(["customer"]));
    assert_eq!(g["speaker_names"]["remote_2"], "Priya");
    let t = s.transcript(ID).unwrap();
    assert_eq!(t["utterances"][0]["refinedText"], "OK, let's start.");
    let n = s.notes(ID).unwrap();
    assert_eq!(n["has_notes"], true);
    assert_eq!(n["template"], "Generic", "from notes.meta.json");
    let hits = s.search("numbers", 10).unwrap();
    assert_eq!(hits["hits"][0]["offset"], 95.0);

    // Byte for byte: transcripts, the pre-cleanup backup, notes, notes.meta.json, attachments.
    let dir = d.path().join(ID);
    assert_eq!(files(&dir), files(&golden_session()));
    // session.json: OpenOats' keys untouched; ours only under x_oats.
    let meta = json_of(&dir.join("session.json"));
    assert_eq!(meta["x_oats"]["schemaVersion"], 1);
    assert_eq!(
        without_x_oats(meta),
        json_of(&golden_session().join("session.json"))
    );
    assert_openoats_reads(&dir);

    // Edits keep it OpenOats-readable and keep OpenOats' keys.
    s.rename(ID, "Renamed").unwrap();
    s.tag(ID, "q4", false).unwrap();
    s.name_speaker(ID, "them", "Sam").unwrap();
    s.edit_notes(ID, "# Edited\n").unwrap();
    let meta = json_of(&dir.join("session.json"));
    assert_eq!(meta["templateSnapshot"]["name"], "Generic");
    assert_eq!(meta["meetingApp"], "zoom.us");
    assert_eq!(meta["tags"], json!(["customer", "q4"]));
    assert_eq!(
        std::fs::read(dir.join("notes.meta.json")).unwrap(),
        std::fs::read(golden_session().join("notes.meta.json")).unwrap(),
        "a hand edit keeps the generated notes' meta"
    );
    assert_openoats_reads(&dir);
}

/// Our export with the `x_oats_*` lines (which OpenOats ignores) taken out.
fn openoats_lines(md: &str) -> String {
    md.split_inclusive('\n')
        .filter(|l| !l.starts_with("x_oats_"))
        .collect()
}

#[test]
fn markdown_export_matches_openoats_byte_for_byte() {
    let d = tempfile::tempdir().unwrap();
    let s = Store::new(d.path().join("sessions"));
    s.import(&golden().join("sessions"), false).unwrap();
    let out = d.path().join("export");
    let v = s.export_markdown(ID, &out, "Test Recorder", ny()).unwrap();
    let path = PathBuf::from(v["path"].as_str().unwrap());
    assert_eq!(
        path.file_name().unwrap(),
        "2026-11-02-1430-q4-roadmap-review.md"
    );
    let md = std::fs::read_to_string(&path).unwrap();
    let expected = std::fs::read_to_string(golden().join("expected-export.md")).unwrap();
    assert_eq!(openoats_lines(&md), expected);
    assert!(md.contains("\nx_oats_tags: [\"customer\"]\n"), "{md}");

    // Re-export replaces the file that names this session, as OpenOats re-mirrors.
    s.rename(ID, "Other title").unwrap();
    let again = s.export_markdown(ID, &out, "Test Recorder", ny()).unwrap();
    assert_eq!(again["path"], v["path"]);
    assert!(std::fs::read_to_string(&path)
        .unwrap()
        .contains("title: \"Other title\""));
    assert_eq!(std::fs::read_dir(&out).unwrap().count(), 1);

    // Another session on the same minute with the same title gets -2 (the renamed one kept its
    // file name, as OpenOats keeps it); no recorder leaves the line out.
    let other = d.path().join("src/session_other");
    std::fs::create_dir_all(&other).unwrap();
    for f in ["session.json", "transcript.live.jsonl"] {
        std::fs::copy(golden_session().join(f), other.join(f)).unwrap();
    }
    let mut m = json_of(&other.join("session.json"));
    m["id"] = json!("session_other");
    std::fs::write(other.join("session.json"), m.to_string()).unwrap();
    s.import(&other, false).unwrap();
    let w = s.export_markdown("session_other", &out, "", ny()).unwrap();
    assert!(w["path"]
        .as_str()
        .unwrap()
        .ends_with("2026-11-02-1430-q4-roadmap-review-2.md"));
    let md2 = std::fs::read_to_string(w["path"].as_str().unwrap()).unwrap();
    assert!(!md2.contains("recorder:"));
    assert!(md2.contains("x_openoats_session: \"session_other\""));
    assert!(!md2.contains("## Notes"), "no notes.md, no Notes section");
}

#[test]
fn export_maps_diarized_speakers_to_them_unless_named() {
    let d = tempfile::tempdir().unwrap();
    let s = Store::new(d.path());
    s.create("diar", "2026-11-02T19:30:00Z", "", "sherpa-onnx:x")
        .unwrap();
    let started = chrono::DateTime::parse_from_rfc3339("2026-11-02T19:30:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc);
    // Timestamps are whole seconds (OpenOats' format), so 3.6 s is 19:30:03.
    for (sp, at) in [
        ("you", 1.0),
        ("remote_1", 2.4),
        ("remote_2", 3.6),
        ("remote_3", 61.5),
    ] {
        let u = Utt {
            speaker: sp.into(),
            text: format!("{sp} says"),
            start: at,
            end: at + 1.0,
        };
        s.append_utterance("diar", "final", &utterance_json(&u, started))
            .unwrap();
    }
    s.name_speaker("diar", "remote_2", "Dana").unwrap();
    s.finish("diar", "2026-11-02T19:31:10Z").unwrap();
    let v = s
        .export_markdown("diar", &d.path().join("out"), "", chrono_tz::Tz::UTC)
        .unwrap();
    let md = std::fs::read_to_string(v["path"].as_str().unwrap()).unwrap();
    assert!(v["path"]
        .as_str()
        .unwrap()
        .ends_with("2026-11-02-1930-meeting.md"));
    assert!(md.contains("title: \"Meeting\"\ndate: 2026-11-02T19:30:00Z\nduration: 1\nparticipants:\n  - You\n  - Them\n  - Dana\n"), "{md}");
    assert!(md.contains("[00:00:01] **You:** you says\n\n[00:00:02] **Them:** remote_1 says\n\n[00:00:03] **Dana:** remote_2 says\n\n[00:01:01] **Them:** remote_3 says\n"), "{md}");
    // OpenOats keeps the speaker keys: the transcript still says remote_N.
    assert_eq!(
        s.transcript("diar").unwrap()["utterances"][1]["speaker"],
        "remote_1"
    );
    // Our offsets live under x_oats.
    assert_eq!(
        s.transcript("diar").unwrap()["utterances"][1]["x_oats"]["startSeconds"],
        2.4
    );
    let empty = Store::new(d.path());
    empty
        .create("blank", "2026-11-02T19:30:00Z", "", "e")
        .unwrap();
    assert_eq!(
        empty
            .export_markdown("blank", &d.path().join("out"), "", chrono_tz::Tz::UTC)
            .unwrap_err()
            .code,
        "empty_transcript"
    );
}

fn template(name: &str, id: Option<&str>) -> templates::Template {
    templates::Template {
        schema_version: templates::SCHEMA_VERSION,
        name: name.into(),
        icon: None,
        system_prompt: "## Summary".into(),
        openoats_id: id.map(str::to_string),
    }
}

#[test]
fn an_oats_session_is_one_openoats_lists_and_opens() {
    let d = tempfile::tempdir().unwrap();
    let s = Store::new(d.path().join("ours"));
    s.create(
        "session_2026-11-06_10-00-57",
        "2026-11-06T15:00:57Z",
        "Weekly",
        "sherpa-onnx:parakeet",
    )
    .unwrap();
    let id = "session_2026-11-06_10-00-57";
    let started = chrono::DateTime::parse_from_rfc3339("2026-11-06T15:00:57Z")
        .unwrap()
        .with_timezone(&chrono::Utc);
    for (sp, at) in [("you", 0.25), ("remote_1", 3.75)] {
        let u = Utt {
            speaker: sp.into(),
            text: "hi".into(),
            start: at,
            end: at + 1.0,
        };
        s.append_utterance(id, "live", &utterance_json(&u, started))
            .unwrap();
    }
    s.set_pending(id, true).unwrap();
    s.set_pending(id, false).unwrap();
    s.finish(id, "2026-11-06T15:30:00Z").unwrap();
    s.set_notes(id, "# Weekly\n", &template("Weekly", None), "ollama", "m")
        .unwrap();
    // Our own store is already readable by OpenOats.
    let dir = d.path().join("ours").join(id);
    assert_openoats_reads(&dir);
    let meta = json_of(&dir.join("session.json"));
    assert_eq!(meta["templateSnapshot"]["name"], "Weekly");
    assert_eq!(
        meta["x_oats"],
        json!({"schemaVersion": 1, "notesTemplate": "Weekly", "notesBackend": "ollama", "notesModel": "m"})
    );
    for k in ["schemaVersion", "notesTemplate", "transcribePending"] {
        assert!(meta.get(k).is_none(), "{k} at the top level");
    }

    // Copied into an OpenOats sessions folder: lists (session.json decodes) and opens.
    let root = d.path().join("OpenOats/sessions");
    let v = s.export_openoats(id, &root, false).unwrap();
    assert_eq!(v["imported"], json!([id]));
    assert_openoats_reads(&root.join(id));
    assert_eq!(
        json_of(&root.join(id).join("session.json"))["utteranceCount"],
        2
    );
    // Only the copy is in the folder (no staging left behind for OpenOats to trip on).
    let names: Vec<String> = std::fs::read_dir(&root)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, vec![id.to_string()]);
    assert_eq!(
        s.export_openoats(id, &root, false).unwrap()["skipped"],
        json!([id])
    );
    s.rename(id, "Changed").unwrap();
    assert_eq!(
        s.export_openoats(id, &root, false).unwrap_err().code,
        "conflict"
    );
    s.export_openoats(id, &root, true).unwrap();
    assert_eq!(
        json_of(&root.join(id).join("session.json"))["title"],
        "Changed"
    );
}

#[test]
fn export_conforms_what_openoats_would_reject() {
    let d = tempfile::tempdir().unwrap();
    let src = d.path().join("src/odd");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(
        src.join("session.json"),
        json!({"id": "odd", "startedAt": "2026-11-06T10:00:57.250-05:00", "templateSnapshot": {"name": "x"}, "speakerNames": [1]}).to_string(),
    )
    .unwrap();
    std::fs::write(
        src.join("transcript.live.jsonl"),
        "{\"speaker\":\"you\",\"text\":\"a\",\"timestamp\":\"2026-11-06T15:00:58.5Z\"}\nnot json\n{\"text\":\"no time\"}\n",
    )
    .unwrap();
    std::fs::write(src.join("notes.md"), "# n\n").unwrap();
    let s = Store::new(d.path().join("ours"));
    s.import(&src, false).unwrap();
    assert!(openoats::decodes_meta(&json_of(&d.path().join("ours/odd/session.json"))).is_err());
    let root = d.path().join("oo");
    s.export_openoats("odd", &root, false).unwrap();
    let out = root.join("odd");
    assert_openoats_reads(&out);
    let meta = json_of(&out.join("session.json"));
    assert_eq!(meta["startedAt"], "2026-11-06T15:00:57Z");
    assert_eq!(meta["hasNotes"], true);
    assert_eq!(meta["utteranceCount"], 1);
    assert!(meta.get("templateSnapshot").is_none());
    assert_eq!(
        json_of(&out.join("notes.meta.json"))["templateSnapshot"]["id"],
        openoats::GENERIC_TEMPLATE_ID
    );
    assert_eq!(jsonl(&out.join("transcript.live.jsonl")).len(), 1);
}

#[test]
fn templates_round_trip_through_openoats_templates_json() {
    let d = tempfile::tempdir().unwrap();
    let ours = d.path().join("templates");
    let r = templates::import_openoats(&golden().join("templates.json"), &ours, false).unwrap();
    assert_eq!(r["imported"], json!(["Generic", "Board Prep"]));
    // Into a copy of OpenOats' file: same ids replaced in place, nothing duplicated.
    let to = d.path().join("OpenOats/templates.json");
    std::fs::create_dir_all(to.parent().unwrap()).unwrap();
    std::fs::copy(golden().join("templates.json"), &to).unwrap();
    let v = templates::export_openoats(&ours, &to).unwrap();
    assert_eq!(v["added"], json!([]));
    assert_eq!(json_of(&to), json_of(&golden().join("templates.json")));

    // A template of ours with no OpenOats id gets a stable one, no icon gets OpenOats' default.
    templates::save(&ours, "Retro", "## Went well", None, None, false).unwrap();
    let v = templates::export_openoats(&ours, &to).unwrap();
    assert_eq!(v["added"], json!(["Retro"]));
    let doc = json_of(&to);
    let retro = doc["templates"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "Retro")
        .unwrap()
        .clone();
    assert_eq!(retro["id"], json!(openoats::template_uuid("Retro")));
    assert_eq!(retro["icon"], "doc.text");
    assert_eq!(retro["isBuiltIn"], false);
    assert_eq!(doc["templates"].as_array().unwrap().len(), 3);
    // And back: the id comes home as openoats_id.
    let back = d.path().join("back");
    templates::import_openoats(&to, &back, false).unwrap();
    assert_eq!(
        templates::find(&back, "Retro").unwrap().openoats_id,
        Some(openoats::template_uuid("Retro"))
    );
    // A fresh file is created when OpenOats has none.
    let fresh = d.path().join("fresh/templates.json");
    templates::export_openoats(&ours, &fresh).unwrap();
    assert_eq!(json_of(&fresh)["version"], 1);
}

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
    .unwrap()
}

fn act(v: Value) -> Action {
    serde_json::from_value(v).unwrap()
}

#[test]
fn the_actions_use_the_config_folders() {
    let d = tempfile::tempdir().unwrap();
    let p = |s: &str| d.path().join(s).to_string_lossy().replace('\\', "/");
    let e = engine(
        d.path(),
        &format!(
            "[sessions]\nexport_dir = \"{}\"\nexport_recorder = \"Test Recorder\"\nopenoats_dir = \"{}\"\n\n[ui]\ntz = \"America/New_York\"\n",
            p("md"),
            p("OpenOats")
        ),
    );
    e.dispatch(&act(
        json!({"action": "session-import", "from": golden().join("sessions")}),
    ))
    .unwrap();
    let v = e
        .dispatch(&act(json!({"action": "session-export", "session": ID})))
        .unwrap();
    assert_eq!(
        PathBuf::from(v["path"].as_str().unwrap()),
        d.path().join("md/2026-11-02-1430-q4-roadmap-review.md")
    );
    let md = std::fs::read_to_string(v["path"].as_str().unwrap()).unwrap();
    assert_eq!(
        openoats_lines(&md),
        std::fs::read_to_string(golden().join("expected-export.md")).unwrap()
    );
    let v = e
        .dispatch(&act(
            json!({"action": "session-export-openoats", "session": ID}),
        ))
        .unwrap();
    assert_eq!(v["imported"], json!([ID]));
    let copy = d.path().join("OpenOats/sessions").join(ID);
    assert_openoats_reads(&copy);
    assert_eq!(files(&copy), files(&golden_session()));
    assert_eq!(
        without_x_oats(json_of(&copy.join("session.json"))),
        json_of(&golden_session().join("session.json"))
    );
    e.dispatch(&act(
        json!({"action": "template-save", "name": "Retro", "system_prompt": "## A"}),
    ))
    .unwrap();
    let v = e
        .dispatch(&act(json!({"action": "template-export"})))
        .unwrap();
    assert_eq!(v["added"], json!(["Retro"]));
    assert!(d.path().join("OpenOats/templates.json").is_file());
}

/// The real OpenOats sessions a developer keeps locally (never committed): each imports, reads
/// back unchanged, exports to markdown and copies back into an OpenOats folder OpenOats reads.
#[test]
fn private_openoats_sessions_round_trip_when_present() {
    let dir = std::env::var_os("OATS_OPENOATS_PRIVATE")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/private/openoats")
        });
    if !dir.is_dir() {
        eprintln!("no private OpenOats fixture at {}; skipped", dir.display());
        return;
    }
    let d = tempfile::tempdir().unwrap();
    let s = Store::new(d.path().join("ours"));
    let r = s.import(&dir, false).unwrap();
    let ids: Vec<String> = r["imported"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    assert!(!ids.is_empty());
    for id in &ids {
        let src = if dir.join("session.json").is_file() {
            dir.clone()
        } else {
            dir.join(id)
        };
        let ours = d.path().join("ours").join(id);
        assert_eq!(files(&ours), files(&src), "{id}");
        let mut m: Map<String, Value> = json_of(&ours.join("session.json"))
            .as_object()
            .unwrap()
            .clone();
        m.remove("x_oats");
        assert_eq!(Value::Object(m), json_of(&src.join("session.json")), "{id}");
        if s.get(id).unwrap()["utterance_count"].as_u64().unwrap_or(0) > 0 {
            s.export_markdown(id, &d.path().join("md"), "", ny())
                .unwrap();
        }
        s.export_openoats(id, &d.path().join("oo"), false).unwrap();
        assert_openoats_reads(&d.path().join("oo").join(id));
    }
}
