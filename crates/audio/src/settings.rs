//! The audio config keys as a plain struct. `oats-core` builds it from the resolved config; the
//! string values are exactly the enum values of the `audio.*` keys.

use std::time::Duration;

use crate::error::{AudioError, Result};

pub const BACKENDS: &[&str] = &["auto", "pulse", "pipewire", "cpal"];
pub const LAYOUTS: &[&str] = &["split", "mix"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendChoice {
    Auto,
    Pulse,
    Pipewire,
    Cpal,
}

impl BackendChoice {
    pub fn parse(s: &str) -> Result<Self> {
        Ok(match s {
            "auto" => Self::Auto,
            "pulse" => Self::Pulse,
            "pipewire" => Self::Pipewire,
            "cpal" => Self::Cpal,
            other => {
                return Err(AudioError::new(
                    "invalid_value",
                    format!("audio.backend '{other}'; allowed: {}", BACKENDS.join(", ")),
                ))
            }
        })
    }
}

/// `split`: two-channel WAV, left = system loopback, right = mic. `mix`: one channel, the sum.
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
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioSettings {
    pub backend: BackendChoice,
    pub layout: Layout,
    pub system_enabled: bool,
    /// Empty means the platform default loopback / monitor.
    pub system_device: String,
    pub mic_enabled: bool,
    /// Empty means the platform default microphone.
    pub mic_device: String,
    pub timing: Timing,
}

impl AudioSettings {
    /// Both sources on, split layout, platform-default devices.
    pub fn new(timing: Timing) -> Self {
        Self {
            backend: BackendChoice::Auto,
            layout: Layout::Split,
            system_enabled: true,
            system_device: String::new(),
            mic_enabled: true,
            mic_device: String::new(),
            timing,
        }
    }
}

#[cfg(test)]
pub(crate) fn test_settings() -> AudioSettings {
    AudioSettings::new(Timing { open_timeout: Duration::from_secs(15), poll: Duration::from_millis(25) })
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
        assert_eq!(BackendChoice::parse("alsa").unwrap_err().code, "invalid_value");
        assert_eq!(Layout::parse("quad").unwrap_err().code, "invalid_value");
        assert_eq!(Layout::Split.channels(), 2);
        assert_eq!(Layout::Mix.channels(), 1);
    }
}
