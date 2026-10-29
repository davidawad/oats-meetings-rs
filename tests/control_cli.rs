//! End to end through the real binary: `--headless` engine process plus `ctl` client processes.
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;

const EXE: &str = env!("CARGO_BIN_EXE_oats-meetings-rs");

struct Engine {
    child: Child,
    rt: String,
    cfg: String,
    _dir: tempfile::TempDir,
}

fn cmd(rt: &str, cfg: &str) -> Command {
    let mut c = Command::new(EXE);
    // The default session store lives beside the runtime dir, inside the test's tempdir.
    c.env("OATS_DATA_DIR", format!("{rt}/../data"))
        .env("OATS_RUNTIME_DIR", rt)
        .env("OATS_CONFIG", cfg)
        .env("OATS_OS", "linux")
        .env("OATS_RELOAD_POLL_MS", "50")
        .env_remove("OATS_PACK");
    c
}

impl Engine {
    fn start() -> Engine {
        let dir = tempfile::tempdir().unwrap();
        let rt = dir.path().join("rt").to_str().unwrap().to_string();
        let cfg = dir.path().join("config.toml").to_str().unwrap().to_string();
        let child = cmd(&rt, &cfg)
            .arg("--headless")
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let e = Engine {
            child,
            rt,
            cfg,
            _dir: dir,
        };
        let start = Instant::now();
        while e.ctl(&["ping"]).0 != 0 {
            assert!(
                start.elapsed() < Duration::from_secs(30),
                "engine did not come up"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
        e
    }

    fn ctl(&self, args: &[&str]) -> (i32, String, String) {
        let o = cmd(&self.rt, &self.cfg)
            .arg("ctl")
            .args(args)
            .output()
            .unwrap();
        (
            o.status.code().unwrap(),
            String::from_utf8_lossy(&o.stdout).into_owned(),
            String::from_utf8_lossy(&o.stderr).into_owned(),
        )
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn ping_returns_ok_true() {
    let e = Engine::start();
    let (code, out, _) = e.ctl(&["--json", "ping"]);
    assert_eq!(code, 0);
    let v: Value = serde_json::from_str(out.trim()).unwrap();
    assert_eq!(v["ok"], true);
}

#[test]
fn list_actions_json_equals_committed_manifest() {
    let e = Engine::start();
    let (code, out, _) = e.ctl(&["--json", "list-actions"]);
    assert_eq!(code, 0);
    let v: Value = serde_json::from_str(out.trim()).unwrap();
    let committed: Value = serde_json::from_str(
        &std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/config/actions.json"))
            .unwrap(),
    )
    .unwrap();
    assert_eq!(v["data"], committed);
}

#[test]
fn subscribe_sees_setting_changed_after_ctl_config_set() {
    let e = Engine::start();
    let mut sub = cmd(&e.rt, &e.cfg)
        .args([
            "ctl",
            "--json",
            "subscribe",
            "--topic",
            "setting-changed",
            "--count",
            "1",
            "--timeout-ms",
            "15000",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut lines = BufReader::new(sub.stdout.take().unwrap()).lines();
    let ack: Value = serde_json::from_str(&lines.next().unwrap().unwrap()).unwrap();
    assert_eq!(ack["ok"], true);
    let (code, _, err) = e.ctl(&["config", "set", "app.menubar", "true"]);
    assert_eq!(code, 0, "{err}");
    let ev: Value = serde_json::from_str(&lines.next().unwrap().unwrap()).unwrap();
    assert_eq!(ev["event"], "setting-changed");
    assert_eq!(ev["data"]["key"], "app.menubar");
    assert_eq!(ev["data"]["new"], true);
    assert_eq!(sub.wait().unwrap().code(), Some(0));
    // Persisted in the user file, and `config check` (offline) accepts it.
    let text = std::fs::read_to_string(&e.cfg).unwrap();
    assert!(text.contains("menubar = true"), "{text}");
    let (code, out, _) = e.ctl(&["config", "get", "app.menubar"]);
    assert_eq!(
        (code, out.trim()),
        (0, "{\"key\":\"app.menubar\",\"value\":true}")
    );
}

#[cfg(unix)]
#[test]
fn garbage_bytes_do_not_kill_the_engine_process() {
    use std::os::unix::net::UnixStream;
    let e = Engine::start();
    let sock = format!("{}/control.sock", e.rt);
    for payload in [
        &b"\xff\xfe\x00garbage"[..],
        b"{\"action\":",
        &[0u8; 5000][..],
        b"\n\n\n",
    ] {
        let mut s = UnixStream::connect(&sock).unwrap();
        let _ = s.write_all(payload);
    }
    let mut s = UnixStream::connect(&sock).unwrap();
    let _ = s.write_all(&vec![b'z'; 3 * 1024 * 1024]);
    drop(s);
    assert_eq!(e.ctl(&["ping"]).0, 0);
}

#[test]
fn errors_use_exit_codes() {
    let e = Engine::start();
    assert_eq!(e.ctl(&["config", "get", "nope"]).0, 1);
    assert_eq!(e.ctl(&["frobnicate"]).0, 2);
    let (code, out, _) = e.ctl(&["--json", "session", "get", "nope"]);
    assert_eq!(code, 1);
    assert!(out.contains("not_found"), "{out}");
    let (code, out, _) = e.ctl(&["--json", "transcript", "search", "x"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("\"hits\""), "{out}");
}

#[test]
fn second_engine_is_refused_and_quit_stops_the_first() {
    let mut e = Engine::start();
    let second = cmd(&e.rt, &e.cfg).arg("--headless").output().unwrap();
    assert_eq!(second.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&second.stderr).contains("already_running"));
    assert_eq!(e.ctl(&["quit"]).0, 0);
    let status = e.child.wait().unwrap();
    assert_eq!(status.code(), Some(0));
    assert_eq!(e.ctl(&["ping"]).0, 3);
}

#[test]
fn ctl_without_engine_exits_three() {
    let d = tempfile::tempdir().unwrap();
    let o = Command::new(EXE)
        .args(["ctl", "ping"])
        .env("OATS_RUNTIME_DIR", d.path().join("none"))
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(3));
}

fn ids_of(out: &str) -> Vec<String> {
    let v: Value = serde_json::from_str(out.trim()).unwrap();
    v["data"]["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["id"].as_str().unwrap().to_string())
        .collect()
}

fn fixture_ids() -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/openoats"
    ))
    .unwrap()
    .flatten()
    .filter(|e| e.path().join("session.json").is_file())
    .map(|e| e.file_name().to_string_lossy().into_owned())
    .collect();
    v.sort();
    v
}

#[test]
fn ctl_session_import_then_list_equals_fixture_ids() {
    let e = Engine::start();
    let fixture = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/openoats");
    let (code, out, err) = e.ctl(&["--json", "session", "import", "--from", fixture]);
    assert_eq!(code, 0, "{out} {err}");
    let (code, out, _) = e.ctl(&["--json", "session", "list"]);
    assert_eq!(code, 0);
    assert_eq!(ids_of(&out), fixture_ids());
    assert_eq!(fixture_ids().len(), 2);
}

#[test]
fn ctl_session_delete_needs_confirm() {
    let e = Engine::start();
    let fixture = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/openoats");
    assert_eq!(e.ctl(&["session", "import", "--from", fixture]).0, 0);
    let id = &fixture_ids()[0];
    let (code, out, _) = e.ctl(&["--json", "session", "delete", id]);
    assert_eq!(code, 1);
    assert!(out.contains("confirmation_required"), "{out}");
    assert_eq!(ids_of(&e.ctl(&["--json", "session", "list"]).1).len(), 2);
    assert_eq!(e.ctl(&["session", "delete", id, "--confirm"]).0, 0);
    assert_eq!(
        ids_of(&e.ctl(&["--json", "session", "list"]).1),
        vec![fixture_ids()[1].clone()]
    );
    assert_eq!(
        e.ctl(&["session", "rename", &fixture_ids()[1], "Budget call"])
            .0,
        0
    );
    assert_eq!(
        e.ctl(&["session", "tag", &fixture_ids()[1], "finance"]).0,
        0
    );
    let (_, out, _) = e.ctl(&["--json", "session", "get", &fixture_ids()[1]]);
    let v: Value = serde_json::from_str(out.trim()).unwrap();
    assert_eq!(v["data"]["title"], "Budget call");
    assert_eq!(v["data"]["tags"], serde_json::json!(["finance"]));
}

/// Writes the store the app produced to $OATS_STORE_DUMP (when set) so `bin/meeting sessions list`
/// can be run against it from the dotfiles side (see ci/meeting-compat.sh).
#[test]
fn engine_store_for_meeting_compat() {
    let Some(dump) = std::env::var_os("OATS_STORE_DUMP") else {
        return;
    };
    let e = Engine::start();
    let fixture = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/openoats");
    assert_eq!(e.ctl(&["session", "import", "--from", fixture]).0, 0);
    assert_eq!(
        e.ctl(&[
            "session",
            "rename",
            &fixture_ids()[0],
            "Acme sync (renamed)"
        ])
        .0,
        0
    );
    assert_eq!(e.ctl(&["session", "tag", &fixture_ids()[0], "sales"]).0, 0);
    let (_, out, _) = e.ctl(&["--json", "session", "list"]);
    let _ = out;
    let store = std::path::Path::new(&e.rt).join("../data/sessions");
    let status = Command::new("cp")
        .arg("-R")
        .arg(&store)
        .arg(&dump)
        .status()
        .unwrap();
    assert!(status.success());
}
