// macOS: screencapturekit's Swift bridge links libswift_Concurrency by @rpath, and a Rust test or
// binary must carry the rpath itself (link args do not propagate from a dependency's build.rs).
// The Swift runtime lives in the OS at /usr/lib/swift.
fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        println!("cargo:rustc-link-arg=-Wl,-rpath,/usr/lib/swift");
    }
}
