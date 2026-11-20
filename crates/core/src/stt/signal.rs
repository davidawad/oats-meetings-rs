//! Process-wide signal collection and transcription progress records.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::json;
use signal_hook::consts::signal::{SIGHUP, SIGINT, SIGQUIT, SIGTERM};
use signal_hook::iterator::Signals;

#[derive(Clone, Copy)]
struct SignalEvent {
    signal: i32,
    at: Instant,
}

#[derive(Default)]
struct SignalHub {
    events: Mutex<Vec<SignalEvent>>,
    watchers: Mutex<Vec<std::thread::Thread>>,
    progress: Mutex<BTreeMap<String, SessionProgress>>,
}

struct SessionProgress {
    started: Instant,
    last_progress: Instant,
    last_progress_at_ms: u64,
    processed_s: f64,
    total_s: f64,
    phase: String,
}

static HUB: OnceLock<Arc<SignalHub>> = OnceLock::new();

/// Installs the process-wide handler once. Its iterator stays alive for the process lifetime.
pub(crate) fn install() -> std::io::Result<()> {
    if HUB.get().is_some() {
        return Ok(());
    }
    let mut signals = Signals::new([SIGTERM, SIGINT, SIGHUP, SIGQUIT])?;
    let hub = Arc::new(SignalHub::default());
    if HUB.set(Arc::clone(&hub)).is_err() {
        return Ok(());
    }
    std::thread::Builder::new()
        .name("oats-signals".into())
        .spawn(move || {
            for signal in signals.forever() {
                hub.events
                    .lock()
                    .expect("signal event lock")
                    .push(SignalEvent {
                        signal,
                        at: Instant::now(),
                    });
                for watcher in hub.watchers.lock().expect("signal watchers").iter() {
                    watcher.unpark();
                }
            }
        })?;
    Ok(())
}

fn hub() -> std::io::Result<&'static Arc<SignalHub>> {
    install()?;
    Ok(HUB.get().expect("signal hub installed"))
}

pub(crate) fn watch_current_thread() -> std::io::Result<()> {
    hub()?
        .watchers
        .lock()
        .expect("signal watchers")
        .push(std::thread::current());
    Ok(())
}

pub(crate) fn received_count() -> usize {
    HUB.get()
        .map(|hub| hub.events.lock().expect("signal event lock").len())
        .unwrap_or_default()
}

pub(crate) fn latest_signal() -> Option<&'static str> {
    HUB.get().and_then(|hub| {
        hub.events
            .lock()
            .expect("signal event lock")
            .last()
            .map(|event| signal_name(event.signal))
    })
}

pub(crate) fn force_exit_requested_after(count: usize, window: Duration) -> bool {
    HUB.get().is_some_and(|hub| {
        let events = hub.events.lock().expect("signal event lock");
        let mut previous_termination = events
            .get(count.saturating_sub(1))
            .filter(|event| matches!(event.signal, SIGINT | SIGTERM))
            .map(|event| event.at);
        for event in events.iter().skip(count) {
            if matches!(event.signal, SIGINT | SIGTERM) {
                if previous_termination
                    .is_some_and(|previous| event.at.duration_since(previous) <= window)
                {
                    return true;
                }
                previous_termination = Some(event.at);
            }
        }
        false
    })
}

pub(crate) fn stalled_sessions(timeout: Duration) -> Vec<String> {
    let Some(hub) = HUB.get() else {
        return Vec::new();
    };
    let events = hub.events.lock().expect("signal event lock");
    let count = events.len();
    let signal = events
        .last()
        .map(|event| signal_name(event.signal))
        .unwrap_or("unknown");
    let mut stalled = Vec::new();
    for (session, progress) in hub.progress.lock().expect("session progress").iter() {
        let last_progress_s_ago = progress.last_progress.elapsed().as_secs_f64();
        if last_progress_s_ago < timeout.as_secs_f64() {
            continue;
        }
        let elapsed = progress.started.elapsed().as_secs_f64();
        let rate = if elapsed > 0.0 {
            progress.processed_s / elapsed
        } else {
            0.0
        };
        let remaining = (progress.total_s - progress.processed_s).max(0.0);
        crate::log::line(json!({
            "event": "signal_stalled",
            "signal": signal,
            "count": count,
            "session_id": session,
            "phase": progress.phase,
            "processed_s": progress.processed_s,
            "total_s": progress.total_s,
            "percent": if progress.total_s > 0.0 { progress.processed_s * 100.0 / progress.total_s } else { 100.0 },
            "rate": rate,
            "eta_s": if rate > 0.0 { remaining / rate } else { 0.0 },
            "pid": std::process::id(),
            "last_progress_at_ms": progress.last_progress_at_ms,
            "last_progress_s_ago": last_progress_s_ago,
            "decision": "pending_for_resume",
        }));
        stalled.push(session.clone());
    }
    stalled
}

/// A single transcription's view of the process-wide signal stream.
pub(super) struct SignalMonitor {
    session: String,
    started: Instant,
    last_progress: Instant,
    last_progress_at_ms: u64,
    seen: usize,
    received: usize,
    deferred: bool,
    last_signal: String,
    last_signal_at: Option<Instant>,
    last_processed_s: f64,
    stall_timeout: Duration,
    force_window: Duration,
}

impl SignalMonitor {
    pub(super) fn new(
        session: &str,
        total_s: f64,
        stall_timeout: Duration,
        force_window: Duration,
    ) -> std::io::Result<Self> {
        let hub = hub()?;
        let seen = hub.events.lock().expect("signal event lock").len();
        let monitor = Self {
            session: session.to_owned(),
            started: Instant::now(),
            last_progress: Instant::now(),
            last_progress_at_ms: unix_time_ms(),
            seen,
            received: 0,
            deferred: false,
            last_signal: String::new(),
            last_signal_at: None,
            last_processed_s: 0.0,
            stall_timeout,
            force_window,
        };
        hub.progress.lock().expect("session progress").insert(
            session.to_owned(),
            SessionProgress {
                started: monitor.started,
                last_progress: monitor.last_progress,
                last_progress_at_ms: monitor.last_progress_at_ms,
                processed_s: 0.0,
                total_s,
                phase: "starting".to_owned(),
            },
        );
        crate::log::line(json!({
            "event": "transcription_start",
            "session_id": session,
            "phase": "starting",
            "processed_s": 0.0,
            "total_s": total_s,
            "stall_timeout_s": stall_timeout.as_secs_f64(),
            "pid": std::process::id(),
        }));
        Ok(monitor)
    }

    pub(super) fn progress(
        &mut self,
        phase: &str,
        processed_s: f64,
        total_s: f64,
    ) -> crate::config::Result<()> {
        if processed_s > self.last_processed_s {
            self.last_processed_s = processed_s;
            self.last_progress = Instant::now();
            self.last_progress_at_ms = unix_time_ms();
        }
        if let Ok(hub) = hub() {
            if let Some(progress) = hub
                .progress
                .lock()
                .expect("session progress")
                .get_mut(&self.session)
            {
                progress.phase = phase.to_owned();
                progress.total_s = total_s;
                if processed_s > progress.processed_s {
                    progress.processed_s = processed_s;
                    progress.last_progress = self.last_progress;
                    progress.last_progress_at_ms = self.last_progress_at_ms;
                }
            }
        }
        let events = hub()
            .map_err(|error| crate::config::ConfigError::new("signal_handler", error.to_string()))?
            .events
            .lock()
            .expect("signal event lock");
        let mut previous_signal_at = self.last_signal_at;
        let mut force_exit = false;
        for event in events.iter().skip(self.seen) {
            self.received += 1;
            self.deferred = true;
            let previous_is_terminating = matches!(self.last_signal.as_str(), "SIGINT" | "SIGTERM");
            self.last_signal = signal_name(event.signal).to_owned();
            if previous_is_terminating
                && matches!(event.signal, SIGINT | SIGTERM)
                && previous_signal_at
                    .is_some_and(|at| event.at.duration_since(at) <= self.force_window)
            {
                force_exit = true;
            }
            self.last_signal_at = Some(event.at);
            previous_signal_at = Some(event.at);
            self.log_progress(
                "signal_decision",
                phase,
                processed_s,
                total_s,
                if force_exit {
                    "force_exit_pending"
                } else {
                    "deferring_exit"
                },
            );
        }
        self.seen = events.len();
        if self.deferred {
            let stalled = self.last_progress.elapsed() >= self.stall_timeout;
            let decision = if stalled || force_exit {
                "pending_for_resume"
            } else {
                "deferring_exit"
            };
            self.log_progress(
                "transcription_progress",
                phase,
                processed_s,
                total_s,
                decision,
            );
            if stalled || force_exit {
                return Err(crate::config::ConfigError::new(
                    "transcription_stalled",
                    "transcription stopped after a termination signal",
                ));
            }
        }
        Ok(())
    }

    pub(super) fn finish(&mut self, phase: &str, decision: &str, total_s: f64) {
        let processed_s = if decision == "completed" {
            total_s
        } else {
            self.last_processed_s
        };
        if decision == "completed" {
            let _ = self.progress(phase, processed_s, total_s);
        }
        self.log_progress("transcription_end", phase, processed_s, total_s, decision);
    }

    fn log_progress(
        &self,
        event: &str,
        phase: &str,
        processed_s: f64,
        total_s: f64,
        decision: &str,
    ) {
        let elapsed = self.started.elapsed().as_secs_f64();
        let rate = if elapsed > 0.0 {
            processed_s / elapsed
        } else {
            0.0
        };
        let remaining = (total_s - processed_s).max(0.0);
        crate::log::line(json!({
            "event": event,
            "signal": self.last_signal,
            "count": self.received,
            "session_id": self.session,
            "phase": phase,
            "processed_s": processed_s,
            "total_s": total_s,
            "percent": if total_s > 0.0 { processed_s * 100.0 / total_s } else { 100.0 },
            "rate": rate,
            "eta_s": if rate > 0.0 { remaining / rate } else { 0.0 },
            "pid": std::process::id(),
            "last_progress_at_ms": self.last_progress_at_ms,
            "last_progress_s_ago": self.last_progress.elapsed().as_secs_f64(),
            "decision": decision,
        }));
    }
}

impl Drop for SignalMonitor {
    fn drop(&mut self) {
        if let Some(hub) = HUB.get() {
            hub.progress
                .lock()
                .expect("session progress")
                .remove(&self.session);
        }
    }
}

fn signal_name(signal: i32) -> &'static str {
    match signal {
        SIGTERM => "SIGTERM",
        SIGINT => "SIGINT",
        SIGHUP => "SIGHUP",
        SIGQUIT => "SIGQUIT",
        _ => "unknown",
    }
}

fn unix_time_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::time::{Duration, Instant};

    use signal_hook::consts::SIGTERM;

    use super::{hub, SignalEvent, SignalMonitor};

    static TEST_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn one_process_signal_source_is_shared_and_progress_defers_exit() {
        let _guard = TEST_LOCK.lock().expect("serialize signal tests");
        let mut monitor = SignalMonitor::new(
            "signal-test",
            4.0,
            Duration::from_secs(10),
            Duration::from_secs(1),
        )
        .expect("install process signal source");
        hub()
            .expect("signal source")
            .events
            .lock()
            .expect("signal event lock")
            .push(SignalEvent {
                signal: SIGTERM,
                at: Instant::now(),
            });
        monitor
            .progress("final_you", 1.0, 4.0)
            .expect("signal is deferred while progress continues");
        assert!(monitor.deferred, "termination waits for the final pass");
        assert_eq!(monitor.received, 1, "one signal was observed");
    }

    #[test]
    fn stalled_progress_returns_for_resume_without_raising_a_process_signal() {
        let _guard = TEST_LOCK.lock().expect("serialize signal tests");
        let mut monitor = SignalMonitor::new(
            "stalled-signal-test",
            4.0,
            Duration::from_millis(1),
            Duration::from_secs(1),
        )
        .expect("install process signal source");
        hub()
            .expect("signal source")
            .events
            .lock()
            .expect("signal event lock")
            .push(SignalEvent {
                signal: SIGTERM,
                at: Instant::now(),
            });
        std::thread::sleep(Duration::from_millis(2));
        let error = monitor
            .progress("final_you", 0.0, 4.0)
            .expect_err("a stalled pass must be left pending");
        assert_eq!(error.code, "transcription_stalled");
    }

    #[test]
    fn second_termination_signal_in_the_force_window_marks_the_pass_pending() {
        let _guard = TEST_LOCK.lock().expect("serialize signal tests");
        let mut monitor = SignalMonitor::new(
            "forced-signal-test",
            4.0,
            Duration::from_secs(10),
            Duration::from_secs(1),
        )
        .expect("install process signal source");
        let now = Instant::now();
        hub()
            .expect("signal source")
            .events
            .lock()
            .expect("signal event lock")
            .extend([
                SignalEvent {
                    signal: SIGTERM,
                    at: now,
                },
                SignalEvent {
                    signal: SIGTERM,
                    at: now,
                },
            ]);
        let error = monitor
            .progress("final_you", 0.0, 4.0)
            .expect_err("a second termination signal forces a pending exit");
        assert_eq!(error.code, "transcription_stalled");
        assert_eq!(monitor.received, 2, "both signals were recorded");
    }
}
