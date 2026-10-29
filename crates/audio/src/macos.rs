//! macOS capture (dot-4uzgol.6): system audio through ScreenCaptureKit, microphone through cpal
//! (CoreAudio).
//!
//! Why ScreenCaptureKit and not a CoreAudio process tap: SCK has a stable public API on macOS 13+
//! (a process tap needs 14.2+), takes one `SCContentFilter` over a display with no per-process
//! aggregate-device plumbing, hands back 48 kHz PCM, and its TCC consent is the Screen Recording
//! permission the user already understands. A tap would avoid the Screen Recording grant, but it
//! is a newer, thinner surface; the `permission_denied` error names the exact pane either way.
//!
//! Everything that decides behaviour is pure and compiles on every OS so the pods test it: the
//! permission gate (`Permissions`), the error classification, the PCM decode, and the backend
//! that joins them over a `Sources` trait. Only the real `Sources` and `Permissions` (SCK,
//! cpal, AVFoundation) are macOS-only.

use std::sync::Arc;

use crate::backend::{Capture, CaptureBackend, DeviceInfo, DeviceKind, OnSamples};
use crate::error::{AudioError, Result};

pub const NAME: &str = "screencapturekit";
/// The only system-audio device: everything the default display session plays.
pub const SYSTEM_DEVICE: &str = "screencapturekit:system";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Permission {
    Microphone,
    ScreenRecording,
}

impl Permission {
    fn pane(self) -> &'static str {
        match self {
            Self::Microphone => "Privacy & Security > Microphone",
            Self::ScreenRecording => "Privacy & Security > Screen & System Audio Recording",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionState {
    Granted,
    Denied,
    /// Never asked. Opening the capture is what makes macOS ask.
    NotDetermined,
}

/// Reads the TCC state without prompting. A fake in tests; `SystemPermissions` on macOS.
pub trait Permissions: Send + Sync {
    fn state(&self, which: Permission) -> PermissionState;
}

pub fn permission_denied(which: Permission) -> AudioError {
    AudioError::new(
        "permission_denied",
        format!(
            "macOS denied {which:?} access to this app; allow it in System Settings > {}",
            which.pane()
        ),
    )
}

/// `Denied` is an error; `NotDetermined` passes so the open triggers the system prompt.
pub fn require(perms: &dyn Permissions, which: Permission) -> Result<()> {
    match perms.state(which) {
        PermissionState::Denied => Err(permission_denied(which)),
        PermissionState::Granted | PermissionState::NotDetermined => Ok(()),
    }
}

/// Map a ScreenCaptureKit failure to a structured error. `code` is the `SCStreamErrorCode`
/// (-3801 user declined, -3803 missing entitlements); `message` catches the TCC wording that
/// arrives without a code.
pub fn classify_sck_failure(code: Option<i32>, message: &str) -> AudioError {
    let m = message.to_ascii_lowercase();
    let denied = matches!(code, Some(-3801 | -3803))
        || m.contains("declined")
        || m.contains("not authorized")
        || m.contains("tcc")
        || m.contains("permission");
    if denied {
        AudioError::new(
            "permission_denied",
            format!(
                "ScreenCaptureKit: {message}; allow {}",
                Permission::ScreenRecording.pane()
            ),
        )
    } else {
        AudioError::new("capture_failed", format!("ScreenCaptureKit: {message}"))
    }
}

/// How a ScreenCaptureKit buffer encodes samples.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PcmFormat {
    F32,
    I16,
}

/// Mono f32 from an audio buffer list: `bufs` are `(channels in this buffer, bytes)`. SCK delivers
/// planar audio (one mono buffer per channel) and interleaved audio (one buffer, n channels);
/// both average down to mono. Short or ragged buffers are cut to the shortest.
pub fn decode_mono(bufs: &[(u32, &[u8])], fmt: PcmFormat) -> Vec<f32> {
    let planes: Vec<(usize, Vec<f32>)> = bufs
        .iter()
        .map(|(ch, bytes)| {
            let samples: Vec<f32> = match fmt {
                PcmFormat::F32 => bytes
                    .chunks_exact(4)
                    .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
                    .collect(),
                PcmFormat::I16 => bytes
                    .chunks_exact(2)
                    .map(|b| f32::from(i16::from_le_bytes([b[0], b[1]])) / 32768.0)
                    .collect(),
            };
            ((*ch).max(1) as usize, samples)
        })
        .collect();
    let Some(frames) = planes.iter().map(|(ch, s)| s.len() / ch).min() else {
        return vec![];
    };
    let channels: usize = planes.iter().map(|(ch, _)| ch).sum();
    (0..frames)
        .map(|i| {
            let sum: f32 = planes
                .iter()
                .map(|(ch, s)| s[i * ch..(i + 1) * ch].iter().sum::<f32>())
                .sum();
            sum / channels as f32
        })
        .collect()
}

/// The real capture calls, behind a trait so the gate and the device logic test without a Mac.
pub trait Sources: Send + Sync {
    fn list_mics(&self) -> Result<Vec<DeviceInfo>>;
    fn open_system(&self, on_samples: OnSamples) -> Result<Box<dyn Capture>>;
    fn open_mic(&self, device: &str, on_samples: OnSamples) -> Result<Box<dyn Capture>>;
}

pub struct MacosBackend {
    perms: Arc<dyn Permissions>,
    sources: Arc<dyn Sources>,
}

impl MacosBackend {
    pub fn with(perms: Arc<dyn Permissions>, sources: Arc<dyn Sources>) -> Self {
        Self { perms, sources }
    }
}

impl CaptureBackend for MacosBackend {
    fn name(&self) -> &'static str {
        NAME
    }

    fn list_devices(&self) -> Result<Vec<DeviceInfo>> {
        let mut v = vec![DeviceInfo {
            id: SYSTEM_DEVICE.into(),
            name: "System audio (ScreenCaptureKit)".into(),
            kind: DeviceKind::Loopback,
            backend: NAME,
            default: true,
        }];
        v.extend(self.sources.list_mics()?);
        Ok(v)
    }

    fn open(
        &self,
        kind: DeviceKind,
        device: &str,
        on_samples: OnSamples,
    ) -> Result<Box<dyn Capture>> {
        match kind {
            DeviceKind::Loopback => {
                if !device.is_empty() && device != SYSTEM_DEVICE {
                    return Err(AudioError::new(
                        "device_not_found",
                        format!(
                            "no system-audio device '{device}'; macOS has only '{SYSTEM_DEVICE}'"
                        ),
                    ));
                }
                require(self.perms.as_ref(), Permission::ScreenRecording)?;
                self.sources.open_system(on_samples)
            }
            DeviceKind::Mic => {
                require(self.perms.as_ref(), Permission::Microphone)?;
                self.sources.open_mic(device, on_samples)
            }
        }
    }
}

#[cfg(target_os = "macos")]
pub use native::native;

#[cfg(target_os = "macos")]
mod native {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{mpsc, Arc, Mutex};
    use std::thread::JoinHandle;

    use screencapturekit::cm::{CMSampleBuffer, SCFrameStatus};
    use screencapturekit::prelude::*;

    use super::*;
    use crate::cpal_backend::CpalBackend;
    use crate::resample::{downmix, Resampler};
    use crate::settings::Timing;
    use crate::SAMPLE_RATE;

    /// The real backend: SCK system audio, cpal mic, TCC state from the system.
    pub fn native(timing: Timing) -> MacosBackend {
        MacosBackend::with(
            Arc::new(SystemPermissions),
            Arc::new(NativeSources { timing }),
        )
    }

    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        fn CGPreflightScreenCaptureAccess() -> bool;
    }

    #[link(name = "AVFoundation", kind = "framework")]
    extern "C" {}

    /// Reads TCC without prompting: `CGPreflightScreenCaptureAccess` never asks, and
    /// `+[AVCaptureDevice authorizationStatusForMediaType:]` only reports.
    pub struct SystemPermissions;

    impl Permissions for SystemPermissions {
        fn state(&self, which: Permission) -> PermissionState {
            match which {
                // Preflight is a plain yes/no: false means denied or never asked. Treat it as
                // NotDetermined so the open still happens; a real denial then arrives as an SCK
                // error that `classify_sck_failure` maps to `permission_denied`.
                Permission::ScreenRecording => {
                    if unsafe { CGPreflightScreenCaptureAccess() } {
                        PermissionState::Granted
                    } else {
                        PermissionState::NotDetermined
                    }
                }
                Permission::Microphone => av_mic_state(),
            }
        }
    }

    fn av_mic_state() -> PermissionState {
        use objc2::msg_send;
        use objc2::runtime::AnyClass;
        use objc2_foundation::NSString;
        let Some(cls) = AnyClass::get(c"AVCaptureDevice") else {
            return PermissionState::NotDetermined;
        };
        let media = NSString::from_str("soun"); // AVMediaTypeAudio
                                                // AVAuthorizationStatus: 0 notDetermined, 1 restricted, 2 denied, 3 authorized.
        let status: isize = unsafe { msg_send![cls, authorizationStatusForMediaType: &*media] };
        match status {
            3 => PermissionState::Granted,
            1 | 2 => PermissionState::Denied,
            _ => PermissionState::NotDetermined,
        }
    }

    struct NativeSources {
        timing: Timing,
    }

    impl Sources for NativeSources {
        fn list_mics(&self) -> Result<Vec<DeviceInfo>> {
            let all = CpalBackend::new(self.timing).list_devices()?;
            Ok(all
                .into_iter()
                .filter(|d| d.kind == DeviceKind::Mic)
                .map(|mut d| {
                    d.backend = NAME;
                    d
                })
                .collect())
        }

        fn open_mic(&self, device: &str, on_samples: OnSamples) -> Result<Box<dyn Capture>> {
            CpalBackend::new(self.timing).open(DeviceKind::Mic, device, on_samples)
        }

        fn open_system(&self, on_samples: OnSamples) -> Result<Box<dyn Capture>> {
            let stop = Arc::new(AtomicBool::new(false));
            let flag = Arc::clone(&stop);
            let timing = self.timing;
            let (tx, rx) = mpsc::channel::<Result<()>>();
            let th = std::thread::spawn(move || {
                let started = start_stream(on_samples);
                match started {
                    Ok(stream) => {
                        let _ = tx.send(Ok(()));
                        while !flag.load(Ordering::SeqCst) {
                            std::thread::sleep(timing.poll);
                        }
                        let _ = stream.stop_capture();
                    }
                    Err(e) => {
                        let _ = tx.send(Err(e));
                    }
                }
            });
            match rx.recv_timeout(timing.open_timeout) {
                Ok(Ok(())) => Ok(Box::new(SckCapture { stop, th: Some(th) })),
                Ok(Err(e)) => {
                    let _ = th.join();
                    Err(e)
                }
                Err(_) => {
                    stop.store(true, Ordering::SeqCst);
                    Err(AudioError::new(
                        "capture_failed",
                        format!(
                            "ScreenCaptureKit did not start within {:?}",
                            timing.open_timeout
                        ),
                    ))
                }
            }
        }
    }

    struct SckCapture {
        stop: Arc<AtomicBool>,
        th: Option<JoinHandle<()>>,
    }

    impl Capture for SckCapture {
        fn stop(&mut self) {
            self.stop.store(true, Ordering::SeqCst);
            if let Some(t) = self.th.take() {
                let _ = t.join();
            }
        }
    }

    impl Drop for SckCapture {
        fn drop(&mut self) {
            self.stop();
        }
    }

    fn sck_err(e: &SCError) -> AudioError {
        classify_sck_failure(e.stream_error_code().map(|c| c as i32), &e.to_string())
    }

    struct AudioOut {
        on_samples: OnSamples,
        resampler: Option<(u32, Resampler)>,
        mono: Vec<f32>,
        out: Vec<f32>,
    }

    struct Handler {
        state: Mutex<AudioOut>,
    }

    impl SCStreamOutputTrait for Handler {
        fn did_output_sample_buffer(&self, sample: CMSampleBuffer, ty: SCStreamOutputType) {
            if !matches!(ty, SCStreamOutputType::Audio) {
                return;
            }
            let Ok(list) = sample.audio_buffer_list() else {
                return;
            };
            let desc = sample.format_description();
            let rate = desc
                .as_ref()
                .and_then(|d| d.audio_sample_rate())
                .unwrap_or(f64::from(SAMPLE_RATE)) as u32;
            let fmt = match desc.as_ref() {
                Some(d) if !d.audio_is_float() && d.audio_bits_per_channel() == Some(16) => {
                    PcmFormat::I16
                }
                _ => PcmFormat::F32,
            };
            let bufs: Vec<(u32, &[u8])> =
                list.iter().map(|b| (b.number_channels, b.data())).collect();
            let mono = decode_mono(&bufs, fmt);
            let Ok(mut st) = self.state.lock() else {
                return;
            };
            let st = &mut *st;
            if st.resampler.as_ref().map(|(r, _)| *r) != Some(rate) {
                st.resampler = Some((rate, Resampler::new(rate, SAMPLE_RATE)));
            }
            st.out.clear();
            if let Some((_, rs)) = st.resampler.as_mut() {
                st.mono.clear();
                downmix(&mono, 1, &mut st.mono);
                rs.push(&st.mono, &mut st.out);
            }
            if !st.out.is_empty() {
                (st.on_samples)(&st.out);
            }
        }
    }

    /// Video is required by SCK to have a screen output, but is not wanted: 2x2 frames.
    struct Sink;
    impl SCStreamOutputTrait for Sink {
        fn did_output_sample_buffer(&self, sample: CMSampleBuffer, _ty: SCStreamOutputType) {
            let _ = sample.frame_status() == Some(SCFrameStatus::Complete);
        }
    }

    fn start_stream(on_samples: OnSamples) -> Result<SCStream> {
        let content = SCShareableContent::get().map_err(|e| sck_err(&e))?;
        let display = content.displays().into_iter().next().ok_or_else(|| {
            AudioError::new(
                "device_not_found",
                "ScreenCaptureKit lists no display to attach audio capture to",
            )
        })?;
        let filter = SCContentFilter::create()
            .with_display(&display)
            .with_excluding_windows(&[])
            .build()
            .map_err(|e| sck_err(&e))?;
        let config = SCStreamConfiguration::new()
            .with_width(2)
            .with_height(2)
            .with_captures_audio(true)
            .with_sample_rate(i32::try_from(SAMPLE_RATE).unwrap_or(48_000))
            .with_channel_count(2);
        let mut stream = SCStream::new(&filter, &config).map_err(|e| sck_err(&e))?;
        stream
            .add_output_handler(Sink, SCStreamOutputType::Screen)
            .map_err(|e| sck_err(&e))?;
        let handler = Handler {
            state: Mutex::new(AudioOut {
                on_samples,
                resampler: None,
                mono: vec![],
                out: vec![],
            }),
        };
        stream
            .add_output_handler(handler, SCStreamOutputType::Audio)
            .map_err(|e| sck_err(&e))?;
        stream.start_capture().map_err(|e| sck_err(&e))?;
        Ok(stream)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct FakePerms {
        mic: PermissionState,
        screen: PermissionState,
    }

    impl Permissions for FakePerms {
        fn state(&self, which: Permission) -> PermissionState {
            match which {
                Permission::Microphone => self.mic,
                Permission::ScreenRecording => self.screen,
            }
        }
    }

    #[derive(Default)]
    struct FakeSources {
        opened: AtomicUsize,
    }

    struct Noop;
    impl Capture for Noop {
        fn stop(&mut self) {}
    }

    impl Sources for FakeSources {
        fn list_mics(&self) -> Result<Vec<DeviceInfo>> {
            Ok(vec![DeviceInfo {
                id: "in:Mac mic".into(),
                name: "Mac mic".into(),
                kind: DeviceKind::Mic,
                backend: NAME,
                default: true,
            }])
        }
        fn open_system(&self, _: OnSamples) -> Result<Box<dyn Capture>> {
            self.opened.fetch_add(1, Ordering::SeqCst);
            Ok(Box::new(Noop))
        }
        fn open_mic(&self, _: &str, _: OnSamples) -> Result<Box<dyn Capture>> {
            self.opened.fetch_add(1, Ordering::SeqCst);
            Ok(Box::new(Noop))
        }
    }

    fn backend(mic: PermissionState, screen: PermissionState) -> (MacosBackend, Arc<FakeSources>) {
        let s = Arc::new(FakeSources::default());
        (
            MacosBackend::with(Arc::new(FakePerms { mic, screen }), s.clone()),
            s,
        )
    }

    fn code(r: Result<Box<dyn Capture>>) -> &'static str {
        match r {
            Ok(_) => "ok",
            Err(e) => e.code,
        }
    }

    #[test]
    fn denied_permission_is_structured_and_opens_nothing() {
        use PermissionState::*;
        let (b, s) = backend(Denied, Denied);
        assert_eq!(
            code(b.open(DeviceKind::Mic, "", Box::new(|_| {}))),
            "permission_denied"
        );
        assert_eq!(
            code(b.open(DeviceKind::Loopback, "", Box::new(|_| {}))),
            "permission_denied"
        );
        assert_eq!(s.opened.load(Ordering::SeqCst), 0);
        let e = b.open(DeviceKind::Mic, "", Box::new(|_| {})).err().unwrap();
        assert!(e.message.contains("Microphone"), "{e}");
    }

    #[test]
    fn grant_and_undetermined_open() {
        use PermissionState::*;
        let (b, s) = backend(Granted, NotDetermined);
        assert_eq!(code(b.open(DeviceKind::Mic, "", Box::new(|_| {}))), "ok");
        assert_eq!(
            code(b.open(DeviceKind::Loopback, "", Box::new(|_| {}))),
            "ok"
        );
        assert_eq!(s.opened.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn each_source_is_gated_by_its_own_permission() {
        use PermissionState::*;
        let (b, _) = backend(Denied, Granted);
        assert_eq!(
            code(b.open(DeviceKind::Mic, "", Box::new(|_| {}))),
            "permission_denied"
        );
        assert_eq!(
            code(b.open(DeviceKind::Loopback, SYSTEM_DEVICE, Box::new(|_| {}))),
            "ok"
        );
        let (b, _) = backend(Granted, Denied);
        assert_eq!(code(b.open(DeviceKind::Mic, "x", Box::new(|_| {}))), "ok");
        assert_eq!(
            code(b.open(DeviceKind::Loopback, "", Box::new(|_| {}))),
            "permission_denied"
        );
    }

    #[test]
    fn unknown_system_device_is_not_found_before_any_permission_check() {
        let (b, _) = backend(PermissionState::Denied, PermissionState::Denied);
        assert_eq!(
            code(b.open(DeviceKind::Loopback, "other", Box::new(|_| {}))),
            "device_not_found"
        );
    }

    #[test]
    fn devices_list_system_then_mics() {
        let (b, _) = backend(PermissionState::Granted, PermissionState::Granted);
        let d = b.list_devices().unwrap();
        assert_eq!(d[0].id, SYSTEM_DEVICE);
        assert_eq!(d[0].kind, DeviceKind::Loopback);
        assert!(d[0].default);
        assert_eq!(d[1].kind, DeviceKind::Mic);
        assert_eq!(b.name(), "screencapturekit");
    }

    #[test]
    fn sck_failures_classify() {
        assert_eq!(
            classify_sck_failure(Some(-3801), "x").code,
            "permission_denied"
        );
        assert_eq!(
            classify_sck_failure(Some(-3803), "x").code,
            "permission_denied"
        );
        assert_eq!(
            classify_sck_failure(None, "The user declined TCCs for application").code,
            "permission_denied"
        );
        assert_eq!(
            classify_sck_failure(Some(-3811), "internal").code,
            "capture_failed"
        );
        assert_eq!(classify_sck_failure(None, "boom").code, "capture_failed");
    }

    fn f32s(v: &[f32]) -> Vec<u8> {
        v.iter().flat_map(|x| x.to_le_bytes()).collect()
    }

    #[test]
    fn planar_stereo_averages() {
        let (l, r) = (f32s(&[1.0, 0.0, 0.5]), f32s(&[0.0, 1.0, 0.5]));
        let m = decode_mono(&[(1, &l), (1, &r)], PcmFormat::F32);
        assert_eq!(m, vec![0.5, 0.5, 0.5]);
    }

    #[test]
    fn interleaved_stereo_averages() {
        let b = f32s(&[1.0, 0.0, 0.2, 0.4]);
        let m = decode_mono(&[(2, &b)], PcmFormat::F32);
        assert_eq!(m.len(), 2);
        assert!((m[0] - 0.5).abs() < 1e-6 && (m[1] - 0.3).abs() < 1e-6);
    }

    #[test]
    fn i16_scales_and_ragged_cuts() {
        let b: Vec<u8> = [16384i16, -16384]
            .iter()
            .flat_map(|x| x.to_le_bytes())
            .collect();
        assert_eq!(decode_mono(&[(1, &b)], PcmFormat::I16), vec![0.5, -0.5]);
        let (a, c) = (f32s(&[1.0, 1.0, 1.0]), f32s(&[1.0]));
        assert_eq!(decode_mono(&[(1, &a), (1, &c)], PcmFormat::F32).len(), 1);
        assert!(decode_mono(&[], PcmFormat::F32).is_empty());
    }
}
