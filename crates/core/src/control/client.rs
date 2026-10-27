//! Blocking client for the control socket (used by `ctl` and tests).

use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::time::{Duration, Instant};

use interprocess::local_socket::{
    traits::Stream as _, GenericFilePath, RecvHalf, SendHalf, Stream, ToFsName,
};
use serde_json::{json, Map, Value};

use super::{paths, PROTOCOL};
use crate::config::{ConfigError, Result};

pub struct Client {
    rx: BufReader<RecvHalf>,
    tx: SendHalf,
    pub hello: Value,
    next_id: u64,
}

impl Client {
    /// Connect to the engine whose runtime dir is `runtime_dir`; reads the hello line.
    pub fn connect(runtime_dir: &Path) -> Result<Client> {
        let sock = paths::socket_path(runtime_dir);
        let name = sock
            .clone()
            .to_fs_name::<GenericFilePath>()
            .map_err(|e| ConfigError::new("io_error", e.to_string()))?;
        let stream = Stream::connect(name).map_err(|e| {
            ConfigError::new("not_running", format!("cannot connect to {}: {e}", sock.display()))
        })?;
        let (rx, tx) = stream.split();
        let mut c = Client {
            rx: BufReader::new(rx),
            tx,
            hello: Value::Null,
            next_id: 1,
        };
        let hello = c.read_line()?;
        if hello["event"] != "hello" || hello["protocol"] != json!(PROTOCOL) {
            return Err(ConfigError::new(
                "unsupported_version",
                format!("server hello was {hello}; this client speaks protocol {PROTOCOL}"),
            ));
        }
        c.hello = hello;
        Ok(c)
    }

    pub fn send_raw(&mut self, bytes: &[u8]) -> Result<()> {
        self.tx
            .write_all(bytes)
            .and_then(|_| self.tx.flush())
            .map_err(|e| ConfigError::new("io_error", e.to_string()))
    }

    pub fn read_line(&mut self) -> Result<Value> {
        let mut line = String::new();
        let n = self
            .rx
            .read_line(&mut line)
            .map_err(|e| ConfigError::new("io_error", e.to_string()))?;
        if n == 0 {
            return Err(ConfigError::new("io_error", "connection closed"));
        }
        serde_json::from_str(line.trim())
            .map_err(|e| ConfigError::new("parse_error", format!("bad server line: {e}")))
    }

    /// Send `{"id":..,"v":1,"action":name, ...params}` with a fresh id and return the envelope
    /// whose id matches (skipping interleaved events).
    pub fn call(&mut self, action: &str, params: Map<String, Value>) -> Result<Value> {
        let id = self.next_id;
        self.next_id += 1;
        self.call_with_id(json!(id), action, params)
    }

    pub fn call_with_id(&mut self, id: Value, action: &str, params: Map<String, Value>) -> Result<Value> {
        let mut obj = params;
        obj.insert("id".into(), id.clone());
        obj.insert("v".into(), json!(PROTOCOL));
        obj.insert("action".into(), json!(action));
        let mut line = Value::Object(obj).to_string();
        line.push('\n');
        self.send_raw(line.as_bytes())?;
        loop {
            let v = self.read_line()?;
            if v.get("event").is_none() && v["id"] == id {
                return Ok(v);
            }
        }
    }

    /// Block until the next event line (skipping any stray responses).
    pub fn next_event(&mut self) -> Result<Value> {
        loop {
            let v = self.read_line()?;
            if v.get("event").is_some() {
                return Ok(v);
            }
        }
    }
}

/// Connect, retrying until `timeout` (the engine may still be starting).
pub fn connect_retry(runtime_dir: &Path, timeout: Duration) -> Result<Client> {
    let start = Instant::now();
    loop {
        match Client::connect(runtime_dir) {
            Ok(c) => return Ok(c),
            Err(e) if start.elapsed() > timeout => return Err(e),
            Err(_) => std::thread::sleep(Duration::from_millis(25)),
        }
    }
}
