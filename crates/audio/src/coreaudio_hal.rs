//! macOS device listing from CoreAudio HAL object properties (dot-4uzgol.59).
//!
//! cpal's `input_devices()` probes each device with `supported_input_configs()`, which creates an
//! AudioUnit and sets a property on it. Without a Microphone (TCC) grant that call never returns,
//! so listing devices hung forever. Names, UIDs, stream configurations and nominal sample rates
//! are plain HAL object properties that read without TCC; listing uses only those. The full
//! supported-config probe runs at record start (cpal, inside the bounded open), where a TCC prompt
//! is expected.
//!
//! The listing logic runs over a `Hal` trait so it tests with a fake; only `SystemHal` is macOS.

use crate::backend::{DeviceInfo, DeviceKind};
use crate::error::{AudioError, Result};

/// Which side of a device a stream configuration describes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    Input,
    Output,
}

/// The HAL properties listing needs. Every per-device read is optional: a device whose property
/// fails to read is skipped or reported without it, never an error for the whole list.
pub trait Hal {
    /// `kAudioHardwarePropertyDevices` on the system object.
    fn device_ids(&self) -> Result<Vec<u32>>;
    /// `kAudioHardwarePropertyDefaultInputDevice`.
    fn default_input(&self) -> Option<u32>;
    /// `kAudioObjectPropertyName`.
    fn name(&self, id: u32) -> Option<String>;
    /// `kAudioDevicePropertyDeviceUID`.
    fn uid(&self, id: u32) -> Option<String>;
    /// Total channels of `kAudioDevicePropertyStreamConfiguration` in `scope`.
    fn channels(&self, id: u32, scope: Scope) -> Option<u32>;
    /// `kAudioDevicePropertyNominalSampleRate`.
    fn nominal_rate(&self, id: u32) -> Option<f64>;
}

/// One HAL device with the stream configuration read from its properties.
#[derive(Debug, Clone, PartialEq)]
pub struct HalDevice {
    pub id: u32,
    pub uid: String,
    pub name: String,
    pub input_channels: u32,
    pub output_channels: u32,
    pub sample_rate: Option<f64>,
    pub default_input: bool,
}

/// Every device the HAL reports; a device without a readable name is skipped.
pub fn devices(hal: &dyn Hal) -> Result<Vec<HalDevice>> {
    let default_in = hal.default_input();
    Ok(hal
        .device_ids()?
        .into_iter()
        .filter_map(|id| {
            let name = hal.name(id)?;
            Some(HalDevice {
                id,
                uid: hal.uid(id).unwrap_or_default(),
                name,
                input_channels: hal.channels(id, Scope::Input).unwrap_or(0),
                output_channels: hal.channels(id, Scope::Output).unwrap_or(0),
                sample_rate: hal.nominal_rate(id),
                default_input: default_in == Some(id),
            })
        })
        .collect())
}

fn is_loopback_name(n: &str) -> bool {
    let l = n.to_ascii_lowercase();
    l.contains("monitor") || l.contains("loopback")
}

/// The microphones: devices with input channels, minus virtual loopbacks. Ids are `in:<name>`,
/// the same ids the cpal listing used, so saved `audio.mic_device` values keep resolving.
pub fn mics(hal: &dyn Hal, backend: &'static str) -> Result<Vec<DeviceInfo>> {
    Ok(devices(hal)?
        .into_iter()
        .filter(|d| d.input_channels > 0 && !is_loopback_name(&d.name))
        .map(|d| DeviceInfo {
            id: format!("in:{}", d.name),
            name: d.name,
            kind: DeviceKind::Mic,
            backend,
            default: d.default_input,
        })
        .collect())
}

/// The device name a mic selector picks: empty is the default mic, else an id or a name.
pub fn resolve_mic(mics: &[DeviceInfo], device: &str) -> Result<String> {
    let pick = if device.is_empty() {
        mics.iter().find(|d| d.default).or_else(|| mics.first())
    } else {
        mics.iter().find(|d| d.id == device || d.name == device)
    };
    pick.map(|d| d.name.clone()).ok_or_else(|| {
        AudioError::new(
            "device_not_found",
            format!("no microphone '{device}' (see `ctl devices list`)"),
        )
    })
}

/// Total channels in an `AudioBufferList` as the HAL lays it out: a `u32` buffer count, then
/// pointer-aligned `AudioBuffer { u32 channels, u32 bytes, void *data }` records. Records past the
/// end of `bytes` are ignored.
pub fn buffer_list_channels(bytes: &[u8]) -> u32 {
    let word = |at: usize| {
        bytes
            .get(at..at + 4)
            .map(|b| u32::from_ne_bytes([b[0], b[1], b[2], b[3]]))
    };
    let Some(n) = word(0) else { return 0 };
    let ptr = std::mem::size_of::<usize>();
    let stride = 8 + ptr;
    (0..n as usize)
        .map_while(|i| word(ptr + i * stride))
        .fold(0u32, u32::saturating_add)
}

#[cfg(target_os = "macos")]
pub use native::SystemHal;

#[cfg(target_os = "macos")]
mod native {
    use std::ffi::c_void;
    use std::mem::{size_of, MaybeUninit};

    use super::{buffer_list_channels, Hal, Scope};
    use crate::error::{AudioError, Result};

    #[repr(C)]
    struct Address {
        selector: u32,
        scope: u32,
        element: u32,
    }

    #[link(name = "CoreAudio", kind = "framework")]
    extern "C" {
        fn AudioObjectGetPropertyDataSize(
            id: u32,
            addr: *const Address,
            qualifier_size: u32,
            qualifier: *const c_void,
            size: *mut u32,
        ) -> i32;
        fn AudioObjectGetPropertyData(
            id: u32,
            addr: *const Address,
            qualifier_size: u32,
            qualifier: *const c_void,
            size: *mut u32,
            data: *mut c_void,
        ) -> i32;
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        fn CFStringGetLength(s: *const c_void) -> isize;
        fn CFStringGetMaximumSizeForEncoding(len: isize, encoding: u32) -> isize;
        fn CFStringGetCString(s: *const c_void, buf: *mut u8, size: isize, encoding: u32) -> u8;
        fn CFRelease(p: *const c_void);
    }

    /// `kAudioObjectSystemObject`.
    const SYSTEM_OBJECT: u32 = 1;
    /// `kAudioObjectPropertyElementMain`.
    const ELEMENT_MAIN: u32 = 0;
    /// `kCFStringEncodingUTF8`.
    const UTF8: u32 = 0x0800_0100;

    // Property selectors and scopes are FourCCs.
    const DEVICES: &[u8; 4] = b"dev#";
    const DEFAULT_INPUT: &[u8; 4] = b"dIn ";
    const OBJECT_NAME: &[u8; 4] = b"lnam";
    const DEVICE_UID: &[u8; 4] = b"uid ";
    const STREAM_CONFIGURATION: &[u8; 4] = b"slay";
    const NOMINAL_SAMPLE_RATE: &[u8; 4] = b"nsrt";
    const SCOPE_GLOBAL: &[u8; 4] = b"glob";
    const SCOPE_INPUT: &[u8; 4] = b"inpt";
    const SCOPE_OUTPUT: &[u8; 4] = b"outp";

    fn addr(selector: &[u8; 4], scope: &[u8; 4]) -> Address {
        Address {
            selector: u32::from_be_bytes(*selector),
            scope: u32::from_be_bytes(*scope),
            element: ELEMENT_MAIN,
        }
    }

    /// A fixed-size property (an id, a rate, a CFStringRef).
    fn read<T: Copy>(id: u32, selector: &[u8; 4], scope: &[u8; 4]) -> Option<T> {
        let a = addr(selector, scope);
        let mut v = MaybeUninit::<T>::uninit();
        let mut size = u32::try_from(size_of::<T>()).ok()?;
        let st = unsafe {
            AudioObjectGetPropertyData(
                id,
                &a,
                0,
                std::ptr::null(),
                &mut size,
                v.as_mut_ptr().cast(),
            )
        };
        (st == 0 && size as usize == size_of::<T>()).then(|| unsafe { v.assume_init() })
    }

    /// A variable-size property, in a pointer-aligned buffer.
    fn read_bytes(id: u32, selector: &[u8; 4], scope: &[u8; 4]) -> Option<Vec<u8>> {
        let a = addr(selector, scope);
        let mut size = 0u32;
        let st = unsafe { AudioObjectGetPropertyDataSize(id, &a, 0, std::ptr::null(), &mut size) };
        if st != 0 {
            return None;
        }
        let words = (size as usize).div_ceil(size_of::<u64>());
        let mut buf = vec![0u64; words];
        let st = unsafe {
            AudioObjectGetPropertyData(
                id,
                &a,
                0,
                std::ptr::null(),
                &mut size,
                buf.as_mut_ptr().cast(),
            )
        };
        if st != 0 {
            return None;
        }
        let len = (size as usize).min(words * size_of::<u64>());
        let bytes = unsafe { std::slice::from_raw_parts(buf.as_ptr().cast::<u8>(), len) };
        Some(bytes.to_vec())
    }

    /// A CFString property, released after copying it out.
    fn read_string(id: u32, selector: &[u8; 4]) -> Option<String> {
        let s: *const c_void = read(id, selector, SCOPE_GLOBAL)?;
        if s.is_null() {
            return None;
        }
        let out = unsafe {
            let cap = CFStringGetMaximumSizeForEncoding(CFStringGetLength(s), UTF8) + 1;
            let mut buf = vec![0u8; usize::try_from(cap).unwrap_or(0).max(1)];
            let ok = CFStringGetCString(s, buf.as_mut_ptr(), cap, UTF8) != 0;
            CFRelease(s);
            ok.then(|| {
                let end = buf.iter().position(|b| *b == 0).unwrap_or(buf.len());
                String::from_utf8_lossy(&buf[..end]).into_owned()
            })
        };
        out.filter(|n| !n.is_empty())
    }

    /// The real HAL. Reads properties only: no AudioUnit, so no TCC prompt and no hang.
    pub struct SystemHal;

    impl Hal for SystemHal {
        fn device_ids(&self) -> Result<Vec<u32>> {
            let b = read_bytes(SYSTEM_OBJECT, DEVICES, SCOPE_GLOBAL).ok_or_else(|| {
                AudioError::new(
                    "backend_unavailable",
                    "CoreAudio: reading the device list failed",
                )
            })?;
            Ok(b.chunks_exact(4)
                .map(|c| u32::from_ne_bytes([c[0], c[1], c[2], c[3]]))
                .collect())
        }

        fn default_input(&self) -> Option<u32> {
            read::<u32>(SYSTEM_OBJECT, DEFAULT_INPUT, SCOPE_GLOBAL).filter(|id| *id != 0)
        }

        fn name(&self, id: u32) -> Option<String> {
            read_string(id, OBJECT_NAME)
        }

        fn uid(&self, id: u32) -> Option<String> {
            read_string(id, DEVICE_UID)
        }

        fn channels(&self, id: u32, scope: Scope) -> Option<u32> {
            let scope = match scope {
                Scope::Input => SCOPE_INPUT,
                Scope::Output => SCOPE_OUTPUT,
            };
            read_bytes(id, STREAM_CONFIGURATION, scope).map(|b| buffer_list_channels(&b))
        }

        fn nominal_rate(&self, id: u32) -> Option<f64> {
            read::<f64>(id, NOMINAL_SAMPLE_RATE, SCOPE_GLOBAL)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    struct FakeDev {
        id: u32,
        name: Option<&'static str>,
        inputs: u32,
        outputs: u32,
    }

    /// A fake HAL: property reads only, as the real one; `reads` counts them.
    struct FakeHal {
        devs: Vec<FakeDev>,
        default_in: Option<u32>,
        fail: bool,
        reads: Cell<usize>,
    }

    impl FakeHal {
        fn dev(&self, id: u32) -> Option<&FakeDev> {
            self.reads.set(self.reads.get() + 1);
            self.devs.iter().find(|d| d.id == id)
        }
    }

    impl Hal for FakeHal {
        fn device_ids(&self) -> Result<Vec<u32>> {
            if self.fail {
                return Err(AudioError::new("backend_unavailable", "fake"));
            }
            Ok(self.devs.iter().map(|d| d.id).collect())
        }
        fn default_input(&self) -> Option<u32> {
            self.default_in
        }
        fn name(&self, id: u32) -> Option<String> {
            self.dev(id)?.name.map(String::from)
        }
        fn uid(&self, id: u32) -> Option<String> {
            Some(format!("uid-{}", self.dev(id)?.id))
        }
        fn channels(&self, id: u32, scope: Scope) -> Option<u32> {
            let d = self.dev(id)?;
            Some(match scope {
                Scope::Input => d.inputs,
                Scope::Output => d.outputs,
            })
        }
        fn nominal_rate(&self, id: u32) -> Option<f64> {
            self.dev(id).map(|_| 48_000.0)
        }
    }

    fn macbook() -> FakeHal {
        FakeHal {
            devs: vec![
                FakeDev {
                    id: 73,
                    name: Some("MacBook Pro Microphone"),
                    inputs: 1,
                    outputs: 0,
                },
                FakeDev {
                    id: 80,
                    name: Some("MacBook Pro Speakers"),
                    inputs: 0,
                    outputs: 2,
                },
                FakeDev {
                    id: 90,
                    name: Some("BlackHole Loopback"),
                    inputs: 2,
                    outputs: 2,
                },
                FakeDev {
                    id: 99,
                    name: None,
                    inputs: 1,
                    outputs: 0,
                },
            ],
            default_in: Some(73),
            fail: false,
            reads: Cell::new(0),
        }
    }

    #[test]
    fn devices_carry_their_stream_configuration() {
        let hal = macbook();
        let d = devices(&hal).unwrap();
        assert_eq!(d.len(), 3, "the nameless device is skipped: {d:?}");
        assert_eq!(
            d[0],
            HalDevice {
                id: 73,
                uid: "uid-73".into(),
                name: "MacBook Pro Microphone".into(),
                input_channels: 1,
                output_channels: 0,
                sample_rate: Some(48_000.0),
                default_input: true,
            }
        );
        assert_eq!((d[1].input_channels, d[1].output_channels), (0, 2));
        assert!(hal.reads.get() > 0);
    }

    #[test]
    fn mics_are_input_devices_without_loopbacks() {
        let m = mics(&macbook(), "screencapturekit").unwrap();
        assert_eq!(m.len(), 1, "{m:?}");
        assert_eq!(m[0].id, "in:MacBook Pro Microphone");
        assert_eq!(m[0].kind, DeviceKind::Mic);
        assert_eq!(m[0].backend, "screencapturekit");
        assert!(m[0].default);
    }

    #[test]
    fn a_failed_device_list_is_an_error_not_a_hang() {
        let mut hal = macbook();
        hal.fail = true;
        assert_eq!(mics(&hal, "x").unwrap_err().code, "backend_unavailable");
    }

    #[test]
    fn mic_selectors_resolve_to_names() {
        let mut hal = macbook();
        let m = mics(&hal, "x").unwrap();
        assert_eq!(resolve_mic(&m, "").unwrap(), "MacBook Pro Microphone");
        assert_eq!(
            resolve_mic(&m, "in:MacBook Pro Microphone").unwrap(),
            "MacBook Pro Microphone"
        );
        assert_eq!(
            resolve_mic(&m, "MacBook Pro Microphone").unwrap(),
            "MacBook Pro Microphone"
        );
        assert_eq!(
            resolve_mic(&m, "nope").unwrap_err().code,
            "device_not_found"
        );
        hal.default_in = None;
        let m = mics(&hal, "x").unwrap();
        assert_eq!(resolve_mic(&m, "").unwrap(), "MacBook Pro Microphone");
        assert_eq!(resolve_mic(&[], "").unwrap_err().code, "device_not_found");
    }

    fn buffer_list(channels: &[u32]) -> Vec<u8> {
        let ptr = std::mem::size_of::<usize>();
        let mut b = vec![0u8; ptr + channels.len() * (8 + ptr)];
        b[..4].copy_from_slice(&(channels.len() as u32).to_ne_bytes());
        for (i, c) in channels.iter().enumerate() {
            let at = ptr + i * (8 + ptr);
            b[at..at + 4].copy_from_slice(&c.to_ne_bytes());
            b[at + 4..at + 8].copy_from_slice(&999u32.to_ne_bytes());
        }
        b
    }

    #[test]
    fn buffer_lists_sum_channels() {
        assert_eq!(buffer_list_channels(&buffer_list(&[])), 0);
        assert_eq!(buffer_list_channels(&buffer_list(&[2])), 2);
        assert_eq!(buffer_list_channels(&buffer_list(&[1, 1, 2])), 4);
        let ptr = std::mem::size_of::<usize>();
        let mut short = buffer_list(&[1, 2]);
        short.truncate(ptr + (8 + ptr) + 2);
        assert_eq!(
            buffer_list_channels(&short),
            1,
            "a truncated record is ignored"
        );
        assert_eq!(buffer_list_channels(&[1, 0]), 0);
    }
}
