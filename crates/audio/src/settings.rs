//! The audio config keys as a plain struct. `oats-core` builds it from the resolved config; the
//! string values are exactly the enum values of the `audio.*` keys.

use std::time::Duration;

use crate::error::{AudioError, Result};

pub const BACKENDS: &[&str] = &["auto", "pulse", "pipewire", "cpal", "screencapturekit"];
pub const LAYOUTS: &[&str] = &["split", "mix"];
pub const SYSTEM_BACKENDS: &[&str] = &["auto", "tap", "screencapturekit"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendChoice {
    Auto,
    Pulse,
    Pipewire,
    Cpal,
    ScreenCaptureKit,
}

impl BackendChoice {
    pub fn parse(s: &str) -> Result<Self> {
        Ok(match s {
            "auto" => Self::Auto,
            "pulse" => Self::Pulse,
            "pipewire" => Self::Pipewire,
            "cpal" => Self::Cpal,
            "screencapturekit" => Self::ScreenCaptureKit,
            other => {
                return Err(AudioError::new(
                    "invalid_value",
                    format!("audio.backend '{other}'; allowed: {}", BACKENDS.join(", ")),
                ))
            }
        })
    }
}

/// The macOS system-audio source (`audio.system_backend`). `Auto` is a Core Audio process tap on
/// macOS 14.2 and later (System Audio Recording permission) with ScreenCaptureKit (Screen
/// Recording) as the fallback; the other two force one source. Other OSes ignore it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SystemBackend {
    Auto,
    Tap,
    ScreenCaptureKit,
}

impl SystemBackend {
    pub fn parse(s: &str) -> Result<Self> {
        Ok(match s {
            "auto" => Self::Auto,
            "tap" => Self::Tap,
            "screencapturekit" => Self::ScreenCaptureKit,
            other => {
                return Err(AudioError::new(
                    "invalid_value",
                    format!(
                        "audio.system_backend '{other}'; allowed: {}",
                        SYSTEM_BACKENDS.join(", ")
                    ),
                ))
            }
        })
    }
}

/// `split`: two-channel WAV, left = mic ("you"), right = system loopback ("them"), the layout
/// `stt-transcribe` reads. `mix`: one channel, the sum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layout {
    Split,
    Mix,
}

impl Layout {
    pub fn parse(s: &str) -> Result<Self> {
        match s {
            "split" => Ok(Self::Split),
            "mix" => Ok(Self::Mix),
            other => Err(AudioError::new(
                "invalid_value",
                format!("audio.layout '{other}'; allowed: {}", LAYOUTS.join(", ")),
            )),
        }
    }

    pub fn channels(self) -> u16 {
        match self {
            Self::Split => 2,
            Self::Mix => 1,
        }
    }
}

/// Waits the backends and the recorder use; `oats-core` fills them from `audio.open_timeout` and
/// `audio.poll_interval`. There is no default here: the key table is the only home of defaults.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timing {
    /// How long a backend waits for a capture to deliver its first audio (or start).
    pub open_timeout: Duration,
    /// Pause of every polling loop (writer, readiness waits, stop flags).
    pub poll: Duration,
    /// How long a capture may go without IO before it counts as lost, and the wait before each
    /// try to reopen a lost one (`audio.restart_wait`).
    pub restart: Duration,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioSettings {
    pub backend: BackendChoice,
    /// The macOS system-audio source; ignored elsewhere.
    pub system_backend: SystemBackend,
    pub layout: Layout,
    pub system_enabled: bool,
    /// Empty means the platform default loopback / monitor.
    pub system_device: String,
    pub mic_enabled: bool,
    /// Empty means the platform default microphone.
    pub mic_device: String,
    pub timing: Timing,
    /// Echo cancellation of the mic against system audio (`audio.echo_cancel`); `None` is off.
    /// Only applies while both sources are recorded.
    pub echo: Option<crate::aec::EchoSettings>,
    /// Digital silence from a system source that a missing grant silences (the macOS tap) before
    /// the recording warns (`audio.silence_warning`); zero never warns.
    pub silence_warning: Duration,
}

impl AudioSettings {
    /// Both sources on, split layout, platform-default devices.
    pub fn new(timing: Timing) -> Self {
        Self {
            backend: BackendChoice::Auto,
            system_backend: SystemBackend::Auto,
            layout: Layout::Split,
            system_enabled: true,
            system_device: String::new(),
            mic_enabled: true,
            mic_device: String::new(),
            timing,
            echo: None,
            silence_warning: Duration::ZERO,
        }
    }
}

#[cfg(test)]
pub(crate) fn test_settings() -> AudioSettings {
    AudioSettings {
        echo: Some(crate::aec::EchoSettings {
            tail: Duration::from_millis(50),
            delay: Duration::from_millis(20),
            block: 256,
            step_permille: 500,
            divergence_percent: 200,
            warmup: Duration::ZERO,
            warmup_step_permille: 500,
            dominance_percent: 50,
            align: Duration::ZERO,
            align_probe: Duration::ZERO,
            align_confidence_percent: 50,
            suppress_percent: 0,
            suppress_floor_percent: 10,
        }),
        ..AudioSettings::new(Timing {
            open_timeout: Duration::from_secs(15),
            poll: Duration::from_millis(25),
            restart: Duration::from_millis(100),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_value_parses_and_nothing_else() {
        for b in BACKENDS {
            BackendChoice::parse(b).unwrap();
        }
        for l in LAYOUTS {
            Layout::parse(l).unwrap();
        }
        for b in SYSTEM_BACKENDS {
            SystemBackend::parse(b).unwrap();
        }
        assert_eq!(
            SystemBackend::parse("sck").unwrap_err().code,
            "invalid_value"
        );
        assert_eq!(
            BackendChoice::parse("alsa").unwrap_err().code,
            "invalid_value"
        );
        assert_eq!(Layout::parse("quad").unwrap_err().code, "invalid_value");
        assert_eq!(Layout::Split.channels(), 2);
        assert_eq!(Layout::Mix.channels(), 1);
    }
}
