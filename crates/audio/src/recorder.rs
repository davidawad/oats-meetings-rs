//! Mix a system-loopback channel and a mic channel into one WAV, streaming to disk.
//!
//! Each source pushes mono samples into its own queue from its capture thread. A writer thread
//! drains both in lock step (frames are written only when every enabled source has them, so the
//! channels stay aligned) and pads the shorter queue with silence on stop.
//!
//! A capture that stops on its own (`Capture::lost`: the macOS tap when another app starts an
//! output stream, a device that goes away) never ends the recording (dot-4uzgol.99): a watch
//! thread reopens it every `Timing::restart` until it is back, the writer fills the source with
//! silence meanwhile, and the outage is kept as a `Gap`. Only `stop` (a user, ctl or hook action)
//! ends a recording; the one failure it cannot ride out, the WAV write failing, is `fault`.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::aec::EchoCanceller;
use crate::backend::{Capture, CaptureBackend, DeviceKind, OnSamples};

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
    /// Cleared first on stop, so a callback that runs late queues nothing.
    live: AtomicBool,
    paused: AtomicBool,
    stop: AtomicBool,
    /// RMS of the mic and system samples the writer handled last (`f32` bits), for level meters.
    mic_level: AtomicU32,
    system_level: AtomicU32,
    /// The system source delivered a sample that is not digital silence.
    system_heard: AtomicBool,
    /// The reference-to-mic lag the echo canceller measured, milliseconds.
    echo_lag_ms: Mutex<Option<i64>>,
    /// A source whose capture is lost and not yet reopened: the writer fills it with silence.
    system_down: AtomicBool,
    mic_down: AtomicBool,
    /// Outages that are over, oldest first.
    gaps: Mutex<Vec<Gap>>,
    /// Why the writer stopped writing (the WAV write failed): the recording cannot go on.
    fault: Mutex<Option<String>>,
}

impl Shared {
    fn down(&self, is_sys: bool) -> &AtomicBool {
        if is_sys {
            &self.system_down
        } else {
            &self.mic_down
        }
    }
}

/// A stretch in which one source's capture was lost and the recording had silence for it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Gap {
    /// `system` or `mic`.
    pub source: &'static str,
    /// Where in the recording it began.
    pub at_ms: u64,
    /// How long the source was gone (so far, for one still open).
    pub ms: u64,
    /// Why the capture stopped (`Capture::lost`).
    pub reason: String,
    /// The last error reopening it, if a try failed.
    pub error: Option<String>,
    /// False while the source is still being reopened.
    pub restored: bool,
}

/// One source of the recording: how to open it again, and its capture while it runs.
struct Slot {
    kind: DeviceKind,
    device: String,
    is_sys: bool,
    capture: Option<Box<dyn Capture>>,
    down: Option<Down>,
}

struct Down {
    at_ms: u64,
    since: Instant,
    reason: String,
    error: Option<String>,
    next_try: Instant,
}

impl Slot {
    fn gap(&self, d: &Down, restored: bool) -> Gap {
        Gap {
            source: if self.is_sys { "system" } else { "mic" },
            at_ms: d.at_ms,
            ms: d.since.elapsed().as_millis() as u64,
            reason: d.reason.clone(),
            error: d.error.clone(),
            restored,
        }
    }
}

type Slots = Arc<Mutex<Vec<Slot>>>;

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
    /// The system-audio source the backend opened (`Capture::source`), when it has several.
    pub system_source: Option<&'static str>,
    /// The reference-to-mic lag the echo canceller measured and aligned to (`audio.echo_align`).
    pub echo_lag_ms: Option<i64>,
    /// Why the system channel may be empty (`Recording::warning`), as it stood at the end.
    pub warning: Option<String>,
    /// Every outage of a source (a lost capture the recording rode out), oldest first.
    pub gaps: Vec<Gap>,
}

pub struct Recording {
    shared: Arc<Shared>,
    slots: Slots,
    watch: Option<JoinHandle<()>>,
    writer: Option<JoinHandle<Result<Summary>>>,
    path: PathBuf,
    layout: Layout,
    system_source: Option<&'static str>,
    /// `Capture::silence_hint` of the system capture, and how much digital silence before it shows.
    silence_hint: Option<(String, Duration)>,
}

fn ms(frames: u64) -> u64 {
    frames * 1000 / u64::from(SAMPLE_RATE)
}

/// Pop the frames ready to write: every enabled source's samples in lock step (all of both,
/// padded with silence, when `flush_all`, which is also how a source that is down is filled); a
/// disabled source is silence.
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

fn elog(msg: &str) {
    use std::io::Write;
    let _ = writeln!(std::io::stderr(), "oats-audio: {msg}");
}

pub(crate) fn rms(x: &[f32]) -> f32 {
    (x.iter().map(|v| v * v).sum::<f32>() / x.len().max(1) as f32).sqrt()
}

/// The capture callback of one source: queue its samples while the recording is live. Weak: a
/// backend that never frees its callback (cpal's macOS stream, dot-4uzgol.80) must not keep the
/// queues alive, or fill them, after the recording is gone.
fn feeder(weak: Weak<Shared>, is_sys: bool) -> OnSamples {
    Box::new(move |samples: &[f32]| {
        let Some(sh) = weak.upgrade() else { return };
        if sh.paused.load(Ordering::SeqCst) || !sh.live.load(Ordering::SeqCst) {
            return;
        }
        let mut q = sh.q.lock().expect("queues");
        if is_sys {
            if !sh.system_heard.load(Ordering::Relaxed) && samples.iter().any(|v| *v != 0.0) {
                sh.system_heard.store(true, Ordering::SeqCst);
            }
            q.system.extend(samples);
            sh.system_in
                .fetch_add(samples.len() as u64, Ordering::SeqCst);
        } else {
            q.mic.extend(samples);
            sh.mic_in.fetch_add(samples.len() as u64, Ordering::SeqCst);
        }
    })
}

/// The watch thread: every `poll`, ask each running capture whether it is lost; take a lost one
/// down (the writer fills it with silence) and reopen it every `restart` until it is back. Runs
/// until the recording stops (`live` cleared); never holds the slots over an open.
fn watch(
    weak: Weak<Shared>,
    slots: Slots,
    backend: Arc<dyn CaptureBackend>,
    poll: Duration,
    restart: Duration,
) {
    loop {
        let Some(sh) = weak.upgrade() else { return };
        if !sh.live.load(Ordering::SeqCst) {
            return;
        }
        let n = slots.lock().expect("slots").len();
        for i in 0..n {
            let now = Instant::now();
            let (lost, reopen) = {
                let mut g = slots.lock().expect("slots");
                let s = &mut g[i];
                let mut lost = None;
                if s.down.is_none() {
                    if let Some(why) = s.capture.as_mut().and_then(|c| c.lost()) {
                        sh.down(s.is_sys).store(true, Ordering::SeqCst);
                        elog(&format!(
                            "{} capture lost ({why}); reopening, the recording goes on",
                            if s.is_sys {
                                "system audio"
                            } else {
                                "microphone"
                            }
                        ));
                        s.down = Some(Down {
                            at_ms: ms(sh.frames.load(Ordering::SeqCst)),
                            since: now,
                            reason: why,
                            error: None,
                            next_try: now + restart,
                        });
                        lost = s.capture.take();
                    }
                }
                let due = s.down.as_ref().is_some_and(|d| now >= d.next_try);
                (lost, due.then(|| (s.kind, s.device.clone(), s.is_sys)))
            };
            if let Some(mut c) = lost {
                c.stop();
            }
            let Some((kind, device, is_sys)) = reopen else {
                continue;
            };
            let opened = backend.open(kind, &device, feeder(Weak::clone(&weak), is_sys));
            let mut g = slots.lock().expect("slots");
            let s = &mut g[i];
            if !sh.live.load(Ordering::SeqCst) {
                // Stopped while it opened: `stop_captures` has run, so this one is stopped here.
                if let Ok(mut c) = opened {
                    c.stop();
                }
                return;
            }
            match opened {
                Ok(c) => {
                    if let Some(d) = s.down.take() {
                        let gap = s.gap(&d, true);
                        elog(&format!(
                            "{} capture reopened after {} ms",
                            gap.source, gap.ms
                        ));
                        sh.gaps.lock().expect("gaps").push(gap);
                    }
                    s.capture = Some(c);
                    sh.down(is_sys).store(false, Ordering::SeqCst);
                }
                Err(e) => {
                    if let Some(d) = s.down.as_mut() {
                        d.error = Some(e.to_string());
                        d.next_try = Instant::now() + restart;
                    }
                }
            }
        }
        drop(sh);
        std::thread::sleep(poll);
    }
}

/// Start capturing per `settings` into a WAV at `path` (parent dirs are created).
pub fn start(
    backend: Arc<dyn CaptureBackend>,
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
        system_heard: AtomicBool::new(false),
        echo_lag_ms: Mutex::new(None),
        system_down: AtomicBool::new(false),
        mic_down: AtomicBool::new(false),
        gaps: Mutex::new(vec![]),
        fault: Mutex::new(None),
    });
    let open = |kind: DeviceKind, device: &str, is_sys: bool| -> Result<Slot> {
        let c = backend.open(kind, device, feeder(Arc::downgrade(&shared), is_sys))?;
        Ok(Slot {
            kind,
            device: device.to_string(),
            is_sys,
            capture: Some(c),
            down: None,
        })
    };
    let mut slots: Vec<Slot> = vec![];
    if sys {
        slots.push(
            open(DeviceKind::Loopback, &settings.system_device, true)
                .map_err(|e| AudioError::new(e.code, format!("system audio: {}", e.message)))?,
        );
    }
    if mic {
        // An early return drops `slots`, which stops the system capture already running.
        slots.push(
            open(DeviceKind::Mic, &settings.mic_device, false)
                .map_err(|e| AudioError::new(e.code, format!("microphone: {}", e.message)))?,
        );
    }
    let first = slots
        .first()
        .filter(|_| sys)
        .and_then(|s| s.capture.as_ref());
    let system_source = first.and_then(|c| c.source());
    let silence_hint = first
        .filter(|_| !settings.silence_warning.is_zero())
        .and_then(|c| c.silence_hint())
        .map(|h| (h, settings.silence_warning));
    let slots: Slots = Arc::new(Mutex::new(slots));
    let mut w = match WavWriter::create(path, layout.channels(), SAMPLE_RATE) {
        Ok(w) => w,
        Err(e) => {
            stop_captures(&shared, &slots);
            return Err(e);
        }
    };
    shared.live.store(true, Ordering::SeqCst);
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
        let wrote = write_loop(
            &sh,
            &mut w,
            layout,
            poll,
            sys,
            mic,
            &mut aec,
            &mut tap_sys,
            &mut tap_mic,
        );
        if let Err(e) = wrote {
            *sh.fault.lock().expect("fault") = Some(e.message.clone());
            return Err(e);
        }
        let frames = w.finish()?;
        let echo_lag_ms = *sh.echo_lag_ms.lock().expect("lag");
        Ok(Summary {
            path: out_path,
            frames,
            rate: SAMPLE_RATE,
            channels,
            duration_ms: ms(frames),
            system_samples: sh.system_in.load(Ordering::SeqCst),
            mic_samples: sh.mic_in.load(Ordering::SeqCst),
            system_source,
            echo_lag_ms,
            warning: None,
            gaps: vec![],
        })
    });
    let watch = {
        let (weak, slots, timing) = (Arc::downgrade(&shared), Arc::clone(&slots), settings.timing);
        std::thread::Builder::new()
            .name("oats-rec-watch".into())
            .spawn(move || watch(weak, slots, backend, timing.poll, timing.restart))
            .ok()
    };
    Ok(Recording {
        shared,
        slots,
        watch,
        writer: Some(writer),
        path: path.to_path_buf(),
        layout,
        system_source,
        silence_hint,
    })
}

type Tap = Option<Box<dyn FnMut(&[f32]) + Send>>;

/// The writer's loop: drain the queues in lock step (a source that is down, like a stopping
/// recording, is padded with silence) until `stop`, then flush.
#[allow(clippy::too_many_arguments)]
fn write_loop(
    sh: &Shared,
    w: &mut WavWriter,
    layout: Layout,
    poll: Duration,
    sys: bool,
    mic: bool,
    aec: &mut Option<EchoCanceller>,
    tap_sys: &mut Tap,
    tap_mic: &mut Tap,
) -> Result<()> {
    loop {
        let stopping = sh.stop.load(Ordering::SeqCst);
        let down = sh.system_down.load(Ordering::SeqCst) || sh.mic_down.load(Ordering::SeqCst);
        let (mut m, mut s) = take(
            &mut sh.q.lock().expect("queues"),
            sys,
            mic,
            stopping || down,
        );
        if let Some(c) = aec.as_mut() {
            let (mut mo, mut so) = (Vec::with_capacity(m.len()), Vec::with_capacity(s.len()));
            c.process(&m, &s, &mut mo, &mut so);
            if stopping {
                c.flush(&mut mo, &mut so);
            }
            (m, s) = (mo, so);
            if let Some(l) = c.lag_ms() {
                *sh.echo_lag_ms.lock().expect("lag") = Some(l);
            }
        }
        write_frames(w, &m, &s, layout)?;
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
            return Ok(());
        }
        std::thread::sleep(poll);
    }
}

/// Close the queues to the callbacks, then stop (and drop) every capture: nothing queues after
/// this, and no capture keeps a device open. Outages still open are closed as gaps.
fn stop_captures(shared: &Shared, slots: &Mutex<Vec<Slot>>) {
    shared.live.store(false, Ordering::SeqCst);
    let mut g = slots.lock().expect("slots");
    for s in g.iter_mut() {
        if let Some(mut c) = s.capture.take() {
            c.stop();
        }
        if let Some(d) = s.down.take() {
            shared.gaps.lock().expect("gaps").push(s.gap(&d, false));
        }
    }
    g.clear();
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

    /// The system-audio source the backend opened (`Capture::source`), e.g. `tap`.
    pub fn system_source(&self) -> Option<&'static str> {
        self.system_source
    }

    /// The reference-to-mic lag the echo canceller has measured so far, milliseconds.
    pub fn echo_lag_ms(&self) -> Option<i64> {
        *self.shared.echo_lag_ms.lock().expect("lag")
    }

    /// Every outage so far, oldest first; one still open has `restored: false`.
    pub fn gaps(&self) -> Vec<Gap> {
        let mut v = self.shared.gaps.lock().expect("gaps").clone();
        v.extend(
            self.slots
                .lock()
                .expect("slots")
                .iter()
                .filter_map(|s| s.down.as_ref().map(|d| s.gap(d, false))),
        );
        v
    }

    /// Why the recording can no longer write audio (the WAV write failed), once it cannot.
    pub fn fault(&self) -> Option<String> {
        self.shared.fault.lock().expect("fault").clone()
    }

    /// Why the window shows a banner over the recording: a source that is down and being
    /// reopened (dot-4uzgol.99), else the silence hint below.
    pub fn warning(&self) -> Option<String> {
        let down = self.gaps().into_iter().find(|g| !g.restored);
        if let Some(g) = down {
            let what = if g.source == "system" {
                "system audio"
            } else {
                "the microphone"
            };
            return Some(format!(
                "{what} stopped ({}) and is being reopened: the recording goes on, with silence for it until it is back",
                g.error.unwrap_or(g.reason)
            ));
        }
        self.silence_warning()
    }

    /// Set while the system source has delivered `audio.silence_warning` of nothing but digital
    /// silence and its capture says why that may be (`Capture::silence_hint`: the macOS tap
    /// before System Audio Recording is allowed, dot-4uzgol.98). Clears at its first sound.
    fn silence_warning(&self) -> Option<String> {
        let (hint, after) = self.silence_hint.as_ref()?;
        let silent = self.shared.system_in.load(Ordering::SeqCst);
        let after = after.as_millis() as u64 * u64::from(SAMPLE_RATE) / 1000;
        (silent >= after && !self.shared.system_heard.load(Ordering::SeqCst)).then(|| hint.clone())
    }

    pub fn set_paused(&self, paused: bool) {
        self.shared.paused.store(paused, Ordering::SeqCst);
    }

    /// Stop the watch thread and every capture: nothing reopens or queues after this.
    fn close(&mut self) {
        self.shared.live.store(false, Ordering::SeqCst);
        if let Some(w) = self.watch.take() {
            let _ = w.join();
        }
        stop_captures(&self.shared, &self.slots);
    }

    pub fn stop(mut self) -> Result<Summary> {
        let warning = self.silence_warning();
        self.close();
        self.shared.stop.store(true, Ordering::SeqCst);
        let gaps = self.shared.gaps.lock().expect("gaps").clone();
        let mut s = self
            .writer
            .take()
            .expect("writer")
            .join()
            .map_err(|_| AudioError::new("internal", "writer thread panicked"))??;
        s.warning = warning;
        s.gaps = gaps;
        Ok(s)
    }
}

impl Drop for Recording {
    fn drop(&mut self) {
        // Dropped without `stop` (engine shutdown): finish the file so it stays a valid WAV.
        self.close();
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
        let r = start(Arc::new(backend.clone()), settings, &p, None).unwrap();
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
    fn a_tap_that_delivers_only_silence_warns_and_one_with_sound_does_not() {
        // dot-4uzgol.98: the recording that raised the System Audio Recording prompt got
        // system_samples 0 of sound and no word of why.
        let silent_tap = FakeBackend {
            loopback_hz: 0.0,
            loopback_tap: true,
            ..FakeBackend::default()
        };
        let st = AudioSettings {
            silence_warning: Duration::from_millis(300),
            ..crate::settings::test_settings()
        };
        let d = tempfile::tempdir().unwrap();
        let r = start(
            Arc::new(silent_tap.clone()),
            &st,
            &d.path().join("a.wav"),
            None,
        )
        .unwrap();
        assert_eq!(r.system_source(), Some("tap"));
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        while r.warning().is_none() {
            assert!(std::time::Instant::now() < deadline, "no warning");
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(r.warning().as_deref(), Some(crate::fake::TAP_HINT));
        let s = r.stop().unwrap();
        assert_eq!(s.system_source, Some("tap"));
        assert_eq!(s.warning.as_deref(), Some(crate::fake::TAP_HINT));
        assert!(s.system_samples > 0);

        // The same tap with sound, a silent source without a hint, the warning off: no warning.
        let sounding = FakeBackend {
            loopback_tap: true,
            ..FakeBackend::default()
        };
        let silent = FakeBackend {
            loopback_hz: 0.0,
            ..FakeBackend::default()
        };
        let off = AudioSettings {
            silence_warning: Duration::ZERO,
            ..st.clone()
        };
        for (b, st) in [(&sounding, &st), (&silent, &st), (&silent_tap, &off)] {
            let r = start(Arc::new(b.clone()), st, &d.path().join("b.wav"), None).unwrap();
            while r.duration_ms() < 600 {
                std::thread::sleep(Duration::from_millis(10));
            }
            assert_eq!(r.warning(), None);
            assert_eq!(r.stop().unwrap().warning, None);
        }
    }

    fn wait_for(what: &str, mut ok: impl FnMut() -> bool) {
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        while !ok() {
            assert!(std::time::Instant::now() < deadline, "timed out: {what}");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn a_lost_capture_is_reopened_and_the_same_recording_goes_on() {
        // dot-4uzgol.99: the macOS tap's IO stopped when another app started an output stream,
        // and the recording ended. Now the source is reopened, with silence for the gap.
        let b = FakeBackend::default();
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("a.wav");
        let r = start(
            Arc::new(b.clone()),
            &crate::settings::test_settings(),
            &p,
            None,
        )
        .unwrap();
        wait_for("first audio", || r.duration_ms() >= 300);
        // Reopening fails twice (the device is still settling), then works.
        b.faults.fail_opens.store(2, Ordering::SeqCst);
        b.faults.lose(DeviceKind::Loopback);
        wait_for("the banner", || r.warning().is_some());
        let w = r.warning().unwrap();
        assert!(w.starts_with("system audio stopped"), "{w}");
        // The mic keeps the recording going while the system source is down.
        let at = r.duration_ms();
        wait_for("audio during the outage", || r.duration_ms() >= at + 100);
        wait_for("the source back", || {
            r.gaps().first().is_some_and(|g| g.restored)
        });
        assert_eq!(r.warning(), None, "the banner goes with the outage");
        let back = r.duration_ms();
        wait_for("audio after", || r.duration_ms() >= back + 600);
        let s = r.stop().unwrap();
        assert_eq!(s.gaps.len(), 1, "{:?}", s.gaps);
        let g = &s.gaps[0];
        assert_eq!(
            (g.source, g.reason.as_str(), g.restored),
            ("system", crate::fake::LOST, true)
        );
        assert!(
            g.error
                .as_deref()
                .is_some_and(|e| e.contains("open failed")),
            "{g:?}"
        );
        assert!(g.at_ms >= 300 && g.ms > 0, "{g:?}");
        assert_eq!(b.faults.opened(DeviceKind::Loopback), 2);
        assert_eq!(b.faults.opened(DeviceKind::Mic), 1);
        // One file: the system tone is back in its last half second, the mic never left.
        let data = wav::read(&p).unwrap();
        assert_eq!(data.samples[0].len() as u64, s.frames);
        let tail = |ch: usize| {
            let x = &data.samples[ch];
            rms(&x[x.len() - SAMPLE_RATE as usize / 2..])
        };
        assert!(tail(1) > 0.3 && tail(0) > 0.1, "{} {}", tail(1), tail(0));
    }

    #[test]
    fn a_lost_mic_is_reopened_too_and_a_stop_while_down_closes_the_gap() {
        let b = FakeBackend::default();
        let d = tempfile::tempdir().unwrap();
        let r = start(
            Arc::new(b.clone()),
            &crate::settings::test_settings(),
            &d.path().join("a.wav"),
            None,
        )
        .unwrap();
        wait_for("first audio", || r.duration_ms() >= 200);
        b.faults.lose(DeviceKind::Mic);
        wait_for("mic back", || r.gaps().first().is_some_and(|g| g.restored));
        assert_eq!(r.gaps()[0].source, "mic");
        // Now every reopen fails: the outage is still open when the user stops.
        b.faults.fail_opens.store(usize::MAX, Ordering::SeqCst);
        b.faults.lose(DeviceKind::Loopback);
        wait_for("system down", || r.gaps().len() == 2);
        let at = r.duration_ms();
        wait_for("audio while down", || r.duration_ms() >= at + 200);
        let s = r.stop().unwrap();
        assert_eq!(s.gaps.len(), 2);
        assert!(s.gaps[0].restored && !s.gaps[1].restored, "{:?}", s.gaps);
        assert_eq!(s.gaps[1].source, "system");
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
            start(Arc::new(FakeBackend::default()), &st, &p, None)
                .err()
                .unwrap()
                .code,
            "invalid_config"
        );
        let st = AudioSettings {
            mic_device: "nope".into(),
            ..crate::settings::test_settings()
        };
        let e = start(Arc::new(FakeBackend::default()), &st, &p, None)
            .err()
            .unwrap();
        assert_eq!(e.code, "device_not_found");
        assert!(e.message.starts_with("microphone:"));
    }

    /// A backend whose streams are never freed (cpal's macOS stream, dot-4uzgol.80): it keeps
    /// every callback forever, and the test plays audio through them after stop.
    #[derive(Default, Clone)]
    struct Leaky {
        kept: Arc<Mutex<Vec<crate::backend::OnSamples>>>,
        opened: Arc<std::sync::atomic::AtomicUsize>,
        released: Arc<std::sync::atomic::AtomicUsize>,
    }

    struct LeakyCapture {
        released: Arc<std::sync::atomic::AtomicUsize>,
        done: bool,
    }

    impl Capture for LeakyCapture {
        fn stop(&mut self) {
            if !std::mem::replace(&mut self.done, true) {
                self.released.fetch_add(1, Ordering::SeqCst);
            }
        }
    }

    impl Drop for LeakyCapture {
        fn drop(&mut self) {
            self.stop();
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
            device: &str,
            on: crate::backend::OnSamples,
        ) -> Result<Box<dyn Capture>> {
            if device == "nope" {
                return Err(AudioError::new("device_not_found", "nope"));
            }
            self.kept.lock().unwrap().push(on);
            self.opened.fetch_add(1, Ordering::SeqCst);
            Ok(Box::new(LeakyCapture {
                released: Arc::clone(&self.released),
                done: false,
            }))
        }
    }

    impl Leaky {
        fn play(&self) {
            for cb in self.kept.lock().unwrap().iter_mut() {
                cb(&[0.25; 480]);
            }
        }

        fn all_released(&self) -> bool {
            self.opened.load(Ordering::SeqCst) == self.released.load(Ordering::SeqCst)
        }
    }

    /// Record from `Leaky`, run `between` on the live recording, stop, then keep playing through
    /// the leaked callbacks: the queues must be freed and every capture released.
    fn leaky_session(between: impl Fn(&Recording, &Leaky)) {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("a.wav");
        let b = Leaky::default();
        let r = start(
            Arc::new(b.clone()),
            &crate::settings::test_settings(),
            &p,
            None,
        )
        .unwrap();
        b.play();
        between(&r, &b);
        let shared = Arc::downgrade(&r.shared);
        r.stop().unwrap();
        assert!(
            shared.upgrade().is_none(),
            "the callbacks keep the queues alive"
        );
        assert!(b.all_released(), "a capture was not stopped");
        b.play();
        assert!(shared.upgrade().is_none());
    }

    #[test]
    fn stop_releases_the_queues_even_when_the_backend_leaks_its_callbacks() {
        leaky_session(|_, _| {});
    }

    #[test]
    fn stop_while_paused_or_after_resume_releases_the_queues() {
        leaky_session(|r, b| {
            r.set_paused(true);
            b.play();
        });
        leaky_session(|r, b| {
            r.set_paused(true);
            b.play();
            r.set_paused(false);
            b.play();
        });
    }

    #[test]
    fn dropping_a_recording_releases_its_captures() {
        let d = tempfile::tempdir().unwrap();
        let b = Leaky::default();
        let r = start(
            Arc::new(b.clone()),
            &crate::settings::test_settings(),
            &d.path().join("a.wav"),
            None,
        );
        let shared = Arc::downgrade(&r.as_ref().unwrap().shared);
        drop(r);
        assert!(shared.upgrade().is_none() && b.all_released());
        b.play();
    }

    #[test]
    fn a_failed_start_releases_the_captures_it_opened() {
        let d = tempfile::tempdir().unwrap();
        let b = Leaky::default();
        let st = AudioSettings {
            mic_device: "nope".into(),
            ..crate::settings::test_settings()
        };
        assert!(start(Arc::new(b.clone()), &st, &d.path().join("a.wav"), None).is_err());
        assert_eq!(b.opened.load(Ordering::SeqCst), 1, "system opened first");
        assert!(b.all_released());
        b.play();
        // The WAV cannot be created (its path is a directory): both opened captures are stopped.
        let b = Leaky::default();
        assert!(start(
            Arc::new(b.clone()),
            &crate::settings::test_settings(),
            d.path(),
            None
        )
        .is_err());
        assert_eq!(b.opened.load(Ordering::SeqCst), 2);
        assert!(b.all_released());
        b.play();
    }

    #[test]
    fn pause_drops_audio_and_duration_tracks_writes() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("a.wav");
        let r = start(
            Arc::new(FakeBackend::default()),
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
