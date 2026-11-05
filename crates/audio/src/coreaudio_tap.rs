//! macOS system audio from a Core Audio process tap (dot-4uzgol.76): a global stereo mixdown tap
//! that excludes this process, wrapped in a private aggregate device and read through an IOProc.
//!
//! A tap needs macOS 14.2+ and the System Audio Recording permission (`NSAudioCaptureUsageDescription`,
//! asked on the first recording from the aggregate device), not Screen Recording. There is no
//! public preflight for that permission, so the gate is the tap's own error: `classify_tap_failure`
//! maps a denial to `permission_denied` naming the pane.
//!
//! The pure parts (version gate, error classification, sample format, aggregate-device description)
//! compile everywhere and test in the pods; only `native` calls Core Audio.

use crate::error::AudioError;
use crate::macos::{PcmFormat, Permission};

/// A macOS product version, `major.minor`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct OsVersion {
    pub major: u32,
    pub minor: u32,
}

impl OsVersion {
    /// `AudioHardwareCreateProcessTap` and `CATapDescription` arrived in macOS 14.2.
    pub fn supports_tap(self) -> bool {
        self >= OsVersion {
            major: 14,
            minor: 2,
        }
    }
}

impl std::fmt::Display for OsVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}", self.major, self.minor)
    }
}

/// The tap setup step that failed; it names the failure and decides whether it can be TCC.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TapStage {
    /// `CATapDescription` / `AudioHardwareCreateProcessTap` missing (macOS before 14.2).
    Unavailable,
    CreateTap,
    ReadFormat,
    CreateAggregate,
    CreateIoProc,
    Start,
}

impl TapStage {
    fn what(self) -> &'static str {
        match self {
            Self::Unavailable => "process taps are unavailable",
            Self::CreateTap => "AudioHardwareCreateProcessTap",
            Self::ReadFormat => "reading the tap format",
            Self::CreateAggregate => "AudioHardwareCreateAggregateDevice",
            Self::CreateIoProc => "AudioDeviceCreateIOProcID",
            Self::Start => "AudioDeviceStart",
        }
    }
}

fn fourcc(code: &[u8; 4]) -> i32 {
    i32::from_be_bytes(*code)
}

/// An `OSStatus` as Core Audio prints it: a FourCC when it is four printable bytes, else a number.
pub fn status_text(status: i32) -> String {
    let b = status.to_be_bytes();
    if b.iter().all(|c| c.is_ascii_graphic() || *c == b' ') {
        format!("'{}'", String::from_utf8_lossy(&b))
    } else {
        status.to_string()
    }
}

/// Map a failed tap step to a structured error. `kAudioDevicePermissionsError` ('!hog') anywhere,
/// and `kAudioHardwareIllegalOperationError` ('nope') from creating or starting the tap, are the
/// System Audio Recording denial; anything else is `capture_failed` (which `auto` falls back from).
pub fn classify_tap_failure(stage: TapStage, status: i32) -> AudioError {
    let denied = status == fourcc(b"!hog")
        || (status == fourcc(b"nope") && matches!(stage, TapStage::CreateTap | TapStage::Start));
    let detail = format!(
        "Core Audio tap: {} failed ({})",
        stage.what(),
        status_text(status)
    );
    if denied {
        AudioError::new(
            "permission_denied",
            format!(
                "{detail}; macOS denied System Audio Recording, allow it in System Settings > {}",
                Permission::SystemAudioRecording.pane()
            ),
        )
    } else if stage == TapStage::Unavailable {
        AudioError::new(
            "unsupported_platform",
            "Core Audio process taps need macOS 14.2 or later",
        )
    } else {
        AudioError::new("capture_failed", detail)
    }
}

/// The sample encoding of a tap's `AudioStreamBasicDescription`: little-endian linear PCM, f32 or
/// i16. `kAudioFormatFlagIsFloat` is bit 0 and `kAudioFormatFlagIsBigEndian` bit 1.
pub fn pcm_format(format_id: u32, flags: u32, bits: u32) -> Option<PcmFormat> {
    if format_id != u32::from_be_bytes(*b"lpcm") || flags & 0b10 != 0 {
        return None;
    }
    match (flags & 0b1 != 0, bits) {
        (true, 32) => Some(PcmFormat::F32),
        (false, 16) => Some(PcmFormat::I16),
        _ => None,
    }
}

/// A value in the aggregate-device description dictionary.
#[derive(Debug, Clone, PartialEq)]
pub enum AggValue {
    Str(String),
    Bool(bool),
    /// An array of dictionaries (the tap list).
    List(Vec<Vec<(&'static str, AggValue)>>),
}

/// The private aggregate device that carries the tap: no sub-devices (so no microphone input is
/// mixed in), the one tap with drift compensation, started with the device. Keys are the
/// `kAudioAggregateDevice*Key` / `kAudioSubTap*Key` strings.
pub fn aggregate_description(tap_uid: &str, agg_uid: &str) -> Vec<(&'static str, AggValue)> {
    vec![
        ("uid", AggValue::Str(agg_uid.to_string())),
        (
            "name",
            AggValue::Str("oats-meetings-rs system audio".into()),
        ),
        ("private", AggValue::Bool(true)),
        ("stacked", AggValue::Bool(false)),
        ("tapautostart", AggValue::Bool(true)),
        (
            "taps",
            AggValue::List(vec![vec![
                ("uid", AggValue::Str(tap_uid.to_string())),
                ("drift", AggValue::Bool(true)),
            ]]),
        ),
    ]
}

#[cfg(target_os = "macos")]
pub use native::{open, os_version};

#[cfg(target_os = "macos")]
mod native {
    use std::ffi::{c_char, c_void};
    use std::panic::{catch_unwind, AssertUnwindSafe};
    use std::sync::{mpsc, Mutex};

    use objc2::rc::{autoreleasepool, Allocated, Retained};
    use objc2::runtime::{AnyClass, AnyObject};
    use objc2::{msg_send, sel};
    use objc2_foundation::{NSArray, NSDictionary, NSNumber, NSProcessInfo, NSString, NSUUID};

    use super::TapStage;
    use super::{aggregate_description, classify_tap_failure, pcm_format, AggValue, OsVersion};
    use crate::backend::{Capture, OnSamples};
    use crate::coreaudio_hal::{read_global, SYSTEM_OBJECT};
    use crate::error::{AudioError, Result};
    use crate::macos::{decode_mono, PcmFormat, SystemOut};
    use crate::settings::Timing;

    /// `AudioStreamBasicDescription`.
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Asbd {
        sample_rate: f64,
        format_id: u32,
        format_flags: u32,
        bytes_per_packet: u32,
        frames_per_packet: u32,
        bytes_per_frame: u32,
        channels_per_frame: u32,
        bits_per_channel: u32,
        reserved: u32,
    }

    #[repr(C)]
    struct AudioBuffer {
        channels: u32,
        bytes: u32,
        data: *mut c_void,
    }

    /// `AudioBufferList`: a count, then that many buffers (declared as one).
    #[repr(C)]
    struct AudioBufferList {
        count: u32,
        buffers: [AudioBuffer; 1],
    }

    type IoProc = unsafe extern "C" fn(
        device: u32,
        now: *const c_void,
        input: *const AudioBufferList,
        input_time: *const c_void,
        output: *mut AudioBufferList,
        output_time: *const c_void,
        client: *mut c_void,
    ) -> i32;

    type CreateTap = unsafe extern "C" fn(desc: *const AnyObject, out: *mut u32) -> i32;
    type DestroyTap = unsafe extern "C" fn(id: u32) -> i32;

    // `AudioDeviceIOProcID` is an opaque function pointer; it is only handed back, so a pointer.
    #[link(name = "CoreAudio", kind = "framework")]
    extern "C" {
        fn AudioHardwareCreateAggregateDevice(desc: *const c_void, out: *mut u32) -> i32;
        fn AudioHardwareDestroyAggregateDevice(id: u32) -> i32;
        fn AudioDeviceCreateIOProcID(
            device: u32,
            proc_: IoProc,
            client: *mut c_void,
            out: *mut *mut c_void,
        ) -> i32;
        fn AudioDeviceDestroyIOProcID(device: u32, id: *mut c_void) -> i32;
        fn AudioDeviceStart(device: u32, id: *mut c_void) -> i32;
        fn AudioDeviceStop(device: u32, id: *mut c_void) -> i32;
    }

    extern "C" {
        fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
    }

    /// `kAudioHardwarePropertyTranslatePIDToProcessObject`.
    const PID_TO_PROCESS: &[u8; 4] = b"id2p";
    /// `kAudioTapPropertyFormat`.
    const TAP_FORMAT: &[u8; 4] = b"tfmt";

    /// The installed macOS version, from `NSProcessInfo`.
    pub fn os_version() -> Option<OsVersion> {
        let v = NSProcessInfo::processInfo().operatingSystemVersion();
        Some(OsVersion {
            major: u32::try_from(v.majorVersion).ok()?,
            minor: u32::try_from(v.minorVersion).ok()?,
        })
    }

    /// The tap entry points, looked up at run time: linking them directly would stop the app
    /// loading on macOS before 14.2, where ScreenCaptureKit is the fallback.
    fn tap_functions() -> Option<(CreateTap, DestroyTap)> {
        // RTLD_DEFAULT is ((void *) -2) on Darwin.
        let any = (-2isize) as *mut c_void;
        let create = unsafe { dlsym(any, c"AudioHardwareCreateProcessTap".as_ptr()) };
        let destroy = unsafe { dlsym(any, c"AudioHardwareDestroyProcessTap".as_ptr()) };
        if create.is_null() || destroy.is_null() {
            return None;
        }
        Some(unsafe {
            (
                std::mem::transmute::<*mut c_void, CreateTap>(create),
                std::mem::transmute::<*mut c_void, DestroyTap>(destroy),
            )
        })
    }

    /// What the IOProc feeds: the tap's format and the resampling sink.
    struct Feed {
        fmt: PcmFormat,
        rate: u32,
        out: Mutex<SystemOut>,
    }

    unsafe extern "C" fn io_proc(
        _device: u32,
        _now: *const c_void,
        input: *const AudioBufferList,
        _input_time: *const c_void,
        _output: *mut AudioBufferList,
        _output_time: *const c_void,
        client: *mut c_void,
    ) -> i32 {
        if input.is_null() || client.is_null() {
            return 0;
        }
        let _ = catch_unwind(AssertUnwindSafe(|| {
            let feed = unsafe { &*client.cast::<Feed>() };
            let count = unsafe { (*input).count } as usize;
            let first = unsafe { std::ptr::addr_of!((*input).buffers) }.cast::<AudioBuffer>();
            let bufs: Vec<(u32, &[u8])> = (0..count)
                .filter_map(|i| {
                    let b = unsafe { &*first.add(i) };
                    (!b.data.is_null()).then(|| {
                        let bytes = unsafe {
                            std::slice::from_raw_parts(b.data.cast::<u8>(), b.bytes as usize)
                        };
                        (b.channels, bytes)
                    })
                })
                .collect();
            let mono = decode_mono(&bufs, feed.fmt);
            if let Ok(mut out) = feed.out.lock() {
                out.push(feed.rate, &mono);
            }
        }));
        0
    }

    /// Everything a running tap owns, torn down in reverse on drop. Ids of 0 are
    /// `kAudioObjectUnknown`: a step that never ran.
    struct Tap {
        destroy_tap: DestroyTap,
        tap: u32,
        agg: u32,
        io: *mut c_void,
        feed: *mut Feed,
        running: bool,
    }

    // The ids are plain integers; `feed` is only touched by the IOProc until `drop` stops it.
    unsafe impl Send for Tap {}

    impl Drop for Tap {
        fn drop(&mut self) {
            unsafe {
                if self.running {
                    AudioDeviceStop(self.agg, self.io);
                }
                if !self.io.is_null() {
                    AudioDeviceDestroyIOProcID(self.agg, self.io);
                }
                if self.agg != 0 {
                    AudioHardwareDestroyAggregateDevice(self.agg);
                }
                if self.tap != 0 {
                    (self.destroy_tap)(self.tap);
                }
                if !self.feed.is_null() {
                    drop(Box::from_raw(self.feed));
                }
            }
        }
    }

    struct TapCapture(Option<Tap>);

    impl Capture for TapCapture {
        fn stop(&mut self) {
            self.0.take();
        }
    }

    /// Start the tap on a worker thread, bounded by `open_timeout`. A tap that comes up after the
    /// timeout is dropped (torn down) when its result cannot be delivered.
    pub fn open(timing: Timing, on_samples: OnSamples) -> Result<Box<dyn Capture>> {
        let (tx, rx) = mpsc::channel::<Result<Tap>>();
        std::thread::spawn(move || {
            let _ = tx.send(autoreleasepool(|_| start(on_samples)));
        });
        match rx.recv_timeout(timing.open_timeout) {
            Ok(Ok(tap)) => Ok(Box::new(TapCapture(Some(tap)))),
            Ok(Err(e)) => Err(e),
            Err(_) => Err(AudioError::new(
                "capture_failed",
                format!(
                    "Core Audio tap did not start within {:?}",
                    timing.open_timeout
                ),
            )),
        }
    }

    /// Our own audio process object, so the tap leaves out anything this app plays.
    fn own_process_object() -> Option<u32> {
        let pid = i32::try_from(std::process::id()).ok()?;
        read_global::<u32, i32>(SYSTEM_OBJECT, PID_TO_PROCESS, Some(&pid)).filter(|id| *id != 0)
    }

    fn any(o: Retained<impl objc2::Message>) -> Retained<AnyObject> {
        // An upcast to `AnyObject` is always valid.
        unsafe { Retained::cast_unchecked(o) }
    }

    fn ns_value(v: &AggValue) -> Retained<AnyObject> {
        match v {
            AggValue::Str(s) => any(NSString::from_str(s)),
            AggValue::Bool(b) => any(NSNumber::numberWithBool(*b)),
            AggValue::List(items) => {
                let dicts: Vec<Retained<AnyObject>> =
                    items.iter().map(|d| any(ns_dict(d))).collect();
                any(NSArray::from_retained_slice(&dicts))
            }
        }
    }

    fn ns_dict(
        entries: &[(&'static str, AggValue)],
    ) -> Retained<NSDictionary<NSString, AnyObject>> {
        let keys: Vec<Retained<NSString>> =
            entries.iter().map(|(k, _)| NSString::from_str(k)).collect();
        let vals: Vec<Retained<AnyObject>> = entries.iter().map(|(_, v)| ns_value(v)).collect();
        let keys: Vec<&NSString> = keys.iter().map(|k| &**k).collect();
        let vals: Vec<&AnyObject> = vals.iter().map(|v| &**v).collect();
        NSDictionary::from_slices(&keys, &vals)
    }

    /// A global stereo mixdown tap description that excludes this process; private when the
    /// running macOS exposes the setter.
    fn describe(cls: &AnyClass) -> Option<Retained<AnyObject>> {
        let own: Vec<Retained<NSNumber>> = own_process_object()
            .into_iter()
            .map(NSNumber::numberWithUnsignedInt)
            .collect();
        let processes = NSArray::from_retained_slice(&own);
        let alloc: Allocated<AnyObject> = unsafe { msg_send![cls, alloc] };
        let desc: Option<Retained<AnyObject>> =
            unsafe { msg_send![alloc, initStereoGlobalTapButExcludeProcesses: &*processes] };
        let desc = desc?;
        let name = NSString::from_str("oats-meetings-rs");
        let _: () = unsafe { msg_send![&*desc, setName: &*name] };
        if cls.responds_to(sel!(setPrivate:)) {
            let _: () = unsafe { msg_send![&*desc, setPrivate: true] };
        }
        Some(desc)
    }

    fn start(on_samples: OnSamples) -> Result<Tap> {
        let fail = classify_tap_failure;
        let (create_tap, destroy_tap) =
            tap_functions().ok_or_else(|| fail(TapStage::Unavailable, 0))?;
        let cls =
            AnyClass::get(c"CATapDescription").ok_or_else(|| fail(TapStage::Unavailable, 0))?;
        let desc = describe(cls).ok_or_else(|| {
            AudioError::new(
                "capture_failed",
                "Core Audio tap: CATapDescription init returned nil",
            )
        })?;
        let uuid: Retained<NSUUID> = unsafe { msg_send![&*desc, UUID] };
        let tap_uid = uuid.UUIDString().to_string();

        let mut tap = Tap {
            destroy_tap,
            tap: 0,
            agg: 0,
            io: std::ptr::null_mut(),
            feed: std::ptr::null_mut(),
            running: false,
        };
        let st = unsafe { create_tap(Retained::as_ptr(&desc), &mut tap.tap) };
        if st != 0 {
            return Err(fail(TapStage::CreateTap, st));
        }

        let asbd = read_global::<Asbd, u8>(tap.tap, TAP_FORMAT, None).ok_or_else(|| {
            AudioError::new(
                "capture_failed",
                "Core Audio tap: reading the tap format failed",
            )
        })?;
        let fmt = pcm_format(asbd.format_id, asbd.format_flags, asbd.bits_per_channel).ok_or_else(
            || {
                AudioError::new(
                    "capture_failed",
                    format!(
                        "Core Audio tap: unsupported format (id {}, flags {:#x}, {} bits)",
                        super::status_text(asbd.format_id as i32),
                        asbd.format_flags,
                        asbd.bits_per_channel
                    ),
                )
            },
        )?;
        let rate = asbd.sample_rate as u32;
        if rate == 0 {
            return Err(fail(TapStage::ReadFormat, 0));
        }

        let agg_uid = format!("oats-meetings-rs.system-tap.{tap_uid}");
        let dict = ns_dict(&aggregate_description(&tap_uid, &agg_uid));
        let st = unsafe {
            AudioHardwareCreateAggregateDevice(Retained::as_ptr(&dict).cast(), &mut tap.agg)
        };
        if st != 0 {
            return Err(fail(TapStage::CreateAggregate, st));
        }

        tap.feed = Box::into_raw(Box::new(Feed {
            fmt,
            rate,
            out: Mutex::new(SystemOut::new(on_samples)),
        }));
        let st =
            unsafe { AudioDeviceCreateIOProcID(tap.agg, io_proc, tap.feed.cast(), &mut tap.io) };
        if st != 0 {
            return Err(fail(TapStage::CreateIoProc, st));
        }
        let st = unsafe { AudioDeviceStart(tap.agg, tap.io) };
        if st != 0 {
            return Err(fail(TapStage::Start, st));
        }
        tap.running = true;
        Ok(tap)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn st(code: &[u8; 4]) -> i32 {
        i32::from_be_bytes(*code)
    }

    #[test]
    fn tap_needs_14_2() {
        let v = |major, minor| OsVersion { major, minor };
        assert!(!v(13, 6).supports_tap());
        assert!(!v(14, 1).supports_tap());
        assert!(v(14, 2).supports_tap());
        assert!(v(15, 0).supports_tap());
        assert!(v(26, 0).supports_tap());
        assert_eq!(v(14, 2).to_string(), "14.2");
    }

    #[test]
    fn denial_names_the_system_audio_recording_pane() {
        for (stage, code) in [
            (TapStage::CreateTap, b"!hog"),
            (TapStage::Start, b"!hog"),
            (TapStage::CreateAggregate, b"!hog"),
            (TapStage::CreateTap, b"nope"),
            (TapStage::Start, b"nope"),
        ] {
            let e = classify_tap_failure(stage, st(code));
            assert_eq!(e.code, "permission_denied", "{stage:?} {code:?}");
            assert!(e.message.contains("System Audio Recording"), "{e}");
            assert!(!e.message.contains("ScreenCaptureKit"), "{e}");
        }
    }

    #[test]
    fn other_failures_are_capture_failed() {
        let e = classify_tap_failure(TapStage::CreateAggregate, st(b"nope"));
        assert_eq!(e.code, "capture_failed");
        assert!(e.message.contains("'nope'"), "{e}");
        assert_eq!(
            classify_tap_failure(TapStage::Start, st(b"!dev")).code,
            "capture_failed"
        );
        assert_eq!(
            classify_tap_failure(TapStage::CreateTap, -50).code,
            "capture_failed"
        );
        assert_eq!(
            classify_tap_failure(TapStage::Unavailable, 0).code,
            "unsupported_platform"
        );
    }

    #[test]
    fn status_prints_fourcc_or_number() {
        assert_eq!(status_text(st(b"!hog")), "'!hog'");
        assert_eq!(status_text(-50), "-50");
        assert_eq!(status_text(0), "0");
    }

    #[test]
    fn pcm_formats() {
        let lpcm = u32::from_be_bytes(*b"lpcm");
        assert_eq!(pcm_format(lpcm, 0b1 | 0b1000, 32), Some(PcmFormat::F32));
        assert_eq!(pcm_format(lpcm, 0b100, 16), Some(PcmFormat::I16));
        assert_eq!(pcm_format(lpcm, 0b11, 32), None, "big-endian");
        assert_eq!(pcm_format(lpcm, 0, 24), None);
        assert_eq!(pcm_format(u32::from_be_bytes(*b"aac "), 0b1, 32), None);
    }

    #[test]
    fn aggregate_is_private_tap_only_and_autostarts() {
        let d = aggregate_description("TAP-UUID", "agg-uid");
        let get = |k: &str| d.iter().find(|(key, _)| *key == k).map(|(_, v)| v.clone());
        assert_eq!(get("uid"), Some(AggValue::Str("agg-uid".into())));
        assert_eq!(get("private"), Some(AggValue::Bool(true)));
        assert_eq!(get("tapautostart"), Some(AggValue::Bool(true)));
        assert_eq!(get("subdevices"), None, "no sub-device: no mic mixed in");
        let Some(AggValue::List(taps)) = get("taps") else {
            panic!("no tap list")
        };
        assert_eq!(taps.len(), 1);
        assert!(taps[0].contains(&("uid", AggValue::Str("TAP-UUID".into()))));
    }
}
