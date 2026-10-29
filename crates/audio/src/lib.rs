//! Audio capture (dot-4uzgol.7): one `CaptureBackend` trait, a backend per OS audio stack, and a
//! `Recorder` that mixes a system-loopback channel and a mic channel into one WAV.
//!
//! Backends: `pulse` (Pulse or PipeWire's pulse server: `pactl` + `parec`), `pipewire` (`pw-dump` +
//! `pw-record`), `cpal` (WASAPI loopback + mic, Windows only). `screencapturekit`
//! (macOS: ScreenCaptureKit system audio + cpal mic, TCC denial is `permission_denied`).
//! All backends deliver mono f32 at the recorder's sample rate through a callback.

pub mod backend;
pub mod coreaudio_hal;
pub mod cpal_backend;
pub mod error;
pub mod fake;
pub mod macos;
pub mod pipewire;
pub mod proc;
pub mod pulse;
pub mod recorder;
pub mod resample;
pub mod settings;
pub mod timefmt;
pub mod wav;

pub use backend::{select, Capture, CaptureBackend, DeviceInfo, DeviceKind, OnSamples};
pub use error::{AudioError, Result};
pub use recorder::{Recording, Source, Summary};
pub use settings::{AudioSettings, BackendChoice, Layout, Timing};

/// Fixed output rate of the recorder and of every backend's callback.
pub const SAMPLE_RATE: u32 = 48_000;
