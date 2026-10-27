//! `oats-meetings-rs config check|resolved|get|generate`. Pure over (args, env) so it is
//! testable without spawning a process; `run_env` is the process entry point.

use std::path::PathBuf;

use serde_json::{json, Value};

use super::resolve::{self, Inputs};
use super::{generate, ConfigError, Result, SCHEMA_VERSION};

#[derive(Debug, PartialEq, Eq)]
pub struct Out {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

pub fn run_env(args: &[String]) -> i32 {
    let out = run(args, &|k| std::env::var(k).ok());
    print!("{}", out.stdout);
    eprint!("{}", out.stderr);
    out.code
}

struct Opts {
    json: bool,
    pack: Option<String>,
    config: Option<String>,
    dir: Option<String>,
    check: bool,
    positional: Vec<String>,
}

fn parse(args: &[String]) -> Result<Opts> {
    let mut o = Opts {
        json: false,
        pack: None,
        config: None,
        dir: None,
        check: false,
        positional: vec![],
    };
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        let mut value = |name: &str| -> Result<String> {
            i += 1;
            args.get(i)
                .cloned()
                .ok_or_else(|| ConfigError::new("usage", format!("{name} needs a value")))
        };
        match a {
            "--json" => o.json = true,
            "--check" => o.check = true,
            "--pack" => o.pack = Some(value("--pack")?),
            "--config" => o.config = Some(value("--config")?),
            "--dir" => o.dir = Some(value("--dir")?),
            f if f.starts_with("--") => {
                return Err(ConfigError::new("usage", format!("unknown flag {f}")))
            }
            _ => o.positional.push(a.to_string()),
        }
        i += 1;
    }
    Ok(o)
}

fn user_config_path(o: &Opts, env: &dyn Fn(&str) -> Option<String>) -> Option<(PathBuf, bool)> {
    default_user_config_path(o.config.clone(), env)
}

/// (path, explicit): an explicitly named file must exist; the default one may be absent.
pub fn default_user_config_path(
    flag: Option<String>,
    env: &dyn Fn(&str) -> Option<String>,
) -> Option<(PathBuf, bool)> {
    if let Some(p) = flag.or_else(|| env("OATS_CONFIG")) {
        return Some((PathBuf::from(p), true));
    }
    let base = env("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| env("APPDATA").map(PathBuf::from))
        .or_else(|| env("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some((base.join("oats-meetings-rs").join("config.toml"), false))
}

fn load_user(o: &Opts, env: &dyn Fn(&str) -> Option<String>) -> Result<Option<String>> {
    let Some((path, explicit)) = user_config_path(o, env) else {
        return Ok(None);
    };
    match std::fs::read_to_string(&path) {
        Ok(s) => Ok(Some(s)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound && !explicit => Ok(None),
        Err(e) => Err(ConfigError::new("io_error", format!("{}: {e}", path.display()))),
    }
}

pub fn host(env: &dyn Fn(&str) -> Option<String>) -> (String, String, String) {
    (
        env("OATS_OS").unwrap_or_else(|| std::env::consts::OS.to_string()),
        env("OATS_ARCH").unwrap_or_else(|| std::env::consts::ARCH.to_string()),
        env("OATS_MACHINE").unwrap_or_else(|| "default".to_string()),
    )
}

fn resolved_for(o: &Opts, env: &dyn Fn(&str) -> Option<String>) -> Result<(resolve::Resolved, (String, String, String))> {
    resolve::validate_all_axes()?;
    resolve::managed_keys()?; // validates every embedded pack
    let user = load_user(o, env)?;
    let pack = o.pack.clone().or_else(|| env("OATS_PACK")).filter(|p| !p.is_empty());
    let h = host(env);
    let r = resolve::resolve(&Inputs {
        pack: pack.as_deref(),
        os: &h.0,
        arch: &h.1,
        machine: &h.2,
        user_toml: user.as_deref(),
    })?;
    Ok((r, h))
}

fn scalar(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

fn dispatch(args: &[String], env: &dyn Fn(&str) -> Option<String>) -> Result<(Value, String)> {
    let o = parse(args)?;
    let sub = o.positional.first().map(String::as_str).unwrap_or("");
    let rest: Vec<&String> = o.positional.iter().skip(1).collect();
    match sub {
        "check" => {
            let (r, _) = resolved_for(&o, env)?;
            r.typed()?;
            let pack = r.pack.clone();
            Ok((
                json!({"valid": true, "pack": pack}),
                format!("ok (pack: {})\n", pack.as_deref().unwrap_or("none")),
            ))
        }
        "resolved" => {
            let (r, h) = resolved_for(&o, env)?;
            let human: String = r
                .flat
                .iter()
                .map(|(k, v)| format!("{k} = {}  ({})\n", scalar(v), r.provenance[k]))
                .collect();
            Ok((
                json!({
                    "pack": r.pack,
                    "axes": {"os": h.0, "arch": h.1, "machine": h.2},
                    "config": r.nested(),
                    "provenance": r.provenance,
                }),
                human,
            ))
        }
        "get" => {
            let key = rest
                .first()
                .ok_or_else(|| ConfigError::new("usage", "config get KEY"))?;
            let (r, _) = resolved_for(&o, env)?;
            let v = r
                .flat
                .get(key.as_str())
                .ok_or_else(|| ConfigError::new("unknown_key", format!("unknown key '{key}'")))?;
            Ok((json!({"key": key, "value": v}), format!("{}\n", scalar(v))))
        }
        "generate" => {
            let dir = PathBuf::from(o.dir.clone().unwrap_or_else(|| "config".into()));
            if o.check {
                generate::check(&dir)?;
                Ok((json!({"drift": false}), "generated files are current\n".into()))
            } else {
                generate::write(&dir)?;
                Ok((json!({"written": dir}), format!("wrote {}\n", dir.display())))
            }
        }
        other => Err(ConfigError::new(
            "usage",
            format!("config: unknown subcommand '{other}' (check|resolved|get|generate)"),
        )),
    }
}

pub fn run(args: &[String], env: &dyn Fn(&str) -> Option<String>) -> Out {
    let json_out = args.iter().any(|a| a == "--json");
    match dispatch(args, env) {
        Ok((data, human)) => Out {
            code: 0,
            stdout: if json_out {
                format!("{}\n", json!({"ok": true, "data": data, "schema_version": SCHEMA_VERSION}))
            } else {
                human
            },
            stderr: String::new(),
        },
        Err(e) => {
            if json_out {
                Out {
                    code: if e.code == "usage" { 2 } else { 1 },
                    stdout: format!(
                        "{}\n",
                        json!({"ok": false, "error": {"code": e.code, "message": e.message}, "schema_version": SCHEMA_VERSION})
                    ),
                    stderr: String::new(),
                }
            } else {
                Out {
                    code: if e.code == "usage" { 2 } else { 1 },
                    stdout: String::new(),
                    stderr: format!("{e}\n"),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(a: &[&str]) -> Vec<String> {
        a.iter().map(|x| x.to_string()).collect()
    }

    fn no_env(_: &str) -> Option<String> {
        None
    }

    fn env_with(pairs: Vec<(&'static str, String)>) -> impl Fn(&str) -> Option<String> {
        move |k| pairs.iter().find(|(n, _)| *n == k).map(|(_, v)| v.clone())
    }

    fn data(o: &Out) -> Value {
        let v: Value = serde_json::from_str(&o.stdout).unwrap();
        assert_eq!(v["ok"], true, "{v}");
        v["data"].clone()
    }

    #[test]
    fn check_exits_zero() {
        let o = run(&s(&["check"]), &no_env);
        assert_eq!(o.code, 0, "{o:?}");
    }

    #[test]
    fn unknown_key_exits_one_with_code() {
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("c.toml");
        std::fs::write(&f, "[app]\nbogus = 1\n").unwrap();
        let o = run(
            &s(&["check", "--json", "--config", f.to_str().unwrap()]),
            &no_env,
        );
        assert_eq!(o.code, 1);
        let v: Value = serde_json::from_str(&o.stdout).unwrap();
        assert_eq!(v["ok"], false);
        assert_eq!(v["error"]["code"], "unknown_key");
        let o = run(&s(&["check", "--config", f.to_str().unwrap()]), &no_env);
        assert_eq!(o.code, 1);
        assert!(o.stderr.contains("unknown_key"), "{o:?}");
    }

    #[test]
    fn missing_explicit_config_is_an_error_default_may_be_absent() {
        let o = run(&s(&["check", "--config", "/nonexistent/x.toml"]), &no_env);
        assert_eq!(o.code, 1);
        let env = env_with(vec![("XDG_CONFIG_HOME", "/nonexistent/xdg".into())]);
        assert_eq!(run(&s(&["check"]), &env).code, 0);
    }

    #[test]
    fn oats_pack_cloud_differs_only_in_managed_keys() {
        let env_default = env_with(vec![("OATS_OS", "linux".into())]);
        let env_cloud = env_with(vec![("OATS_OS", "linux".into()), ("OATS_PACK", "cloud".into())]);
        let a = data(&run(&s(&["resolved", "--json"]), &env_default));
        let b = data(&run(&s(&["resolved", "--json"]), &env_cloud));
        assert_eq!(b["pack"], "cloud");
        let managed = resolve::managed_keys().unwrap();
        let mut fa = resolve::Flat::new();
        let mut fb = resolve::Flat::new();
        resolve::flatten(&a["config"], "", &mut fa);
        resolve::flatten(&b["config"], "", &mut fb);
        let diff: Vec<_> = fa.iter().filter(|(k, v)| fb.get(*k) != Some(*v)).map(|(k, _)| k.clone()).collect();
        assert!(!diff.is_empty());
        assert!(diff.iter().all(|k| managed.contains(k)), "{diff:?}");
    }

    #[test]
    fn unknown_pack_via_env() {
        let env = env_with(vec![("OATS_PACK", "nope".into())]);
        let o = run(&s(&["check", "--json"]), &env);
        assert_eq!(o.code, 1);
        assert!(o.stdout.contains("unknown_pack"));
    }

    #[test]
    fn get_and_usage() {
        let o = run(&s(&["get", "license.spdx"]), &no_env);
        assert_eq!(o.stdout, "LicenseRef-Undecided\n");
        assert_eq!(run(&s(&["get", "nope.key"]), &no_env).code, 1);
        assert_eq!(run(&s(&["frobnicate"]), &no_env).code, 2);
        assert_eq!(run(&s(&[]), &no_env).code, 2);
    }

    #[test]
    fn generate_check_cli() {
        let d = tempfile::tempdir().unwrap();
        let dir = d.path().to_str().unwrap();
        assert_eq!(run(&s(&["generate", "--check", "--dir", dir]), &no_env).code, 1);
        assert_eq!(run(&s(&["generate", "--dir", dir]), &no_env).code, 0);
        assert_eq!(run(&s(&["generate", "--check", "--dir", dir]), &no_env).code, 0);
        std::fs::write(d.path().join("defaults.toml"), "# hand edited\n").unwrap();
        let o = run(&s(&["generate", "--check", "--json", "--dir", dir]), &no_env);
        assert_eq!(o.code, 1);
        assert!(o.stdout.contains("\"drift\""));
    }
}
