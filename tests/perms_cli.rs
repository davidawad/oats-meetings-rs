//! perms-status / perms-ensure end to end (dot-4uzgol.96): the real binary, `--headless` with no
//! display, against a fake PipeWire: `pactl` / `parec` scripts on PATH that answer like
//! pipewire-pulse, with a sink monitor only once `pactl load-module module-null-sink` ran.
#![cfg(target_os = "linux")]

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;

const EXE: &str = env!("CARGO_BIN_EXE_oats-meetings-rs");

const PACTL: &str = r#"#!/bin/sh
# Fake pipewire-pulse pactl. $FAKE_PW holds the state: a file 'sink' once a null sink is loaded.
echo "$*" >> "$FAKE_PW/calls"
case "$1 $2 $3" in
  "info  ") echo "Server Name: PulseAudio (on PipeWire 1.2.0)" ;;
  "--format=json list sources")
    if [ -f "$FAKE_PW/sink" ]; then
      echo '[{"name":"alsa_input.usb-mic","description":"USB Mic","monitor_of_sink":null,"properties":{}},{"name":"oats_meetings_null.monitor","description":"Monitor of oats_meetings_null","monitor_of_sink":1,"properties":{"device.class":"monitor"}}]'
    else
      echo '[{"name":"alsa_input.usb-mic","description":"USB Mic","monitor_of_sink":null,"properties":{}}]'
    fi ;;
  "get-default-sink  ") [ -f "$FAKE_PW/sink" ] && echo oats_meetings_null ;;
  "get-default-source  ") echo alsa_input.usb-mic ;;
  "load-module module-null-sink "*) touch "$FAKE_PW/sink"; echo 536870913 ;;
  *) echo "fake pactl: unsupported: $*" >&2; exit 1 ;;
esac
"#;

fn script(dir: &Path, name: &str, body: &str) {
    let p = dir.join(name);
    std::fs::write(&p, body).unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
}

struct Engine {
    child: Child,
    dir: tempfile::TempDir,
}

impl Engine {
    fn start() -> Engine {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("bin");
        let pw = dir.path().join("pw");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::create_dir_all(&pw).unwrap();
        script(&bin, "pactl", PACTL);
        // No real capture here: the tools a recording or perms-ensure might reach fail at once.
        for tool in [
            "parec",
            "pw-record",
            "pw-dump",
            "pw-cli",
            "systemctl",
            "pulseaudio",
        ] {
            script(&bin, tool, "#!/bin/sh\nexit 1\n");
        }
        std::fs::write(dir.path().join("config.toml"), "").unwrap();
        let mut e = Engine {
            child: Command::new("true").spawn().unwrap(),
            dir,
        };
        e.child = e
            .cmd()
            .arg("--headless")
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let start = Instant::now();
        while e.ctl(&["ping"]).0 != 0 {
            assert!(
                start.elapsed() < Duration::from_secs(30),
                "engine did not come up"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
        e
    }

    fn cmd(&self) -> Command {
        let d = self.dir.path();
        let path = format!(
            "{}:{}",
            d.join("bin").display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let mut c = Command::new(EXE);
        c.env("OATS_DATA_DIR", d.join("data"))
            .env("OATS_RUNTIME_DIR", d.join("rt"))
            .env("OATS_CONFIG", d.join("config.toml"))
            .env("OATS_OS", "linux")
            .env("FAKE_PW", d.join("pw"))
            .env("PATH", path)
            .env("XDG_RUNTIME_DIR", d.join("rt"))
            .env_remove("DISPLAY")
            .env_remove("WAYLAND_DISPLAY")
            .env_remove("PULSE_SERVER")
            .env_remove("FLATPAK_ID")
            .env_remove("SNAP_NAME")
            .env_remove("OATS_PACK");
        c
    }

    fn ctl(&self, args: &[&str]) -> (i32, String) {
        let o = self.cmd().arg("ctl").args(args).output().unwrap();
        (
            o.status.code().unwrap(),
            String::from_utf8_lossy(&o.stdout).into_owned(),
        )
    }

    fn json(&self, args: &[&str]) -> Value {
        let (code, out) = self.ctl(args);
        assert_eq!(code, 0, "{args:?}: {out}");
        serde_json::from_str(out.trim()).unwrap()
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn state(status: &Value, name: &str) -> String {
    status["permissions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["name"] == name)
        .and_then(|r| r["state"].as_str())
        .unwrap_or_else(|| panic!("no {name} in {status}"))
        .to_string()
}

#[test]
fn headless_perms_status_then_ensure_grants_everything_without_a_prompt() {
    let e = Engine::start();
    let s = e.json(&["perms", "status"]);
    assert_eq!(s["os"], "linux");
    assert_eq!(s["headless"], true, "{s}");
    assert_eq!(s["sandbox"], "none");
    assert_eq!(state(&s, "microphone"), "granted", "{s}");
    assert_eq!(state(&s, "system_audio"), "missing", "{s}");
    assert_eq!(state(&s, "screencast_portal"), "not_needed");
    // The default os-local source: the desktop's own calendar files, no OS permission.
    assert_eq!(state(&s, "calendar"), "granted");
    assert_eq!(s["all_granted"], false);

    let v = e.json(&["perms", "ensure"]);
    let steps = v["steps"].as_array().unwrap();
    assert_eq!(steps.len(), 1, "{v}");
    assert_eq!(steps[0]["step"], "null_sink");
    assert_eq!(steps[0]["ok"], true, "{v}");
    assert_eq!(v["status"]["all_granted"], true, "{v}");
    let calls = std::fs::read_to_string(e.dir.path().join("pw/calls")).unwrap();
    assert!(
        calls.contains("load-module module-null-sink sink_name=oats_meetings_null"),
        "{calls}"
    );

    let s = e.json(&["perms", "status"]);
    assert_eq!(s["all_granted"], true, "{s}");
    let sys = s["permissions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["name"] == "system_audio")
        .unwrap();
    assert_eq!(sys["device"], "oats_meetings_null.monitor");
}
