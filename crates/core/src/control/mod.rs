//! Control API (dot-4uzgol.5): an engine that executes `Action`s, a JSON-lines server on a local
//! socket (unix domain socket; Windows named pipe) and a client. tmux-shaped: one command grammar,
//! a versioned envelope, an event stream, and a server that survives any client.
//!
//! Wire protocol (one JSON object per line):
//!   server -> client on connect: {"event":"hello","protocol":1,"app":..,"version":..,"schema_version":1}
//!   client -> server: {"id":<any>,"v":1,"action":"<name>", ...params}
//!   server -> client: {"ok":true,"id":..,"data":..,"schema_version":1}
//!                  or {"ok":false,"id":..,"error":{"code":..,"message":..},"schema_version":1}
//!   after `subscribe`: {"event":"<topic>","seq":N,"data":..,"schema_version":1} lines interleave.

pub mod bus;
pub mod client;
pub mod engine;
pub mod half;
pub mod paths;
pub mod server;
pub mod subsystem;

pub const PROTOCOL: u32 = 1;

use std::sync::Arc;

use crate::config::ConfigError;

/// Process entry for `oats-meetings-rs --headless`: engine + server until `quit`.
pub fn run_headless(env: &dyn Fn(&str) -> Option<String>) -> i32 {
    let started = (|| -> Result<server::ServerHandle, ConfigError> {
        let engine = engine::Engine::from_env(env)?;
        server::serve(Arc::clone(&engine), &paths::runtime_dir(env))
    })();
    match started {
        Ok(handle) => {
            eprintln!("oats-meetings-rs: engine listening on {}", handle.socket_display());
            handle.wait();
            0
        }
        Err(e) => {
            eprintln!("{e}");
            1
        }
    }
}

pub fn run_headless_env() -> i32 {
    run_headless(&|k| std::env::var(k).ok())
}
