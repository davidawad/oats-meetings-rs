//! Mix a system-loopback channel and a mic channel into one WAV, streaming to disk.
//!
//! Each source pushes mono samples into its own queue from its capture thread. A writer thread
//! drains both in lock step (frames are written only when every enabled source has them, so the
//! channels stay aligned) and pads the shorter queue with silence on stop.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use serde::Serialize;

use crate::aec::EchoCanceller;
use crate::backend::{Capture, CaptureBackend, DeviceKind};

/// Which input a sample came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    System,
    Mic,
}

/// Per-source tap: called with every block of that source as it is written to the WAV (live,
/// non-paused audio; the mic after echo cancellation), at `SAMPLE_RATE`, from the writer thread.
/// It must not block (queue the work).
pub type TapFactory<'a> = &'a dyn Fn(Source) -> Box<dyn FnMut(&[f32]) + Send>;
use crate::error::{AudioError, Result};
use crate::settings::{AudioSettings, Layout};
use crate::wav::WavWriter;
use crate::SAMPLE_RATE;

#[derive(Default)]
struct Queues {
    system: VecDeque<f32>,
    mic: VecDeque<f32>,
}

struct Shared {
    q: Mutex<Queues>,
    frames: AtomicU64,
    system_in: AtomicU64,
    mic_in: AtomicU64,
    /// False until every capture is open: sources connect at different speeds (a cold `parec` takes
    /// ~2 s), and audio delivered before the last one is up would put the channels out of step.
    live: AtomicBool,
    paused: AtomicBool,
    stop: AtomicBool,
    /// RMS of the mic and system samples the writer handled last (`f32` bits), for level meters.
    mic_level: AtomicU32,
    system_level: AtomicU32,
}

#[derive(Debug, Clone, Serialize)]
pub struct Summary {
    pub path: PathBuf,
    pub frames: u64,
    pub rate: u32,
    pub channels: u16,
    pub duration_ms: u64,
    /// Samples each source delivered (before lock-step padding); a source far below the other
    /// delivered audio slower than real time.
    pub system_samples: u64,
    pub mic_samples: u64,
}

pub struct Recording {
    shared: Arc<Shared>,
    captures: Vec<Box<dyn Capture>>,
    writer: Option<JoinHandle<Result<Summary>>>,
    path: PathBuf,
    layout: Layout,
}

fn ms(frames: u64) -> u64 {
    frames * 1000 / u64::from(SAMPLE_RATE)
}

/// Pop the frames ready to write: every enabled source's samples in lock step (all of both,
/// padded with silence, when `flush_all`); a disabled source is silence.
fn take(q: &mut Queues, sys: bool, mic: bool, flush_all: bool) -> (Vec<f32>, Vec<f32>) {
    let n = match (sys, mic) {
        (true, true) if flush_all => q.system.len().max(q.mic.len()),
        (true, true) => q.system.len().min(q.mic.len()),
        (true, false) => q.system.len(),
        (false, true) => q.mic.len(),
        (false, false) => 0,
    };
    let pop = |enabled: bool, from: &mut VecDeque<f32>| -> Vec<f32> {
        let mut v: Vec<f32> = if enabled {
            from.drain(..n.min(from.len())).collect()
        } else {
            vec![]
        };
        v.resize(n, 0.0);
        v
    };
    let s = pop(sys, &mut q.system);
    let m = pop(mic, &mut q.mic);
    (m, s)
}

fn write_frames(w: &mut WavWriter, m: &[f32], s: &[f32], layout: Layout) -> Result<()> {
    let mut buf = Vec::with_capacity(m.len() * 2);
    for (&m, &s) in m.iter().zip(s) {
        match layout {
            // Left = mic, right = system: the same convention `stt-transcribe` reads (left "you").
            Layout::Split => {
                buf.push(m);
                buf.push(s);
            }
            Layout::Mix => buf.push((s + m).clamp(-1.0, 1.0)),
        }
    }
    if buf.is_empty() {
        Ok(())
    } else {
        w.write(&buf)
    }
}

fn rms(x: &[f32]) -> f32 {
    (x.iter().map(|v| v * v).sum::<f32>() / x.len().max(1) as f32).sqrt()
}

/// Start capturing per `settings` into a WAV at `path` (parent dirs are created).
pub fn start(
    backend: &dyn CaptureBackend,
    settings: &AudioSettings,
    path: &Path,
    tap: Option<TapFactory>,
) -> Result<Recording> {
    if !settings.system_enabled && !settings.mic_enabled {
        return Err(AudioError::new(
            "invalid_config",
            "audio.system.enabled and audio.mic.enabled are both false: nothing to record",
        ));
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)
            .map_err(|e| crate::error::io_err(&dir.display().to_string(), e))?;
    }
    let (sys, mic, layout) = (
        settings.system_enabled,
        settings.mic_enabled,
        settings.layout,
    );
    let shared = Arc::new(Shared {
        q: Mutex::new(Queues::default()),
        frames: AtomicU64::new(0),
        system_in: AtomicU64::new(0),
        mic_in: AtomicU64::new(0),
        live: AtomicBool::new(false),
        paused: AtomicBool::new(false),
        stop: AtomicBool::new(false),
        mic_level: AtomicU32::new(0),
        system_level: AtomicU32::new(0),
    });
    let open = |kind: DeviceKind, device: &str, is_sys: bool| -> Result<Box<dyn Capture>> {
        let sh = Arc::clone(&shared);
        backend.open(
            kind,
            device,
            Box::new(move |samples: &[f32]| {
                if sh.paused.load(Ordering::SeqCst) || !sh.live.load(Ordering::SeqCst) {
                    return;
                }
                let mut q = sh.q.lock().expect("queues");
                if is_sys {
                    q.system.extend(samples);
                    sh.system_in
                        .fetch_add(samples.len() as u64, Ordering::SeqCst);
                } else {
                    q.mic.extend(samples);
                    sh.mic_in.fetch_add(samples.len() as u64, Ordering::SeqCst);
                }
            }),
        )
    };
    let mut captures: Vec<Box<dyn Capture>> = vec![];
    if sys {
        captures.push(
            open(DeviceKind::Loopback, &settings.system_device, true)
                .map_err(|e| AudioError::new(e.code, format!("system audio: {}", e.message)))?,
        );
    }
    if mic {
        // An early return drops `captures`, which stops the system capture already running.
        captures.push(
            open(DeviceKind::Mic, &settings.mic_device, false)
                .map_err(|e| AudioError::new(e.code, format!("microphone: {}", e.message)))?,
        );
    }
    shared.live.store(true, Ordering::SeqCst);
    let mut w = WavWriter::create(path, layout.channels(), SAMPLE_RATE)?;
    let sh = Arc::clone(&shared);
    let (out_path, channels) = (path.to_path_buf(), layout.channels());
    let poll = settings.timing.poll;
    // The system channel is what the speakers played: the far-end reference for the mic.
    let mut aec = settings
        .echo
        .filter(|_| sys && mic)
        .map(|e| EchoCanceller::new(&e));
    let mut tap_sys = tap.filter(|_| sys).map(|f| f(Source::System));
    let mut tap_mic = tap.filter(|_| mic).map(|f| f(Source::Mic));
    let writer = std::thread::spawn(move || -> Result<Summary> {
        loop {
            let stopping = sh.stop.load(Ordering::SeqCst);
            let (mut m, mut s) = take(&mut sh.q.lock().expect("queues"), sys, mic, stopping);
            if let Some(c) = aec.as_mut() {
                let (mut mo, mut so) = (Vec::with_capacity(m.len()), Vec::with_capacity(s.len()));
                c.process(&m, &s, &mut mo, &mut so);
                if stopping {
                    c.flush(&mut mo, &mut so);
                }
                (m, s) = (mo, so);
            }
            write_frames(&mut w, &m, &s, layout)?;
            if sh.paused.load(Ordering::SeqCst) {
                sh.mic_level.store(0, Ordering::SeqCst);
                sh.system_level.store(0, Ordering::SeqCst);
            } else {
                for (level, x) in [(&sh.mic_level, &m), (&sh.system_level, &s)] {
                    if !x.is_empty() {
                        level.store(rms(x).to_bits(), Ordering::SeqCst);
                    }
                }
            }
            for (t, x) in [(tap_sys.as_mut(), &s), (tap_mic.as_mut(), &m)] {
                if let (Some(t), false) = (t, x.is_empty()) {
                    t(x);
                }
            }
            sh.frames.store(w.frames(), Ordering::SeqCst);
            if stopping {
                break;
            }
            std::thread::sleep(poll);
        }
        let frames = w.finish()?;
        Ok(Summary {
            path: out_path,
            frames,
            rate: SAMPLE_RATE,
            channels,
            duration_ms: ms(frames),
            system_samples: sh.system_in.load(Ordering::SeqCst),
            mic_samples: sh.mic_in.load(Ordering::SeqCst),
        })
    });
    Ok(Recording {
        shared,
        captures,
        writer: Some(writer),
        path: path.to_path_buf(),
        layout,
    })
}

impl Recording {
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn layout(&self) -> Layout {
        self.layout
    }

    /// Audio written so far.
    pub fn duration_ms(&self) -> u64 {
        ms(self.shared.frames.load(Ordering::SeqCst))
    }

    pub fn paused(&self) -> bool {
        self.shared.paused.load(Ordering::SeqCst)
    }

    /// RMS (0..1) of the latest mic and system audio written, as `(mic, system)`; zero while
    /// paused or for a disabled source.
    pub fn levels(&self) -> (f32, f32) {
        let get = |a: &AtomicU32| f32::from_bits(a.load(Ordering::SeqCst));
        (get(&self.shared.mic_level), get(&self.shared.system_level))
    }

    pub fn set_paused(&self, paused: bool) {
        self.shared.paused.store(paused, Ordering::SeqCst);
    }

    pub fn stop(mut self) -> Result<Summary> {
        for c in &mut self.captures {
            c.stop();
        }
        self.shared.stop.store(true, Ordering::SeqCst);
        self.writer
            .take()
            .expect("writer")
            .join()
            .map_err(|_| AudioError::new("internal", "writer thread panicked"))?
    }
}

impl Drop for Recording {
    fn drop(&mut self) {
        // Dropped without `stop` (engine shutdown): finish the file so it stays a valid WAV.
        for c in &mut self.captures {
            c.stop();
        }
        self.shared.stop.store(true, Ordering::SeqCst);
        if let Some(w) = self.writer.take() {
            let _ = w.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake::FakeBackend;
    use crate::wav;
    use std::time::Duration;

    fn record(settings: &AudioSettings, secs: f64) -> (Summary, wav::WavData) {
        record_with(&FakeBackend::default(), settings, secs)
    }

    fn record_with(
        backend: &FakeBackend,
        settings: &AudioSettings,
        secs: f64,
    ) -> (Summary, wav::WavData) {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("nested/a.wav");
        let r = start(backend, settings, &p, None).unwrap();
        // Wait for the audio itself (not wall time): poll until `secs` have been written.
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        while r.duration_ms() < (secs * 1000.0) as u64 {
            assert!(
                std::time::Instant::now() < deadline,
                "only {} ms written",
                r.duration_ms()
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        let (mic, sys) = r.levels();
        let s = r.stop().unwrap();
        let data = wav::read(&p).unwrap();
        // The meters follow the audio written: a silent channel reads zero.
        assert_eq!(
            mic > 0.0,
            settings.mic_enabled && data.rms(0) > 0.0,
            "mic {mic}"
        );
        if data.channels == 2 {
            assert_eq!(
                sys > 0.0,
                settings.system_enabled && data.rms(1) > 0.0,
                "sys {sys}"
            );
        }
        (s, data)
    }

    #[test]
    fn split_layout_keeps_each_source_on_its_channel() {
        let (s, d) = record(&crate::settings::test_settings(), 1.2);
        assert_eq!((d.channels, d.rate, s.channels), (2, 48000, 2));
        assert!(d.duration_secs() >= 1.2, "{}", d.duration_secs());
        assert!(
            (d.tone_hz(0) - 880.0).abs() < 25.0,
            "left (mic) {}",
            d.tone_hz(0)
        );
        assert!(
            (d.tone_hz(1) - 440.0).abs() < 15.0,
            "right (system) {}",
            d.tone_hz(1)
        );
        assert!(d.rms(0) > 0.3 && d.rms(1) > 0.3);
    }

    #[test]
    fn echo_cancel_takes_the_system_audio_out_of_the_mic() {
        // The mic hears exactly what the speakers play (same tone, its own phase).
        let echoing = FakeBackend {
            mic_hz: 440.0,
            ..FakeBackend::default()
        };
        let last_second = |d: &wav::WavData, ch: usize| {
            let x = &d.samples[ch];
            let tail = &x[x.len() - SAMPLE_RATE as usize..];
            (tail.iter().map(|v| v * v).sum::<f32>() / tail.len() as f32).sqrt()
        };
        let (_, on) = record_with(&echoing, &crate::settings::test_settings(), 2.5);
        let off_settings = AudioSettings {
            echo: None,
            ..crate::settings::test_settings()
        };
        let (_, off) = record_with(&echoing, &off_settings, 2.5);
        assert!(last_second(&off, 0) > 0.3, "mic without echo cancel");
        assert!(last_second(&on, 1) > 0.3, "system channel is left alone");
        assert!(
            last_second(&on, 0) < last_second(&off, 0) / 10.0,
            "mic residual {} vs {} without echo cancel",
            last_second(&on, 0),
            last_second(&off, 0)
        );
    }

    #[test]
    fn mix_layout_is_one_channel() {
        let st = AudioSettings {
            layout: Layout::Mix,
            ..crate::settings::test_settings()
        };
        let (_, d) = record(&st, 0.6);
        assert_eq!(d.channels, 1);
        assert!(d.rms(0) > 0.3);
    }

    #[test]
    fn disabled_sources_are_silent_channels() {
        let st = AudioSettings {
            mic_enabled: false,
            ..crate::settings::test_settings()
        };
        let (_, d) = record(&st, 0.6);
        assert!(
            d.rms(1) > 0.3 && d.rms(0) == 0.0,
            "system only: right channel"
        );
        let st = AudioSettings {
            system_enabled: false,
            ..crate::settings::test_settings()
        };
        let (_, d) = record(&st, 0.6);
        assert!(d.rms(1) == 0.0 && d.rms(0) > 0.3, "mic only: left channel");
    }

    #[test]
    fn nothing_enabled_and_bad_device_are_errors() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("a.wav");
        let st = AudioSettings {
            system_enabled: false,
            mic_enabled: false,
            ..crate::settings::test_settings()
        };
        assert_eq!(
            start(&FakeBackend::default(), &st, &p, None)
                .err()
                .unwrap()
                .code,
            "invalid_config"
        );
        let st = AudioSettings {
            mic_device: "nope".into(),
            ..crate::settings::test_settings()
        };
        let e = start(&FakeBackend::default(), &st, &p, None).err().unwrap();
        assert_eq!(e.code, "device_not_found");
        assert!(e.message.starts_with("microphone:"));
    }

    #[test]
    fn pause_drops_audio_and_duration_tracks_writes() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("a.wav");
        let r = start(
            &FakeBackend::default(),
            &crate::settings::test_settings(),
            &p,
            None,
        )
        .unwrap();
        std::thread::sleep(Duration::from_millis(500));
        r.set_paused(true);
        std::thread::sleep(Duration::from_millis(100));
        let at_pause = r.duration_ms();
        assert_eq!(r.levels(), (0.0, 0.0), "meters drop while paused");
        std::thread::sleep(Duration::from_millis(600));
        assert!(
            r.duration_ms() <= at_pause + 150,
            "{} vs {}",
            r.duration_ms(),
            at_pause
        );
        assert!(r.paused());
        r.set_paused(false);
        let s = r.stop().unwrap();
        assert!(s.duration_ms < 1000, "{}", s.duration_ms);
    }
}
