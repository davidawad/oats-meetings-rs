//! dot-4uzgol.56 runs this on a macOS machine with a display:
//! `cargo test -p oats-ui --test macos_window_visible`.
//! It opens the main window the way launch does (`host::window_options(cx, false)`, no
//! `cx.activate`) and reads AppKit back: the window is visible with a non-zero frame, is not key,
//! and the app did not make itself active. Then it closes the window and shows it again the way a
//! show request without activation does (`capture::order_front`): visible, non-zero frame again.
//! `harness = false`: GPUI must own the main thread. Elsewhere this prints a skip and passes.

#[cfg(target_os = "macos")]
fn main() {
    use gpui::{App, AppContext as _, AsyncApp, Empty};
    use oats_ui::capture::{app_is_active, order_front, read_window_state};
    use oats_ui::host::window_options;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    let failed = Arc::new(AtomicBool::new(false));
    let f = failed.clone();
    gpui_platform::application().run(move |cx: &mut App| {
        let check = |what: &str, st: Option<(bool, bool, f64, f64)>, f: &AtomicBool| {
            println!("{what}: (visible, key, width, height) = {st:?}");
            match st {
                Some((true, _, w, h)) if w > 0.0 && h > 0.0 => {}
                _ => {
                    eprintln!("FAIL: {what}: window not on screen with a non-zero frame");
                    f.store(true, Ordering::SeqCst);
                }
            }
        };
        let w = cx
            .open_window(window_options(cx, false), |_, cx| cx.new(|_| Empty))
            .expect("window");
        let st = w
            .update(cx, |_, window, _| read_window_state(window))
            .expect("update");
        check("launch", st, &f);
        if st.is_some_and(|(_, key, _, _)| key) {
            eprintln!("FAIL: launch: the window became key (focus steal)");
            f.store(true, Ordering::SeqCst);
        }
        let f = f.clone();
        cx.spawn(async move |cx: &mut AsyncApp| {
            // Let the run loop turn: activation from makeKeyAndOrderFront lands asynchronously.
            cx.background_executor()
                .timer(Duration::from_millis(500))
                .await;
            let _ = cx.update(|cx| {
                let active = app_is_active();
                println!("after launch: app active = {active}");
                if active {
                    eprintln!("FAIL: the app activated itself on launch");
                    f.store(true, Ordering::SeqCst);
                }
                let _ = w.update(cx, |_, window, _| window.remove_window());
                let w2 = cx
                    .open_window(window_options(cx, false), |_, cx| cx.new(|_| Empty))
                    .expect("window");
                let st = w2
                    .update(cx, |_, window, _| {
                        order_front(window);
                        read_window_state(window)
                    })
                    .expect("update");
                check("show", st, &f);
                cx.quit();
            });
        })
        .detach();
    });
    if failed.load(Ordering::SeqCst) {
        eprintln!("FAIL: window lifecycle");
        std::process::exit(1);
    }
    println!("PASS");
}

#[cfg(not(target_os = "macos"))]
fn main() {
    println!("skipped: macOS only (dot-4uzgol.56)");
}
