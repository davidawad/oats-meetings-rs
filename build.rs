// Windows only: embed the application icon and version info as a Win32 resource.
// The .ico is derived by ci/icons.sh into assets/icon/derived/ (gitignored).
fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=assets/icon/derived/oats-meetings-rs.ico");
    // macOS: the Swift runtime for screencapturekit (see crates/audio/build.rs).
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        println!("cargo:rustc-link-arg=-Wl,-rpath,/usr/lib/swift");
    }
    embed();
}

fn embed() {
    use std::{env, fs, path::PathBuf};
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let ico = PathBuf::from("assets/icon/derived/oats-meetings-rs.ico");
    let out = PathBuf::from(env::var("OUT_DIR").unwrap());
    fs::copy(&ico, out.join("oats-meetings-rs.ico"))
        .unwrap_or_else(|e| panic!("{}: {e} (run ci/icons.sh first)", ico.display()));
    let v = env::var("CARGO_PKG_VERSION").unwrap();
    let nums: Vec<String> = v.split('.').map(|p| p.to_string()).chain(std::iter::repeat("0".into())).take(4).collect();
    let n = nums.join(",");
    let rc = format!(
        "1 ICON \"oats-meetings-rs.ico\"\n\
         1 VERSIONINFO\nFILEVERSION {n}\nPRODUCTVERSION {n}\n\
         BEGIN\n BLOCK \"StringFileInfo\"\n BEGIN\n  BLOCK \"040904b0\"\n  BEGIN\n\
         \x20  VALUE \"FileDescription\", \"Oats Meetings\"\n\
         \x20  VALUE \"ProductName\", \"Oats Meetings\"\n\
         \x20  VALUE \"FileVersion\", \"{v}\"\n\
         \x20  VALUE \"OriginalFilename\", \"oats-meetings-rs.exe\"\n\
         \x20 END\n END\n BLOCK \"VarFileInfo\"\n BEGIN\n  VALUE \"Translation\", 0x409, 1200\n END\nEND\n"
    );
    let rc_path = out.join("oats-meetings-rs.rc");
    fs::write(&rc_path, rc).unwrap();
    embed_resource::compile(&rc_path, embed_resource::NONE)
        .manifest_required()
        .unwrap();
}
