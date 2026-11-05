//! cpal: WASAPI on Windows (a loopback is an output device opened as an input stream, which
//! cpal's WASAPI host turns into a loopback capture; the mic is an input device). Windows only:
//! on Linux cpal means ALSA, and linking nix's libasound into the binary pulls a second glibc into
//! the pod build (symbol lookup error at run time), while the pulse and pipewire backends cover Linux.
//!
//! The cpal stream is not `Send`, so it lives on its own thread until stopped.
//!
//! Dropping a cpal 0.15 macOS stream does not stop it when cpal registered a disconnect listener
//! (any device not opened as the default): the listener holds a clone of the stream, so the
//! AudioUnit outlives every handle and its input callback keeps running with the mic open
//! (dot-4uzgol.80). Stopping therefore pauses the stream (`AudioOutputUnitStop`) and empties the
//! `Slot` the callback calls through, so whatever the stream leaks no longer reaches the recorder.

use std::sync::Mutex;

use crate::backend::OnSamples;

/// The callback a stream calls through. `release` drops it (and everything it holds) even when
/// the stream that shares the slot is never freed; later calls are no-ops.
#[cfg_attr(not(any(windows, target_os = "macos")), allow(dead_code))]
pub(crate) struct Slot(Mutex<Option<OnSamples>>);

#[cfg_attr(not(any(windows, target_os = "macos")), allow(dead_code))]
impl Slot {
    pub(crate) fn new(on_samples: OnSamples) -> Self {
        Self(Mutex::new(Some(on_samples)))
    }

    pub(crate) fn call(&self, samples: &[f32]) {
        if let Ok(mut g) = self.0.lock() {
            if let Some(cb) = g.as_mut() {
                cb(samples);
            }
        }
    }

    pub(crate) fn release(&self) {
        let cb = self.0.lock().map(|mut g| g.take());
        drop(cb);
    }
}

#[cfg(any(windows, target_os = "macos"))]
mod imp {
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
    use cpal::{FromSample, Sample, SampleFormat, SizedSample, StreamConfig};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{mpsc, Arc};
    use std::thread::JoinHandle;

    use super::Slot;
    use crate::backend::{Capture, CaptureBackend, DeviceInfo, DeviceKind, OnSamples};
    use crate::error::{AudioError, Result};
    use crate::resample::{downmix, Resampler};
    use crate::settings::Timing;
    use crate::SAMPLE_RATE;

    pub struct CpalBackend {
        timing: Timing,
    }

    impl CpalBackend {
        pub fn new(timing: Timing) -> Self {
            Self { timing }
        }
    }

    const IN: &str = "in:";
    const OUT: &str = "out:";

    fn is_loopback_name(n: &str) -> bool {
        let l = n.to_ascii_lowercase();
        l.contains("monitor") || l.contains("loopback")
    }

    fn err(code: &'static str, what: &str, e: impl std::fmt::Display) -> AudioError {
        AudioError::new(code, format!("{what}: {e}"))
    }

    /// (id, display name, kind, default, is_output_device)
    fn enumerate() -> Result<Vec<(DeviceInfo, bool)>> {
        let host = cpal::default_host();
        let mut v = vec![];
        let default_in = host.default_input_device().and_then(|d| d.name().ok());
        for d in host
            .input_devices()
            .map_err(|e| err("backend_unavailable", "cpal inputs", e))?
        {
            let Ok(name) = d.name() else { continue };
            let kind = if is_loopback_name(&name) {
                DeviceKind::Loopback
            } else {
                DeviceKind::Mic
            };
            let default = kind == DeviceKind::Mic && default_in.as_deref() == Some(name.as_str());
            v.push((
                DeviceInfo {
                    id: format!("{IN}{name}"),
                    name,
                    kind,
                    backend: "cpal",
                    default,
                },
                false,
            ));
        }
        if cfg!(windows) {
            let default_out = host.default_output_device().and_then(|d| d.name().ok());
            for d in host
                .output_devices()
                .map_err(|e| err("backend_unavailable", "cpal outputs", e))?
            {
                let Ok(name) = d.name() else { continue };
                let default = default_out.as_deref() == Some(name.as_str());
                v.push((
                    DeviceInfo {
                        id: format!("{OUT}{name}"),
                        name,
                        kind: DeviceKind::Loopback,
                        backend: "cpal",
                        default,
                    },
                    true,
                ));
            }
        }
        Ok(v)
    }

    struct CpalCapture {
        stop: Arc<AtomicBool>,
        th: Option<JoinHandle<()>>,
    }

    impl Capture for CpalCapture {
        fn stop(&mut self) {
            self.stop.store(true, Ordering::SeqCst);
            if let Some(t) = self.th.take() {
                let _ = t.join();
            }
        }
    }

    impl Drop for CpalCapture {
        fn drop(&mut self) {
            self.stop();
        }
    }

    fn build<T>(
        device: &cpal::Device,
        cfg: &StreamConfig,
        on_samples: Arc<Slot>,
    ) -> std::result::Result<cpal::Stream, cpal::BuildStreamError>
    where
        T: SizedSample,
        f32: FromSample<T>,
    {
        let channels = usize::from(cfg.channels);
        let mut rs = Resampler::new(cfg.sample_rate.0, SAMPLE_RATE);
        let (mut f, mut mono, mut out) = (Vec::<f32>::new(), Vec::<f32>::new(), Vec::<f32>::new());
        device.build_input_stream(
            cfg,
            move |data: &[T], _| {
                f.clear();
                f.extend(data.iter().map(|s| f32::from_sample(*s)));
                mono.clear();
                downmix(&f, channels, &mut mono);
                out.clear();
                rs.push(&mono, &mut out);
                if !out.is_empty() {
                    on_samples.call(&out);
                }
            },
            |e| {
                use std::io::Write;
                let _ = writeln!(std::io::stderr(), "oats-audio: cpal stream error: {e}");
            },
            None,
        )
    }

    impl CaptureBackend for CpalBackend {
        fn name(&self) -> &'static str {
            "cpal"
        }

        fn list_devices(&self) -> Result<Vec<DeviceInfo>> {
            Ok(enumerate()?.into_iter().map(|(d, _)| d).collect())
        }

        fn open(
            &self,
            kind: DeviceKind,
            device: &str,
            on_samples: OnSamples,
        ) -> Result<Box<dyn Capture>> {
            let all = enumerate()?;
            let pick = if device.is_empty() {
                all.iter().find(|(d, _)| d.kind == kind && d.default)
            } else {
                all.iter().find(|(d, _)| d.id == device || d.name == device)
            };
            let Some((info, is_output)) = pick else {
                return Err(AudioError::new(
                    "device_not_found",
                    format!("no cpal device for {kind:?} '{device}' (see `ctl devices list`)"),
                ));
            };
            self.spawn(info.name.clone(), *is_output, on_samples)
        }
    }

    impl CpalBackend {
        /// Open the input device `name` without enumerating first: the lookup and the config
        /// probe both run inside the bounded open. macOS lists devices from the HAL
        /// (`coreaudio_hal`), since cpal's enumeration creates an AudioUnit per device and hangs
        /// without a Microphone grant; here, at record start, the TCC prompt is expected.
        #[cfg(target_os = "macos")]
        pub fn open_input_named(
            &self,
            name: &str,
            on_samples: OnSamples,
        ) -> Result<Box<dyn Capture>> {
            self.spawn(name.to_string(), false, on_samples)
        }

        fn spawn(
            &self,
            name: String,
            is_output: bool,
            on_samples: OnSamples,
        ) -> Result<Box<dyn Capture>> {
            let stop = Arc::new(AtomicBool::new(false));
            let flag = Arc::clone(&stop);
            let timing = self.timing;
            let (tx, rx) = mpsc::channel::<Result<()>>();
            let cb = Arc::new(Slot::new(on_samples));
            let slot = Arc::clone(&cb);
            let th = std::thread::spawn(move || {
                let started = (|| -> Result<cpal::Stream> {
                    let host = cpal::default_host();
                    let found = if is_output {
                        host.output_devices()
                            .map_err(|e| err("backend_unavailable", "cpal outputs", e))?
                            .find(|d| d.name().ok().as_deref() == Some(name.as_str()))
                    } else if cfg!(target_os = "macos") {
                        // The default device as cpal's default: cpal adds no disconnect listener
                        // to it, so dropping the stream frees its AudioUnit.
                        let default = host
                            .default_input_device()
                            .filter(|d| d.name().ok().as_deref() == Some(name.as_str()));
                        match default {
                            Some(d) => Some(d),
                            // `devices()`, not `input_devices()`: the latter probes every device.
                            None => host
                                .devices()
                                .map_err(|e| err("backend_unavailable", "cpal devices", e))?
                                .find(|d| d.name().ok().as_deref() == Some(name.as_str())),
                        }
                    } else {
                        host.input_devices()
                            .map_err(|e| err("backend_unavailable", "cpal inputs", e))?
                            .find(|d| d.name().ok().as_deref() == Some(name.as_str()))
                    };
                    let dev = found.ok_or_else(|| {
                        AudioError::new(
                            "device_not_found",
                            format!("cpal device '{name}' vanished"),
                        )
                    })?;
                    let supported = (if is_output {
                        dev.default_output_config()
                    } else {
                        dev.default_input_config()
                    })
                    .map_err(|e| err("capture_failed", &name, e))?;
                    let fmt = supported.sample_format();
                    let cfg: StreamConfig = supported.into();
                    let stream = match fmt {
                        SampleFormat::F32 => build::<f32>(&dev, &cfg, cb),
                        SampleFormat::I16 => build::<i16>(&dev, &cfg, cb),
                        SampleFormat::U16 => build::<u16>(&dev, &cfg, cb),
                        SampleFormat::I32 => build::<i32>(&dev, &cfg, cb),
                        other => {
                            return Err(AudioError::new(
                                "capture_failed",
                                format!("{name}: unsupported sample format {other:?}"),
                            ))
                        }
                    }
                    .map_err(|e| err("capture_failed", &name, e))?;
                    if let Err(e) = stream.play() {
                        let _ = stream.pause();
                        return Err(err("capture_failed", &name, e));
                    }
                    Ok(stream)
                })();
                match started {
                    Ok(stream) => {
                        let _ = tx.send(Ok(()));
                        while !flag.load(Ordering::SeqCst) {
                            std::thread::sleep(timing.poll);
                        }
                        // Pause first: dropping alone may leave the AudioUnit running (see the
                        // module doc).
                        let _ = stream.pause();
                        drop(stream);
                    }
                    Err(e) => {
                        let _ = tx.send(Err(e));
                    }
                }
                // Started, failed or timed out: nothing the stream leaked may call back.
                slot.release();
            });
            match rx.recv_timeout(timing.open_timeout) {
                Ok(Ok(())) => Ok(Box::new(CpalCapture { stop, th: Some(th) })),
                Ok(Err(e)) => {
                    let _ = th.join();
                    Err(e)
                }
                Err(_) => {
                    stop.store(true, Ordering::SeqCst);
                    Err(AudioError::new(
                        "capture_failed",
                        format!("cpal stream did not start within {:?}", timing.open_timeout),
                    ))
                }
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn loopback_names() {
            assert!(is_loopback_name("Monitor of Null Output"));
            assert!(is_loopback_name("Loopback: PCM"));
            assert!(!is_loopback_name("default"));
        }
    }
}

#[cfg(any(windows, target_os = "macos"))]
pub use imp::CpalBackend;

#[cfg(test)]
mod slot_tests {
    use super::Slot;
    use std::sync::Arc;

    #[test]
    fn release_frees_the_callback_of_a_stream_that_is_never_dropped() {
        let held = Arc::new(());
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let (h, n) = (Arc::clone(&held), Arc::clone(&calls));
        let slot = Arc::new(Slot::new(Box::new(move |_: &[f32]| {
            let _ = &h;
            n.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        })));
        // The stream's copy, kept alive forever as cpal's disconnect listener keeps it.
        let leaked = Arc::clone(&slot);
        leaked.call(&[0.0]);
        slot.release();
        assert_eq!(Arc::strong_count(&held), 1, "the callback outlived release");
        leaked.call(&[0.0]);
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        slot.release();
        std::mem::forget(leaked);
    }
}
