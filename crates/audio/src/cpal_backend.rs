//! cpal: WASAPI on Windows (a loopback is an output device opened as an input stream, which
//! cpal's WASAPI host turns into a loopback capture; the mic is an input device). Windows only:
//! on Linux cpal means ALSA, and linking nix's libasound into the binary pulls a second glibc into
//! the pod build (symbol lookup error at run time), while the pulse and pipewire backends cover Linux.
//!
//! The cpal stream is not `Send`, so it lives on its own thread until stopped.

#[cfg(any(windows, target_os = "macos"))]
mod imp {
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
    use cpal::{FromSample, Sample, SampleFormat, SizedSample, StreamConfig};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{mpsc, Arc, Mutex};
    use std::thread::JoinHandle;

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
        on_samples: Arc<Mutex<OnSamples>>,
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
                    if let Ok(mut cb) = on_samples.lock() {
                        cb(&out);
                    }
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
            let cb = Arc::new(Mutex::new(on_samples));
            let th = std::thread::spawn(move || {
                let started = (|| -> Result<cpal::Stream> {
                    let host = cpal::default_host();
                    let found = if is_output {
                        host.output_devices()
                            .map_err(|e| err("backend_unavailable", "cpal outputs", e))?
                            .find(|d| d.name().ok().as_deref() == Some(name.as_str()))
                    } else if cfg!(target_os = "macos") {
                        // `devices()`, not `input_devices()`: the latter probes every device.
                        host.devices()
                            .map_err(|e| err("backend_unavailable", "cpal devices", e))?
                            .find(|d| d.name().ok().as_deref() == Some(name.as_str()))
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
                    stream.play().map_err(|e| err("capture_failed", &name, e))?;
                    Ok(stream)
                })();
                match started {
                    Ok(stream) => {
                        let _ = tx.send(Ok(()));
                        while !flag.load(Ordering::SeqCst) {
                            std::thread::sleep(timing.poll);
                        }
                        drop(stream);
                    }
                    Err(e) => {
                        let _ = tx.send(Err(e));
                    }
                }
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
