//! Live macOS audio proof through the real binary (dot-4uzgol.6). It captures system audio and the
//! microphone, so it triggers the Screen Recording and Microphone TCC prompts: it is skipped
//! (passes) unless `OATS_AUDIO_LIVE=1`, and runs only at David's review (dot-4uzgol.14), never from
//! an agent session.
//!
//! Acceptance: a 440 Hz tone is played with `afplay` while `ctl record start; sleep 5; ctl record
//! stop` runs; the two-channel WAV has a non-silent system channel (right, RMS above threshold at
//! the tone), duration >= 4 s, and `ctl session get` reports the duration. The mic channel (left) is
//! reported, not asserted: the room may be silent. A denied permission must come back as
//! `permission_denied`, not a crash.
#![cfg(target_os = "macos")]

use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use oats_audio::wav;
use serde_json::Value;

const EXE: &str = env!("CARGO_BIN_EXE_oats-meetings-rs");
const RMS_MIN: f64 = 0.02;

fn live() -> bool {
    let on = std::env::var("OATS_AUDIO_LIVE").ok().as_deref() == Some("1");
    if !on {
        eprintln!("audio_live_macos: skipped (set OATS_AUDIO_LIVE=1; prompts for Microphone and Screen Recording)");
    }
    on
}

struct Engine {
    child: Child,
    dir: tempfile::TempDir,
}

impl Engine {
    fn cmd(dir: &std::path::Path) -> Command {
        let mut c = Command::new(EXE);
        c.env("OATS_RUNTIME_DIR", dir.join("rt"))
            .env("OATS_CONFIG", dir.join("config.toml"))
            .env("OATS_DATA_DIR", dir.join("data"))
            .env_remove("OATS_PACK");
        c
    }

    fn start() -> Engine {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.toml"),
            "[audio]\nbackend = \"screencapturekit\"\ntranscribe = \"off\"\n",
        )
        .unwrap();
        let child = Self::cmd(dir.path())
            .arg("--headless")
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let e = Engine { child, dir };
        let t = Instant::now();
        while e.ctl(&["ping"]).0 != 0 {
            assert!(
                t.elapsed() < Duration::from_secs(30),
                "engine did not come up"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
        e
    }

    fn ctl(&self, args: &[&str]) -> (i32, String) {
        let o = Self::cmd(self.dir.path())
            .arg("ctl")
            .args(args)
            .output()
            .unwrap();
        (
            o.status.code().unwrap(),
            String::from_utf8_lossy(&o.stdout).into_owned(),
        )
    }

    fn json(&self, args: &[&str]) -> (i32, Value) {
        let mut a = vec!["--json"];
        a.extend_from_slice(args);
        let (code, out) = self.ctl(&a);
        (
            code,
            serde_json::from_str(out.trim()).unwrap_or_else(|e| panic!("{args:?}: {e}: {out}")),
        )
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// A 7 s 440 Hz stereo tone file played through the default output with `afplay`.
fn play_tone(dir: &std::path::Path) -> Child {
    let path = dir.join("tone.wav");
    let mut w = wav::WavWriter::create(&path, 2, 48_000).unwrap();
    let samples: Vec<f32> = (0..48_000 * 7)
        .flat_map(|i| {
            let s = (i as f32 / 48_000.0 * 440.0 * std::f32::consts::TAU).sin() * 0.5;
            [s, s]
        })
        .collect();
    w.write(&samples).unwrap();
    w.finish().unwrap();
    Command::new("afplay").arg(&path).spawn().expect("afplay")
}

#[test]
fn five_second_recording_is_non_silent_stereo_or_permission_denied() {
    if !live() {
        return;
    }
    let e = Engine::start();
    let (code, v) = e.json(&["devices", "list"]);
    assert_eq!(code, 0, "{v}");
    assert!(
        v["data"]["devices"]
            .as_array()
            .unwrap()
            .iter()
            .any(|d| d["kind"] == "loopback"),
        "{v}"
    );
    let mut tone = play_tone(e.dir.path());
    let (code, v) = e.json(&["record", "start", "--session", "live_mac"]);
    if code != 0 {
        // TCC denied: a structured error, never a crash.
        assert_eq!(v["error"]["code"], "permission_denied", "{v}");
        let _ = tone.kill();
        return;
    }
    std::thread::sleep(Duration::from_secs(5));
    let (code, v) = e.json(&["record", "stop"]);
    assert_eq!(code, 0, "{v}");
    let _ = tone.kill();
    let (code, got) = e.json(&["session", "get", "live_mac"]);
    assert_eq!(code, 0, "{got}");
    let dur = got["data"]["duration_ms"].as_u64().unwrap();
    assert!(dur >= 4000, "duration_ms {dur}");
    let w = wav::read(std::path::Path::new(
        got["data"]["audio_path"].as_str().unwrap(),
    ))
    .unwrap();
    eprintln!(
        "audio_live_macos: {} ch, {} Hz, {:.2}s, rms mic {:.3} system {:.3}, system tone {:.0} Hz",
        w.channels,
        w.rate,
        w.duration_secs(),
        w.rms(0),
        w.rms(1),
        w.tone_hz(1)
    );
    assert_eq!(w.channels, 2);
    assert!(w.duration_secs() >= 4.0, "wav {:.2}s", w.duration_secs());
    assert!(w.rms(1) > RMS_MIN, "system rms {}", w.rms(1));
    assert!(
        (w.tone_hz(1) - 440.0).abs() < 15.0,
        "system tone {} Hz",
        w.tone_hz(1)
    );
}
