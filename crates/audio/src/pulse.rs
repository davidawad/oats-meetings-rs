//! PulseAudio, or PipeWire through its pulse server: devices from `pactl`, capture with `parec`
//! (server-side conversion to mono f32 at `SAMPLE_RATE`). A loopback is a sink's `.monitor` source.

use std::process::Command;

use serde_json::Value;

use crate::backend::{Capture, CaptureBackend, DeviceInfo, DeviceKind, OnSamples};
use crate::error::{io_err, AudioError, Result};
use crate::settings::Timing;
use crate::{proc, SAMPLE_RATE};

pub struct PulseBackend {
    timing: Timing,
}

impl PulseBackend {
    pub fn new(timing: Timing) -> Self {
        Self { timing }
    }
}

fn pactl(args: &[&str]) -> Result<String> {
    let out = Command::new("pactl").args(args).output().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            AudioError::new("tool_missing", "pactl not found on PATH (install pulseaudio-utils or pipewire-pulse)")
        } else {
            io_err("pactl", e)
        }
    })?;
    if !out.status.success() {
        return Err(AudioError::new(
            "backend_unavailable",
            format!("pactl {}: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim()),
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Parse `pactl --format=json list sources` into (name, description, is_monitor).
pub fn parse_sources(json: &str) -> Result<Vec<(String, String, bool)>> {
    let v: Value = serde_json::from_str(json)
        .map_err(|e| AudioError::new("parse_error", format!("pactl json: {e}")))?;
    let arr = v
        .as_array()
        .ok_or_else(|| AudioError::new("parse_error", "pactl json: expected an array"))?;
    Ok(arr
        .iter()
        .filter_map(|s| {
            let name = s["name"].as_str()?.to_string();
            let desc = s["description"].as_str().unwrap_or(&name).to_string();
            let monitor = name.ends_with(".monitor")
                || !s["monitor_of_sink"].is_null() && s["monitor_of_sink"] != Value::String("n/a".into())
                || s["properties"]["device.class"] == "monitor";
            Some((name, desc, monitor))
        })
        .collect())
}

/// Mark which of `sources` an empty setting resolves to, for each kind.
pub fn build_devices(
    sources: &[(String, String, bool)],
    default_sink: &str,
    default_source: &str,
) -> Vec<DeviceInfo> {
    let want_monitor = format!("{default_sink}.monitor");
    let first_mic = sources.iter().find(|s| !s.2).map(|s| s.0.as_str());
    let default_mic = if !default_source.is_empty() && sources.iter().any(|s| s.0 == default_source && !s.2) {
        Some(default_source)
    } else {
        first_mic
    };
    sources
        .iter()
        .map(|(name, desc, monitor)| DeviceInfo {
            id: name.clone(),
            name: desc.clone(),
            kind: if *monitor { DeviceKind::Loopback } else { DeviceKind::Mic },
            backend: "pulse",
            default: if *monitor { *name == want_monitor } else { Some(name.as_str()) == default_mic },
        })
        .collect()
}

pub(crate) fn resolve(devices: &[DeviceInfo], kind: DeviceKind, wanted: &str) -> Result<String> {
    let of_kind = devices.iter().filter(|d| d.kind == kind);
    if wanted.is_empty() {
        let mut v: Vec<&DeviceInfo> = of_kind.collect();
        v.sort_by_key(|d| !d.default);
        return v.first().map(|d| d.id.clone()).ok_or_else(|| {
            AudioError::new("device_not_found", format!("no {kind:?} device found (see `ctl devices list`)"))
        });
    }
    // An explicit id or name may name any source of either kind (a monitor can serve as a mic).
    devices
        .iter()
        .find(|d| d.id == wanted || d.name == wanted)
        .map(|d| d.id.clone())
        .ok_or_else(|| AudioError::new("device_not_found", format!("device '{wanted}' not found (see `ctl devices list`)")))
}

impl CaptureBackend for PulseBackend {
    fn name(&self) -> &'static str {
        "pulse"
    }

    fn list_devices(&self) -> Result<Vec<DeviceInfo>> {
        let sources = parse_sources(&pactl(&["--format=json", "list", "sources"])?)?;
        let sink = pactl(&["get-default-sink"]).unwrap_or_default();
        let source = pactl(&["get-default-source"]).unwrap_or_default();
        Ok(build_devices(&sources, &sink, &source))
    }

    fn open(&self, kind: DeviceKind, device: &str, on_samples: OnSamples) -> Result<Box<dyn Capture>> {
        let id = resolve(&self.list_devices()?, kind, device)?;
        let mut cmd = Command::new("parec");
        cmd.arg(format!("--device={id}"))
            .arg("--format=float32le")
            .arg(format!("--rate={SAMPLE_RATE}"))
            .arg("--channels=1")
            // 100 ms, not 20: on a loaded host (CI pods) a 20 ms stream underruns and drops audio, measured
            // as 8 s of audio per 10 s of wall clock; at 100 ms the full 10 s arrives (ci/audio-probe.sh).
            .arg("--latency-msec=100");
        proc::spawn(cmd, on_samples, self.timing)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = r#"[
      {"index":0,"name":"oats_sink.monitor","description":"Monitor of Null Output","monitor_of_sink":0,"properties":{"device.class":"monitor"}},
      {"index":1,"name":"alsa_input.usb-mic","description":"USB Mic","monitor_of_sink":null,"properties":{}},
      {"index":2,"name":"other.monitor","description":"Monitor of Other","monitor_of_sink":1,"properties":{}}
    ]"#;

    #[test]
    fn parses_and_classifies() {
        let s = parse_sources(FIXTURE).unwrap();
        assert_eq!(s.len(), 3);
        assert!(s[0].2 && !s[1].2 && s[2].2);
        let d = build_devices(&s, "oats_sink", "alsa_input.usb-mic");
        assert_eq!(d[0].kind, DeviceKind::Loopback);
        assert!(d[0].default && !d[2].default);
        assert_eq!(d[1].kind, DeviceKind::Mic);
        assert!(d[1].default);
        assert!(parse_sources("nope").is_err());
        assert!(parse_sources("{}").is_err());
    }

    #[test]
    fn default_mic_skips_a_monitor_default_source() {
        let s = parse_sources(FIXTURE).unwrap();
        let d = build_devices(&s, "oats_sink", "oats_sink.monitor");
        assert!(d.iter().find(|x| x.id == "alsa_input.usb-mic").unwrap().default);
    }

    #[test]
    fn resolve_default_explicit_and_missing() {
        let s = parse_sources(FIXTURE).unwrap();
        let d = build_devices(&s, "oats_sink", "alsa_input.usb-mic");
        assert_eq!(resolve(&d, DeviceKind::Loopback, "").unwrap(), "oats_sink.monitor");
        assert_eq!(resolve(&d, DeviceKind::Mic, "").unwrap(), "alsa_input.usb-mic");
        assert_eq!(resolve(&d, DeviceKind::Mic, "oats_sink.monitor").unwrap(), "oats_sink.monitor");
        assert_eq!(resolve(&d, DeviceKind::Mic, "USB Mic").unwrap(), "alsa_input.usb-mic");
        assert_eq!(resolve(&d, DeviceKind::Mic, "zzz").unwrap_err().code, "device_not_found");
        assert_eq!(resolve(&[], DeviceKind::Loopback, "").unwrap_err().code, "device_not_found");
    }
}
