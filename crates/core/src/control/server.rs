//! The socket server. Robustness contract (the frankenterm #85 class): nothing a client sends can
//! stop the server. Every connection runs on its own thread inside `catch_unwind`; bad bytes,
//! bad JSON, oversize lines and abrupt disconnects get an error line or a close, never a panic
//! that reaches the accept loop.
//!
//! Two config-driven pieces are `Subsystem`s so they take part in whole-config reload:
//! `control` (re-binds the socket when `control.socket` changes) and `lock` (takes or releases the
//! single-instance lock when `app.single_instance` changes).

use std::fs::{File, TryLockError};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use interprocess::local_socket::{
    traits::{Listener as _, Stream as _},
    GenericFilePath, Listener, ListenerNonblockingMode, ListenerOptions, Stream, ToFsName,
};
use serde_json::{json, Value};

use super::engine::Engine;
use super::half::Half;
use super::subsystem::Subsystem;
use super::{paths, PROTOCOL};
use crate::action::{self, Action, EVENTS};
use crate::config::typed::Config;
use crate::config::{ConfigError, Result, SCHEMA_VERSION};

pub struct ServerHandle {
    engine: Arc<Engine>,
    accept: Option<JoinHandle<()>>,
    watcher: Option<JoinHandle<()>>,
    current: Arc<Mutex<Option<PathBuf>>>,
    lock: Arc<LockSubsystem>,
}

/// The single-instance lock, for a shutdown that cannot wait for `ServerHandle::shutdown` (the GUI
/// on macOS, where `cx.quit()` ends the process from inside the run loop).
#[derive(Clone)]
pub struct InstanceLock(Arc<LockSubsystem>);

impl InstanceLock {
    /// Remove `engine.lock` and unlock it, if held. Idempotent.
    pub fn release(&self) {
        self.0.release();
    }
}

impl ServerHandle {
    /// Where the socket is bound right now (it moves when `control.socket` is reloaded).
    pub fn socket_path(&self) -> PathBuf {
        self.current
            .lock()
            .expect("socket path")
            .clone()
            .unwrap_or_default()
    }

    pub fn socket_display(&self) -> String {
        self.socket_path().display().to_string()
    }

    pub fn engine(&self) -> &Arc<Engine> {
        &self.engine
    }

    pub fn instance_lock(&self) -> InstanceLock {
        InstanceLock(Arc::clone(&self.lock))
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
        // Release the lock (removing engine.lock) and stop taking part in reloads.
        self.lock.release();
        self.engine.unregister("lock");
        self.engine.unregister("control");
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

fn acquire_lock(lock_path: &Path) -> Result<File> {
    let lock = File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(lock_path)
        .map_err(|e| io_err(&lock_path.display().to_string(), e))?;
    match lock.try_lock() {
        Ok(()) => Ok(lock),
        Err(TryLockError::WouldBlock) => Err(ConfigError::new(
            "already_running",
            format!("another engine holds {}", lock_path.display()),
        )),
        Err(TryLockError::Error(e)) => Err(io_err(&lock_path.display().to_string(), e)),
    }
}

/// `app.single_instance`: the lock is held while the key is true.
struct LockSubsystem {
    path: PathBuf,
    held: Mutex<Option<File>>,
    pending: Mutex<Option<File>>,
}

impl LockSubsystem {
    /// Unlink while still locked, then unlock: no other engine ever sees the file unlocked.
    fn release(&self) {
        *self.pending.lock().expect("lock") = None;
        if let Some(f) = self.held.lock().expect("lock").take() {
            let _ = std::fs::remove_file(&self.path);
            drop(f);
        }
    }
}

impl Subsystem for LockSubsystem {
    fn name(&self) -> &'static str {
        "lock"
    }

    fn validate(&self, new: &Config) -> Result<()> {
        if new.app.single_instance && self.held.lock().expect("lock").is_none() {
            *self.pending.lock().expect("lock") = Some(acquire_lock(&self.path)?);
        }
        Ok(())
    }

    fn apply(&self, _old: &Config, new: &Config) {
        if new.app.single_instance {
            if let Some(f) = self.pending.lock().expect("lock").take() {
                *self.held.lock().expect("lock") = Some(f);
            }
        } else {
            *self.held.lock().expect("lock") = None;
            *self.pending.lock().expect("lock") = None;
        }
    }

    fn abort(&self) {
        *self.pending.lock().expect("lock") = None;
    }
}

fn socket_target(cfg: &Config, default: &Path) -> PathBuf {
    if cfg.control.socket.is_empty() {
        default.to_path_buf()
    } else {
        PathBuf::from(&cfg.control.socket)
    }
}

/// A leftover socket file is removed only if nothing answers on it.
#[cfg(unix)]
fn clear_stale(path: &Path) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }
    let live = path
        .to_path_buf()
        .to_fs_name::<GenericFilePath>()
        .ok()
        .and_then(|n| Stream::connect(n).ok())
        .is_some();
    if live {
        return Err(ConfigError::new(
            "already_running",
            format!("{} is in use by a running engine", path.display()),
        ));
    }
    std::fs::remove_file(path).map_err(|e| io_err(&path.display().to_string(), e))
}

#[cfg(not(unix))]
fn clear_stale(_path: &Path) -> Result<()> {
    Ok(())
}

fn bind(path: &Path) -> Result<Listener> {
    clear_stale(path)?;
    if let Some(dir) = path
        .parent()
        .filter(|d| !d.as_os_str().is_empty() && cfg!(unix))
    {
        std::fs::create_dir_all(dir).map_err(|e| io_err(&dir.display().to_string(), e))?;
    }
    let name = path
        .to_path_buf()
        .to_fs_name::<GenericFilePath>()
        .map_err(|e| io_err("socket name", e))?;
    ListenerOptions::new()
        .name(name)
        .nonblocking(ListenerNonblockingMode::Accept)
        .create_sync()
        .map_err(|e| io_err(&path.display().to_string(), e))
}

/// `control.socket`: the accept loop re-binds when the desired path changes.
struct ControlSubsystem {
    default: PathBuf,
    desired: Arc<Mutex<Option<PathBuf>>>,
}

struct McpHttpSubsystem {
    desired: Mutex<Option<String>>,
}
impl Subsystem for McpHttpSubsystem {
    fn name(&self) -> &'static str {
        "mcp-http"
    }
    fn validate(&self, new: &Config) -> Result<()> {
        let next = new
            .mcp
            .http
            .enabled
            .then(|| format!("{}:{}", new.mcp.http.bind, new.mcp.http.port));
        if next != *self.desired.lock().expect("MCP HTTP address") {
            if let Some(addr) = next {
                drop(TcpListener::bind(&addr).map_err(|e| io_err(&addr, e))?);
            }
        }
        Ok(())
    }
    fn apply(&self, _old: &Config, new: &Config) {
        *self.desired.lock().expect("MCP HTTP address") = new
            .mcp
            .http
            .enabled
            .then(|| format!("{}:{}", new.mcp.http.bind, new.mcp.http.port));
    }
}

impl Subsystem for ControlSubsystem {
    fn name(&self) -> &'static str {
        "control"
    }

    fn validate(&self, new: &Config) -> Result<()> {
        let target = new.api.enabled.then(|| socket_target(new, &self.default));
        if target != *self.desired.lock().expect("desired") {
            // Prove the new address can be bound before anything changes.
            if let Some(target) = target {
                drop(bind(&target)?);
                #[cfg(unix)]
                let _ = std::fs::remove_file(&target);
            }
        }
        Ok(())
    }

    fn apply(&self, _old: &Config, new: &Config) {
        *self.desired.lock().expect("desired") =
            new.api.enabled.then(|| socket_target(new, &self.default));
    }
}

/// Bind the socket, take the single-instance lock (while `app.single_instance`), and start the
/// accept and watcher threads.
pub fn serve(engine: Arc<Engine>, runtime_dir: &Path) -> Result<ServerHandle> {
    // OATS_RELOAD_POLL_MS overrides `config.poll_interval` (read live) for the watcher.
    let poll = std::env::var("OATS_RELOAD_POLL_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .map(Duration::from_millis);
    serve_inner(engine, runtime_dir, poll)
}

/// `poll` is how often the watcher checks the user config file for hot reload.
pub fn serve_with_poll(
    engine: Arc<Engine>,
    runtime_dir: &Path,
    poll: Duration,
) -> Result<ServerHandle> {
    serve_inner(engine, runtime_dir, Some(poll))
}

fn serve_inner(
    engine: Arc<Engine>,
    runtime_dir: &Path,
    poll: Option<Duration>,
) -> Result<ServerHandle> {
    engine.set_runtime_dir(runtime_dir);
    std::fs::create_dir_all(runtime_dir)
        .map_err(|e| io_err(&runtime_dir.display().to_string(), e))?;
    let cfg = engine.snapshot();
    let first_http = if cfg.config.mcp.http.enabled {
        let addr = format!("{}:{}", cfg.config.mcp.http.bind, cfg.config.mcp.http.port);
        let listener = TcpListener::bind(&addr).map_err(|e| io_err(&addr, e))?;
        listener
            .set_nonblocking(true)
            .map_err(|e| io_err("MCP HTTP listener", e))?;
        Some(listener)
    } else {
        None
    };
    engine.register(Arc::new(McpHttpSubsystem {
        desired: Mutex::new(
            cfg.config
                .mcp
                .http
                .enabled
                .then(|| format!("{}:{}", cfg.config.mcp.http.bind, cfg.config.mcp.http.port)),
        ),
    }));
    let mut address = cfg
        .config
        .mcp
        .http
        .enabled
        .then(|| format!("{}:{}", cfg.config.mcp.http.bind, cfg.config.mcp.http.port));
    let held = if cfg.config.app.single_instance {
        Some(acquire_lock(&paths::lock_path(runtime_dir))?)
    } else {
        None
    };
    let default_socket = paths::socket_path(runtime_dir);
    let socket = cfg
        .config
        .api
        .enabled
        .then(|| socket_target(&cfg.config, &default_socket));
    let listener = socket.as_ref().map(|s| bind(s)).transpose()?;
    let current = Arc::new(Mutex::new(socket.clone()));
    let desired = Arc::new(Mutex::new(socket));
    let lock = Arc::new(LockSubsystem {
        path: paths::lock_path(runtime_dir),
        held: Mutex::new(held),
        pending: Mutex::new(None),
    });
    engine.register(Arc::clone(&lock) as Arc<dyn Subsystem>);
    engine.register(Arc::new(ControlSubsystem {
        default: default_socket,
        desired: Arc::clone(&desired),
    }));

    let http_engine = Arc::clone(&engine);
    thread::Builder::new()
        .name("oats-mcp-http".into())
        .spawn(move || {
            let mut listener = first_http;
            while !http_engine.is_shutdown() {
                let cfg = http_engine.snapshot();
                let want = if cfg.config.mcp.http.enabled {
                    Some(format!(
                        "{}:{}",
                        cfg.config.mcp.http.bind, cfg.config.mcp.http.port
                    ))
                } else {
                    None
                };
                if want != address {
                    let next = want
                        .as_deref()
                        .and_then(|addr| TcpListener::bind(addr).ok());
                    if let Some(next) = next {
                        let _ = next.set_nonblocking(true);
                        address = want;
                        listener = Some(next);
                    } else if !cfg.config.mcp.http.enabled {
                        listener = None;
                        address = None;
                    }
                }
                if let Some(l) = listener.as_ref() {
                    match l.accept() {
                        Ok((stream, _)) => {
                            let e = Arc::clone(&http_engine);
                            let _ = thread::Builder::new()
                                .name("oats-mcp-http-conn".into())
                                .spawn(move || mcp_http(e, stream));
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                        Err(_) => {}
                    }
                }
                thread::sleep(
                    http_engine
                        .snapshot()
                        .config
                        .control
                        .accept_poll
                        .as_duration(),
                );
            }
        })
        .map_err(|e| io_err("spawn MCP HTTP", e))?;

    let eng = Arc::clone(&engine);
    let cur = Arc::clone(&current);
    let accept = thread::Builder::new()
        .name("oats-accept".into())
        .spawn(move || {
            let mut listener = listener;
            while !eng.is_shutdown() {
                let want = desired.lock().expect("desired").clone();
                let have = cur.lock().expect("current").clone();
                if want != have {
                    match want.as_ref().map(|p| bind(p)).transpose() {
                        Ok(l) => {
                            listener = l;
                            #[cfg(unix)]
                            if let Some(old) = have.as_ref() {
                                let _ = std::fs::remove_file(old);
                            }
                            *cur.lock().expect("current") = want;
                        }
                        Err(_) => *desired.lock().expect("desired") = have,
                    }
                }
                let Some(active) = listener.as_ref() else {
                    thread::sleep(eng.snapshot().config.control.accept_poll.as_duration());
                    continue;
                };
                match active.accept() {
                    Ok(stream) => {
                        let e = Arc::clone(&eng);
                        let _ = thread::Builder::new()
                            .name("oats-conn".into())
                            .spawn(move || {
                                let _ = catch_unwind(AssertUnwindSafe(|| handle(e, stream)));
                            });
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(eng.snapshot().config.control.accept_poll.as_duration());
                    }
                    Err(_) => {
                        thread::sleep(eng.snapshot().config.control.accept_backoff.as_duration())
                    }
                }
            }
            #[cfg(unix)]
            if let Some(path) = cur.lock().expect("current").as_ref() {
                let _ = std::fs::remove_file(path);
            }
        })
        .map_err(|e| io_err("spawn accept", e))?;

    let eng = Arc::clone(&engine);
    let watcher =
        thread::Builder::new()
            .name("oats-watch".into())
            .spawn(move || {
                while !eng.is_shutdown() {
                    thread::sleep(poll.unwrap_or_else(|| {
                        eng.snapshot().config.config.poll_interval.as_duration()
                    }));
                    // config.hot_reload is read live: turning it off stops file-driven reloads. A
                    // file that was rejected is not retried until its content changes.
                    if eng.hot_reload_enabled() && eng.file_needs_reload() {
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
        current,
        lock,
    })
}

/// Minimal Streamable HTTP transport for MCP: JSON-RPC POST requests at `/mcp`.
fn mcp_http(engine: Arc<Engine>, mut stream: TcpStream) {
    use std::io::{Read, Write};
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while head.len() < 16_384 && !head.ends_with(b"\r\n\r\n") {
        if stream.read_exact(&mut byte).is_err() {
            return;
        }
        head.push(byte[0]);
    }
    let text = String::from_utf8_lossy(&head);
    let mut lines = text.lines();
    let request = lines.next().unwrap_or_default();
    let length = lines
        .filter_map(|l| l.split_once(':'))
        .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, v)| v.trim().parse::<usize>().ok())
        .unwrap_or(0);
    let mut body = vec![0; length.min(16 * 1024 * 1024)];
    if stream.read_exact(&mut body).is_err() || !request.starts_with("POST /mcp ") {
        let _ = stream.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n");
        return;
    }
    let msg: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(_) => {
            let _ = stream.write_all(b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\n\r\n");
            return;
        }
    };
    let id = msg.get("id").cloned().unwrap_or(Value::Null);
    let method = msg.get("method").and_then(Value::as_str).unwrap_or("");
    let result = if !engine.snapshot().config.mcp.enabled {
        Err((-32603, "MCP is disabled by mcp.enabled".into()))
    } else {
        match method {
            "initialize" => Ok(
                json!({"protocolVersion":"2025-06-18","capabilities":{"tools":{"listChanged":false}},"serverInfo":{"name":crate::APP_NAME,"version":env!("CARGO_PKG_VERSION")}}),
            ),
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({"tools":action::mcp_tools()})),
            "tools/call" => {
                let name = msg["params"]["name"].as_str().unwrap_or("");
                let wire = action::from_tool_name(name);
                let args = msg["params"]["arguments"]
                    .as_object()
                    .cloned()
                    .unwrap_or_default();
                if !engine.snapshot().config.mcp.enabled {
                    Err((-32603, "MCP is disabled by mcp.enabled".into()))
                } else if engine.snapshot().config.mcp.read_only
                    && wire
                        .and_then(|w| action::SPECS.iter().find(|s| s.0 == w))
                        .is_some_and(|s| s.3 != "read")
                {
                    Ok(
                        json!({"content":[{"type":"text","text":"MCP is read-only (mcp.read_only=true)"}],"isError":true}),
                    )
                } else if let Some(wire) = wire {
                    let mut o = args;
                    o.insert("action".into(), json!(wire));
                    match serde_json::from_value::<Action>(Value::Object(o)) {
                        Ok(a) => match engine.dispatch(&a) {
                            Ok(v) => Ok(
                                json!({"content":[{"type":"text","text":v.to_string()}],"isError":false}),
                            ),
                            Err(e) => Ok(
                                json!({"content":[{"type":"text","text":e.to_string()}],"isError":true}),
                            ),
                        },
                        Err(e) => Err((-32602, e.to_string())),
                    }
                } else {
                    Err((-32602, "unknown MCP tool".into()))
                }
            }
            _ => Err((-32601, format!("method not found: {method}"))),
        }
    };
    let reply = if id.is_null() {
        Value::Null
    } else {
        match result {
            Ok(r) => json!({"jsonrpc":"2.0","id":id,"result":r}),
            Err((c, m)) => json!({"jsonrpc":"2.0","id":id,"error":{"code":c,"message":m}}),
        }
    };
    if reply.is_null() {
        let _ = stream.write_all(b"HTTP/1.1 202 Accepted\r\nContent-Length: 0\r\n\r\n");
        return;
    }
    let bytes = reply.to_string();
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
        bytes.len(),
        bytes
    );
    let _ = stream.write_all(response.as_bytes());
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
    // BSD/macOS accept() returns sockets that inherit the listener's O_NONBLOCK (Linux's do not);
    // the per-connection loop blocks, so make that explicit.
    if stream.set_nonblocking(false).is_err() {
        return;
    }
    let (rx, tx) = Half::pair(stream);
    let conn = Arc::new(Conn { tx: Mutex::new(tx) });
    if conn.send(&hello()).is_err() {
        return;
    }
    let mut reader = BufReader::new(rx);
    let mut buf: Vec<u8> = Vec::new();
    loop {
        buf.clear();
        // control.max_line is read per request, so a reload applies to open connections.
        let max_line = engine.snapshot().config.control.max_line;
        let n = match reader
            .by_ref()
            .take(max_line + 1)
            .read_until(b'\n', &mut buf)
        {
            Ok(0) | Err(_) => return,
            Ok(n) => n,
        };
        if n as u64 > max_line && buf.last() != Some(&b'\n') {
            // Discard the rest of the line (bounded) so the error line is not lost to a reset
            // from unread data, then resync on the next line; give up on an endless line.
            let ctl = &engine.snapshot().config.control;
            let resynced = drain_line(&mut reader, ctl.drain_chunk as u64, ctl.drain_max);
            let _ = conn.send(&err_envelope(
                &Value::Null,
                "request_too_large",
                &format!("request line exceeds control.max_line ({max_line} bytes)"),
            ));
            if resynced {
                continue;
            }
            return;
        }
        let reply = match std::str::from_utf8(&buf) {
            Err(_) => Some(err_envelope(
                &Value::Null,
                "bad_request",
                "request is not valid UTF-8",
            )),
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

/// Skip input up to the next newline, `chunk` bytes at a time and at most `max` bytes in all
/// (`control.drain_chunk`, `control.drain_max`). True if a newline was found.
fn drain_line<R: BufRead>(reader: &mut R, chunk: u64, max: u64) -> bool {
    let mut junk = Vec::new();
    let mut drained = 0u64;
    while drained < max {
        junk.clear();
        let step = chunk.min(max - drained);
        match reader.by_ref().take(step).read_until(b'\n', &mut junk) {
            Ok(0) | Err(_) => return false,
            Ok(_) if junk.last() == Some(&b'\n') => return true,
            Ok(n) => drained += n as u64,
        }
    }
    false
}

/// Parse one request line and execute it. `None` means the reply was already written (subscribe).
fn process(engine: &Arc<Engine>, conn: &Arc<Conn>, line: &str) -> Option<Value> {
    let parsed: Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(e) => {
            return Some(err_envelope(
                &Value::Null,
                "bad_request",
                &format!("invalid JSON: {e}"),
            ))
        }
    };
    let Value::Object(mut obj) = parsed else {
        return Some(err_envelope(
            &Value::Null,
            "bad_request",
            "request must be a JSON object",
        ));
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
    let Some(name) = obj
        .get("action")
        .and_then(Value::as_str)
        .map(str::to_string)
    else {
        return Some(err_envelope(
            &id,
            "bad_request",
            "missing string field 'action'",
        ));
    };
    let mcp = obj
        .remove("_mcp")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    if mcp {
        let cfg = engine.snapshot();
        if !cfg.config.mcp.enabled {
            return Some(err_envelope(
                &id,
                "mcp_disabled",
                "MCP is disabled by mcp.enabled",
            ));
        }
        if cfg.config.mcp.read_only
            && action::SPECS
                .iter()
                .find(|s| s.0 == name)
                .is_some_and(|s| s.3 != "read")
        {
            return Some(err_envelope(
                &id,
                "mcp_read_only",
                "MCP action is disabled by mcp.read_only",
            ));
        }
    }
    if !action::exists(&name) {
        return Some(err_envelope(
            &id,
            "unknown_action",
            &format!("unknown action '{name}'"),
        ));
    }
    // A stop or quit over the socket says it came from ctl unless the caller said who it is.
    if matches!(name.as_str(), "record-stop" | "quit") {
        obj.entry("by").or_insert_with(|| Value::from("ctl"));
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
fn subscribe(
    engine: &Arc<Engine>,
    conn: &Arc<Conn>,
    id: &Value,
    topics: Vec<String>,
) -> Option<Value> {
    if let Some(bad) = topics.iter().find(|t| !EVENTS.contains(&t.as_str())) {
        return Some(err_envelope(
            id,
            "bad_params",
            &format!("unknown topic '{bad}'; known: {}", EVENTS.join(", ")),
        ));
    }
    let rx = engine.bus.subscribe_with(
        topics.clone(),
        engine.snapshot().config.control.event_queue as usize,
    );
    let subscribed = if topics.is_empty() {
        json!(EVENTS)
    } else {
        json!(topics)
    };
    if conn
        .send(&ok_envelope(id, json!({"subscribed": subscribed})))
        .is_err()
    {
        return None;
    }
    let c = Arc::clone(conn);
    let _ = thread::Builder::new()
        .name("oats-sub".into())
        .spawn(move || {
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

#[cfg(test)]
mod drain_tests {
    use super::*;

    /// control.drain_chunk / control.drain_max: the newline is found when it lies within max bytes
    /// of the start, not when it lies beyond; the chunk size changes nothing but the read pattern.
    #[test]
    fn drain_stops_at_the_newline_or_the_cap() {
        for chunk in [1024u64, 4096, 65_536] {
            for (len, max, want) in [
                (1_000usize, 2_000u64, true),
                (1_999, 2_000, true),
                (2_000, 2_000, false),
                (5_000, 2_000, false),
            ] {
                let mut data = vec![b'x'; len];
                data.push(b'\n');
                data.extend_from_slice(b"rest");
                let mut r = std::io::BufReader::new(&data[..]);
                assert_eq!(
                    drain_line(&mut r, chunk, max),
                    want,
                    "chunk {chunk} len {len} max {max}"
                );
            }
        }
        // EOF before a newline is not a resync.
        let mut r = std::io::BufReader::new(&b"no newline"[..]);
        assert!(!drain_line(&mut r, 1024, 4096));
    }
}
