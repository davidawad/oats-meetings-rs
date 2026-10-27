//! Where the socket and the single-instance lock live.

use std::path::{Path, PathBuf};

/// `OATS_RUNTIME_DIR`, else `$XDG_RUNTIME_DIR/oats-meetings-rs`, else a per-user dir in the
/// temp dir.
pub fn runtime_dir(env: &dyn Fn(&str) -> Option<String>) -> PathBuf {
    if let Some(d) = env("OATS_RUNTIME_DIR") {
        return PathBuf::from(d);
    }
    if let Some(d) = env("XDG_RUNTIME_DIR") {
        return PathBuf::from(d).join("oats-meetings-rs");
    }
    let user = env("USER")
        .or_else(|| env("USERNAME"))
        .unwrap_or_else(|| "user".into());
    std::env::temp_dir().join(format!("oats-meetings-rs-{user}"))
}

pub fn lock_path(dir: &Path) -> PathBuf {
    dir.join("engine.lock")
}

#[cfg(unix)]
pub fn socket_path(dir: &Path) -> PathBuf {
    dir.join("control.sock")
}

/// Windows named pipe names are global, so derive one from the runtime dir.
#[cfg(windows)]
pub fn socket_path(dir: &Path) -> PathBuf {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in dir.to_string_lossy().bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x100000001b3);
    }
    PathBuf::from(format!(r"\\.\pipe\oats-meetings-rs-{h:016x}"))
}

#[cfg(not(any(unix, windows)))]
pub fn socket_path(dir: &Path) -> PathBuf {
    dir.join("control.sock")
}
