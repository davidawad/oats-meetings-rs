//! The socket server. Robustness contract (the frankenterm #85 class): nothing a client sends can
//! stop the server. Every connection runs on its own thread inside `catch_unwind`; bad bytes,
//! bad JSON, oversize lines and abrupt disconnects get an error line or a close, never a panic
//! that reaches the accept loop.

use std::fs::{File, TryLockError};
use std::io::{BufRead, BufReader, Read, Write};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use interprocess::local_socket::{
    traits::Listener as _,
    GenericFilePath, ListenerNonblockingMode, ListenerOptions, Stream, ToFsName,
};
use serde_json::{json, Value};

use super::engine::Engine;
use super::half::Half;
use super::{paths, MAX_LINE, PROTOCOL};
use crate::action::{self, Action, EVENTS};
use crate::config::{ConfigError, Result, SCHEMA_VERSION};

pub struct ServerHandle {
    engine: Arc<Engine>,
    accept: Option<JoinHandle<()>>,
    watcher: Option<JoinHandle<()>>,
    socket: PathBuf,
}

impl ServerHandle {
    pub fn socket_display(&self) -> String {
        self.socket.display().to_string()
    }

    pub fn engine(&self) -> &Arc<Engine> {
        &self.engine
    }

    /// Block until the engine is told to quit and the accept loop has ended.
    pub fn wait(mut self) {
        self.join();
    }

    /// Ask the engine to stop and wait for the server threads.
    pub fn shutdown(mut self) {
        self.engine.request_shutdown();
        self.join();
    }

    fn join(&mut self) {
        if let Some(h) = self.accept.take() {
            let _ = h.join();
        }
        if let Some(h) = self.watcher.take() {
            let _ = h.join();
        }
    }
}

impl Drop for ServerHandle {
    fn drop(&mut self) {
        self.engine.request_shutdown();
        self.join();
    }
}

fn io_err(what: &str, e: impl std::fmt::Display) -> ConfigError {
    ConfigError::new("io_error", format!("{what}: {e}"))
}

/// Take the single-instance lock, bind the socket, and start the accept and watcher threads.
pub fn serve(engine: Arc<Engine>, runtime_dir: &Path) -> Result<ServerHandle> {
    let poll = std::env::var("OATS_RELOAD_POLL_MS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(250u64);
    serve_with_poll(engine, runtime_dir, Duration::from_millis(poll))
}

/// `poll` is how often the watcher checks the user config file for hot reload.
pub fn serve_with_poll(engine: Arc<Engine>, runtime_dir: &Path, poll: Duration) -> Result<ServerHandle> {
    std::fs::create_dir_all(runtime_dir).map_err(|e| io_err(&runtime_dir.display().to_string(), e))?;
    let lock_path = paths::lock_path(runtime_dir);
    let lock = File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&lock_path)
        .map_err(|e| io_err(&lock_path.display().to_string(), e))?;
    match lock.try_lock() {
        Ok(()) => {}
        Err(TryLockError::WouldBlock) => {
            return Err(ConfigError::new(
                "already_running",
                format!("another engine holds {}", lock_path.display()),
            ))
        }
        Err(TryLockError::Error(e)) => return Err(io_err(&lock_path.display().to_string(), e)),
    }
    let socket = paths::socket_path(runtime_dir);
    // We hold the lock, so any socket file left behind is stale.
    #[cfg(unix)]
    let _ = std::fs::remove_file(&socket);
    let name = socket
        .clone()
        .to_fs_name::<GenericFilePath>()
        .map_err(|e| io_err("socket name", e))?;
    let listener = ListenerOptions::new()
        .name(name)
        .nonblocking(ListenerNonblockingMode::Accept)
        .create_sync()
        .map_err(|e| io_err(&socket.display().to_string(), e))?;

    let eng = Arc::clone(&engine);
    let sock_for_cleanup = socket.clone();
    let accept = thread::Builder::new()
        .name("oats-accept".into())
        .spawn(move || {
            let _lock = lock; // held for the life of the accept loop
            while !eng.is_shutdown() {
                match listener.accept() {
                    Ok(stream) => {
                        let e = Arc::clone(&eng);
                        let _ = thread::Builder::new()
                            .name("oats-conn".into())
                            .spawn(move || {
                                let _ = catch_unwind(AssertUnwindSafe(|| handle(e, stream)));
                            });
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(10));
                    }
                    Err(_) => thread::sleep(Duration::from_millis(50)),
                }
            }
            #[cfg(unix)]
            let _ = std::fs::remove_file(&sock_for_cleanup);
            #[cfg(not(unix))]
            let _ = &sock_for_cleanup;
        })
        .map_err(|e| io_err("spawn accept", e))?;

    let eng = Arc::clone(&engine);
    let watcher = thread::Builder::new()
        .name("oats-watch".into())
        .spawn(move || {
            while !eng.is_shutdown() {
                thread::sleep(poll);
                // config.hot_reload is read live: turning it off stops file-driven reloads.
                if eng.hot_reload_enabled() && eng.file_changed() {
                    let _ = eng.reload("hot-reload");
                }
            }
        })
        .map_err(|e| io_err("spawn watcher", e))?;

    // Detached: ends on its own when the engine shuts down.
    crate::ics::spawn_scheduler(Arc::clone(&engine));

    Ok(ServerHandle {
        engine,
        accept: Some(accept),
        watcher: Some(watcher),
        socket,
    })
}

struct Conn {
    tx: Mutex<Half>,
}

impl Conn {
    fn send(&self, v: &Value) -> std::io::Result<()> {
        let mut line = v.to_string();
        line.push('\n');
        let mut tx = self.tx.lock().expect("conn writer");
        tx.write_all(line.as_bytes())?;
        tx.flush()
    }
}

pub fn ok_envelope(id: &Value, data: Value) -> Value {
    json!({"ok": true, "id": id, "data": data, "schema_version": SCHEMA_VERSION})
}

pub fn err_envelope(id: &Value, code: &str, message: &str) -> Value {
    json!({"ok": false, "id": id, "error": {"code": code, "message": message}, "schema_version": SCHEMA_VERSION})
}

fn hello() -> Value {
    json!({
        "event": "hello", "protocol": PROTOCOL, "app": crate::APP_NAME,
        "version": env!("CARGO_PKG_VERSION"), "schema_version": SCHEMA_VERSION,
    })
}

fn handle(engine: Arc<Engine>, stream: Stream) {
    let (rx, tx) = Half::pair(stream);
    let conn = Arc::new(Conn { tx: Mutex::new(tx) });
    if conn.send(&hello()).is_err() {
        return;
    }
    let mut reader = BufReader::new(rx);
    let mut buf: Vec<u8> = Vec::new();
    loop {
        buf.clear();
        let n = match reader.by_ref().take(MAX_LINE + 1).read_until(b'\n', &mut buf) {
            Ok(0) | Err(_) => return,
            Ok(n) => n,
        };
        if n as u64 > MAX_LINE && buf.last() != Some(&b'\n') {
            // Discard the rest of the line (bounded) so the error line is not lost to a reset
            // from unread data, then resync on the next line; give up on an endless line.
            let resynced = drain_line(&mut reader);
            let _ = conn.send(&err_envelope(&Value::Null, "request_too_large", "request line exceeds 1 MiB"));
            if resynced {
                continue;
            }
            return;
        }
        let reply = match std::str::from_utf8(&buf) {
            Err(_) => Some(err_envelope(&Value::Null, "bad_request", "request is not valid UTF-8")),
            Ok(text) if text.trim().is_empty() => None,
            Ok(text) => process(&engine, &conn, text.trim()),
        };
        if let Some(r) = reply {
            if conn.send(&r).is_err() {
                return;
            }
        }
    }
}

/// Skip input up to the next newline, reading at most 8 MiB. True if a newline was found.
fn drain_line<R: BufRead>(reader: &mut R) -> bool {
    let mut junk = Vec::new();
    for _ in 0..128 {
        junk.clear();
        match reader.by_ref().take(64 * 1024).read_until(b'\n', &mut junk) {
            Ok(0) | Err(_) => return false,
            Ok(_) if junk.last() == Some(&b'\n') => return true,
            Ok(_) => {}
        }
    }
    false
}

/// Parse one request line and execute it. `None` means the reply was already written (subscribe).
fn process(engine: &Arc<Engine>, conn: &Arc<Conn>, line: &str) -> Option<Value> {
    let parsed: Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(e) => return Some(err_envelope(&Value::Null, "bad_request", &format!("invalid JSON: {e}"))),
    };
    let Value::Object(mut obj) = parsed else {
        return Some(err_envelope(&Value::Null, "bad_request", "request must be a JSON object"));
    };
    let id = obj.remove("id").unwrap_or(Value::Null);
    if let Some(v) = obj.remove("v") {
        if v.as_u64() != Some(u64::from(PROTOCOL)) {
            return Some(err_envelope(
                &id,
                "unsupported_version",
                &format!("server speaks protocol {PROTOCOL}, request asked for {v}"),
            ));
        }
    }
    let Some(name) = obj.get("action").and_then(Value::as_str).map(str::to_string) else {
        return Some(err_envelope(&id, "bad_request", "missing string field 'action'"));
    };
    if !action::exists(&name) {
        return Some(err_envelope(&id, "unknown_action", &format!("unknown action '{name}'")));
    }
    let act: Action = match serde_json::from_value(Value::Object(obj)) {
        Ok(a) => a,
        Err(e) => return Some(err_envelope(&id, "bad_params", &e.to_string())),
    };
    if let Action::Subscribe { topics } = &act {
        return subscribe(engine, conn, &id, topics.clone().unwrap_or_default());
    }
    Some(match engine.dispatch(&act) {
        Ok(data) => ok_envelope(&id, data),
        Err(e) => err_envelope(&id, e.code, &e.message),
    })
}

/// Sends the ack itself (so queued events can never precede it) and returns None, or returns the
/// error envelope.
fn subscribe(engine: &Arc<Engine>, conn: &Arc<Conn>, id: &Value, topics: Vec<String>) -> Option<Value> {
    if let Some(bad) = topics.iter().find(|t| !EVENTS.contains(&t.as_str())) {
        return Some(err_envelope(
            id,
            "bad_params",
            &format!("unknown topic '{bad}'; known: {}", EVENTS.join(", ")),
        ));
    }
    let rx = engine.bus.subscribe(topics.clone());
    let subscribed = if topics.is_empty() { json!(EVENTS) } else { json!(topics) };
    if conn.send(&ok_envelope(id, json!({"subscribed": subscribed}))).is_err() {
        return None;
    }
    let c = Arc::clone(conn);
    let _ = thread::Builder::new().name("oats-sub".into()).spawn(move || {
        while let Ok(line) = rx.recv() {
            let mut l = line;
            l.push('\n');
            let mut tx = c.tx.lock().expect("conn writer");
            if tx.write_all(l.as_bytes()).and_then(|_| tx.flush()).is_err() {
                break;
            }
        }
    });
    None
}
