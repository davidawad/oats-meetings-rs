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
}

impl Default for FakeBackend {
    fn default() -> Self {
        Self { loopback_hz: 440.0, mic_hz: 880.0, mic2_hz: 1320.0, amplitude: 0.5 }
    }
}

struct FakeCapture {
    stop: Arc<AtomicBool>,
    th: Option<std::thread::JoinHandle<()>>,
}

impl Capture for FakeCapture {
    fn stop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.th.take() {
            let _ = t.join();
        }
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
        let d = |id: &str, kind| DeviceInfo { id: id.into(), name: id.into(), kind, backend: "fake", default: true };
        let mut mic2 = d("fake.mic2", DeviceKind::Mic);
        mic2.default = false;
        Ok(vec![d("fake.monitor", DeviceKind::Loopback), d("fake.mic", DeviceKind::Mic), mic2])
    }

    fn open(&self, kind: DeviceKind, device: &str, mut on_samples: OnSamples) -> Result<Box<dyn Capture>> {
        let hz = match (kind, device) {
            (DeviceKind::Loopback, "" | "fake.monitor") => self.loopback_hz,
            (DeviceKind::Mic, "" | "fake.mic") => self.mic_hz,
            (DeviceKind::Mic, "fake.mic2") => self.mic2_hz,
            _ => return Err(AudioError::new("device_not_found", format!("device '{device}' not found"))),
        };
        let amp = self.amplitude;
        let stop = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&stop);
        let th = std::thread::spawn(move || {
            let chunk = SAMPLE_RATE as usize / 100; // 10 ms of audio per delivery
            let pace = Duration::from_secs_f64(chunk as f64 / f64::from(SAMPLE_RATE));
            let mut n: u64 = 0;
            while !flag.load(Ordering::SeqCst) {
                let buf: Vec<f32> = (0..chunk)
                    .map(|i| {
                        let t = (n + i as u64) as f32 / SAMPLE_RATE as f32;
                        (t * hz * std::f32::consts::TAU).sin() * amp
                    })
                    .collect();
                n += chunk as u64;
                on_samples(&buf);
                std::thread::sleep(pace);
            }
        });
        Ok(Box::new(FakeCapture { stop, th: Some(th) }))
    }
}
