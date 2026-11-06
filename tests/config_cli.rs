//! End-to-end: the real binary, real exit codes (dot-4uzgol.4 acceptance).
use std::process::Command;

fn run(args: &[&str], envs: &[(&str, &str)]) -> (i32, String, String) {
    let mut c = Command::new(env!("CARGO_BIN_EXE_oats-meetings-rs"));
    c.args(args)
        .env_remove("OATS_PACK")
        .env_remove("OATS_CONFIG")
        .env("XDG_CONFIG_HOME", "/nonexistent-oats-xdg")
        .env("OATS_OS", "linux");
    for (k, v) in envs {
        c.env(k, v);
    }
    let o = c.output().unwrap();
    (
        o.status.code().unwrap(),
        String::from_utf8_lossy(&o.stdout).into_owned(),
        String::from_utf8_lossy(&o.stderr).into_owned(),
    )
}

#[test]
fn config_check_exits_zero() {
    assert_eq!(run(&["config", "check"], &[]).0, 0);
}

#[test]
fn unknown_key_exits_one_with_unknown_key() {
    let d = tempfile::tempdir().unwrap();
    let f = d.path().join("c.toml");
    std::fs::write(&f, "[nope]\nx = 1\n").unwrap();
    let (code, out, _) = run(
        &["config", "check", "--json"],
        &[("OATS_CONFIG", f.to_str().unwrap())],
    );
    assert_eq!(code, 1);
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["error"]["code"], "unknown_key");
}

#[test]
fn cloud_pack_differs_from_default_only_in_pack_keys() {
    let (c1, a, _) = run(&["config", "resolved", "--json"], &[]);
    let (c2, b, _) = run(&["config", "resolved", "--json"], &[("OATS_PACK", "cloud")]);
    assert_eq!((c1, c2), (0, 0));
    let a: serde_json::Value = serde_json::from_str(&a).unwrap();
    let b: serde_json::Value = serde_json::from_str(&b).unwrap();
    let (ca, cb) = (&a["data"]["config"], &b["data"]["config"]);
    let mut changed = vec![];
    for (sec, m) in ca.as_object().unwrap() {
        // A root list ([[hooks]]) is a leaf, not a section.
        let Some(m) = m.as_object() else {
            if &cb[sec] != m {
                changed.push(sec.clone());
            }
            continue;
        };
        for (k, v) in m {
            if &cb[sec][k] != v {
                changed.push(format!("{sec}.{k}"));
            }
        }
    }
    changed.sort();
    // Pack-managed top-level leaves/sections only: stt (nested), llm.backend, network.allow.
    assert_eq!(
        changed,
        vec!["llm.backend", "network.allow", "stt.backend", "stt.cloud"]
    );
    assert_eq!(b["data"]["pack"], "cloud");
}

#[test]
fn committed_generated_files_are_current_and_hand_edits_drift() {
    let cfg = concat!(env!("CARGO_MANIFEST_DIR"), "/config");
    assert_eq!(
        run(&["config", "generate", "--check", "--dir", cfg], &[]).0,
        0
    );
    let d = tempfile::tempdir().unwrap();
    let dir = d.path().to_str().unwrap();
    assert_eq!(run(&["config", "generate", "--dir", dir], &[]).0, 0);
    std::fs::write(d.path().join("schema.json"), "{}\n").unwrap();
    let (code, out, _) = run(
        &["config", "generate", "--check", "--json", "--dir", dir],
        &[],
    );
    assert_eq!(code, 1);
    assert!(out.contains("\"drift\""), "{out}");
}

#[test]
fn headless_version_still_exits_zero() {
    let (code, out, _) = run(&["--headless", "--version"], &[]);
    assert_eq!(code, 0);
    assert!(out.starts_with("oats-meetings-rs "));
}
