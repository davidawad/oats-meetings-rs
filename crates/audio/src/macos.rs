//! macOS capture (dot-4uzgol.6, dot-4uzgol.76): system audio through a Core Audio process tap
//! (`coreaudio_tap`, macOS 14.2+) or ScreenCaptureKit, microphone through cpal (CoreAudio). Mics
//! are listed from CoreAudio HAL properties (`coreaudio_hal`), not cpal.
//!
//! Which system-audio source: `audio.system_backend`. `auto` (the default) takes the tap on 14.2
//! and later, because its TCC consent is System Audio Recording rather than Screen Recording, and
//! falls back to ScreenCaptureKit on older macOS or when the tap fails for a reason other than a
//! denial. `tap` and `screencapturekit` force one. The tap is gated on its own error (there is no
//! public preflight for System Audio Recording); only the ScreenCaptureKit path preflights Screen
//! Recording, so a tap recording never touches that grant.
//!
//! Everything that decides behaviour is pure and compiles on every OS so the pods test it: the
//! permission gate (`Permissions`), the source plan and fallback, the error classification, the
//! PCM decode, and the backend that joins them over a `Sources` trait. Only the real `Sources` and
//! `Permissions` (tap, SCK, cpal, AVFoundation) are macOS-only.

use std::sync::{Arc, Mutex};

use crate::backend::{Capture, CaptureBackend, DeviceInfo, DeviceKind, OnSamples};
use crate::coreaudio_tap::OsVersion;
use crate::error::{AudioError, Result};
use crate::resample::{downmix, Resampler};
use crate::settings::SystemBackend;
use crate::SAMPLE_RATE;

/// The `audio.backend` value (and device `backend`) of the whole macOS backend, kept from when
/// ScreenCaptureKit was its only system-audio source.
pub const NAME: &str = "screencapturekit";
/// The only system-audio device: everything the machine plays, whichever source records it.
pub const SYSTEM_DEVICE: &str = "macos:system";
/// The id `SYSTEM_DEVICE` had before the tap; a saved `audio.system.device` keeps resolving.
pub const LEGACY_SYSTEM_DEVICE: &str = "screencapturekit:system";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Permission {
    Microphone,
    ScreenRecording,
    /// The Core Audio tap's consent (`NSAudioCaptureUsageDescription`).
    SystemAudioRecording,
}

impl Permission {
    pub(crate) fn pane(self) -> &'static str {
        match self {
            Self::Microphone => "Privacy & Security > Microphone",
            Self::ScreenRecording => "Privacy & Security > Screen & System Audio Recording",
            Self::SystemAudioRecording => {
                "Privacy & Security > Screen & System Audio Recording > System Audio Recording Only"
            }
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

/// How a system-audio buffer (ScreenCaptureKit or tap) encodes samples.
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

/// Mono buffers at the source rate in, mono at `SAMPLE_RATE` out to `on_samples`; the resampler is
/// rebuilt when the source rate changes. Both system-audio sources feed through one.
pub struct SystemOut {
    on_samples: OnSamples,
    resampler: Option<(u32, Resampler)>,
    mono: Vec<f32>,
    out: Vec<f32>,
}

impl SystemOut {
    pub fn new(on_samples: OnSamples) -> Self {
        Self {
            on_samples,
            resampler: None,
            mono: vec![],
            out: vec![],
        }
    }

    pub fn push(&mut self, rate: u32, mono: &[f32]) {
        if rate == 0 {
            return;
        }
        if self.resampler.as_ref().map(|(r, _)| *r) != Some(rate) {
            self.resampler = Some((rate, Resampler::new(rate, SAMPLE_RATE)));
        }
        self.out.clear();
        if let Some((_, rs)) = self.resampler.as_mut() {
            self.mono.clear();
            downmix(mono, 1, &mut self.mono);
            rs.push(&self.mono, &mut self.out);
        }
        if !self.out.is_empty() {
            (self.on_samples)(&self.out);
        }
    }
}

/// One way to record system audio.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SystemSource {
    /// Core Audio process tap (macOS 14.2+, System Audio Recording).
    Tap,
    /// ScreenCaptureKit (macOS 13+, Screen Recording).
    ScreenCaptureKit,
}

impl SystemSource {
    fn label(self) -> &'static str {
        match self {
            Self::Tap => "Core Audio tap",
            Self::ScreenCaptureKit => "ScreenCaptureKit",
        }
    }
}

/// The sources to try, in order, for `audio.system_backend` on macOS `os` (`None`: unknown, so
/// the tap is tried and its own failure decides).
pub fn system_plan(choice: SystemBackend, os: Option<OsVersion>) -> Result<Vec<SystemSource>> {
    let tap_ok = os.is_none_or(OsVersion::supports_tap);
    Ok(match choice {
        SystemBackend::ScreenCaptureKit => vec![SystemSource::ScreenCaptureKit],
        SystemBackend::Auto if tap_ok => vec![SystemSource::Tap, SystemSource::ScreenCaptureKit],
        SystemBackend::Auto => vec![SystemSource::ScreenCaptureKit],
        SystemBackend::Tap if tap_ok => vec![SystemSource::Tap],
        SystemBackend::Tap => {
            return Err(AudioError::new(
                "unsupported_platform",
                format!(
                    "audio.system_backend 'tap' needs macOS 14.2 or later (this is {}); use auto or screencapturekit",
                    os.map(|v| v.to_string()).unwrap_or_default()
                ),
            ))
        }
    })
}

/// Whether a failed source gives way to the next one. A denial does not: falling back from a
/// denied tap would ask for Screen Recording, the grant the tap exists to avoid.
pub fn falls_back(e: &AudioError) -> bool {
    e.code != "permission_denied"
}

/// The real capture calls, behind a trait so the gate and the device logic test without a Mac.
pub trait Sources: Send + Sync {
    fn list_mics(&self) -> Result<Vec<DeviceInfo>>;
    fn open_system(&self, source: SystemSource, on_samples: OnSamples) -> Result<Box<dyn Capture>>;
    fn open_mic(&self, device: &str, on_samples: OnSamples) -> Result<Box<dyn Capture>>;
}

pub struct MacosBackend {
    perms: Arc<dyn Permissions>,
    sources: Arc<dyn Sources>,
    system: Result<Vec<SystemSource>>,
}

impl MacosBackend {
    /// `system` is the `system_plan` to record system audio with.
    pub fn with(
        perms: Arc<dyn Permissions>,
        sources: Arc<dyn Sources>,
        system: Result<Vec<SystemSource>>,
    ) -> Self {
        Self {
            perms,
            sources,
            system,
        }
    }

    fn open_system(&self, on_samples: OnSamples) -> Result<Box<dyn Capture>> {
        let plan = self.system.clone()?;
        // A failed attempt consumes its callback, so every attempt feeds one shared sink.
        let shared = Arc::new(Mutex::new(on_samples));
        let mut failed: Vec<String> = vec![];
        let mut last = None;
        for source in plan {
            let feed = Arc::clone(&shared);
            let on: OnSamples = Box::new(move |x: &[f32]| {
                if let Ok(mut f) = feed.lock() {
                    f(x);
                }
            });
            let r = match source {
                SystemSource::Tap => self.sources.open_system(source, on),
                SystemSource::ScreenCaptureKit => {
                    require(self.perms.as_ref(), Permission::ScreenRecording)
                        .and_then(|()| self.sources.open_system(source, on))
                }
            };
            match r {
                Ok(c) => return Ok(c),
                Err(e) if !falls_back(&e) => return Err(e),
                Err(e) => {
                    failed.push(format!("{}: {}", source.label(), e.message));
                    last = Some(e);
                }
            }
        }
        let mut e = last
            .unwrap_or_else(|| AudioError::new("capture_failed", "no system-audio source to try"));
        if failed.len() > 1 {
            e.message = failed.join("; then ");
        }
        Err(e)
    }
}

impl CaptureBackend for MacosBackend {
    fn name(&self) -> &'static str {
        NAME
    }

    fn list_devices(&self) -> Result<Vec<DeviceInfo>> {
        let via = match self.system.as_deref() {
            Ok([first, rest @ ..]) => {
                let fallback: Vec<&str> = rest.iter().map(|s| s.label()).collect();
                if fallback.is_empty() {
                    first.label().to_string()
                } else {
                    format!("{}, else {}", first.label(), fallback.join(", "))
                }
            }
            _ => "unavailable".to_string(),
        };
        let mut v = vec![DeviceInfo {
            id: SYSTEM_DEVICE.into(),
            name: format!("System audio ({via})"),
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
                if !device.is_empty() && device != SYSTEM_DEVICE && device != LEGACY_SYSTEM_DEVICE {
                    return Err(AudioError::new(
                        "device_not_found",
                        format!(
                            "no system-audio device '{device}'; macOS has only '{SYSTEM_DEVICE}'"
                        ),
                    ));
                }
                self.open_system(on_samples)
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
    use crate::coreaudio_hal::{self, SystemHal};
    use crate::coreaudio_tap;
    use crate::cpal_backend::CpalBackend;
    use crate::settings::Timing;

    /// The real backend: tap or SCK system audio per `system`, cpal mic, TCC state from the system.
    pub fn native(timing: Timing, system: SystemBackend) -> MacosBackend {
        MacosBackend::with(
            Arc::new(SystemPermissions),
            Arc::new(NativeSources { timing }),
            system_plan(system, coreaudio_tap::os_version()),
        )
    }

    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        fn CGPreflightScreenCaptureAccess() -> bool;
    }

    #[link(name = "AVFoundation", kind = "framework")]
    extern "C" {}

    /// Reads TCC without prompting: `CGPreflightScreenCaptureAccess` never asks, and
    /// `+[AVCaptureDevice authorizationStatusForMediaType:]` only reports. System Audio Recording
    /// has no public preflight: it reads as `NotDetermined` and the tap's own error decides.
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
                Permission::SystemAudioRecording => PermissionState::NotDetermined,
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
            // HAL properties only: no AudioUnit, so no Microphone grant and no hang (dot-4uzgol.59).
            coreaudio_hal::mics(&SystemHal, NAME)
        }

        fn open_mic(&self, device: &str, on_samples: OnSamples) -> Result<Box<dyn Capture>> {
            let name = coreaudio_hal::resolve_mic(&self.list_mics()?, device)?;
            CpalBackend::new(self.timing).open_input_named(&name, on_samples)
        }

        fn open_system(
            &self,
            source: SystemSource,
            on_samples: OnSamples,
        ) -> Result<Box<dyn Capture>> {
            match source {
                SystemSource::Tap => coreaudio_tap::open(self.timing, on_samples),
                SystemSource::ScreenCaptureKit => self.open_sck(on_samples),
            }
        }
    }

    impl NativeSources {
        fn open_sck(&self, on_samples: OnSamples) -> Result<Box<dyn Capture>> {
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

    struct Handler {
        state: Mutex<SystemOut>,
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
            if let Ok(mut st) = self.state.lock() {
                st.push(rate, &mono);
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
            state: Mutex::new(SystemOut::new(on_samples)),
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
    use std::sync::Mutex;

    struct FakePerms {
        mic: PermissionState,
        screen: PermissionState,
    }

    impl Permissions for FakePerms {
        fn state(&self, which: Permission) -> PermissionState {
            match which {
                Permission::Microphone => self.mic,
                Permission::ScreenRecording => self.screen,
                // The tap is never preflighted; a fake that reports it would hide that.
                Permission::SystemAudioRecording => unreachable!("tap permission is preflighted"),
            }
        }
    }

    #[derive(Default)]
    struct FakeSources {
        opened: AtomicUsize,
        /// Every system source asked for, in order.
        tried: Mutex<Vec<SystemSource>>,
        /// The error code a source fails with.
        fail: Vec<(SystemSource, &'static str)>,
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
        fn open_system(
            &self,
            source: SystemSource,
            mut on_samples: OnSamples,
        ) -> Result<Box<dyn Capture>> {
            self.tried.lock().unwrap().push(source);
            if let Some((_, code)) = self.fail.iter().find(|(s, _)| *s == source) {
                return Err(AudioError::new(code, format!("{source:?} failed")));
            }
            self.opened.fetch_add(1, Ordering::SeqCst);
            on_samples(&[0.25]);
            Ok(Box::new(Noop))
        }
        fn open_mic(&self, _: &str, _: OnSamples) -> Result<Box<dyn Capture>> {
            self.opened.fetch_add(1, Ordering::SeqCst);
            Ok(Box::new(Noop))
        }
    }

    /// The pre-tap behaviour: ScreenCaptureKit only.
    fn backend(mic: PermissionState, screen: PermissionState) -> (MacosBackend, Arc<FakeSources>) {
        planned(
            mic,
            screen,
            Ok(vec![SystemSource::ScreenCaptureKit]),
            vec![],
        )
    }

    fn planned(
        mic: PermissionState,
        screen: PermissionState,
        plan: Result<Vec<SystemSource>>,
        fail: Vec<(SystemSource, &'static str)>,
    ) -> (MacosBackend, Arc<FakeSources>) {
        let s = Arc::new(FakeSources {
            fail,
            ..FakeSources::default()
        });
        (
            MacosBackend::with(Arc::new(FakePerms { mic, screen }), s.clone(), plan),
            s,
        )
    }

    fn v(major: u32, minor: u32) -> Option<OsVersion> {
        Some(OsVersion { major, minor })
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

    #[test]
    fn plan_prefers_the_tap_from_14_2() {
        use SystemSource::*;
        assert_eq!(
            system_plan(SystemBackend::Auto, v(26, 0)).unwrap(),
            vec![Tap, ScreenCaptureKit]
        );
        assert_eq!(
            system_plan(SystemBackend::Auto, v(14, 2)).unwrap(),
            vec![Tap, ScreenCaptureKit]
        );
        assert_eq!(
            system_plan(SystemBackend::Auto, v(14, 1)).unwrap(),
            vec![ScreenCaptureKit]
        );
        assert_eq!(
            system_plan(SystemBackend::Auto, v(13, 6)).unwrap(),
            vec![ScreenCaptureKit]
        );
        assert_eq!(
            system_plan(SystemBackend::Auto, None).unwrap(),
            vec![Tap, ScreenCaptureKit],
            "unknown version: the tap's own failure decides"
        );
        assert_eq!(
            system_plan(SystemBackend::Tap, v(15, 1)).unwrap(),
            vec![Tap]
        );
        assert_eq!(
            system_plan(SystemBackend::ScreenCaptureKit, v(26, 0)).unwrap(),
            vec![ScreenCaptureKit]
        );
        let e = system_plan(SystemBackend::Tap, v(13, 6)).unwrap_err();
        assert_eq!(e.code, "unsupported_platform");
        assert!(e.message.contains("13.6"), "{e}");
    }

    #[test]
    fn tap_records_without_screen_recording_preflight() {
        use PermissionState::*;
        // A Screen Recording denial is irrelevant to the tap: it is never read on that path.
        let plan = system_plan(SystemBackend::Auto, v(26, 0));
        let (b, s) = planned(Granted, Denied, plan, vec![]);
        assert_eq!(
            code(b.open(DeviceKind::Loopback, "", Box::new(|_| {}))),
            "ok"
        );
        assert_eq!(*s.tried.lock().unwrap(), vec![SystemSource::Tap]);
    }

    #[test]
    fn failed_tap_falls_back_to_screencapturekit_with_the_same_sink() {
        use PermissionState::*;
        let plan = system_plan(SystemBackend::Auto, v(15, 0));
        let (b, s) = planned(
            Granted,
            Granted,
            plan,
            vec![(SystemSource::Tap, "capture_failed")],
        );
        let got = Arc::new(Mutex::new(vec![]));
        let sink = Arc::clone(&got);
        let r = b.open(
            DeviceKind::Loopback,
            SYSTEM_DEVICE,
            Box::new(move |x| sink.lock().unwrap().extend_from_slice(x)),
        );
        assert_eq!(code(r), "ok");
        assert_eq!(
            *s.tried.lock().unwrap(),
            vec![SystemSource::Tap, SystemSource::ScreenCaptureKit]
        );
        assert_eq!(*got.lock().unwrap(), vec![0.25]);
    }

    #[test]
    fn tap_denial_is_final_and_never_asks_for_screen_recording() {
        use PermissionState::*;
        let plan = system_plan(SystemBackend::Auto, v(26, 0));
        let (b, s) = planned(
            Granted,
            Granted,
            plan,
            vec![(SystemSource::Tap, "permission_denied")],
        );
        assert_eq!(
            code(b.open(DeviceKind::Loopback, "", Box::new(|_| {}))),
            "permission_denied"
        );
        assert_eq!(*s.tried.lock().unwrap(), vec![SystemSource::Tap]);
    }

    #[test]
    fn forced_tap_does_not_fall_back_and_both_failures_are_reported() {
        use PermissionState::*;
        let (b, s) = planned(
            Granted,
            Granted,
            Ok(vec![SystemSource::Tap]),
            vec![(SystemSource::Tap, "capture_failed")],
        );
        assert_eq!(
            code(b.open(DeviceKind::Loopback, "", Box::new(|_| {}))),
            "capture_failed"
        );
        assert_eq!(*s.tried.lock().unwrap(), vec![SystemSource::Tap]);

        let plan = system_plan(SystemBackend::Auto, v(26, 0));
        let fail = vec![
            (SystemSource::Tap, "capture_failed"),
            (SystemSource::ScreenCaptureKit, "capture_failed"),
        ];
        let (b, _) = planned(Granted, Granted, plan, fail);
        let e = b
            .open(DeviceKind::Loopback, "", Box::new(|_| {}))
            .err()
            .unwrap();
        assert!(e.message.contains("Core Audio tap: Tap failed"), "{e}");
        assert!(
            e.message
                .contains("ScreenCaptureKit: ScreenCaptureKit failed"),
            "{e}"
        );
    }

    #[test]
    fn fallback_to_screencapturekit_is_still_gated_on_screen_recording() {
        use PermissionState::*;
        let plan = system_plan(SystemBackend::Auto, v(26, 0));
        let (b, s) = planned(
            Granted,
            Denied,
            plan,
            vec![(SystemSource::Tap, "capture_failed")],
        );
        let e = b
            .open(DeviceKind::Loopback, "", Box::new(|_| {}))
            .err()
            .unwrap();
        assert_eq!(e.code, "permission_denied");
        assert!(e.message.contains("Screen & System Audio Recording"), "{e}");
        assert_eq!(*s.tried.lock().unwrap(), vec![SystemSource::Tap]);
    }

    #[test]
    fn unsupported_plan_fails_at_open_and_lists_unavailable() {
        use PermissionState::*;
        let plan = system_plan(SystemBackend::Tap, v(13, 0));
        let (b, s) = planned(Granted, Granted, plan, vec![]);
        assert_eq!(
            code(b.open(DeviceKind::Loopback, "", Box::new(|_| {}))),
            "unsupported_platform"
        );
        assert!(s.tried.lock().unwrap().is_empty());
        assert_eq!(
            b.list_devices().unwrap()[0].name,
            "System audio (unavailable)"
        );
        assert_eq!(code(b.open(DeviceKind::Mic, "", Box::new(|_| {}))), "ok");
    }

    #[test]
    fn system_device_names_its_source_and_keeps_the_old_id() {
        use PermissionState::*;
        let plan = system_plan(SystemBackend::Auto, v(26, 0));
        let (b, _) = planned(Granted, Granted, plan, vec![]);
        assert_eq!(
            b.list_devices().unwrap()[0].name,
            "System audio (Core Audio tap, else ScreenCaptureKit)"
        );
        assert_eq!(
            code(b.open(DeviceKind::Loopback, LEGACY_SYSTEM_DEVICE, Box::new(|_| {}))),
            "ok"
        );
        let (b, _) = backend(Granted, Granted);
        assert_eq!(
            b.list_devices().unwrap()[0].name,
            "System audio (ScreenCaptureKit)"
        );
    }

    #[test]
    fn system_out_resamples_to_the_output_rate() {
        let got = Arc::new(Mutex::new(0usize));
        let n = Arc::clone(&got);
        let mut out = SystemOut::new(Box::new(move |x| *n.lock().unwrap() += x.len()));
        out.push(SAMPLE_RATE, &[0.1; 480]);
        assert_eq!(*got.lock().unwrap(), 480);
        out.push(0, &[0.1; 480]);
        assert_eq!(*got.lock().unwrap(), 480, "a zero rate is dropped");
        out.push(SAMPLE_RATE / 2, &[0.1; 4800]);
        let total = *got.lock().unwrap();
        assert!((480 + 9000..=480 + 9800).contains(&total), "{total}");
    }
}
