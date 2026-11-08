//! A model confined to a thread of its own (dot-4uzgol.107). onnxruntime and its CoreML provider
//! leave state on the threads that load and run a model: MLAS's per-thread packing buffers,
//! thread-local caches, and on macOS objects autoreleased into a thread's pool, which a
//! long-lived recording or control thread never drains. Freeing the model frees none of that,
//! so on sodium 280-400 MB stayed in the footprint after both sherpa-onnx models were unloaded.
//! Here the model is made, used and dropped on one thread that exits with it, so everything it
//! left on that thread goes too; on macOS each call runs in an autorelease pool of its own.

use std::sync::mpsc::{self, Sender};
use std::sync::Mutex;
use std::thread::JoinHandle;

type Job<T> = Box<dyn FnOnce(&T) + Send>;

pub struct Confined<T> {
    jobs: Mutex<Option<Sender<Job<T>>>>,
    thread: Option<JoinHandle<()>>,
}

/// Run `f` in an autorelease pool of its own on macOS (what CoreML frees is released at its end).
pub(super) fn pooled<R>(f: impl FnOnce() -> R) -> R {
    #[cfg(target_os = "macos")]
    return objc2::rc::autoreleasepool(|_| f());
    #[cfg(not(target_os = "macos"))]
    f()
}

impl<T: 'static> Confined<T> {
    /// Make the model with `make` on a new thread `name`; `None` when `make` gives none (the
    /// thread has exited by then).
    pub fn spawn(name: &str, make: impl FnOnce() -> Option<T> + Send + 'static) -> Option<Self> {
        let (jobs, rx) = mpsc::channel::<Job<T>>();
        let (made, ready) = mpsc::channel::<bool>();
        let thread = std::thread::Builder::new()
            .name(name.into())
            .spawn(move || {
                let Some(model) = pooled(make) else {
                    let _ = made.send(false);
                    return;
                };
                let _ = made.send(true);
                // Until the last handle drops the sender.
                while let Ok(job) = rx.recv() {
                    pooled(|| job(&model));
                }
                pooled(|| drop(model));
            })
            .ok()?;
        if ready.recv().unwrap_or(false) {
            Some(Confined {
                jobs: Mutex::new(Some(jobs)),
                thread: Some(thread),
            })
        } else {
            let _ = thread.join();
            None
        }
    }

    /// Run `f` on the model's thread and wait for its result.
    pub fn run<R: Send + 'static>(&self, f: impl FnOnce(&T) -> R + Send + 'static) -> R {
        let (tx, rx) = mpsc::channel();
        self.jobs
            .lock()
            .expect("confined model")
            .as_ref()
            .expect("confined model thread")
            .send(Box::new(move |m: &T| {
                let _ = tx.send(f(m));
            }))
            .expect("confined model thread");
        rx.recv().expect("confined model thread panicked")
    }
}

impl<T> Drop for Confined<T> {
    /// The model is dropped on its thread, and the thread has exited, before this returns.
    fn drop(&mut self) {
        drop(self.jobs.lock().map(|mut j| j.take()));
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    struct Model {
        dropped: Arc<AtomicBool>,
        home: std::thread::ThreadId,
    }

    impl Drop for Model {
        fn drop(&mut self) {
            assert_eq!(std::thread::current().id(), self.home);
            self.dropped.store(true, Ordering::SeqCst);
        }
    }

    #[test]
    fn the_model_lives_runs_and_dies_on_its_own_thread() {
        let dropped = Arc::new(AtomicBool::new(false));
        let d = Arc::clone(&dropped);
        let c = Confined::spawn("oats-test-model", move || {
            Some(Model {
                dropped: d,
                home: std::thread::current().id(),
            })
        })
        .unwrap();
        let here = std::thread::current().id();
        let (name, home) = c.run(|m| (std::thread::current().name().map(String::from), m.home));
        assert_eq!(name.as_deref(), Some("oats-test-model"));
        assert_ne!(home, here);
        assert!(c.run(move |m| std::thread::current().id() == m.home));
        drop(c);
        assert!(dropped.load(Ordering::SeqCst));
    }

    #[test]
    fn no_model_no_handle() {
        assert!(Confined::<u8>::spawn("oats-test-model", || None).is_none());
    }
}
