use std::process::Command;

use serde::Serialize;

use crate::error::{AudioError, Result};
use crate::settings::{AudioSettings, BackendChoice};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DeviceKind {
    /// What the machine is playing (Pulse/PipeWire monitor, WASAPI loopback).
    Loopback,
    /// A microphone.
    Mic,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DeviceInfo {
    pub id: String,
    pub name: String,
    pub kind: DeviceKind,
    pub backend: &'static str,
    /// The device an empty `audio.*.device` resolves to.
    pub default: bool,
}

/// Receives mono f32 samples at `SAMPLE_RATE`, from a capture thread.
pub type OnSamples = Box<dyn FnMut(&[f32]) + Send>;

pub trait Capture: Send {
    /// Stop delivering samples and release the device. Idempotent.
    fn stop(&mut self);
}

pub trait CaptureBackend: Send + Sync {
    fn name(&self) -> &'static str;
    fn list_devices(&self) -> Result<Vec<DeviceInfo>>;
    /// `device` empty means the default for `kind`.
    fn open(&self, kind: DeviceKind, device: &str, on_samples: OnSamples) -> Result<Box<dyn Capture>>;
}

pub(crate) fn tool_works(tool: &str, arg: &str) -> bool {
    Command::new(tool)
        .arg(arg)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Choose the backend for `settings.backend` on this OS.
pub fn select(settings: &AudioSettings) -> Result<Box<dyn CaptureBackend>> {
    select_for(settings.backend, std::env::consts::OS, &tool_works)
}

/// Pure on the OS name and a tool probe, so every (choice, os) pair is testable anywhere.
pub fn select_for(
    choice: BackendChoice,
    os: &str,
    probe: &dyn Fn(&str, &str) -> bool,
) -> Result<Box<dyn CaptureBackend>> {
    let unsupported = |what: &str| {
        Err(AudioError::new(
            "unsupported_platform",
            format!("audio backend '{what}' is not available on {os}"),
        ))
    };
    match (choice, os) {
        (_, "macos") => Err(AudioError::new(
            "unsupported_platform",
            "macOS audio capture is dot-4uzgol.6, not part of this backend set",
        )),
        (BackendChoice::Pulse, "linux") => Ok(Box::new(crate::pulse::PulseBackend::new())),
        (BackendChoice::Pulse, _) => unsupported("pulse"),
        (BackendChoice::Pipewire, "linux") => Ok(Box::new(crate::pipewire::PipewireBackend::new())),
        (BackendChoice::Pipewire, _) => unsupported("pipewire"),
        (BackendChoice::Cpal, "windows") => cpal_boxed(),
        (BackendChoice::Cpal, _) => unsupported("cpal"),
        (BackendChoice::Auto, "windows") => cpal_boxed(),
        (BackendChoice::Auto, "linux") => {
            if probe("pactl", "info") {
                Ok(Box::new(crate::pulse::PulseBackend::new()))
            } else if probe("pw-dump", "--version") {
                Ok(Box::new(crate::pipewire::PipewireBackend::new()))
            } else {
                Err(AudioError::new(
                    "tool_missing",
                    "no audio server tools on PATH: install pulseaudio-utils (pactl, parec) or pipewire (pw-dump, pw-record)",
                ))
            }
        }
        (BackendChoice::Auto, _) => unsupported("auto"),
    }
}

#[cfg(windows)]
fn cpal_boxed() -> Result<Box<dyn CaptureBackend>> {
    Ok(Box::new(crate::cpal_backend::CpalBackend::new()))
}

#[cfg(not(windows))]
fn cpal_boxed() -> Result<Box<dyn CaptureBackend>> {
    Err(AudioError::new("unsupported_platform", "cpal (WASAPI) backend is built on Windows only"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn name(r: Result<Box<dyn CaptureBackend>>) -> String {
        match r {
            Ok(b) => b.name().to_string(),
            Err(e) => e.code.to_string(),
        }
    }

    #[test]
    fn every_choice_on_every_os() {
        let all = |_: &str, _: &str| true;
        let none = |_: &str, _: &str| false;
        let only_pw = |t: &str, _: &str| t == "pw-dump";
        for os in ["linux", "windows", "macos", "freebsd"] {
            for c in [BackendChoice::Auto, BackendChoice::Pulse, BackendChoice::Pipewire, BackendChoice::Cpal] {
                // must never panic; returns a backend or a structured error
                let _ = select_for(c, os, &all);
            }
        }
        assert_eq!(name(select_for(BackendChoice::Auto, "linux", &all)), "pulse");
        assert_eq!(name(select_for(BackendChoice::Auto, "linux", &only_pw)), "pipewire");
        assert_eq!(name(select_for(BackendChoice::Auto, "linux", &none)), "tool_missing");
        assert_eq!(name(select_for(BackendChoice::Cpal, "linux", &all)), "unsupported_platform");
        #[cfg(windows)]
        assert_eq!(name(select_for(BackendChoice::Auto, "windows", &none)), "cpal");
        assert_eq!(name(select_for(BackendChoice::Pulse, "linux", &none)), "pulse");
        assert_eq!(name(select_for(BackendChoice::Pipewire, "linux", &none)), "pipewire");
        #[cfg(windows)]
        assert_eq!(name(select_for(BackendChoice::Cpal, "windows", &none)), "cpal");
        assert_eq!(name(select_for(BackendChoice::Pulse, "windows", &all)), "unsupported_platform");
        assert_eq!(name(select_for(BackendChoice::Pipewire, "windows", &all)), "unsupported_platform");
        assert_eq!(name(select_for(BackendChoice::Auto, "macos", &all)), "unsupported_platform");
    }
}
