//! Permissions (dot-4uzgol.96): what the app needs to record and title a meeting, whether it has
//! it, how it got it, and acquiring whatever can be acquired without a person.
//!
//! - Linux: mic and system audio are clients of the user's PipeWire / PulseAudio server (the pulse
//!   protocol, or `pw-record`). A process of the user's session needs no portal and sees no
//!   prompt; system audio is the monitor source of a sink. No xdg-desktop-portal ScreenCast or
//!   Camera request is made anywhere, so there is no dialog and no `restore_token` to keep. In a
//!   Flatpak the audio socket is the manifest's `--socket=pulseaudio`, and the Device portal's
//!   permission store (`flatpak permission-set devices microphone <app> yes`) can pre-grant the
//!   microphone; in a Snap it is the `audio-record` interface.
//! - Calendar: ics and CalDAV feeds are plain file / HTTP reads: no OS permission, gated only by
//!   `network.allow`, `ics.allow_file` and the feed's header secret. EventKit (macOS Calendars)
//!   is the one calendar source with a system grant.
//! - macOS: mic and system audio are TCC grants; they are read without prompting and only a
//!   person can grant them.
//!
//! `perms-status` never changes anything. `perms-ensure` starts the user audio server, loads a
//! null sink when there is no output to monitor (`audio.ensure_null_sink`) and pre-grants the
//! Flatpak device permission, then reports the status again. Every outside command goes through
//! `Host`, so tests drive it with a fake.

use std::process::Command;
use std::time::Instant;

use serde_json::{json, Value};

use crate::config::typed::Config;
use crate::control::engine::Engine;
use crate::ics::fetch::{self, Source};

/// The null sink `perms-ensure` loads when the audio server has no output at all.
pub const NULL_SINK: &str = "oats_meetings_null";

/// States a permission row counts as fine for `all_granted`.
const OK_STATES: &[&str] = &["granted", "not_needed", "disabled"];

/// The machine as perms sees it: environment, files, commands.
pub trait Host {
    fn env(&self, key: &str) -> Option<String>;
    fn read(&self, path: &str) -> Option<String>;
    /// Run a program; its trimmed stdout on success, else a short reason.
    fn run(&self, prog: &str, args: &[&str]) -> std::result::Result<String, String>;
    fn sleep(&self, d: std::time::Duration) {
        std::thread::sleep(d);
    }
}

pub struct SystemHost;

impl Host for SystemHost {
    fn env(&self, key: &str) -> Option<String> {
        std::env::var(key).ok().filter(|v| !v.is_empty())
    }

    fn read(&self, path: &str) -> Option<String> {
        std::fs::read_to_string(path).ok()
    }

    fn run(&self, prog: &str, args: &[&str]) -> std::result::Result<String, String> {
        let out = Command::new(prog)
            .args(args)
            .output()
            .map_err(|e| format!("{prog}: {e}"))?;
        if out.status.success() {
            Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
        } else {
            let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
            Err(format!("{prog} {}: {}: {err}", args.join(" "), out.status))
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Sandbox {
    None,
    /// App id and the `[Context]` of `/.flatpak-info`.
    Flatpak {
        app: String,
        sockets: Vec<String>,
        shared: Vec<String>,
    },
    Snap {
        name: String,
    },
}

impl Sandbox {
    pub fn name(&self) -> &'static str {
        match self {
            Sandbox::None => "none",
            Sandbox::Flatpak { .. } => "flatpak",
            Sandbox::Snap { .. } => "snap",
        }
    }
}

/// The value list of `key=` inside `[section]` of a keyfile (`;`-separated).
fn keyfile_list(text: &str, section: &str, key: &str) -> Vec<String> {
    let mut inside = false;
    for line in text.lines().map(str::trim) {
        if line.starts_with('[') {
            inside = line == format!("[{section}]");
        } else if inside {
            if let Some((k, v)) = line.split_once('=') {
                if k.trim() == key {
                    return v
                        .split(';')
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(str::to_string)
                        .collect();
                }
            }
        }
    }
    vec![]
}

pub fn sandbox_of(host: &dyn Host) -> Sandbox {
    if let Some(info) = host.read("/.flatpak-info") {
        let app = keyfile_list(&info, "Application", "name")
            .into_iter()
            .next()
            .or_else(|| host.env("FLATPAK_ID"))
            .unwrap_or_default();
        return Sandbox::Flatpak {
            app,
            sockets: keyfile_list(&info, "Context", "sockets"),
            shared: keyfile_list(&info, "Context", "shared"),
        };
    }
    if let Some(name) = host.env("SNAP_NAME") {
        return Sandbox::Snap { name };
    }
    Sandbox::None
}

fn row(name: &str, state: &str, how: impl Into<String>) -> Value {
    json!({"name": name, "state": state, "how": how.into()})
}

fn with_fix(mut v: Value, fix: impl Into<String>) -> Value {
    v["fix"] = json!(fix.into());
    v
}

/// The device an empty or named `audio.*.device` resolves to, from a `devices-list` answer.
#[cfg_attr(target_os = "macos", allow(dead_code))]
fn pick<'a>(devices: &'a [Value], kind: &str, wanted: &str) -> Option<&'a Value> {
    if !wanted.is_empty() {
        return devices
            .iter()
            .find(|d| d["id"] == wanted || d["name"] == wanted);
    }
    let of_kind = || devices.iter().filter(|d| d["kind"] == kind);
    of_kind()
        .find(|d| d["default"] == true)
        .or_else(|| of_kind().next())
}

#[cfg_attr(target_os = "macos", allow(dead_code))]
fn server_hint(host: &dyn Host) -> String {
    if host.env("PULSE_SERVER").is_none() && host.env("XDG_RUNTIME_DIR").is_none() {
        " XDG_RUNTIME_DIR is unset, so a client cannot find the user's audio server (an ssh or cron session): export XDG_RUNTIME_DIR=/run/user/$(id -u), or loginctl enable-linger so the user's server runs headless.".into()
    } else {
        String::new()
    }
}

/// Mic and system-audio rows for a Linux (or Windows) audio server, from a `devices-list` answer.
#[cfg_attr(target_os = "macos", allow(dead_code))]
pub fn audio_rows(
    os: &str,
    sandbox: &Sandbox,
    devices: &crate::config::Result<Value>,
    cfg: &Config,
    host: &dyn Host,
) -> Vec<Value> {
    let ensure_null = cfg.audio.ensure_null_sink;
    if let Sandbox::Flatpak { app, sockets, .. } = sandbox {
        if !sockets.iter().any(|s| s == "pulseaudio") {
            let fix =
                format!("flatpak override --user --socket=pulseaudio {app} (then restart the app)");
            return ["microphone", "system_audio"]
                .iter()
                .map(|n| {
                    with_fix(
                        row(
                            n,
                            "denied",
                            "the Flatpak sandbox has no audio socket (--socket=pulseaudio)",
                        ),
                        fix.clone(),
                    )
                })
                .collect();
        }
    }
    let v = match devices {
        Ok(v) => v,
        Err(e) => {
            let fix = if os == "linux" {
                format!(
                    "start the user audio server (perms-ensure runs systemctl --user start pipewire pipewire-pulse wireplumber, else pulseaudio --start).{}",
                    server_hint(host)
                )
            } else {
                "check the audio service and devices-list".into()
            };
            return ["microphone", "system_audio"]
                .iter()
                .map(|n| {
                    let mut r = with_fix(
                        row(
                            n,
                            "unavailable",
                            format!("the audio backend did not list devices: {}", e.message),
                        ),
                        fix.clone(),
                    );
                    r["error"] = json!({"code": e.code, "message": e.message});
                    r
                })
                .collect();
        }
    };
    let backend = v["backend"].as_str().unwrap_or("?").to_string();
    let devices = v["devices"].as_array().cloned().unwrap_or_default();
    let via = match sandbox {
        Sandbox::Flatpak { .. } => "through the Flatpak --socket=pulseaudio socket; ",
        Sandbox::Snap { .. } => "through the Snap audio-record interface; ",
        Sandbox::None => "",
    };
    let session = if os == "linux" {
        format!("{backend} client of the user's PipeWire/PulseAudio server {via}a session process needs no portal and gets no prompt")
    } else {
        format!("{backend} capture")
    };
    let mut rows = vec![];
    // Microphone.
    rows.push(if !cfg.audio.mic.enabled {
        row("microphone", "disabled", "audio.mic.enabled = false")
    } else {
        match pick(&devices, "mic", &cfg.audio.mic.device) {
            Some(d) => {
                let mut r = row("microphone", "granted", session.clone());
                r["device"] = d["id"].clone();
                r
            }
            None if !cfg.audio.mic.device.is_empty() => with_fix(
                row("microphone", "missing", format!("no {backend} source '{}'", cfg.audio.mic.device)),
                "set audio.mic.device to a source from devices-list (empty is the default source)",
            ),
            None => with_fix(
                row("microphone", "missing", format!("the {backend} server has no microphone source")),
                "connect a microphone (a USB or Bluetooth headset), or set audio.mic.device to any source from devices-list",
            ),
        }
    });
    // System audio.
    rows.push(if !cfg.audio.system.enabled {
        row("system_audio", "disabled", "audio.system.enabled = false")
    } else {
        match pick(&devices, "loopback", &cfg.audio.system.device) {
            Some(d) => {
                let how = if os == "linux" {
                    format!("{session}; the monitor source of a sink, no ScreenCast portal")
                } else {
                    session.clone()
                };
                let mut r = row("system_audio", "granted", how);
                r["device"] = d["id"].clone();
                r
            }
            None => {
                let fix = if os == "linux" && ensure_null {
                    format!("perms-ensure loads a null sink ({NULL_SINK}) whose monitor is the system-audio source")
                } else {
                    "add an output sink (pactl load-module module-null-sink) or set audio.system.device".into()
                };
                with_fix(
                    row("system_audio", "missing", format!("the {backend} server has no sink monitor to record")),
                    fix,
                )
            }
        }
    });
    rows
}

fn portal_row() -> Value {
    row(
        "screencast_portal",
        "not_needed",
        "no xdg-desktop-portal ScreenCast or Camera request is made: audio is the PipeWire/Pulse socket, so there is no dialog and no restore_token to keep",
    )
}

/// The calendar row: every enabled feed readable under the policy keys and its secret present,
/// plus the macOS Calendars grant when an eventkit source is on.
pub fn calendar_row(engine: &Engine, cfg: &Config, sandbox: &Sandbox) -> Value {
    let feeds =
        crate::ics::parse_feeds(&cfg.ics.feeds, cfg.ics.max_name_len as usize).unwrap_or_default();
    let enabled: Vec<_> = feeds.iter().filter(|f| f.enabled).collect();
    let sources = crate::calendar::parse_sources(&cfg.calendar.sources).unwrap_or_default();
    let eventkit = sources
        .iter()
        .any(|s| s.enabled && s.kind == crate::calendar::SourceKind::Eventkit);
    let store = engine.secret_store(cfg);
    let mut problems = vec![];
    let mut kinds = std::collections::BTreeSet::new();
    let net_shared = match sandbox {
        Sandbox::Flatpak { shared, .. } => shared.iter().any(|s| s == "network"),
        _ => true,
    };
    for f in &enabled {
        match fetch::classify(&f.url, true) {
            Ok(Source::Http(_)) | Ok(Source::CalDav(_)) => {
                kinds.insert(if f.url.trim().to_ascii_lowercase().starts_with("caldav") {
                    "caldav"
                } else {
                    "ics"
                });
                if !cfg.network.allow {
                    problems.push(json!({"feed": f.name, "reason": "network.allow is off",
                                         "fix": "ctl config-set network.allow true"}));
                } else if !net_shared {
                    problems.push(json!({"feed": f.name, "reason": "the Flatpak sandbox has no network (--share=network)"}));
                }
            }
            Ok(Source::File(p)) => {
                kinds.insert("ics");
                if !cfg.ics.allow_file {
                    problems.push(json!({"feed": f.name, "reason": "ics.allow_file is off",
                                         "fix": "ctl config-set ics.allow_file true"}));
                } else if std::fs::metadata(&p).is_err() {
                    problems.push(json!({"feed": f.name, "reason": format!("{} is not readable", p.display())}));
                }
            }
            Err(e) => problems.push(json!({"feed": f.name, "reason": e.message})),
        }
        if let Some(h) = &f.header {
            let found = crate::ics::header_secret(h).and_then(|(_, key)| {
                crate::secrets::resolve(
                    &*store,
                    &format!("{}{key}", crate::secrets::REF_PREFIX),
                    cfg.secrets.max_key_len as usize,
                )
            });
            if let Err(e) = found {
                problems.push(json!({"feed": f.name, "reason": e.message,
                                     "fix": "ctl secret-set <key> <value>"}));
            }
        }
    }
    let mut v = if !problems.is_empty() {
        let mut r = row(
            "calendar",
            "blocked",
            "ics/CalDAV feeds need no OS permission; a policy key or secret stops these",
        );
        r["problems"] = json!(problems);
        r
    } else if enabled.is_empty() && !eventkit {
        row("calendar", "not_needed", "no calendar feed or source is enabled; ics and CalDAV feeds need no OS permission (ics-add)")
    } else {
        row("calendar", "granted", "ics/CalDAV feeds are plain file and HTTP reads: no OS permission and no prompt, only network.allow, ics.allow_file and the feed secrets")
    };
    v["feeds"] = json!(enabled.len());
    v["kinds"] = json!(kinds);
    if eventkit {
        let ek = engine.event_store();
        let auth = ek.authorization();
        v["eventkit"] = json!(auth.as_str());
        if !auth.readable() {
            v["state"] = json!(
                if auth == crate::calendar::eventkit::Access::NotDetermined {
                    "not_determined"
                } else {
                    "denied"
                }
            );
            v["how"] = json!("an eventkit source needs the macOS Calendars grant");
            v["fix"] = json!("ctl calendar-access request=true asks once; or System Settings > Privacy & Security > Calendars");
        }
    }
    v
}

#[cfg(target_os = "macos")]
fn macos_audio_rows(cfg: &Config) -> Vec<Value> {
    use oats_audio::macos::{Permission, PermissionState, Permissions, SystemPermissions};
    let tcc = |p: Permission| match SystemPermissions.state(p) {
        PermissionState::Granted => "granted",
        PermissionState::Denied => "denied",
        PermissionState::NotDetermined => "not_determined",
    };
    let mic = if cfg.audio.mic.enabled {
        with_fix(
            row("microphone", tcc(Permission::Microphone), "macOS TCC Microphone grant"),
            "only a person can grant it: the first record-start asks once, or System Settings > Privacy & Security > Microphone",
        )
    } else {
        row("microphone", "disabled", "audio.mic.enabled = false")
    };
    let system = if cfg.audio.system.enabled {
        with_fix(
            row("system_audio", tcc(Permission::SystemAudioRecording), "macOS System Audio Recording (process tap) or Screen Recording (ScreenCaptureKit); the tap has no preflight"),
            "only a person can grant it: the first record-start asks once, or System Settings > Privacy & Security > Screen & System Audio Recording",
        )
    } else {
        row("system_audio", "disabled", "audio.system.enabled = false")
    };
    vec![mic, system]
}

/// `perms-status` against `host`.
pub fn status_with(engine: &Engine, host: &dyn Host) -> Value {
    let cfg = engine.snapshot().config.clone();
    let os = std::env::consts::OS;
    let sandbox = if os == "linux" {
        sandbox_of(host)
    } else {
        Sandbox::None
    };
    #[cfg(target_os = "macos")]
    let mut rows = macos_audio_rows(&cfg);
    #[cfg(not(target_os = "macos"))]
    let mut rows = audio_rows(os, &sandbox, &engine.devices(false), &cfg, host);
    if os == "linux" {
        rows.push(portal_row());
    }
    rows.push(calendar_row(engine, &cfg, &sandbox));
    let all = rows
        .iter()
        .all(|r| OK_STATES.contains(&r["state"].as_str().unwrap_or("")));
    let headless = host.env("DISPLAY").is_none() && host.env("WAYLAND_DISPLAY").is_none();
    let mut v = json!({
        "os": os,
        "sandbox": sandbox.name(),
        "headless": os == "linux" && headless,
        "all_granted": all,
        "permissions": rows,
    });
    if let Sandbox::Flatpak { app, .. } | Sandbox::Snap { name: app } = &sandbox {
        v["app_id"] = json!(app);
    }
    v
}

pub fn status(engine: &Engine) -> Value {
    status_with(engine, &SystemHost)
}

fn step(name: &str, r: std::result::Result<String, String>) -> Value {
    match r {
        Ok(out) => json!({"step": name, "ok": true, "detail": out}),
        Err(e) => json!({"step": name, "ok": false, "error": e}),
    }
}

fn needs_human(name: &str, why: &str, command: String) -> Value {
    json!({"step": name, "ok": false, "needs_human": true, "error": why, "command": command})
}

fn state_of(status: &Value, name: &str) -> String {
    status["permissions"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|r| r["name"] == name)
        .and_then(|r| r["state"].as_str())
        .unwrap_or("")
        .to_string()
}

/// Start the user's audio server, then wait up to `audio.ensure_wait` for it to answer.
fn start_audio_server(host: &dyn Host, cfg: &Config) -> Value {
    let units = [
        "start",
        "pipewire.socket",
        "pipewire-pulse.socket",
        "wireplumber.service",
    ];
    let mut how = Vec::new();
    let mut started = host.run("systemctl", &[&["--user"][..], &units[..]].concat());
    match &started {
        Ok(_) => how.push("systemctl --user start pipewire pipewire-pulse wireplumber".to_string()),
        Err(e) => {
            how.push(e.clone());
            started = host.run("pulseaudio", &["--start"]);
            match &started {
                Ok(_) => how.push("pulseaudio --start".to_string()),
                Err(e) => how.push(e.clone()),
            }
        }
    }
    if started.is_err() {
        return json!({"step": "start_audio_server", "ok": false, "error": how.join("; ")});
    }
    let deadline = Instant::now() + cfg.audio.ensure_wait.as_duration();
    let poll = cfg.audio.poll_interval.as_duration();
    loop {
        if host.run("pactl", &["info"]).is_ok() || host.run("pw-cli", &["info", "0"]).is_ok() {
            return json!({"step": "start_audio_server", "ok": true, "detail": how.join("; ")});
        }
        if Instant::now() >= deadline {
            return json!({"step": "start_audio_server", "ok": false,
                          "error": format!("{}; the server did not answer within audio.ensure_wait", how.join("; "))});
        }
        host.sleep(poll);
    }
}

fn load_null_sink(host: &dyn Host) -> Value {
    let sink = format!("sink_name={NULL_SINK}");
    let props = format!("sink_properties=device.description={NULL_SINK}");
    let r = host
        .run("pactl", &["load-module", "module-null-sink", &sink, &props])
        .or_else(|e| {
            let node = format!(
                "{{ factory.name=support.null-audio-sink node.name={NULL_SINK} media.class=Audio/Sink audio.position=[FL FR] object.linger=true }}"
            );
            host.run("pw-cli", &["create-node", "adapter", &node])
                .map_err(|e2| format!("{e}; {e2}"))
        });
    step("null_sink", r)
}

/// `perms-ensure` against `host`: every step it took, then the status after them.
pub fn ensure_with(engine: &Engine, host: &dyn Host) -> Value {
    let cfg = engine.snapshot().config.clone();
    let mut steps = vec![];
    let before = status_with(engine, host);
    if std::env::consts::OS != "linux" {
        for r in before["permissions"].as_array().into_iter().flatten() {
            if !OK_STATES.contains(&r["state"].as_str().unwrap_or("")) {
                steps.push(needs_human(
                    r["name"].as_str().unwrap_or(""),
                    "an OS grant only a person can give",
                    r["fix"].as_str().unwrap_or("").to_string(),
                ));
            }
        }
        return json!({"steps": steps, "status": before});
    }
    let sandbox = sandbox_of(host);
    match &sandbox {
        Sandbox::Flatpak { app, sockets, .. } => {
            if !sockets.iter().any(|s| s == "pulseaudio") {
                let args = [
                    "--host",
                    "flatpak",
                    "override",
                    "--user",
                    "--socket=pulseaudio",
                    app,
                ];
                steps.push(match host.run("flatpak-spawn", &args) {
                    Ok(_) => json!({"step": "flatpak_socket", "ok": true,
                                    "detail": "flatpak override --user --socket=pulseaudio; restart the app to apply"}),
                    Err(e) => needs_human("flatpak_socket", &e, format!("flatpak override --user --socket=pulseaudio {app}")),
                });
            }
            let args = [
                "--host",
                "flatpak",
                "permission-set",
                "devices",
                "microphone",
                app,
                "yes",
            ];
            steps.push(match host.run("flatpak-spawn", &args) {
                Ok(_) => json!({"step": "flatpak_device_microphone", "ok": true,
                                "detail": format!("flatpak permission-set devices microphone {app} yes")}),
                Err(e) => needs_human(
                    "flatpak_device_microphone",
                    &e,
                    format!("flatpak permission-set devices microphone {app} yes"),
                ),
            });
        }
        Sandbox::Snap { name } => {
            if host
                .run("snapctl", &["is-connected", "audio-record"])
                .is_err()
            {
                steps.push(needs_human(
                    "snap_audio_record",
                    "the audio-record interface is not connected; only root can connect it",
                    format!("sudo snap connect {name}:audio-record"),
                ));
            }
        }
        Sandbox::None => {}
    }
    let unavailable = |s: &Value| {
        state_of(s, "microphone") == "unavailable" || state_of(s, "system_audio") == "unavailable"
    };
    let mut now = before;
    if unavailable(&now) && sandbox == Sandbox::None {
        steps.push(start_audio_server(host, &cfg));
        now = refreshed(engine, host);
    }
    if state_of(&now, "system_audio") == "missing" && cfg.audio.system.device.is_empty() {
        if cfg.audio.ensure_null_sink {
            steps.push(load_null_sink(host));
            now = refreshed(engine, host);
        } else {
            steps.push(json!({"step": "null_sink", "ok": false, "skipped": true,
                              "error": "audio.ensure_null_sink is off"}));
        }
    }
    for r in now["permissions"].as_array().into_iter().flatten() {
        let name = r["name"].as_str().unwrap_or("");
        if !OK_STATES.contains(&r["state"].as_str().unwrap_or(""))
            && !steps.iter().any(|s| s["step"] == name)
        {
            steps.push(needs_human(
                name,
                r["how"].as_str().unwrap_or(""),
                r["fix"].as_str().unwrap_or("").to_string(),
            ));
        }
    }
    json!({"steps": steps, "status": now})
}

/// The status with a fresh device listing (after a step changed the audio graph).
fn refreshed(engine: &Engine, host: &dyn Host) -> Value {
    let _ = engine.devices(true);
    status_with(engine, host)
}

pub fn ensure(engine: &Engine) -> Value {
    ensure_with(engine, &SystemHost)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct Fake {
        env: Vec<(&'static str, &'static str)>,
        flatpak_info: Option<&'static str>,
    }

    impl Host for Fake {
        fn env(&self, key: &str) -> Option<String> {
            self.env
                .iter()
                .find(|(k, _)| *k == key)
                .map(|(_, v)| v.to_string())
        }
        fn read(&self, path: &str) -> Option<String> {
            (path == "/.flatpak-info").then_some(self.flatpak_info?.to_string())
        }
        fn run(&self, prog: &str, _: &[&str]) -> std::result::Result<String, String> {
            Err(format!("{prog}: not here"))
        }
    }

    const INFO: &str = "[Application]\nname=org.example.Oats\nruntime=x\n\n[Context]\nshared=network;ipc;\nsockets=x11;wayland;\n";

    #[test]
    fn sandbox_detection() {
        assert_eq!(sandbox_of(&Fake::default()), Sandbox::None);
        let snap = Fake {
            env: vec![("SNAP_NAME", "oats")],
            ..Fake::default()
        };
        assert_eq!(
            sandbox_of(&snap),
            Sandbox::Snap {
                name: "oats".into()
            }
        );
        let fp = Fake {
            flatpak_info: Some(INFO),
            ..Fake::default()
        };
        assert_eq!(
            sandbox_of(&fp),
            Sandbox::Flatpak {
                app: "org.example.Oats".into(),
                sockets: vec!["x11".into(), "wayland".into()],
                shared: vec!["network".into(), "ipc".into()],
            }
        );
    }

    fn cfg() -> Config {
        crate::config::resolve::Resolved {
            pack: None,
            flat: crate::config::resolve::defaults_flat(),
            provenance: Default::default(),
        }
        .typed()
        .unwrap()
    }

    fn listing(devs: Value) -> crate::config::Result<Value> {
        Ok(json!({"backend": "pulse", "devices": devs}))
    }

    #[test]
    fn rows_from_a_listing() {
        let c = cfg();
        let both = listing(json!([
            {"id": "mic1", "name": "Mic", "kind": "mic", "default": true},
            {"id": "out.monitor", "name": "Monitor", "kind": "loopback", "default": true},
        ]));
        let rows = audio_rows("linux", &Sandbox::None, &both, &c, &Fake::default());
        assert_eq!(rows[0]["state"], "granted");
        assert_eq!(rows[0]["device"], "mic1");
        assert_eq!(rows[1]["state"], "granted");
        assert_eq!(rows[1]["device"], "out.monitor");
        assert!(rows[1]["how"]
            .as_str()
            .unwrap()
            .contains("no ScreenCast portal"));

        let mic_only =
            listing(json!([{"id": "mic1", "name": "Mic", "kind": "mic", "default": false}]));
        let rows = audio_rows("linux", &Sandbox::None, &mic_only, &c, &Fake::default());
        assert_eq!(rows[0]["state"], "granted");
        assert_eq!(rows[1]["state"], "missing");
        assert!(rows[1]["fix"].as_str().unwrap().contains(NULL_SINK));
    }

    #[test]
    fn an_unreachable_server_names_the_runtime_dir() {
        let c = cfg();
        let e = Err(crate::config::ConfigError::new(
            "backend_unavailable",
            "Connection refused",
        ));
        let rows = audio_rows("linux", &Sandbox::None, &e, &c, &Fake::default());
        assert_eq!(rows[0]["state"], "unavailable");
        assert!(rows[0]["fix"].as_str().unwrap().contains("XDG_RUNTIME_DIR"));
        let with_dir = Fake {
            env: vec![("XDG_RUNTIME_DIR", "/run/user/1000")],
            ..Fake::default()
        };
        let rows = audio_rows("linux", &Sandbox::None, &e, &c, &with_dir);
        assert!(!rows[1]["fix"].as_str().unwrap().contains("XDG_RUNTIME_DIR"));
    }

    #[test]
    fn a_flatpak_without_the_audio_socket_is_denied_with_the_override() {
        let c = cfg();
        let sb = sandbox_of(&Fake {
            flatpak_info: Some(INFO),
            ..Fake::default()
        });
        let rows = audio_rows("linux", &sb, &listing(json!([])), &c, &Fake::default());
        assert_eq!(rows[0]["state"], "denied");
        assert!(rows[0]["fix"]
            .as_str()
            .unwrap()
            .contains("flatpak override --user --socket=pulseaudio org.example.Oats"));
    }

    #[test]
    fn keyfile_lists() {
        assert_eq!(
            keyfile_list(INFO, "Context", "sockets"),
            vec!["x11", "wayland"]
        );
        assert!(keyfile_list(INFO, "Context", "filesystems").is_empty());
        assert!(keyfile_list(INFO, "Nope", "sockets").is_empty());
    }
}
