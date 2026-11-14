//! `oats-meetings-rs mcp`: an MCP server on stdio (JSON-RPC 2.0, one message per line, protocol
//! 2025-06-18) over the control socket (dot-4uzgol.114). Its tools are the action manifest
//! (`action::mcp_tools`, also `config/mcp-tools.json`), never a hand-written list; `tools/call`
//! forwards the arguments to the running app as that action, one connection per call, so the
//! server outlives engine restarts.
//!
//! Options: `--runtime-dir D` or `--socket S`, as for ctl. Exit 0 at end of input.

use std::io::{self, BufRead, Write};
use std::path::PathBuf;

use oats_core::action;
use oats_core::control::{client::Client, paths};
use serde_json::{json, Map, Value};

pub const PROTOCOL_VERSION: &str = "2025-06-18";

/// Where `tools/call` connects.
pub enum Target {
    RuntimeDir(PathBuf),
    Socket(PathBuf),
}

pub fn run(args: &[String]) -> i32 {
    let env = |k: &str| std::env::var(k).ok();
    run_with(args, &env)
}

pub fn run_with(args: &[String], env: &dyn Fn(&str) -> Option<String>) -> i32 {
    let mut target = Target::RuntimeDir(paths::runtime_dir(env));
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        let Some(v) = args
            .get(i + 1)
            .filter(|_| a == "--runtime-dir" || a == "--socket")
        else {
            oats_core::elog!(
                "mcp: unknown argument '{a}'\nusage: oats-meetings-rs mcp [--runtime-dir D | --socket S]"
            );
            return 2;
        };
        target = if a == "--socket" {
            Target::Socket(PathBuf::from(v))
        } else {
            Target::RuntimeDir(PathBuf::from(v))
        };
        i += 2;
    }
    match oats_core::control::engine::Engine::from_env(env) {
        Ok(engine) if !engine.snapshot().config.mcp.enabled => {
            oats_core::elog!("mcp: disabled by mcp.enabled=false");
            return 3;
        }
        Err(e) => {
            oats_core::elog!("mcp: cannot read configuration: {e}");
            return 1;
        }
        _ => {}
    }
    match serve(&target, io::stdin().lock(), &mut io::stdout()) {
        Ok(()) => 0,
        Err(e) if e.kind() == io::ErrorKind::BrokenPipe => 0,
        Err(e) => {
            oats_core::elog!("mcp: {e}");
            1
        }
    }
}

/// Answer each request line of `input` on `out` until end of input.
pub fn serve(target: &Target, input: impl BufRead, out: &mut dyn Write) -> io::Result<()> {
    for line in input.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        if let Some(reply) = handle(target, &line) {
            writeln!(out, "{reply}")?;
            out.flush()?;
        }
    }
    Ok(())
}

fn error(id: Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

/// The reply to one message, or None for a notification.
pub fn handle(target: &Target, line: &str) -> Option<Value> {
    let msg: Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(e) => return Some(error(Value::Null, -32700, &format!("parse error: {e}"))),
    };
    let Some(method) = msg.get("method").and_then(Value::as_str) else {
        // A response (we send no requests, so nothing awaits it), else not a request at all.
        if msg.get("result").is_some() || msg.get("error").is_some() {
            return None;
        }
        let id = msg.get("id").cloned().unwrap_or(Value::Null);
        return Some(error(id, -32600, "invalid request"));
    };
    let id = msg.get("id")?.clone();
    let params = msg.get("params").cloned().unwrap_or(json!({}));
    let result = match method {
        "initialize" => Ok(json!({
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": {"tools": {"listChanged": false}},
            "serverInfo": {
                "name": oats_core::APP_NAME,
                "title": "oats-meetings-rs",
                "version": env!("CARGO_PKG_VERSION"),
            },
            "instructions": "Each tool is one oats-meetings-rs control action, run in the open app over its control socket; the app must be running (oats-meetings-rs or oats-meetings-rs --headless).",
        })),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({"tools": action::mcp_tools()})),
        "tools/call" => call(target, &params),
        _ => Err((-32601, format!("method not found: {method}"))),
    };
    Some(match result {
        Ok(r) => json!({"jsonrpc": "2.0", "id": id, "result": r}),
        Err((code, m)) => error(id, code, &m),
    })
}

fn tool_result(text: String, is_error: bool) -> Value {
    json!({"content": [{"type": "text", "text": text}], "isError": is_error})
}

fn call(target: &Target, params: &Value) -> Result<Value, (i64, String)> {
    let name = params.get("name").and_then(Value::as_str).unwrap_or("");
    let Some(wire) = action::from_tool_name(name) else {
        return Err((-32602, format!("unknown tool: '{name}'")));
    };
    let args: Map<String, Value> = match params.get("arguments") {
        None | Some(Value::Null) => Map::new(),
        Some(Value::Object(m)) => m.clone(),
        Some(_) => return Err((-32602, "arguments must be an object".into())),
    };
    let connected = match target {
        Target::RuntimeDir(d) => Client::connect(d),
        Target::Socket(s) => Client::connect_socket(s),
    };
    let mut client = match connected {
        Ok(c) => c,
        Err(e) => return Ok(tool_result(e.to_string(), true)),
    };
    let mut args = args;
    args.insert("_mcp".into(), json!(true));
    let v = match client.call(wire, args) {
        Ok(v) => v,
        Err(e) => return Ok(tool_result(e.to_string(), true)),
    };
    if v["ok"] == json!(true) {
        let text = match &v["data"] {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        };
        Ok(tool_result(text, false))
    } else {
        let text = format!(
            "error[{}]: {}",
            v["error"]["code"].as_str().unwrap_or("?"),
            v["error"]["message"].as_str().unwrap_or("")
        );
        Ok(tool_result(text, true))
    }
}
