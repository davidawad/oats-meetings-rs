//! PipeWire native: devices from `pw-dump`, capture with `pw-record`. A loopback is a sink node
//! captured with `stream.capture.sink=true` (its monitor); a mic is an `Audio/Source` node.

use std::process::Command;

use serde_json::Value;

use crate::backend::{Capture, CaptureBackend, DeviceInfo, DeviceKind, OnSamples};
use crate::error::{io_err, AudioError, Result};
use crate::settings::Timing;
use crate::{proc, pulse, SAMPLE_RATE};

pub struct PipewireBackend {
    timing: Timing,
}

impl PipewireBackend {
    pub fn new(timing: Timing) -> Self {
        Self { timing }
    }
}

/// Parse `pw-dump` JSON into devices. Sinks are Loopback devices, sources are Mics.
pub fn parse_dump(json: &str) -> Result<Vec<DeviceInfo>> {
    let v: Value = serde_json::from_str(json)
        .map_err(|e| AudioError::new("parse_error", format!("pw-dump json: {e}")))?;
    let objs = v
        .as_array()
        .ok_or_else(|| AudioError::new("parse_error", "pw-dump json: expected an array"))?;
    let default_of = |key: &str| -> Option<String> {
        objs.iter()
            .filter(|o| o["type"] == "PipeWire:Interface:Metadata")
            .flat_map(|o| o["metadata"].as_array().into_iter().flatten())
            .find(|m| m["key"] == key)
            .and_then(|m| match &m["value"] {
                Value::Object(o) => o.get("name").and_then(Value::as_str).map(str::to_string),
                Value::String(s) => serde_json::from_str::<Value>(s)
                    .ok()
                    .and_then(|x| x["name"].as_str().map(str::to_string)),
                _ => None,
            })
    };
    let (dsink, dsource) = (default_of("default.audio.sink"), default_of("default.audio.source"));
    Ok(objs
        .iter()
        .filter(|o| o["type"] == "PipeWire:Interface:Node")
        .filter_map(|o| {
            let p = &o["info"]["props"];
            let name = p["node.name"].as_str()?.to_string();
            let class = p["media.class"].as_str()?;
            let (kind, default) = match class {
                "Audio/Sink" => (DeviceKind::Loopback, dsink.as_deref() == Some(name.as_str())),
                "Audio/Source" => (DeviceKind::Mic, dsource.as_deref() == Some(name.as_str())),
                _ => return None,
            };
            let desc = p["node.description"].as_str().unwrap_or(&name).to_string();
            Some(DeviceInfo { id: name, name: desc, kind, backend: "pipewire", default })
        })
        .collect())
}

impl CaptureBackend for PipewireBackend {
    fn name(&self) -> &'static str {
        "pipewire"
    }

    fn list_devices(&self) -> Result<Vec<DeviceInfo>> {
        let out = Command::new("pw-dump").output().map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                AudioError::new("tool_missing", "pw-dump not found on PATH (install pipewire)")
            } else {
                io_err("pw-dump", e)
            }
        })?;
        if !out.status.success() {
            return Err(AudioError::new(
                "backend_unavailable",
                format!("pw-dump: {}", String::from_utf8_lossy(&out.stderr).trim()),
            ));
        }
        parse_dump(&String::from_utf8_lossy(&out.stdout))
    }

    fn open(&self, kind: DeviceKind, device: &str, on_samples: OnSamples) -> Result<Box<dyn Capture>> {
        let devices = self.list_devices()?;
        let id = pulse::resolve(&devices, kind, device)?;
        // Whether the node is a sink decides monitor capture, whatever kind the caller asked for.
        let is_sink = devices.iter().any(|d| d.id == id && d.kind == DeviceKind::Loopback);
        let mut cmd = Command::new("pw-record");
        cmd.args(["--target", &id, "--rate", &SAMPLE_RATE.to_string(), "--channels", "1", "--format", "f32"]);
        if is_sink {
            cmd.args(["-P", "stream.capture.sink=true"]);
        }
        cmd.arg("-");
        proc::spawn(cmd, on_samples, self.timing)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DUMP: &str = r#"[
      {"id":40,"type":"PipeWire:Interface:Metadata","metadata":[
        {"subject":0,"key":"default.audio.sink","type":"Spa:String:JSON","value":{"name":"oats_sink"}},
        {"subject":0,"key":"default.audio.source","type":"Spa:String:JSON","value":"{\"name\":\"usb_mic\"}"}]},
      {"id":50,"type":"PipeWire:Interface:Node","info":{"props":{"node.name":"oats_sink","node.description":"Oats Sink","media.class":"Audio/Sink"}}},
      {"id":51,"type":"PipeWire:Interface:Node","info":{"props":{"node.name":"usb_mic","media.class":"Audio/Source"}}},
      {"id":52,"type":"PipeWire:Interface:Node","info":{"props":{"node.name":"app","media.class":"Stream/Output/Audio"}}},
      {"id":53,"type":"PipeWire:Interface:Port","info":{}}
    ]"#;

    #[test]
    fn parses_nodes_and_defaults() {
        let d = parse_dump(DUMP).unwrap();
        assert_eq!(d.len(), 2);
        assert_eq!((d[0].kind, d[0].default, d[0].name.as_str()), (DeviceKind::Loopback, true, "Oats Sink"));
        assert_eq!((d[1].kind, d[1].default, d[1].id.as_str()), (DeviceKind::Mic, true, "usb_mic"));
        assert!(parse_dump("[").is_err());
        assert!(parse_dump("{}").is_err());
    }
}
