//! `ctl list-actions | head`: stdout's reader closes before ctl is done writing. That is a normal
//! early exit (0, no panic), not "failed printing to stdout: Broken pipe" (dot-4uzgol.47).

use std::io::Read;
use std::time::Duration;

use oats_core::control::engine::{Engine, EngineOpts};
use oats_core::control::server;

fn s(a: &[&str]) -> Vec<String> {
    a.iter().map(|x| x.to_string()).collect()
}

#[test]
fn closed_reader_is_a_quiet_early_exit() {
    let dir = tempfile::tempdir().unwrap();
    let rt = dir.path().join("rt");
    let engine = Engine::new(EngineOpts {
        user_config: dir.path().join("cfg").join("config.toml"),
        pack: None,
        os: "linux".into(),
        arch: "x86_64".into(),
        machine: "default".into(),
        sessions_dir: dir.path().join("sessions"),
        secrets: None,
    })
    .unwrap();
    let _handle = server::serve_with_poll(engine, &rt, Duration::from_millis(40)).unwrap();
    let rt_s = rt.to_str().unwrap();
    let env = |_: &str| None;

    // Sanity: with a live reader the manifest arrives.
    let (mut r, mut w) = std::io::pipe().unwrap();
    let reader = std::thread::spawn(move || {
        let mut got = String::new();
        r.read_to_string(&mut got).unwrap();
        got
    });
    let code = oats_ctl::run_to(&s(&["--runtime-dir", rt_s, "list-actions"]), &env, &mut w);
    drop(w);
    assert_eq!(code, 0);
    assert!(reader.join().unwrap().contains("list-actions"));

    // The reader is gone before ctl writes: every output path exits 0 without panicking.
    for args in [
        vec!["--runtime-dir", rt_s, "list-actions"],
        vec!["--runtime-dir", rt_s, "--json", "list-actions"],
        vec!["--runtime-dir", rt_s, "config", "get", "app.menubar"],
    ] {
        let (r, mut w) = std::io::pipe().unwrap();
        drop(r);
        assert_eq!(oats_ctl::run_to(&s(&args), &env, &mut w), 0, "{args:?}");
    }

    // Engine not running, --json: the error envelope goes to stdout; a closed reader still
    // must not panic (exit 0 as for any early close).
    let (r, mut w) = std::io::pipe().unwrap();
    drop(r);
    let none = dir.path().join("none");
    let args = s(&["--runtime-dir", none.to_str().unwrap(), "--json", "ping"]);
    assert_eq!(oats_ctl::run_to(&args, &env, &mut w), 0);
}
