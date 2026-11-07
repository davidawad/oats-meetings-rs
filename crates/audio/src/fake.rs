//! A deterministic in-memory backend for tests of the recorder and of the engine actions:
//! one loopback device playing a tone and one mic playing another, delivered in real time.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::backend::{Capture, CaptureBackend, DeviceInfo, DeviceKind, OnSamples};
use crate::error::{AudioError, Result};
use crate::SAMPLE_RATE;

#[derive(Clone)]
pub struct FakeBackend {
    pub loopback_hz: f32,
    pub mic_hz: f32,
    /// Second microphone (`fake.mic2`), to test switching devices.
    pub mic2_hz: f32,
    pub amplitude: f32,
    /// The loopback stands in for the macOS Core Audio tap: its capture names itself `tap` and
    /// has a silence hint (a `loopback_hz` of 0 is the zeros of a tap without its grant).
    pub loopback_tap: bool,
    /// Faults a test injects into the captures it opens.
    pub faults: Arc<Faults>,
}

/// Faults injected into a `FakeBackend` (dot-4uzgol.99): a capture whose IO stops on its own (the
/// macOS tap when another app starts an output stream), and opens that fail.
#[derive(Default)]
pub struct Faults {
    /// Kinds whose running capture stops delivering and reports itself lost (once per entry).
    lose: Mutex<Vec<DeviceKind>>,
    /// This many opens fail (`capture_failed`) before opens work again.
    pub fail_opens: AtomicUsize,
    /// Captures opened, by kind: loopback, mic.
    pub opened: [AtomicUsize; 2],
}

/// Why a capture the test made lose its IO says it stopped.
pub const LOST: &str = "fake: IO stopped";

impl Faults {
    /// Stop the running capture of `kind`, as a device change would.
    pub fn lose(&self, kind: DeviceKind) {
        self.lose.lock().expect("faults").push(kind);
    }

    pub fn opened(&self, kind: DeviceKind) -> usize {
        self.opened[usize::from(kind == DeviceKind::Mic)].load(Ordering::SeqCst)
    }

    /// True once (per `lose` call) for `kind`.
    fn take(&self, kind: DeviceKind) -> bool {
        let mut l = self.lose.lock().expect("faults");
        match l.iter().position(|k| *k == kind) {
            Some(i) => {
                l.remove(i);
                true
            }
            None => false,
        }
    }
}

impl Default for FakeBackend {
    fn default() -> Self {
        Self {
            loopback_hz: 440.0,
            mic_hz: 880.0,
            mic2_hz: 1320.0,
            amplitude: 0.5,
            loopback_tap: false,
            faults: Arc::default(),
        }
    }
}

struct FakeCapture {
    stop: Arc<AtomicBool>,
    /// Set by the capture thread when a test made it lose its IO.
    dead: Arc<AtomicBool>,
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

    fn lost(&mut self) -> Option<String> {
        self.dead.load(Ordering::SeqCst).then(|| LOST.to_string())
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
        let faults = Arc::clone(&self.faults);
        let fail = faults
            .fail_opens
            .try_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1));
        if fail.is_ok() {
            return Err(AudioError::new("capture_failed", "fake: open failed"));
        }
        faults.opened[usize::from(kind == DeviceKind::Mic)].fetch_add(1, Ordering::SeqCst);
        let amp = self.amplitude;
        let tap = self.loopback_tap && kind == DeviceKind::Loopback;
        let stop = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&stop);
        let dead = Arc::new(AtomicBool::new(false));
        let died = Arc::clone(&dead);
        let th = std::thread::spawn(move || {
            let chunk = SAMPLE_RATE as usize / 100; // 10 ms of audio per delivery
            let pace = Duration::from_secs_f64(chunk as f64 / f64::from(SAMPLE_RATE));
            let t0 = std::time::Instant::now();
            let mut n: u64 = 0;
            while !flag.load(Ordering::SeqCst) {
                if died.load(Ordering::SeqCst) || faults.take(kind) {
                    // IO stopped: the thread stays until `stop`, delivering nothing.
                    died.store(true, Ordering::SeqCst);
                    std::thread::sleep(pace);
                    continue;
                }
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
            dead,
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
