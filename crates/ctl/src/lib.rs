//! `oats-meetings-rs ctl <verb> [args]`: a thin client. Verbs are not hand-written: the longest
//! dash-joined prefix of the words that names an action in the manifest is the action
//! (`config set k v` -> `config-set`), and the remaining words fill its parameters.
//!
//! Exit codes: 0 ok, 1 error response, 2 usage, 3 engine not running.

use std::path::PathBuf;
use std::sync::mpsc;
use std::time::Duration;

use oats_core::action::{self, SPECS};
use oats_core::control::{client::Client, paths};
use serde_json::{json, Map, Value};

struct Parsed {
    json: bool,
    runtime_dir: Option<PathBuf>,
    socket: Option<PathBuf>,
    count: Option<usize>,
    timeout_ms: u64,
    action: String,
    params: Map<String, Value>,
}

fn usage(msg: &str) -> Result<Parsed, String> {
    let names: Vec<&str> = SPECS.iter().map(|s| s.0).collect();
    Err(format!(
        "ctl: {msg}\nusage: oats-meetings-rs ctl [--json] [--runtime-dir D] <action words> [params]\nactions: {}",
        names.join(" ")
    ))
}

fn param_type(wire: &str, param: &str) -> &'static str {
    SPECS
        .iter()
        .find(|s| s.0 == wire)
        .and_then(|s| s.4.iter().find(|p| p.0 == param))
        .map(|p| p.1)
        .unwrap_or("String")
}

/// Parameters of type Option<bool> across all actions: `--confirm` alone means true.
fn is_bool_flag(name: &str) -> bool {
    SPECS.iter().any(|s| {
        s.4.iter()
            .any(|p| p.0 == name && p.1.replace(' ', "") == "Option<bool>")
    })
}

/// The engine is a different process with a different cwd: send absolute paths.
fn absolutize(raw: &str) -> String {
    let p = std::path::Path::new(raw);
    if p.is_absolute() {
        return raw.to_string();
    }
    std::env::current_dir()
        .map(|d| d.join(p).to_string_lossy().into_owned())
        .unwrap_or_else(|_| raw.to_string())
}

fn typed_value(wire: &str, param: &str, raw: &str) -> Value {
    if (wire == "session-import" && param == "from")
        || (wire == "stt-transcribe" && param == "path")
    {
        return Value::String(absolutize(raw));
    }
    let t = param_type(wire, param).replace(' ', "");
    if t == "String" || t == "Option<String>" {
        return Value::String(raw.to_string());
    }
    serde_json::from_str(raw).unwrap_or_else(|_| Value::String(raw.to_string()))
}

fn parse(args: &[String]) -> Result<Parsed, String> {
    let mut json = false;
    let mut runtime_dir = None;
    let mut socket = None;
    let mut count = None;
    let mut timeout_ms = u64::try_from(
        oats_core::config::keys::default_duration("control.client_timeout").as_millis(),
    )
    .unwrap_or(u64::MAX);
    let mut words: Vec<String> = vec![];
    let mut flags: Vec<(String, String)> = vec![];
    let mut topics: Vec<String> = vec![];
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        match a {
            "--json" => json = true,
            "--runtime-dir" | "--socket" | "--count" | "--timeout-ms" | "--topic" => {
                i += 1;
                let Some(v) = args.get(i) else {
                    return usage(&format!("{a} needs a value"));
                };
                match a {
                    "--runtime-dir" => runtime_dir = Some(PathBuf::from(v)),
                    "--socket" => socket = Some(PathBuf::from(v)),
                    "--count" => count = v.parse().ok(),
                    "--timeout-ms" => timeout_ms = v.parse().unwrap_or(timeout_ms),
                    _ => topics.push(v.clone()),
                }
            }
            f if f.starts_with("--") && is_bool_flag(f.trim_start_matches("--")) => {
                let v = match args.get(i + 1).map(String::as_str) {
                    Some(b @ ("true" | "false")) => {
                        i += 1;
                        b.to_string()
                    }
                    _ => "true".to_string(),
                };
                flags.push((f.trim_start_matches("--").to_string(), v));
            }
            f if f.starts_with("--") => {
                i += 1;
                let Some(v) = args.get(i) else {
                    return usage(&format!("{f} needs a value"));
                };
                flags.push((f.trim_start_matches("--").to_string(), v.clone()));
            }
            _ => words.push(a.to_string()),
        }
        i += 1;
    }
    let Some(n) = (1..=words.len())
        .rev()
        .find(|n| action::exists(&words[..*n].join("-")))
    else {
        return usage("unknown or missing action");
    };
    let wire = words[..n].join("-");
    let names = action::param_names(&wire);
    let mut params = Map::new();
    for (k, v) in flags {
        if !names.contains(&k.as_str()) {
            return usage(&format!("action '{wire}' has no parameter '{k}'"));
        }
        params.insert(k.clone(), typed_value(&wire, &k, &v));
    }
    let free_names: Vec<&str> = names
        .iter()
        .copied()
        .filter(|p| !params.contains_key(*p))
        .collect();
    let mut free = free_names.into_iter();
    for w in &words[n..] {
        let Some(p) = free.next() else {
            return usage(&format!("too many arguments for '{wire}'"));
        };
        params.insert(p.to_string(), typed_value(&wire, p, w));
    }
    if !topics.is_empty() {
        params.insert("topics".into(), json!(topics));
    }
    Ok(Parsed {
        json,
        runtime_dir,
        socket,
        count,
        timeout_ms,
        action: wire,
        params,
    })
}

pub fn run(args: &[String]) -> i32 {
    run_with(args, &|k| std::env::var(k).ok())
}

fn print_envelope(p: &Parsed, v: &Value) -> i32 {
    let ok = v["ok"] == json!(true);
    if p.json {
        println!("{v}");
    } else if ok {
        match &v["data"] {
            Value::String(s) => println!("{s}"),
            other => println!("{other}"),
        }
    } else {
        eprintln!(
            "error[{}]: {}",
            v["error"]["code"].as_str().unwrap_or("?"),
            v["error"]["message"].as_str().unwrap_or("")
        );
    }
    if ok {
        0
    } else {
        1
    }
}

pub fn run_with(args: &[String], env: &dyn Fn(&str) -> Option<String>) -> i32 {
    let p = match parse(args) {
        Ok(p) => p,
        Err(m) => {
            eprintln!("{m}");
            return 2;
        }
    };
    let dir = p
        .runtime_dir
        .clone()
        .unwrap_or_else(|| paths::runtime_dir(env));
    let connected = match &p.socket {
        Some(s) => Client::connect_socket(s),
        None => Client::connect(&dir),
    };
    let mut client = match connected {
        Ok(c) => c,
        Err(e) => {
            if p.json {
                println!(
                    "{}",
                    json!({"ok": false, "error": {"code": e.code, "message": e.message}})
                );
            } else {
                eprintln!("{e}");
            }
            return 3;
        }
    };
    if p.action == "subscribe" {
        return subscribe(client, &p);
    }
    match client.call(&p.action, p.params.clone()) {
        Ok(v) => print_envelope(&p, &v),
        Err(e) => {
            eprintln!("{e}");
            1
        }
    }
}

/// Print the ack, then event lines until `--count` events or `--timeout-ms` pass.
fn subscribe(mut client: Client, p: &Parsed) -> i32 {
    let ack = match client.call("subscribe", p.params.clone()) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("{e}");
            return 1;
        }
    };
    let code = print_envelope(p, &ack);
    if code != 0 {
        return code;
    }
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        while let Ok(ev) = client.next_event() {
            if tx.send(ev).is_err() {
                break;
            }
        }
    });
    let mut seen = 0usize;
    loop {
        match rx.recv_timeout(Duration::from_millis(p.timeout_ms)) {
            Ok(ev) => {
                println!("{ev}");
                seen += 1;
                if p.count.is_some_and(|c| seen >= c) {
                    return 0;
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => return if p.count.is_some() { 1 } else { 0 },
            Err(mpsc::RecvTimeoutError::Disconnected) => return 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(a: &[&str]) -> Vec<String> {
        a.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn words_map_to_actions_and_params() {
        let p = parse(&s(&["config", "set", "app.menubar", "true"])).unwrap();
        assert_eq!(p.action, "config-set");
        assert_eq!(p.params["key"], json!("app.menubar"));
        assert_eq!(p.params["value"], json!(true));
        let p = parse(&s(&["ping"])).unwrap();
        assert_eq!(p.action, "ping");
        let p = parse(&s(&["subscribe", "--topic", "setting-changed"])).unwrap();
        assert_eq!(p.params["topics"], json!(["setting-changed"]));
    }

    #[test]
    fn strings_stay_strings() {
        let p = parse(&s(&["config", "set", "stt.local.model", "123"])).unwrap();
        assert_eq!(p.params["key"], json!("stt.local.model"));
        assert_eq!(p.params["value"], json!(123));
        let p = parse(&s(&["session", "get", "42"])).unwrap();
        assert_eq!(p.params["session"], json!("42"));
    }

    #[test]
    fn bool_flags_and_absolute_import_path() {
        let p = parse(&s(&["session", "delete", "abc", "--confirm"])).unwrap();
        assert_eq!(p.action, "session-delete");
        assert_eq!(p.params["session"], json!("abc"));
        assert_eq!(p.params["confirm"], json!(true));
        let p = parse(&s(&["session", "delete", "--confirm", "false", "abc"])).unwrap();
        assert_eq!(p.params["confirm"], json!(false));
        assert_eq!(p.params["session"], json!("abc"));
        let p = parse(&s(&["session", "import", "--from", "rel/dir"])).unwrap();
        assert!(std::path::Path::new(p.params["from"].as_str().unwrap()).is_absolute());
        let p = parse(&s(&["session", "tag", "abc", "t", "--remove"])).unwrap();
        assert_eq!(p.params["remove"], json!(true));
    }

    #[test]
    fn usage_errors() {
        assert!(parse(&s(&[])).is_err());
        assert!(parse(&s(&["frobnicate"])).is_err());
        assert!(parse(&s(&["ping", "extra"])).is_err());
        assert!(parse(&s(&["config", "get", "--nope", "x"])).is_err());
    }
}
