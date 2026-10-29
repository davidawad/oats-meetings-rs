use std::process::Command;

use serde::Serialize;

use crate::error::{AudioError, Result};
use crate::settings::{AudioSettings, BackendChoice, Timing};

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
    fn open(
        &self,
        kind: DeviceKind,
        device: &str,
        on_samples: OnSamples,
    ) -> Result<Box<dyn Capture>>;
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
    select_for(
        settings.backend,
        std::env::consts::OS,
        &tool_works,
        settings.timing,
    )
}

/// Pure on the OS name and a tool probe, so every (choice, os) pair is testable anywhere.
pub fn select_for(
    choice: BackendChoice,
    os: &str,
    probe: &dyn Fn(&str, &str) -> bool,
    timing: Timing,
) -> Result<Box<dyn CaptureBackend>> {
    let unsupported = |what: &str| {
        Err(AudioError::new(
            "unsupported_platform",
            format!("audio backend '{what}' is not available on {os}"),
        ))
    };
    match (choice, os) {
        (BackendChoice::ScreenCaptureKit | BackendChoice::Auto, "macos") => macos_boxed(timing),
        (BackendChoice::ScreenCaptureKit, _) => unsupported("screencapturekit"),
        (BackendChoice::Pulse, "linux") => Ok(Box::new(crate::pulse::PulseBackend::new(timing))),
        (BackendChoice::Pulse, _) => unsupported("pulse"),
        (BackendChoice::Pipewire, "linux") => {
            Ok(Box::new(crate::pipewire::PipewireBackend::new(timing)))
        }
        (BackendChoice::Pipewire, _) => unsupported("pipewire"),
        (BackendChoice::Cpal, "windows") => cpal_boxed(timing),
        (BackendChoice::Cpal, _) => unsupported("cpal"),
        (BackendChoice::Auto, "windows") => cpal_boxed(timing),
        (BackendChoice::Auto, "linux") => {
            if probe("pactl", "info") {
                Ok(Box::new(crate::pulse::PulseBackend::new(timing)))
            } else if probe("pw-dump", "--version") {
                Ok(Box::new(crate::pipewire::PipewireBackend::new(timing)))
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

#[cfg(target_os = "macos")]
fn macos_boxed(timing: Timing) -> Result<Box<dyn CaptureBackend>> {
    Ok(Box::new(crate::macos::native(timing)))
}

#[cfg(not(target_os = "macos"))]
fn macos_boxed(_timing: Timing) -> Result<Box<dyn CaptureBackend>> {
    Err(AudioError::new(
        "unsupported_platform",
        "screencapturekit backend is built on macOS only",
    ))
}

#[cfg(windows)]
fn cpal_boxed(timing: Timing) -> Result<Box<dyn CaptureBackend>> {
    Ok(Box::new(crate::cpal_backend::CpalBackend::new(timing)))
}

#[cfg(not(windows))]
fn cpal_boxed(_timing: Timing) -> Result<Box<dyn CaptureBackend>> {
    Err(AudioError::new(
        "unsupported_platform",
        "cpal (WASAPI) backend is built on Windows only",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t() -> Timing {
        Timing {
            open_timeout: std::time::Duration::from_secs(1),
            poll: std::time::Duration::from_millis(5),
        }
    }

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
            for c in [
                BackendChoice::Auto,
                BackendChoice::Pulse,
                BackendChoice::Pipewire,
                BackendChoice::Cpal,
                BackendChoice::ScreenCaptureKit,
            ] {
                // must never panic; returns a backend or a structured error
                let _ = select_for(c, os, &all, t());
            }
        }
        assert_eq!(
            name(select_for(BackendChoice::Auto, "linux", &all, t())),
            "pulse"
        );
        assert_eq!(
            name(select_for(BackendChoice::Auto, "linux", &only_pw, t())),
            "pipewire"
        );
        assert_eq!(
            name(select_for(BackendChoice::Auto, "linux", &none, t())),
            "tool_missing"
        );
        assert_eq!(
            name(select_for(BackendChoice::Cpal, "linux", &all, t())),
            "unsupported_platform"
        );
        #[cfg(windows)]
        assert_eq!(
            name(select_for(BackendChoice::Auto, "windows", &none, t())),
            "cpal"
        );
        assert_eq!(
            name(select_for(BackendChoice::Pulse, "linux", &none, t())),
            "pulse"
        );
        assert_eq!(
            name(select_for(BackendChoice::Pipewire, "linux", &none, t())),
            "pipewire"
        );
        #[cfg(windows)]
        assert_eq!(
            name(select_for(BackendChoice::Cpal, "windows", &none, t())),
            "cpal"
        );
        assert_eq!(
            name(select_for(BackendChoice::Pulse, "windows", &all, t())),
            "unsupported_platform"
        );
        assert_eq!(
            name(select_for(BackendChoice::Pipewire, "windows", &all, t())),
            "unsupported_platform"
        );
        let mac = if cfg!(target_os = "macos") {
            "screencapturekit"
        } else {
            "unsupported_platform"
        };
        assert_eq!(
            name(select_for(BackendChoice::Auto, "macos", &all, t())),
            mac
        );
        assert_eq!(
            name(select_for(
                BackendChoice::ScreenCaptureKit,
                "macos",
                &none,
                t()
            )),
            mac
        );
        assert_eq!(
            name(select_for(BackendChoice::Pulse, "macos", &all, t())),
            "unsupported_platform"
        );
        assert_eq!(
            name(select_for(BackendChoice::Pipewire, "macos", &all, t())),
            "unsupported_platform"
        );
        assert_eq!(
            name(select_for(BackendChoice::Cpal, "macos", &all, t())),
            "unsupported_platform"
        );
        for os in ["linux", "windows"] {
            assert_eq!(
                name(select_for(BackendChoice::ScreenCaptureKit, os, &all, t())),
                "unsupported_platform"
            );
        }
    }
}
