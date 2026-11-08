//! Releasing the loaded models when transcription goes idle (dot-4uzgol.102). The recognisers
//! (whisper.cpp, sherpa-onnx, CoreML) and the speaker-embedding models are cached process-wide,
//! one of each, shared by the live and the final pass; a Parakeet int8 recogniser plus its
//! onnxruntime arenas holds well over a gigabyte. `touch` marks a use; a reaper thread drops every
//! cached model once none has been in use for `stt.local.unload_after` (0 keeps them loaded) and,
//! on glibc, hands the freed heap back to the OS. The next recording loads them again.
//!
//! Idle means idle (dot-4uzgol.106): nothing but a recording or a transcription loads a model.
//! Every load from disk goes through `loaded`, which counts it (`stt-status` `models.loads`) and
//! logs the thread that asked, so a load while idle names its caller; the diarizer's speaker model
//! starts the same idle clock as the recogniser. A cache outside this module's (an injected
//! `AsrFactory` that keeps its model) joins the reaper with `watch`.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

struct State {
    /// The last use seen; `None` once the models are unloaded (nothing to watch).
    last: Option<Instant>,
    after: Duration,
    started: bool,
}

/// Watches one set of caches: `in_use` says whether any cached model is held outside its cache
/// (a recording, a transcription), `unload` drops them all.
pub struct Reaper {
    state: Mutex<State>,
    wake: Condvar,
    in_use: fn() -> bool,
    unload: fn(),
}

impl Reaper {
    pub const fn new(in_use: fn() -> bool, unload: fn()) -> Reaper {
        Reaper {
            state: Mutex::new(State {
                last: None,
                after: Duration::ZERO,
                started: false,
            }),
            wake: Condvar::new(),
            in_use,
            unload,
        }
    }

    /// A model was just used; unload them all once `after` passes with none in use.
    pub fn touch(&'static self, after: Duration) {
        let mut st = self.state.lock().expect("idle reaper");
        st.last = Some(Instant::now());
        st.after = after;
        if !st.started {
            st.started = std::thread::Builder::new()
                .name("oats-stt-unload".into())
                .spawn(move || self.run())
                .is_ok();
        }
        self.wake.notify_all();
    }

    /// Whether the models are being watched (loaded and not yet unloaded).
    #[cfg(test)]
    fn watching(&self) -> bool {
        self.state.lock().expect("idle reaper").last.is_some()
    }

    fn run(&self) {
        let mut st = self.state.lock().expect("idle reaper");
        loop {
            let Some(last) = st.last.filter(|_| !st.after.is_zero()) else {
                st = self.wake.wait(st).expect("idle reaper");
                continue;
            };
            let idle = last.elapsed();
            if idle < st.after {
                let left = st.after - idle;
                st = self.wake.wait_timeout(st, left).expect("idle reaper").0;
                continue;
            }
            if (self.in_use)() {
                st.last = Some(Instant::now());
                continue;
            }
            st.last = None;
            drop(st);
            (self.unload)();
            st = self.state.lock().expect("idle reaper");
        }
    }
}

/// The process-wide models' reaper.
static MODELS: Reaper = Reaper::new(models_in_use, unload_models);

/// A model was just used (`stt.local.unload_after` is `after`).
pub fn touch(after: Duration) {
    MODELS.touch(after);
}

/// A model cache the reaper empties with the built-in ones.
pub trait Cache: Send + Sync {
    /// Whether its model is held outside the cache (a recording, a transcription).
    fn in_use(&self) -> bool;
    /// Drop the cached model.
    fn unload(&self);
}

static WATCHED: Mutex<Vec<Arc<dyn Cache>>> = Mutex::new(Vec::new());

/// Unload `cache` with the rest once `stt.local.unload_after` passes unused.
pub fn watch(cache: Arc<dyn Cache>) {
    WATCHED.lock().expect("idle caches").push(cache);
}

fn watched() -> Vec<Arc<dyn Cache>> {
    WATCHED.lock().expect("idle caches").clone()
}

static LOADS: AtomicU64 = AtomicU64::new(0);
static UNLOADS: AtomicU64 = AtomicU64::new(0);

/// `what` was just loaded from disk (not taken from its cache): counted, and logged with the
/// thread that asked for it.
pub fn loaded(what: &str) {
    LOADS.fetch_add(1, Ordering::SeqCst);
    let t = std::thread::current();
    crate::elog!(
        "oats-meetings-rs: loaded {what} (thread {})",
        t.name().unwrap_or("unnamed")
    );
}

/// The `models` object of `stt-status`: loads from disk and idle unloads since launch, and the
/// model worker processes alive now (one per loaded sherpa-onnx model).
pub fn status() -> serde_json::Value {
    #[cfg(feature = "parakeet")]
    let workers = super::worker::live().len();
    #[cfg(not(feature = "parakeet"))]
    let workers = 0;
    serde_json::json!({
        "loads": LOADS.load(Ordering::SeqCst),
        "unloads": UNLOADS.load(Ordering::SeqCst),
        "workers": workers,
    })
}

fn models_in_use() -> bool {
    if super::diarize::in_use() || watched().iter().any(|c| c.in_use()) {
        return true;
    }
    #[cfg(feature = "whisper")]
    if super::local::in_use() {
        return true;
    }
    #[cfg(feature = "parakeet")]
    if super::sherpa::in_use() {
        return true;
    }
    #[cfg(target_os = "macos")]
    if super::coreml::in_use() {
        return true;
    }
    false
}

/// Held for the whole of an unload, so a status read never sees one cache emptied while the
/// rest, and the heap they leave, are still being freed.
static UNLOADING: Mutex<()> = Mutex::new(());

/// Wait for an unload in progress, if any, to finish (the loaded-model status reads after it).
pub fn settled() {
    drop(UNLOADING.lock().expect("idle unload"));
}

/// Drop every cached model, then return the freed heap to the OS.
fn unload_models() {
    let _unloading = UNLOADING.lock().expect("idle unload");
    // CoreML (and onnxruntime's CoreML provider) autorelease what they free; this thread has no
    // pool of its own, so without one those objects would outlive the unload.
    #[cfg(target_os = "macos")]
    objc2::rc::autoreleasepool(|_| clear_caches());
    #[cfg(not(target_os = "macos"))]
    clear_caches();
    UNLOADS.fetch_add(1, Ordering::SeqCst);
    trim();
}

fn clear_caches() {
    #[cfg(feature = "whisper")]
    super::local::clear_cache();
    #[cfg(feature = "parakeet")]
    super::sherpa::clear_cache();
    #[cfg(target_os = "macos")]
    super::coreml::clear_cache();
    // The speaker models, and the diarizer's "active" in stt-status with them.
    super::diarize::clear();
    for c in watched() {
        c.unload();
    }
}

/// glibc keeps freed memory in its arenas, so RSS would stay at the models' size without this.
#[cfg(all(target_os = "linux", target_env = "gnu"))]
fn trim() {
    // SAFETY: malloc_trim only walks glibc's own arenas.
    unsafe {
        libc::malloc_trim(0);
    }
}

/// macOS keeps freed pages in its malloc zones, counted in the process footprint, until asked.
#[cfg(target_os = "macos")]
fn trim() {
    extern "C" {
        fn malloc_zone_pressure_relief(zone: *mut std::ffi::c_void, goal: usize) -> usize;
    }
    // SAFETY: a null zone asks every malloc zone to return what it can; goal 0 means all.
    unsafe {
        malloc_zone_pressure_relief(std::ptr::null_mut(), 0);
    }
}

#[cfg(not(any(all(target_os = "linux", target_env = "gnu"), target_os = "macos")))]
fn trim() {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    static BUSY: AtomicBool = AtomicBool::new(false);
    static UNLOADS: AtomicUsize = AtomicUsize::new(0);
    static FAKE: Reaper = Reaper::new(
        || BUSY.load(Ordering::SeqCst),
        || {
            UNLOADS.fetch_add(1, Ordering::SeqCst);
        },
    );

    fn wait_for(n: usize, within: Duration) -> bool {
        let t = Instant::now();
        while t.elapsed() < within {
            if UNLOADS.load(Ordering::SeqCst) >= n {
                return true;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        false
    }

    /// One test, since the fake reaper and its counters are shared.
    #[test]
    fn models_unload_after_the_idle_time_and_never_while_in_use() {
        let after = Duration::from_millis(150);
        // Idle: unloaded once `after` passes, not before.
        let t = Instant::now();
        FAKE.touch(after);
        assert!(FAKE.watching());
        assert!(wait_for(1, Duration::from_secs(5)));
        assert!(t.elapsed() >= after);
        assert!(!FAKE.watching());
        // Nothing loaded: no second unload.
        std::thread::sleep(after * 2);
        assert_eq!(UNLOADS.load(Ordering::SeqCst), 1);

        // In use (a long recording): kept, however long; unloaded `after` once released.
        BUSY.store(true, Ordering::SeqCst);
        FAKE.touch(after);
        std::thread::sleep(after * 4);
        assert_eq!(UNLOADS.load(Ordering::SeqCst), 1);
        let freed = Instant::now();
        BUSY.store(false, Ordering::SeqCst);
        assert!(wait_for(2, Duration::from_secs(5)));
        assert!(freed.elapsed() <= after * 3, "{:?}", freed.elapsed());

        // Each use restarts the clock.
        FAKE.touch(after);
        for _ in 0..4 {
            std::thread::sleep(after / 2);
            FAKE.touch(after);
        }
        assert_eq!(UNLOADS.load(Ordering::SeqCst), 2);
        assert!(wait_for(3, Duration::from_secs(5)));

        // 0 keeps them loaded.
        FAKE.touch(Duration::ZERO);
        std::thread::sleep(after * 2);
        assert_eq!(UNLOADS.load(Ordering::SeqCst), 3);
        assert!(FAKE.watching());
    }
}
