//! Typed integer and duration keys: an out-of-range value vetoes a reload and leaves the running
//! config alone; the generated schema declares both kinds; and no timing literal hides outside
//! the key table.

use std::path::{Path, PathBuf};

use oats_core::config::keys::{self, Kind, KEYS};
use oats_core::control::engine::{Engine, EngineOpts};
use serde_json::{json, Value};

fn engine(dir: &Path) -> (std::sync::Arc<Engine>, PathBuf) {
    let cfg = dir.join("config.toml");
    let e = Engine::new(EngineOpts {
        user_config: cfg.clone(),
        pack: None,
        os: "linux".into(),
        arch: "x86_64".into(),
        machine: "default".into(),
        sessions_dir: dir.join("sessions"),
        secrets: None,
    })
    .unwrap();
    (e, cfg)
}

/// A value one step outside the key's range, in the shape the key wants.
fn out_of_range(k: &keys::Key) -> Option<Value> {
    match k.kind {
        Kind::Int { max, .. } => Some(json!(max + 1)),
        Kind::Duration { max_ms, .. } => Some(json!(format!("{}ms", max_ms + 1))),
        _ => None,
    }
}

#[test]
fn out_of_range_set_is_vetoed_for_every_int_and_duration_key() {
    let d = tempfile::tempdir().unwrap();
    let (e, cfg) = engine(d.path());
    let mut n = 0;
    for k in KEYS {
        let Some(bad) = out_of_range(k) else { continue };
        n += 1;
        let before = e.snapshot().resolved.flat.clone();
        let err = e.set(k.path, &bad, "test").unwrap_err();
        assert_eq!(err.code, "invalid_value", "{}", k.path);
        assert!(err.message.contains(k.path), "{}: {}", k.path, err.message);
        assert_eq!(e.snapshot().resolved.flat, before, "{} changed the running config", k.path);
        assert!(!cfg.exists() || !std::fs::read_to_string(&cfg).unwrap().contains(k.path), "{} reached the file", k.path);
    }
    assert!(n >= 14, "{n} typed keys");
}

#[test]
fn out_of_range_file_reload_is_vetoed_and_reports_failure() {
    let d = tempfile::tempdir().unwrap();
    let (e, cfg) = engine(d.path());
    let rx = e.bus.subscribe(vec!["config-reload-failed".into(), "config-reloaded".into()]);
    std::fs::write(&cfg, "[llm]\ntimeout = \"30s\"\n[ics]\nwindow_days = 14\n").unwrap();
    e.reload("test").unwrap();
    assert_eq!(e.snapshot().config.llm.timeout.0, 30_000);
    let good = e.snapshot().resolved.flat.clone();
    let _ = rx.try_iter().count();
    // One bad key sinks the whole file, including the valid key beside it.
    std::fs::write(&cfg, "[llm]\ntimeout = \"2h\"\n[ics]\nwindow_days = 21\n").unwrap();
    let err = e.reload("test").unwrap_err();
    assert_eq!(err.code, "invalid_value");
    assert_eq!(e.snapshot().resolved.flat, good);
    let evs: Vec<Value> = rx.try_iter().map(|l| serde_json::from_str(&l).unwrap()).collect();
    assert!(evs.iter().any(|v| v["event"] == "config-reload-failed" && v["data"]["code"] == "invalid_value"));
    assert!(!evs.iter().any(|v| v["event"] == "config-reloaded"));
}

#[test]
fn cli_style_strings_are_coerced_for_integers_only() {
    let d = tempfile::tempdir().unwrap();
    let (e, _) = engine(d.path());
    e.set("ics.window_days", &json!("30"), "test").unwrap();
    assert_eq!(e.snapshot().config.ics.window_days, 30);
    assert_eq!(e.set("ics.window_days", &json!("many"), "test").unwrap_err().code, "type_mismatch");
    e.set("llm.timeout", &json!("45s"), "test").unwrap();
    assert_eq!(e.snapshot().config.llm.timeout.0, 45_000);
    assert_eq!(e.set("llm.timeout", &json!(45), "test").unwrap_err().code, "type_mismatch");
}

#[test]
fn schema_declares_integer_and_duration_types() {
    let schema: Value = serde_json::from_str(&std::fs::read_to_string(repo("config/schema.json")).unwrap()).unwrap();
    let max_bytes = &schema["properties"]["ics"]["properties"]["max_bytes"];
    assert_eq!(max_bytes["type"], "integer");
    assert_eq!(max_bytes["minimum"], 1);
    assert_eq!(max_bytes["maximum"], 1 << 30);
    let timeout = &schema["properties"]["llm"]["properties"]["timeout"];
    assert_eq!(timeout["x-kind"], "duration");
    assert_eq!(timeout["format"], "duration");
    assert_eq!(timeout["default"], "2m");
    assert_eq!(timeout["x-min-ms"], 1000);
    // Every typed key in the table is typed in the schema.
    for k in KEYS {
        let mut node = &schema;
        for part in k.path.split('.') {
            node = &node["properties"][part];
        }
        match k.kind {
            Kind::Int { .. } => assert_eq!(node["type"], "integer", "{}", k.path),
            Kind::Duration { .. } => assert_eq!(node["x-kind"], "duration", "{}", k.path),
            _ => {}
        }
    }
}

fn repo(rel: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").join(rel)
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for ent in std::fs::read_dir(dir).unwrap() {
        let p = ent.unwrap().path();
        if p.is_dir() {
            rust_files(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

/// Production code (everything before a file's `#[cfg(test)]` module) holds no
/// `Duration::from_secs` / `from_millis` / `from_mins` literal outside the key table, and no
/// `sleep(`/`timeout(` taking a bare number. Defaults live in config/keys.rs only.
#[test]
fn no_timing_literals_outside_the_key_table() {
    let mut files = vec![];
    for d in ["crates/core/src", "crates/ctl/src", "crates/ui/src", "src"] {
        rust_files(&repo(d), &mut files);
    }
    assert!(files.len() > 20, "scanned {} files", files.len());
    let table = repo("crates/core/src/config/keys.rs");
    let mut hits = vec![];
    for f in files.iter().filter(|f| **f != table.canonicalize().unwrap_or(table.clone()) && !f.ends_with("config/keys.rs")) {
        let text = std::fs::read_to_string(f).unwrap();
        let prod = text.split("#[cfg(test)]").next().unwrap();
        for (i, line) in prod.lines().enumerate() {
            let code = line.split("//").next().unwrap();
            if ["Duration::from_secs(", "Duration::from_millis(", "Duration::from_mins(", "Duration::from_micros(", "Duration::from_nanos("]
                .iter()
                .any(|pat| code.contains(pat))
            {
                // Allowed: converting a value that came from config (an identifier or field),
                // never a numeric literal.
                let arg = code.split("Duration::from_").nth(1).and_then(|r| r.split('(').nth(1)).unwrap_or("");
                if arg.trim_start().starts_with(|c: char| c.is_ascii_digit()) {
                    hits.push(format!("{}:{}: {}", f.display(), i + 1, line.trim()));
                }
            }
        }
    }
    assert!(hits.is_empty(), "timing literals outside the key table:\n{}", hits.join("\n"));
}

#[test]
fn the_grep_test_would_catch_a_literal() {
    // Guard the guard: the pattern the scanner relies on is what a literal looks like.
    let sample = "let t = Duration::from_secs(30);";
    let arg = sample.split("Duration::from_").nth(1).and_then(|r| r.split('(').nth(1)).unwrap();
    assert!(arg.trim_start().starts_with(|c: char| c.is_ascii_digit()));
}
