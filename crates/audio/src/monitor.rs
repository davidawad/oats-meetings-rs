//! Input monitor (dot-4uzgol.92): the mic and the system audio opened for the level meters only.
//!
//! Each capture's callback turns its block into an RMS level in an atomic and drops the samples:
//! nothing is queued, written or transcribed, so memory stays flat however long it runs. Stopping
//! (or dropping) the monitor releases every device; a callback a backend never frees (cpal's
//! macOS stream, dot-4uzgol.80) holds only a `Weak` and does nothing once the monitor is gone.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Weak};

use crate::backend::{Capture, CaptureBackend, DeviceKind};
use crate::error::{AudioError, Result};
use crate::recorder::{rms, Source};
use crate::settings::AudioSettings;

#[derive(Default)]
struct Levels {
    /// Cleared first on stop, so a callback that runs late counts and stores nothing.
    live: AtomicBool,
    mic: AtomicU32,
    system: AtomicU32,
    /// Callback blocks seen while live, per source.
    mic_blocks: AtomicU64,
    system_blocks: AtomicU64,
}

pub struct Monitor {
    levels: Arc<Levels>,
    captures: Vec<Box<dyn Capture>>,
}

/// Sees every block the monitor meters, per source, on the capture's thread (it must not block):
/// the auto-start detector (dot-4uzgol.110).
pub type MonitorTap = Arc<dyn Fn(Source, &[f32]) + Send + Sync>;

/// Open the enabled sources of `settings` for metering. At least one must open: a source that
/// fails beside one that opened is left out (its meter stays at zero), so a denied mic does not
/// blank the system meter.
pub fn start(backend: &dyn CaptureBackend, settings: &AudioSettings) -> Result<Monitor> {
    start_tapped(backend, settings, None)
}

/// `start`, with every metered block also handed to `tap`.
pub fn start_tapped(
    backend: &dyn CaptureBackend,
    settings: &AudioSettings,
    tap: Option<MonitorTap>,
) -> Result<Monitor> {
    let levels = Arc::new(Levels::default());
    let mut captures: Vec<Box<dyn Capture>> = vec![];
    let mut first_err = None;
    let sources = [
        (
            settings.system_enabled,
            DeviceKind::Loopback,
            &settings.system_device,
            true,
        ),
        (
            settings.mic_enabled,
            DeviceKind::Mic,
            &settings.mic_device,
            false,
        ),
    ];
    for (on, kind, device, is_sys) in sources {
        if !on {
            continue;
        }
        let weak: Weak<Levels> = Arc::downgrade(&levels);
        let tap = tap.clone();
        let source = if is_sys { Source::System } else { Source::Mic };
        let opened = backend.open(
            kind,
            device,
            Box::new(move |samples: &[f32]| {
                let Some(l) = weak.upgrade() else { return };
                if !l.live.load(Ordering::SeqCst) || samples.is_empty() {
                    return;
                }
                let (level, blocks) = if is_sys {
                    (&l.system, &l.system_blocks)
                } else {
                    (&l.mic, &l.mic_blocks)
                };
                level.store(rms(samples).to_bits(), Ordering::SeqCst);
                blocks.fetch_add(1, Ordering::SeqCst);
                if let Some(t) = tap.as_ref() {
                    t(source, samples);
                }
            }),
        );
        match opened {
            Ok(c) => captures.push(c),
            Err(e) => {
                let what = if is_sys { "system audio" } else { "microphone" };
                first_err.get_or_insert(AudioError::new(e.code, format!("{what}: {}", e.message)));
            }
        }
    }
    if captures.is_empty() {
        return Err(first_err.unwrap_or_else(|| {
            AudioError::new(
                "invalid_config",
                "audio.system.enabled and audio.mic.enabled are both false: nothing to monitor",
            )
        }));
    }
    levels.live.store(true, Ordering::SeqCst);
    Ok(Monitor { levels, captures })
}

impl Monitor {
    /// RMS (0..1) of the latest mic and system block, as `(mic, system)`; zero for a source that
    /// is off or did not open.
    pub fn levels(&self) -> (f32, f32) {
        let get = |a: &AtomicU32| f32::from_bits(a.load(Ordering::SeqCst));
        (get(&self.levels.mic), get(&self.levels.system))
    }

    /// Callback blocks seen so far as `(mic, system)`.
    pub fn blocks(&self) -> (u64, u64) {
        (
            self.levels.mic_blocks.load(Ordering::SeqCst),
            self.levels.system_blocks.load(Ordering::SeqCst),
        )
    }

    /// Release every device. Idempotent; dropping the monitor does the same.
    pub fn stop(&mut self) {
        self.levels.live.store(false, Ordering::SeqCst);
        for c in self.captures.iter_mut() {
            c.stop();
        }
        self.captures.clear();
    }
}

impl Drop for Monitor {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake::FakeBackend;
    use std::sync::atomic::AtomicUsize;
    use std::sync::Mutex;
    use std::time::{Duration, Instant};

    fn wait_for(cond: impl Fn() -> bool) -> bool {
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if cond() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        false
    }

    #[test]
    fn meters_follow_the_fake_backend() {
        let m = start(&FakeBackend::default(), &crate::settings::test_settings()).unwrap();
        assert!(wait_for(|| {
            let (mic, sys) = m.levels();
            mic > 0.3 && sys > 0.3
        }));
        let (mic, sys) = m.blocks();
        assert!(mic > 0 && sys > 0);
    }

    #[test]
    fn a_disabled_source_reads_zero_and_both_off_is_an_error() {
        let st = crate::settings::AudioSettings {
            system_enabled: false,
            ..crate::settings::test_settings()
        };
        let m = start(&FakeBackend::default(), &st).unwrap();
        assert!(wait_for(|| m.levels().0 > 0.3));
        assert_eq!((m.levels().1, m.blocks().1), (0.0, 0));
        let st = crate::settings::AudioSettings {
            system_enabled: false,
            mic_enabled: false,
            ..crate::settings::test_settings()
        };
        assert_eq!(
            start(&FakeBackend::default(), &st).err().unwrap().code,
            "invalid_config"
        );
    }

    #[test]
    fn a_source_that_fails_beside_one_that_opens_is_left_out() {
        let st = crate::settings::AudioSettings {
            mic_device: "nope".into(),
            ..crate::settings::test_settings()
        };
        let m = start(&FakeBackend::default(), &st).unwrap();
        assert!(wait_for(|| m.levels().1 > 0.3));
        assert_eq!(m.blocks().0, 0);
        let st = crate::settings::AudioSettings {
            mic_device: "nope".into(),
            system_enabled: false,
            ..crate::settings::test_settings()
        };
        let e = start(&FakeBackend::default(), &st).err().unwrap();
        assert_eq!(e.code, "device_not_found");
        assert!(e.message.starts_with("microphone:"));
    }

    /// Keeps every callback forever (cpal's macOS stream, dot-4uzgol.80) and counts releases.
    #[derive(Default)]
    struct Leaky {
        kept: Arc<Mutex<Vec<crate::backend::OnSamples>>>,
        opened: Arc<AtomicUsize>,
        released: Arc<AtomicUsize>,
    }

    struct LeakyCapture(Arc<AtomicUsize>, bool);

    impl Capture for LeakyCapture {
        fn stop(&mut self) {
            if !std::mem::replace(&mut self.1, true) {
                self.0.fetch_add(1, Ordering::SeqCst);
            }
        }
    }

    impl CaptureBackend for Leaky {
        fn name(&self) -> &'static str {
            "leaky"
        }

        fn list_devices(&self) -> Result<Vec<crate::backend::DeviceInfo>> {
            Ok(vec![])
        }

        fn open(
            &self,
            _: DeviceKind,
            _: &str,
            on: crate::backend::OnSamples,
        ) -> Result<Box<dyn Capture>> {
            self.kept.lock().unwrap().push(on);
            self.opened.fetch_add(1, Ordering::SeqCst);
            Ok(Box::new(LeakyCapture(Arc::clone(&self.released), false)))
        }
    }

    impl Leaky {
        fn play(&self) {
            for cb in self.kept.lock().unwrap().iter_mut() {
                cb(&[0.25; 480]);
            }
        }
    }

    #[test]
    fn stop_releases_every_device_and_late_callbacks_do_nothing() {
        let b = Leaky::default();
        let mut m = start(&b, &crate::settings::test_settings()).unwrap();
        b.play();
        assert_eq!(m.blocks(), (1, 1));
        assert!((m.levels().0 - 0.25).abs() < 1e-6);
        let levels = Arc::downgrade(&m.levels);
        m.stop();
        assert_eq!(b.released.load(Ordering::SeqCst), 2);
        b.play();
        assert_eq!(m.blocks(), (1, 1), "a callback counted after stop");
        drop(m);
        assert!(
            levels.upgrade().is_none(),
            "the leaked callbacks keep the meters alive"
        );
        b.play();
        // Dropping without stop releases too.
        let b = Leaky::default();
        drop(start(&b, &crate::settings::test_settings()).unwrap());
        assert_eq!(b.opened.load(Ordering::SeqCst), 2);
        assert_eq!(b.released.load(Ordering::SeqCst), 2);
    }
}
