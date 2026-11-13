//! `oats-meetings-rs mcp` (dot-4uzgol.114): initialize and tools/list over the stdio framing, the
//! tool list generated from the manifest, tools/call forwarded to a live engine, and isError when
//! the engine is down.

use std::time::Duration;

use oats_audio::fake::FakeBackend;
use oats_core::action::SPECS;
use oats_core::control::engine::{Engine, EngineOpts};
use oats_core::control::server;
use oats_ctl::mcp::{self, Target};
use serde_json::{json, Value};

fn rpc(id: u64, method: &str, params: Value) -> String {
    json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}).to_string()
}

/// Feed `lines` to the server as stdin and parse each output line.
fn session(target: &Target, lines: &[String]) -> Vec<Value> {
    let input = lines.join("\n") + "\n";
    let mut out = vec![];
    mcp::serve(target, input.as_bytes(), &mut out).unwrap();
    String::from_utf8(out)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("{l}: {e}")))
        .collect()
}

fn init() -> String {
    rpc(
        1,
        "initialize",
        json!({"protocolVersion": "2025-06-18", "capabilities": {},
               "clientInfo": {"name": "test", "version": "0"}}),
    )
}

fn initialized() -> String {
    json!({"jsonrpc": "2.0", "method": "notifications/initialized"}).to_string()
}

fn call(id: u64, name: &str, args: Value) -> String {
    rpc(id, "tools/call", json!({"name": name, "arguments": args}))
}

#[test]
fn initialize_then_tools_list_is_the_manifest() {
    let dir = tempfile::tempdir().unwrap();
    let target = Target::RuntimeDir(dir.path().join("none"));
    let replies = session(
        &target,
        &[init(), initialized(), rpc(2, "tools/list", json!({}))],
    );
    // The notification gets no reply.
    assert_eq!(replies.len(), 2, "{replies:?}");
    let r = &replies[0];
    assert_eq!(r["jsonrpc"], "2.0");
    assert_eq!(r["id"], 1);
    assert_eq!(r["result"]["protocolVersion"], "2025-06-18");
    assert!(r["result"]["capabilities"]["tools"].is_object());
    assert_eq!(r["result"]["serverInfo"]["name"], "oats-meetings-rs");

    let tools = replies[1]["result"]["tools"].as_array().unwrap();
    assert_eq!(replies[1]["id"], 2);
    assert!(tools.len() >= SPECS.len());
    for (wire, doc, _, effect, params) in SPECS {
        let name = wire.replace('-', "_");
        let t = tools
            .iter()
            .find(|t| t["name"] == name.as_str())
            .unwrap_or_else(|| panic!("no tool {name}"));
        let expected_doc = if *wire == "subscribe" {
            "Native control-socket stream only; MCP clients should use events_since to read engine events by cursor.".to_string()
        } else {
            oats_core::action::doc(doc)
        };
        assert_eq!(t["description"], expected_doc);
        let s = &t["inputSchema"];
        assert_eq!(s["type"], "object", "{name}");
        let props = s["properties"].as_object().unwrap();
        assert_eq!(props.len(), params.len(), "{name}");
        let required: Vec<&str> = s["required"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        for (p, ty, req) in *params {
            let ps = props.get(*p).unwrap_or_else(|| panic!("{name}.{p}"));
            assert!(ps.is_object(), "{name}.{p}");
            assert_eq!(required.contains(p), *req, "{name}.{p}");
            assert_eq!(ty.starts_with("Option"), !req, "{name}.{p}");
            match ps.get("type").and_then(Value::as_str) {
                Some("string" | "boolean" | "integer" | "array") | None => {}
                Some(other) => panic!("{name}.{p}: type {other}"),
            }
        }
        assert!(required.iter().all(|r| props.contains_key(*r)), "{name}");
        let a = &t["annotations"];
        assert_eq!(a["readOnlyHint"], json!(*effect == "read"), "{name}");
    }
    let hint = |n: &str| {
        tools.iter().find(|t| t["name"] == n).unwrap()["annotations"]["destructiveHint"].clone()
    };
    assert_eq!(hint("session_delete"), json!(true));
    assert_eq!(hint("hook_remove"), json!(true));
    assert_eq!(hint("session_rename"), json!(false));
    let list = tools.iter().find(|t| t["name"] == "session_list").unwrap();
    assert_eq!(list["inputSchema"]["required"], json!([]));
    assert_eq!(list["inputSchema"]["properties"]["from"]["type"], "string");

    // The committed projection is what tools/list answers.
    let committed: Value = serde_json::from_str(
        &std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../config/mcp-tools.json"
        ))
        .unwrap(),
    )
    .unwrap();
    assert_eq!(committed["tools"].as_array().unwrap(), tools);
}

#[test]
fn tools_call_reaches_the_engine() {
    let dir = tempfile::tempdir().unwrap();
    let rt = dir.path().join("rt");
    let engine = Engine::new(EngineOpts {
        user_config: dir.path().join("cfg").join("config.toml"),
        pack: None,
        os: "linux".into(),
        arch: "x86_64".into(),
        machine: "default".into(),
        sessions_dir: dir.path().join("sessions"),
        secrets: None,
    })
    .unwrap();
    let _handle = server::serve_with_poll(engine, &rt, Duration::from_millis(40)).unwrap();
    let target = Target::RuntimeDir(rt);
    let replies = session(
        &target,
        &[
            init(),
            initialized(),
            call(2, "ping", json!({})),
            call(3, "config_get", json!({"key": "app.menubar"})),
            call(4, "config_get", json!({"key": "no.such.key"})),
            call(5, "session_list", json!({})),
            rpc(6, "ping", json!({})),
        ],
    );
    assert_eq!(replies.len(), 6, "{replies:?}");
    let text = |r: &Value| {
        r["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .to_string()
    };

    let pong = &replies[1];
    assert_eq!(pong["id"], 2);
    assert_eq!(pong["result"]["isError"], false);
    assert_eq!(pong["result"]["content"][0]["type"], "text");
    let v: Value = serde_json::from_str(&text(pong)).unwrap();
    assert_eq!(v["pong"], true);

    assert_eq!(replies[2]["result"]["isError"], false, "{}", replies[2]);
    assert_eq!(replies[3]["result"]["isError"], true, "{}", replies[3]);
    assert!(text(&replies[3]).starts_with("error["), "{}", replies[3]);
    assert_eq!(replies[4]["result"]["isError"], false, "{}", replies[4]);
    assert_eq!(replies[5]["id"], 6);
    assert_eq!(replies[5]["result"], json!({}));
}

#[test]
fn tools_call_with_the_engine_down_is_an_error_result() {
    let dir = tempfile::tempdir().unwrap();
    let target = Target::RuntimeDir(dir.path().join("none"));
    let replies = session(&target, &[init(), call(2, "ping", json!({}))]);
    let r = &replies[1]["result"];
    assert_eq!(r["isError"], true, "{r}");
    let t = r["content"][0]["text"].as_str().unwrap();
    assert!(t.contains("not_running"), "{t}");
}

#[test]
fn mcp_events_since_reads_bus_events_by_cursor() {
    let dir = tempfile::tempdir().unwrap();
    let rt = dir.path().join("rt");
    let engine = Engine::new(EngineOpts {
        user_config: dir.path().join("cfg").join("config.toml"),
        pack: None,
        os: "linux".into(),
        arch: "x86_64".into(),
        machine: "default".into(),
        sessions_dir: dir.path().join("sessions"),
        secrets: None,
    })
    .unwrap();
    engine.set_audio_factory(Box::new(|_| Ok(Box::new(FakeBackend::default()))));
    let _handle = server::serve_with_poll(engine, &rt, Duration::from_millis(40)).unwrap();
    let target = Target::RuntimeDir(rt);
    let replies = session(
        &target,
        &[
            call(1, "record_start", json!({"session": "mcp-cursor-test"})),
            call(2, "events_since", json!({"after": 0})),
        ],
    );
    assert_eq!(replies.len(), 2, "{replies:?}");
    assert_eq!(replies[0]["result"]["isError"], false, "{}", replies[0]);
    let first_text = replies[1]["result"]["content"][0]["text"].as_str().unwrap();
    let first: Value = serde_json::from_str(first_text).unwrap();
    let events = first["events"].as_array().unwrap();
    let started = events
        .iter()
        .find(|event| event["event"] == "recording-started")
        .expect("recording-started event");
    let cursor = started["seq"].as_u64().expect("bus sequence");
    assert_eq!(started["data"]["session"], "mcp-cursor-test");
    // Emit a later event, then pass the recording event's cursor to the next MCP call.
    let after_replies = session(
        &target,
        &[
            call(3, "window_show", json!({})),
            call(4, "events_since", json!({"after": cursor})),
        ],
    );
    assert_eq!(
        after_replies[0]["result"]["isError"], false,
        "{}",
        after_replies[0]
    );
    let after: Value = serde_json::from_str(
        after_replies[1]["result"]["content"][0]["text"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    let after_events = after["events"].as_array().unwrap();
    assert!(after_events
        .iter()
        .all(|event| event["seq"].as_u64().unwrap() > cursor));
    assert!(after_events
        .iter()
        .any(|event| event["event"] == "window-changed"));
}

#[test]
fn protocol_errors() {
    let dir = tempfile::tempdir().unwrap();
    let target = Target::RuntimeDir(dir.path().join("none"));
    let replies = session(
        &target,
        &[
            "{not json".into(),
            rpc(2, "nope/nope", json!({})),
            call(3, "no_such_tool", json!({})),
            call(4, "ping", json!([1])),
            "[]".into(),
            json!({"jsonrpc": "2.0", "id": 9, "result": {}}).to_string(),
        ],
    );
    let codes: Vec<i64> = replies
        .iter()
        .map(|r| r["error"]["code"].as_i64().unwrap())
        .collect();
    assert_eq!(codes, [-32700, -32601, -32602, -32602, -32600]);
    assert_eq!(replies[0]["id"], Value::Null);
    assert_eq!(replies[2]["id"], 3);
}
