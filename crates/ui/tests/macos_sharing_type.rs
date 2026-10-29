//! dot-4uzgol.15 runs this on a macOS machine with a display: `cargo test -p oats-ui --test macos_sharing_type`.
//! It opens a real GPUI window, applies the capture plan the engine would choose on macOS and reads
//! `NSWindow.sharingType` back: hiding on => 0 (NSWindowSharingNone), off => 1 (ReadOnly).
//! `harness = false`: GPUI must own the main thread. Elsewhere this prints a skip and passes.

#[cfg(target_os = "macos")]
fn main() {
    use gpui::{App, AppContext as _, Empty, WindowOptions};
    use oats_ui::capture::{apply, plan, read_sharing_type, Applied};
    let failed = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let f = failed.clone();
    gpui_platform::application().run(move |cx: &mut App| {
        let w = cx
            .open_window(WindowOptions::default(), |_, cx| cx.new(|_| Empty))
            .expect("window");
        w.update(cx, |_, window, _| {
            for (hide, method, want_applied, want_ty) in [
                (true, "sharing_type", Applied::Hidden, 0usize),
                (false, "sharing_type", Applied::Capturable, 1usize),
                (true, "none", Applied::Capturable, 1usize),
            ] {
                let got = apply(window, plan(hide, method));
                let ty = read_sharing_type(window);
                println!("hide={hide} method={method}: applied={got:?} sharingType={ty:?}");
                if got != want_applied || ty != Some(want_ty) {
                    f.store(true, std::sync::atomic::Ordering::SeqCst);
                }
            }
        })
        .expect("update");
        cx.quit();
    });
    if failed.load(std::sync::atomic::Ordering::SeqCst) {
        eprintln!("FAIL: sharingType did not match");
        std::process::exit(1);
    }
    println!("PASS");
}

#[cfg(not(target_os = "macos"))]
fn main() {
    println!("skipped: macOS only (dot-4uzgol.15)");
}
