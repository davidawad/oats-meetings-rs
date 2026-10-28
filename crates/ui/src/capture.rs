//! Hide the window from screen capture / sharing: `privacy.hide_from_capture` decides whether,
//! `privacy.hide_method` (set per OS by the os axis) decides how.
//!
//! - macOS `sharing_type`: `NSWindow.sharingType = NSWindowSharingNone (0)`; restored to
//!   `NSWindowSharingReadOnly (1)` when off.
//! - Windows `display_affinity`: `SetWindowDisplayAffinity(WDA_EXCLUDEFROMCAPTURE)`; `WDA_NONE`
//!   when off.
//! - Linux: nothing exists; `none`.
//!
//! The decision (`plan`) is pure and tested everywhere. Applying it to a real window is per OS
//! and verified on macOS by `tests/macos_sharing_type.rs` (dot-4uzgol.15 runs it).

use gpui::Window;
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Plan {
    /// Leave the window capturable (setting off, or method `none`).
    Visible,
    SharingNone,
    ExcludeFromCapture,
}

pub fn plan(hide: bool, method: &str) -> Plan {
    match (hide, method) {
        (true, "sharing_type") => Plan::SharingNone,
        (true, "display_affinity") => Plan::ExcludeFromCapture,
        _ => Plan::Visible,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Applied {
    /// The OS now excludes the window from capture.
    Hidden,
    /// The OS now allows capture.
    Capturable,
    /// The plan needs a platform this is not.
    WrongPlatform(&'static str),
    Failed(String),
}

/// Apply `plan` to a real window.
pub fn apply(window: &Window, plan: Plan) -> Applied {
    let handle = match window.window_handle() {
        Ok(h) => h.as_raw(),
        Err(e) => return Applied::Failed(format!("no window handle: {e}")),
    };
    apply_raw(handle, plan)
}

#[cfg(target_os = "macos")]
fn apply_raw(handle: RawWindowHandle, plan: Plan) -> Applied {
    use objc2::msg_send;
    use objc2::runtime::AnyObject;
    let (hide, ty) = match plan {
        Plan::SharingNone => (true, 0usize),
        Plan::Visible => (false, 1usize),
        Plan::ExcludeFromCapture => return Applied::WrongPlatform("display_affinity is Windows-only"),
    };
    let RawWindowHandle::AppKit(h) = handle else {
        return Applied::Failed("not an AppKit window".into());
    };
    // SAFETY: `ns_view` is the live NSView of this window (raw-window-handle contract); `window`
    // and `setSharingType:` are plain AppKit messages, sent on the main thread (GPUI's thread).
    unsafe {
        let view = h.ns_view.as_ptr() as *mut AnyObject;
        let win: *mut AnyObject = msg_send![view, window];
        if win.is_null() {
            return Applied::Failed("view has no window yet".into());
        }
        let _: () = msg_send![win, setSharingType: ty];
    }
    if hide { Applied::Hidden } else { Applied::Capturable }
}

/// Read back `NSWindow.sharingType` (0 none, 1 read-only, 2 read-write) for the .15 check.
#[cfg(target_os = "macos")]
pub fn read_sharing_type(window: &Window) -> Option<usize> {
    use objc2::msg_send;
    use objc2::runtime::AnyObject;
    let RawWindowHandle::AppKit(h) = window.window_handle().ok()?.as_raw() else {
        return None;
    };
    // SAFETY: as in `apply_raw`.
    unsafe {
        let view = h.ns_view.as_ptr() as *mut AnyObject;
        let win: *mut AnyObject = msg_send![view, window];
        if win.is_null() {
            return None;
        }
        let ty: usize = msg_send![win, sharingType];
        Some(ty)
    }
}

#[cfg(target_os = "windows")]
fn apply_raw(handle: RawWindowHandle, plan: Plan) -> Applied {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{
        SetWindowDisplayAffinity, WDA_EXCLUDEFROMCAPTURE, WDA_NONE,
    };
    let (hide, aff) = match plan {
        Plan::ExcludeFromCapture => (true, WDA_EXCLUDEFROMCAPTURE),
        Plan::Visible => (false, WDA_NONE),
        Plan::SharingNone => return Applied::WrongPlatform("sharing_type is macOS-only"),
    };
    let RawWindowHandle::Win32(h) = handle else {
        return Applied::Failed("not a Win32 window".into());
    };
    let hwnd = HWND(h.hwnd.get() as *mut core::ffi::c_void);
    // SAFETY: `hwnd` is this process's live top-level window from raw-window-handle.
    match unsafe { SetWindowDisplayAffinity(hwnd, aff) } {
        Ok(()) if hide => Applied::Hidden,
        Ok(()) => Applied::Capturable,
        Err(e) => Applied::Failed(e.to_string()),
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn apply_raw(_handle: RawWindowHandle, plan: Plan) -> Applied {
    match plan {
        Plan::Visible => Applied::Capturable,
        Plan::SharingNone => Applied::WrongPlatform("sharing_type is macOS-only"),
        Plan::ExcludeFromCapture => Applied::WrongPlatform("display_affinity is Windows-only"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use oats_core::config::keys::HIDE_METHODS;

    #[test]
    fn plan_covers_every_setting_and_method() {
        for method in HIDE_METHODS {
            assert_eq!(plan(false, method), Plan::Visible, "off + {method}");
        }
        assert_eq!(plan(true, "none"), Plan::Visible);
        assert_eq!(plan(true, "sharing_type"), Plan::SharingNone);
        assert_eq!(plan(true, "display_affinity"), Plan::ExcludeFromCapture);
        assert_eq!(plan(true, "bogus"), Plan::Visible);
    }

    #[test]
    fn the_os_axis_picks_a_method_the_ui_can_apply() {
        // config/axes.json sets privacy.hide_method per OS; each value must be a known method.
        let (engine, _d) = crate::testutil::engine(&[]);
        let m = engine.resolved().flat["privacy.hide_method"].as_str().unwrap().to_string();
        assert!(HIDE_METHODS.contains(&m.as_str()));
    }
}
