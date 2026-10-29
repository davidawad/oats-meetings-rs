//! dot-4uzgol.52 runs this on a macOS machine: `cargo test -p oats-ui --test macos_text_system`.
//! Without gpui_platform's `font-kit` feature gpui_macos installs `NoopTextSystem`, which knows no
//! fonts and draws no glyphs. This asserts the real text system is in place: it lists fonts and
//! gives 'm' a positive advance in the system UI font.
//! `harness = false`: GPUI must own the main thread. Elsewhere this prints a skip and passes.

#[cfg(target_os = "macos")]
fn main() {
    use gpui::{font, px, App};
    let failed = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let f = failed.clone();
    gpui_platform::application().run(move |cx: &mut App| {
        let ts = cx.text_system().clone();
        let names = ts.all_font_names();
        println!("font names: {}", names.len());
        if names.is_empty() {
            f.store(true, std::sync::atomic::Ordering::SeqCst);
        }
        let id = ts.resolve_font(&font(".SystemUIFont"));
        match ts.advance(id, px(14.), 'm') {
            Ok(adv) => {
                println!("advance of 'm' in .SystemUIFont at 14px: {:?}", adv.width);
                if adv.width <= px(0.) {
                    f.store(true, std::sync::atomic::Ordering::SeqCst);
                }
            }
            Err(e) => {
                println!("advance of 'm' failed: {e:#}");
                f.store(true, std::sync::atomic::Ordering::SeqCst);
            }
        }
        cx.quit();
    });
    if failed.load(std::sync::atomic::Ordering::SeqCst) {
        eprintln!("FAIL: no working text system (gpui_platform built without font-kit?)");
        std::process::exit(1);
    }
    println!("PASS");
}

#[cfg(not(target_os = "macos"))]
fn main() {
    println!("skipped: macOS only (dot-4uzgol.52)");
}
