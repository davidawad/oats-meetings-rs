//! Mix a system-loopback channel and a mic channel into one WAV, streaming to disk.
//!
//! Each source pushes mono samples into its own queue from its capture thread. A writer thread
//! drains both in lock step (frames are written only when every enabled source has them, so the
//! channels stay aligned) and pads the shorter queue with silence on stop.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use serde::Serialize;

use crate::backend::{Capture, CaptureBackend, DeviceKind};
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

fn drain_into(w: &mut WavWriter, q: &mut Queues, sys: bool, mic: bool, layout: Layout, flush_all: bool) -> Result<()> {
    let n = match (sys, mic) {
        (true, true) if flush_all => q.system.len().max(q.mic.len()),
        (true, true) => q.system.len().min(q.mic.len()),
        (true, false) => q.system.len(),
        (false, true) => q.mic.len(),
        (false, false) => 0,
    };
    let mut buf = Vec::with_capacity(n * 2);
    for _ in 0..n {
        let s = if sys { q.system.pop_front().unwrap_or(0.0) } else { 0.0 };
        let m = if mic { q.mic.pop_front().unwrap_or(0.0) } else { 0.0 };
        match layout {
            Layout::Split => {
                buf.push(s);
                buf.push(m);
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

/// Start capturing per `settings` into a WAV at `path` (parent dirs are created).
pub fn start(backend: &dyn CaptureBackend, settings: &AudioSettings, path: &Path) -> Result<Recording> {
    if !settings.system_enabled && !settings.mic_enabled {
        return Err(AudioError::new(
            "invalid_config",
            "audio.system.enabled and audio.mic.enabled are both false: nothing to record",
        ));
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| crate::error::io_err(&dir.display().to_string(), e))?;
    }
    let (sys, mic, layout) = (settings.system_enabled, settings.mic_enabled, settings.layout);
    let shared = Arc::new(Shared {
        q: Mutex::new(Queues::default()),
        frames: AtomicU64::new(0),
        system_in: AtomicU64::new(0),
        mic_in: AtomicU64::new(0),
        live: AtomicBool::new(false),
        paused: AtomicBool::new(false),
        stop: AtomicBool::new(false),
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
                    sh.system_in.fetch_add(samples.len() as u64, Ordering::SeqCst);
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
    let writer = std::thread::spawn(move || -> Result<Summary> {
        loop {
            let stopping = sh.stop.load(Ordering::SeqCst);
            {
                let mut q = sh.q.lock().expect("queues");
                drain_into(&mut w, &mut q, sys, mic, layout, stopping)?;
            }
            sh.frames.store(w.frames(), Ordering::SeqCst);
            if stopping {
                break;
            }
            std::thread::sleep(Duration::from_millis(40));
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
    Ok(Recording { shared, captures, writer: Some(writer), path: path.to_path_buf(), layout })
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

    fn record(settings: &AudioSettings, secs: f64) -> (Summary, wav::WavData) {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("nested/a.wav");
        let r = start(&FakeBackend::default(), settings, &p).unwrap();
        std::thread::sleep(Duration::from_millis((secs * 1000.0) as u64));
        let s = r.stop().unwrap();
        let data = wav::read(&p).unwrap();
        (s, data)
    }

    #[test]
    fn split_layout_keeps_each_source_on_its_channel() {
        let (s, d) = record(&AudioSettings::default(), 1.2);
        assert_eq!((d.channels, d.rate, s.channels), (2, 48000, 2));
        assert!(d.duration_secs() > 1.0 && d.duration_secs() < 1.6, "{}", d.duration_secs());
        assert!((d.tone_hz(0) - 440.0).abs() < 15.0, "left {}", d.tone_hz(0));
        assert!((d.tone_hz(1) - 880.0).abs() < 25.0, "right {}", d.tone_hz(1));
        assert!(d.rms(0) > 0.3 && d.rms(1) > 0.3);
    }

    #[test]
    fn mix_layout_is_one_channel() {
        let st = AudioSettings { layout: Layout::Mix, ..Default::default() };
        let (_, d) = record(&st, 0.6);
        assert_eq!(d.channels, 1);
        assert!(d.rms(0) > 0.3);
    }

    #[test]
    fn disabled_sources_are_silent_channels() {
        let st = AudioSettings { mic_enabled: false, ..Default::default() };
        let (_, d) = record(&st, 0.6);
        assert!(d.rms(0) > 0.3 && d.rms(1) == 0.0);
        let st = AudioSettings { system_enabled: false, ..Default::default() };
        let (_, d) = record(&st, 0.6);
        assert!(d.rms(0) == 0.0 && d.rms(1) > 0.3);
    }

    #[test]
    fn nothing_enabled_and_bad_device_are_errors() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("a.wav");
        let st = AudioSettings { system_enabled: false, mic_enabled: false, ..Default::default() };
        assert_eq!(start(&FakeBackend::default(), &st, &p).err().unwrap().code, "invalid_config");
        let st = AudioSettings { mic_device: "nope".into(), ..Default::default() };
        let e = start(&FakeBackend::default(), &st, &p).err().unwrap();
        assert_eq!(e.code, "device_not_found");
        assert!(e.message.starts_with("microphone:"));
    }

    #[test]
    fn pause_drops_audio_and_duration_tracks_writes() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("a.wav");
        let r = start(&FakeBackend::default(), &AudioSettings::default(), &p).unwrap();
        std::thread::sleep(Duration::from_millis(500));
        r.set_paused(true);
        std::thread::sleep(Duration::from_millis(100));
        let at_pause = r.duration_ms();
        std::thread::sleep(Duration::from_millis(600));
        assert!(r.duration_ms() <= at_pause + 150, "{} vs {}", r.duration_ms(), at_pause);
        assert!(r.paused());
        r.set_paused(false);
        let s = r.stop().unwrap();
        assert!(s.duration_ms < 1000, "{}", s.duration_ms);
    }
}
