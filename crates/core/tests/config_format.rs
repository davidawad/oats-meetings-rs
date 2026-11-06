//! dot-4uzgol.87: the user file may be `config.toml`, `config.json` or `config.yaml`, one schema.
//! The same keys load the same from each; `config-set` writes back in the file's own format (TOML
//! keeping its comments); two default files is a `conflicting_config` error naming both; and
//! `config-convert` moves the file from one format to another.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use oats_core::config::cli;
use oats_core::control::engine::{Engine, EngineOpts};
use serde_json::{json, Value};

const TOML: &str = r#"# my settings
[ui]
show_elapsed = false # keep the bar quiet
live_max_lines = 70

[ui.keys]
record-toggle = "mod-shift-j"

[privacy]
hide_method = "none"

[[hooks]]
name = "done"
event = "record.stop"
command = ["true"]
"#;

const JSON: &str = r#"{
  "$schema": "../config/schema.json",
  "ui": {"show_elapsed": false, "live_max_lines": 70, "keys": {"record-toggle": "mod-shift-j"}},
  "privacy": {"hide_method": "none"},
  "hooks": [{"name": "done", "event": "record.stop", "command": ["true"]}]
}
"#;

const YAML: &str = r#"# my settings
ui:
  show_elapsed: false
  live_max_lines: 70
  keys:
    record-toggle: mod-shift-j
privacy:
  hide_method: none
hooks:
  - name: done
    event: record.stop
    command: ["true"]
"#;

fn args(a: &[&str]) -> Vec<String> {
    a.iter().map(|s| s.to_string()).collect()
}

fn cli_json(a: &[&str], env: &dyn Fn(&str) -> Option<String>) -> Value {
    serde_json::from_str(&cli::run(&args(a), env).stdout).unwrap()
}

fn resolved(file: &Path) -> Value {
    let v = cli_json(
        &["resolved", "--json", "--config", file.to_str().unwrap()],
        &|_| None,
    );
    assert_eq!(v["ok"], true, "{v}");
    v["data"].clone()
}

fn engine(cfg: &Path) -> Arc<Engine> {
    Engine::new(EngineOpts {
        user_config: cfg.to_path_buf(),
        pack: None,
        os: "linux".into(),
        arch: "x86_64".into(),
        machine: "default".into(),
        sessions_dir: cfg.parent().unwrap().join("sessions"),
        secrets: None,
    })
    .unwrap()
}

fn act(e: &Engine, v: Value) -> oats_core::config::Result<Value> {
    e.dispatch(&serde_json::from_value(v).unwrap())
}

fn write(dir: &Path, name: &str, text: &str) -> PathBuf {
    let p = dir.join(name);
    std::fs::write(&p, text).unwrap();
    p
}

#[test]
fn the_same_keys_load_identically_from_toml_json_and_yaml() {
    let d = tempfile::tempdir().unwrap();
    let t = resolved(&write(d.path(), "config.toml", TOML));
    let j = resolved(&write(d.path(), "c.json", JSON));
    let y = resolved(&write(d.path(), "c.yaml", YAML));
    let yml = resolved(&write(d.path(), "c.yml", YAML));
    assert_eq!(t["config"]["ui"]["live_max_lines"], 70);
    assert_eq!(t["config"]["hooks"][0]["name"], "done");
    assert_eq!(t["provenance"]["ui.show_elapsed"], "user");
    assert_eq!(t, j);
    assert_eq!(t, y);
    assert_eq!(t, yml);
    // The engine reads them the same way.
    let te = engine(&d.path().join("config.toml"))
        .snapshot()
        .resolved
        .clone();
    let ye = engine(&d.path().join("c.yaml")).snapshot().resolved.clone();
    assert_eq!(te, ye);
}

#[test]
fn json_and_yaml_errors_are_the_same_errors() {
    let d = tempfile::tempdir().unwrap();
    for (name, text, code) in [
        ("a.json", "{\"app\": {\"bogus\": 1}}", "unknown_key"),
        ("b.yaml", "app:\n  bogus: 1\n", "unknown_key"),
        ("c.yaml", "ui:\n  live_max_lines: lots\n", "invalid_value"),
        ("d.json", "{\"ui\": ", "parse_error"),
        ("e.yaml", "- 1\n- 2\n", "parse_error"),
    ] {
        let f = write(d.path(), name, text);
        let v = cli_json(
            &["check", "--json", "--config", f.to_str().unwrap()],
            &|_| None,
        );
        assert_eq!(v["ok"], false, "{name}: {v}");
        assert!(
            [code, "type_mismatch"].contains(&v["error"]["code"].as_str().unwrap()),
            "{name}: {v}"
        );
    }
}

#[test]
fn config_set_writes_back_in_the_files_own_format() {
    for (name, text) in [
        ("config.toml", TOML),
        ("config.json", JSON),
        ("config.yaml", YAML),
    ] {
        let d = tempfile::tempdir().unwrap();
        let f = write(d.path(), name, text);
        let e = engine(&f);
        let r = act(
            &e,
            json!({"action": "config-set", "key": "ui.live_max_lines", "value": 90}),
        )
        .unwrap();
        assert_eq!(r["path"], json!(f));
        act(
            &e,
            json!({"action": "config-set", "key": "ui.speaker_width", "value": "120"}),
        )
        .unwrap();
        let after = std::fs::read_to_string(&f).unwrap();
        // Still the same format, and only that format.
        let parsed: Value = match name {
            "config.toml" => {
                serde_json::to_value(toml::from_str::<toml::Value>(&after).unwrap()).unwrap()
            }
            "config.json" => serde_json::from_str(&after).unwrap(),
            _ => serde_yaml_ng::from_str(&after).unwrap(),
        };
        assert_eq!(parsed["ui"]["live_max_lines"], 90, "{name}: {after}");
        assert_eq!(parsed["ui"]["speaker_width"], 120, "{name}: {after}");
        assert_eq!(
            parsed["ui"]["keys"]["record-toggle"], "mod-shift-j",
            "{name}"
        );
        assert_eq!(parsed["hooks"][0]["event"], "record.stop", "{name}");
        if name == "config.toml" {
            assert!(
                after.contains("# my settings") && after.contains("# keep the bar quiet"),
                "{after}"
            );
        }
        if name == "config.json" {
            // Key order is the user's, not alphabetical.
            assert!(after.find("\"ui\"") < after.find("\"privacy\""), "{after}");
        }
        assert_eq!(
            std::fs::read_dir(d.path())
                .unwrap()
                .filter_map(|x| x.ok())
                .filter(|x| x.path().is_file())
                .count(),
            1,
            "{name}: a stray temp file"
        );
        // A fresh read of the file gives what the engine has.
        let r = resolved(&f);
        assert_eq!(r["config"]["ui"]["live_max_lines"], 90);
        assert_eq!(engine(&f).snapshot().resolved, e.snapshot().resolved);
        // Hooks (an array of tables) written back too, and an empty list removes the key.
        act(
            &e,
            json!({"action": "config-set", "key": "hooks", "value": []}),
        )
        .unwrap();
        assert!(
            !std::fs::read_to_string(&f).unwrap().contains("record.stop"),
            "{name}"
        );
    }
}

#[test]
fn a_new_json_or_yaml_file_is_created_in_its_format() {
    let d = tempfile::tempdir().unwrap();
    let f = d.path().join("config.yaml");
    let e = engine(&f);
    act(
        &e,
        json!({"action": "config-set", "key": "ui.live_max_lines", "value": 500}),
    )
    .unwrap();
    assert_eq!(
        std::fs::read_to_string(&f).unwrap(),
        "ui:\n  live_max_lines: 500\n"
    );
}

#[test]
fn two_default_files_is_a_config_check_error_naming_both() {
    let d = tempfile::tempdir().unwrap();
    let dir = d.path().join("oats-meetings-rs");
    std::fs::create_dir_all(&dir).unwrap();
    let xdg = d.path().to_str().unwrap().to_string();
    let env = move |k: &str| (k == "XDG_CONFIG_HOME").then(|| xdg.clone());
    // One file of any format is found by itself.
    write(&dir, "config.yml", YAML);
    let v = cli_json(&["get", "--json", "ui.live_max_lines"], &env);
    assert_eq!(v["data"]["value"], 70, "{v}");
    let found = cli::default_user_config_path(None, &env).unwrap().unwrap();
    assert_eq!(found.0, dir.join("config.yml"));

    write(&dir, "config.toml", "");
    let v = cli_json(&["check", "--json"], &env);
    assert_eq!(v["ok"], false);
    assert_eq!(v["error"]["code"], "conflicting_config");
    let msg = v["error"]["message"].as_str().unwrap();
    assert!(
        msg.contains("config.toml") && msg.contains("config.yml"),
        "{msg}"
    );
    let o = cli::run(&args(&["check"]), &env);
    assert_eq!(o.code, 1);
    assert!(o.stderr.contains("conflicting_config"), "{o:?}");
    assert_eq!(
        Engine::from_env(&env).err().map(|e| e.code),
        Some("conflicting_config")
    );
    // A running engine's config-check says so when a second file appears beside its own.
    std::fs::remove_file(dir.join("config.toml")).unwrap();
    let e = Engine::from_env(&env).unwrap();
    assert_eq!(
        act(&e, json!({"action": "config-check"})).unwrap()["valid"],
        true
    );
    write(&dir, "config.json", "{}");
    let err = act(&e, json!({"action": "config-check"})).unwrap_err();
    assert_eq!(err.code, "conflicting_config");
    assert!(err.message.contains("config.json") && err.message.contains("config.yml"));
    // An explicitly named file is never ambiguous.
    let f = dir.join("config.json");
    let v = cli_json(&["check", "--json", "--config", f.to_str().unwrap()], &env);
    assert_eq!(v["ok"], true, "{v}");
}

#[test]
fn config_convert_migrates_between_formats() {
    let d = tempfile::tempdir().unwrap();
    let toml_file = write(d.path(), "config.toml", TOML);
    let e = engine(&toml_file);
    let before = e.snapshot().resolved.clone();
    let mut cur = toml_file.clone();
    for to in ["json", "yaml", "toml"] {
        let r = act(&e, json!({"action": "config-convert", "to": to})).unwrap();
        let next = d.path().join(format!("config.{to}"));
        assert_eq!(r["path"], json!(next));
        assert_eq!(r["changed"], true);
        assert!(next.is_file() && !cur.exists(), "{to}");
        assert_eq!(e.user_config(), next);
        assert_eq!(resolved(&next)["config"], before.nested(), "{to}");
        cur = next;
    }
    assert_eq!(e.snapshot().resolved, before);
    // Converting to the format it already has changes nothing.
    let r = act(&e, json!({"action": "config-convert", "to": "toml"})).unwrap();
    assert_eq!(r["changed"], false);
    // After a convert, config-set writes the new file in the new format.
    act(&e, json!({"action": "config-convert", "to": "yml"})).unwrap();
    act(
        &e,
        json!({"action": "config-set", "key": "ui.live_max_lines", "value": 300}),
    )
    .unwrap();
    let y = std::fs::read_to_string(d.path().join("config.yaml")).unwrap();
    assert!(y.contains("live_max_lines: 300"), "{y}");
    // A target that already exists is refused, and an unknown format too.
    write(d.path(), "config.json", "{}");
    assert_eq!(
        act(&e, json!({"action": "config-convert", "to": "json"}))
            .unwrap_err()
            .code,
        "conflicting_config"
    );
    assert_eq!(
        act(&e, json!({"action": "config-convert", "to": "ini"}))
            .unwrap_err()
            .code,
        "invalid_value"
    );
}
