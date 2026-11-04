//! packaging/macos/sign.sh ad-hoc signs with an identifier-only designated requirement so TCC
//! grants survive rebuilds (dot-4uzgol.74). Runs on Linux against a stub `codesign` that logs
//! its arguments and answers `-dr -` with a canned requirement.

use std::path::Path;
use std::process::{Command, Output};

const ID: &str = "com.davidawad.oats-meetings-rs";

fn sign_sh() -> String {
    format!("{}/packaging/macos/sign.sh", env!("CARGO_MANIFEST_DIR"))
}

fn bash(script: &str, path_prefix: Option<&Path>) -> Output {
    let mut c = Command::new("bash");
    c.args(["-euo", "pipefail", "-c", script]);
    if let Some(p) = path_prefix {
        c.env(
            "PATH",
            format!(
                "{}:{}",
                p.display(),
                std::env::var("PATH").unwrap_or_default()
            ),
        );
    }
    c.output().expect("bash runs")
}

/// A fake .app with two dylibs and a stub codesign whose `-dr -` prints `dr`.
fn fake_app(dir: &Path, dr: &str) -> std::path::PathBuf {
    let app = dir.join("Oats Meetings.app");
    let macos = app.join("Contents/MacOS");
    std::fs::create_dir_all(&macos).unwrap();
    for f in [
        "oats-meetings-rs",
        "libsherpa-onnx-c-api.dylib",
        "libonnxruntime.dylib",
    ] {
        std::fs::write(macos.join(f), "").unwrap();
    }
    let plist = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/packaging/macos/Info.plist.in"
    ))
    .unwrap();
    std::fs::write(app.join("Contents/Info.plist"), plist).unwrap();
    let bin = dir.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let stub = bin.join("codesign");
    std::fs::write(
        &stub,
        format!(
            "#!/usr/bin/env bash\nprintf '%s\\n' \"$*\" >> '{}'\n\
             if [ \"$1\" = -dr ]; then echo 'Executable=x' >&2; printf '%s\\n' '{}'; fi\n",
            dir.join("log").display(),
            dr
        ),
    )
    .unwrap();
    Command::new("chmod").arg("+x").arg(&stub).status().unwrap();
    app
}

fn run_sign(dr: &str) -> (Output, String) {
    let dir = tempfile_dir();
    let app = fake_app(&dir, dr);
    let out = bash(
        &format!(". '{}'; sign_app '{}'", sign_sh(), app.display()),
        Some(&dir.join("bin")),
    );
    let log = std::fs::read_to_string(dir.join("log")).unwrap_or_default();
    let _ = std::fs::remove_dir_all(&dir);
    (out, log)
}

fn tempfile_dir() -> std::path::PathBuf {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static N: AtomicUsize = AtomicUsize::new(0);
    let d = std::env::temp_dir().join(format!(
        "oats-sign-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn signs_inside_out_ad_hoc_with_the_identifier_dr() {
    let (out, log) = run_sign(&format!("designated => identifier \"{ID}\""));
    assert!(
        out.status.success(),
        "sign_app failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let lines: Vec<&str> = log.lines().collect();
    assert_eq!(lines.len(), 5, "codesign calls: {lines:?}");
    for l in &lines[..2] {
        assert!(
            l.starts_with("--force -s - ") && l.ends_with(".dylib"),
            "dylib first: {l}"
        );
    }
    assert!(
        lines[2].starts_with(&format!(
            "--force -s - --identifier {ID} -r=designated => identifier \"{ID}\" "
        )) && lines[2].ends_with("Oats Meetings.app"),
        "bundle signed last with the DR: {}",
        lines[2]
    );
    assert!(lines[3].starts_with("-dr - "), "DR read back: {}", lines[3]);
    assert!(
        lines[4].starts_with("--verify --deep --strict "),
        "strict verify: {}",
        lines[4]
    );
    assert!(
        lines[..3].iter().all(|l| !l.contains("--deep")),
        "no --deep when signing"
    );
}

#[test]
fn fails_when_the_dr_is_a_cdhash() {
    let (out, log) = run_sign("designated => cdhash H\"0123abcd\"");
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("cdhash"));
    assert!(!log.contains("--verify"), "stops before verify");
}

#[test]
fn fails_when_the_dr_lacks_the_identifier() {
    let (out, _) = run_sign("designated => identifier \"com.example.other\"");
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("lacks identifier"));
}

#[test]
fn bundle_id_comes_from_the_info_plist_and_make_app_uses_sign_sh() {
    let out = bash(
        &format!(
            ". '{}'; plist_bundle_id '{}/packaging/macos/Info.plist.in'",
            sign_sh(),
            env!("CARGO_MANIFEST_DIR")
        ),
        None,
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), ID);
    let sign = std::fs::read_to_string(sign_sh()).unwrap();
    assert!(
        !sign.contains(ID),
        "sign.sh must not hard-code the bundle id"
    );
    let code: String = sign
        .lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n");
    for banned in ["security", "keychain", "CODESIGN_IDENTITY"] {
        assert!(
            !code.to_lowercase().contains(&banned.to_lowercase()),
            "{banned}"
        );
    }
    let make = include_str!("../packaging/macos/make-app.sh");
    assert!(make.contains("sign.sh") && make.contains("sign_app \"$app\""));
    assert!(make.contains(r#"[ "$(uname)" = Darwin ]"#));
    assert!(!make.contains("codesign --force --deep"));
}
