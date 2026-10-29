//! `--help` and unknown flags never start the app (dot-4uzgol.25).
use std::process::Command;

const EXE: &str = env!("CARGO_BIN_EXE_oats-meetings-rs");

fn run(args: &[&str]) -> (i32, String, String) {
    let dir = tempfile::tempdir().unwrap();
    let o = Command::new(EXE)
        .args(args)
        .env("OATS_RUNTIME_DIR", dir.path().join("rt"))
        .env("OATS_DATA_DIR", dir.path().join("data"))
        .env("OATS_CONFIG", dir.path().join("config.toml"))
        .output()
        .unwrap();
    (
        o.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&o.stdout).into(),
        String::from_utf8_lossy(&o.stderr).into(),
    )
}

#[test]
fn help_prints_usage_and_exits_zero() {
    for flag in ["--help", "-h"] {
        let (code, out, _) = run(&[flag]);
        assert_eq!(code, 0, "{flag}");
        assert!(out.starts_with("usage: oats-meetings-rs"), "{out}");
        assert!(out.contains("--headless") && out.contains("ctl"));
    }
}

#[test]
fn unknown_flags_exit_two_with_the_flag_named_and_never_start_the_app() {
    for args in [
        &["--frobnicate"][..],
        &["-x"],
        &["stray"],
        &["--headless", "--nope"],
    ] {
        let (code, out, err) = run(args);
        assert_eq!(code, 2, "{args:?}");
        assert!(out.is_empty(), "{out}");
        assert!(
            err.contains(&format!("unknown argument '{}'", args.last().unwrap())),
            "{err}"
        );
        assert!(err.contains("usage: oats-meetings-rs"), "{err}");
    }
}

#[test]
fn version_still_works() {
    let (code, out, _) = run(&["--version"]);
    assert_eq!((code, out.starts_with("oats-meetings-rs ")), (0, true));
}
