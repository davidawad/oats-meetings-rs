//! A deterministic in-memory backend for tests of the recorder and of the engine actions:
//! one loopback device playing a tone and one mic playing another, delivered in real time.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use crate::backend::{Capture, CaptureBackend, DeviceInfo, DeviceKind, OnSamples};
use crate::error::{AudioError, Result};
use crate::SAMPLE_RATE;

pub struct FakeBackend {
    pub loopback_hz: f32,
    pub mic_hz: f32,
    /// Second microphone (`fake.mic2`), to test switching devices.
    pub mic2_hz: f32,
    pub amplitude: f32,
    /// The loopback stands in for the macOS Core Audio tap: its capture names itself `tap` and
    /// has a silence hint (a `loopback_hz` of 0 is the zeros of a tap without its grant).
    pub loopback_tap: bool,
}

impl Default for FakeBackend {
    fn default() -> Self {
        Self {
            loopback_hz: 440.0,
            mic_hz: 880.0,
            mic2_hz: 1320.0,
            amplitude: 0.5,
            loopback_tap: false,
        }
    }
}

struct FakeCapture {
    stop: Arc<AtomicBool>,
    th: Option<std::thread::JoinHandle<()>>,
    tap: bool,
}

/// The silence hint of a `loopback_tap` capture.
pub const TAP_HINT: &str = "fake tap: allow System Audio Recording";

impl Capture for FakeCapture {
    fn stop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.th.take() {
            let _ = t.join();
        }
    }

    fn source(&self) -> Option<&'static str> {
        self.tap.then_some("tap")
    }

    fn silence_hint(&self) -> Option<String> {
        self.tap.then(|| TAP_HINT.to_string())
    }
}

impl Drop for FakeCapture {
    fn drop(&mut self) {
        self.stop();
    }
}

impl CaptureBackend for FakeBackend {
    fn name(&self) -> &'static str {
        "fake"
    }

    fn list_devices(&self) -> Result<Vec<DeviceInfo>> {
        let d = |id: &str, kind| DeviceInfo {
            id: id.into(),
            name: id.into(),
            kind,
            backend: "fake",
            default: true,
        };
        let mut mic2 = d("fake.mic2", DeviceKind::Mic);
        mic2.default = false;
        Ok(vec![
            d("fake.monitor", DeviceKind::Loopback),
            d("fake.mic", DeviceKind::Mic),
            mic2,
        ])
    }

    fn open(
        &self,
        kind: DeviceKind,
        device: &str,
        mut on_samples: OnSamples,
    ) -> Result<Box<dyn Capture>> {
        let hz = match (kind, device) {
            (DeviceKind::Loopback, "" | "fake.monitor") => self.loopback_hz,
            (DeviceKind::Mic, "" | "fake.mic") => self.mic_hz,
            (DeviceKind::Mic, "fake.mic2") => self.mic2_hz,
            _ => {
                return Err(AudioError::new(
                    "device_not_found",
                    format!("device '{device}' not found"),
                ))
            }
        };
        let amp = self.amplitude;
        let tap = self.loopback_tap && kind == DeviceKind::Loopback;
        let stop = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&stop);
        let th = std::thread::spawn(move || {
            let chunk = SAMPLE_RATE as usize / 100; // 10 ms of audio per delivery
            let pace = Duration::from_secs_f64(chunk as f64 / f64::from(SAMPLE_RATE));
            let t0 = std::time::Instant::now();
            let mut n: u64 = 0;
            while !flag.load(Ordering::SeqCst) {
                // Deliver by media time, not by loop count: a thread starved by CPU load
                // catches up with the audio it owes instead of dropping it (a slept-through
                // 10 ms chunk is delivered late, never lost).
                let owed = (t0.elapsed().as_secs_f64() * f64::from(SAMPLE_RATE)) as u64;
                if n + chunk as u64 > owed {
                    std::thread::sleep(pace);
                    continue;
                }
                let buf: Vec<f32> = (0..chunk)
                    .map(|i| {
                        let t = (n + i as u64) as f32 / SAMPLE_RATE as f32;
                        (t * hz * std::f32::consts::TAU).sin() * amp
                    })
                    .collect();
                n += chunk as u64;
                on_samples(&buf);
            }
        });
        Ok(Box::new(FakeCapture {
            stop,
            th: Some(th),
            tap,
        }))
    }
}

/// A backend whose `list_devices` does not return until `release` gets a message or is dropped:
/// a device that never answers its probe (a CoreAudio AudioUnit, dot-4uzgol.57). Opening
/// devices works as in `FakeBackend`.
pub struct StuckBackend {
    pub gate: Arc<std::sync::Mutex<std::sync::mpsc::Receiver<()>>>,
}

impl StuckBackend {
    /// The backend factory side of the gate and the sender that releases every listing waiting
    /// on it.
    pub fn gate() -> (
        Arc<std::sync::Mutex<std::sync::mpsc::Receiver<()>>>,
        std::sync::mpsc::Sender<()>,
    ) {
        let (tx, rx) = std::sync::mpsc::channel();
        (Arc::new(std::sync::Mutex::new(rx)), tx)
    }
}

impl CaptureBackend for StuckBackend {
    fn name(&self) -> &'static str {
        "stuck"
    }

    fn list_devices(&self) -> Result<Vec<DeviceInfo>> {
        let _ = self.gate.lock().map(|rx| rx.recv());
        FakeBackend::default().list_devices()
    }

    fn open(&self, kind: DeviceKind, device: &str, on: OnSamples) -> Result<Box<dyn Capture>> {
        FakeBackend::default().open(kind, device, on)
    }
}
