//! The binary carries the @loader_path rpath packaging/macos/make-app.sh requires, so the
//! sherpa-onnx dylibs beside it resolve (dot-4uzgol.73).

#[test]
fn build_rs_emits_the_loader_path_rpath_for_macos() {
    let src = include_str!("../build.rs");
    let gate = src
        .find(r#"== Ok("macos")"#)
        .expect("build.rs has a macOS branch");
    let arg = src
        .find(r#"cargo:rustc-link-arg=-Wl,-rpath,@loader_path"#)
        .expect("build.rs emits -rpath @loader_path");
    assert!(
        arg > gate,
        "the @loader_path rpath sits inside the macOS branch"
    );
}

#[cfg(target_os = "macos")]
#[test]
fn built_binary_has_loader_path_and_swift_rpaths() {
    let o = std::process::Command::new("otool")
        .args(["-l", env!("CARGO_BIN_EXE_oats-meetings-rs")])
        .output()
        .expect("otool runs");
    assert!(o.status.success());
    let out = String::from_utf8_lossy(&o.stdout);
    let rpaths: Vec<&str> = out
        .split("cmd LC_RPATH")
        .skip(1)
        .filter_map(|c| c.lines().find_map(|l| l.trim().strip_prefix("path ")))
        .map(|p| p.split(" (offset").next().unwrap_or(p))
        .collect();
    for want in ["@loader_path", "/usr/lib/swift"] {
        assert!(rpaths.contains(&want), "LC_RPATH {rpaths:?} lacks {want}");
    }
}
