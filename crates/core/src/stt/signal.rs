//! Signal-aware progress records for a transcription pass.

use std::time::{Duration, Instant};

use serde_json::json;
use signal_hook::consts::signal::{SIGHUP, SIGINT, SIGQUIT, SIGTERM};
use signal_hook::iterator::Signals;

/// Collects termination signals while a session transcription is making progress.
pub(super) struct SignalMonitor {
    signals: Signals,
    session: String,
    started: Instant,
    last_progress: Instant,
    received: usize,
    deferred: bool,
    last_processed_s: f64,
    last_phase: String,
    stall_timeout: Duration,
}

impl SignalMonitor {
    pub(super) fn new(
        session: &str,
        total_s: f64,
        stall_timeout: Duration,
    ) -> std::io::Result<Self> {
        let monitor = Self {
            signals: Signals::new([SIGTERM, SIGINT, SIGHUP, SIGQUIT])?,
            session: session.to_owned(),
            started: Instant::now(),
            last_progress: Instant::now(),
            received: 0,
            deferred: false,
            last_processed_s: 0.0,
            last_phase: "starting".to_owned(),
            stall_timeout,
        };
        crate::log::line(json!({
            "event": "transcription_start",
            "session_id": session,
            "phase": "starting",
            "processed_s": 0.0,
            "total_s": total_s,
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
        if phase != self.last_phase || processed_s > self.last_processed_s {
            self.last_phase = phase.to_owned();
            self.last_processed_s = processed_s;
            self.last_progress = Instant::now();
        }
        for signal in self.signals.pending() {
            self.received += 1;
            self.deferred = true;
            let signal_name = match signal {
                SIGTERM => "SIGTERM",
                SIGINT => "SIGINT",
                SIGHUP => "SIGHUP",
                SIGQUIT => "SIGQUIT",
                _ => "unknown",
            };
            self.log_progress(
                "signal_decision",
                signal_name,
                phase,
                processed_s,
                total_s,
                "deferring_exit",
            );
        }
        if self.deferred {
            let stalled = self.last_progress.elapsed() >= self.stall_timeout;
            self.log_progress(
                "transcription_progress",
                "",
                phase,
                processed_s,
                total_s,
                if stalled {
                    "pending_for_resume"
                } else {
                    "deferring_exit"
                },
            );
            if stalled {
                return Err(crate::config::ConfigError::new(
                    "transcription_stalled",
                    "transcription stopped making progress after a termination signal",
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
        self.log_progress(
            "transcription_end",
            "",
            phase,
            processed_s,
            total_s,
            decision,
        );
    }

    fn log_progress(
        &self,
        event: &str,
        signal_name: &str,
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
        let eta_s = if rate > 0.0 { remaining / rate } else { 0.0 };
        crate::log::line(json!({
            "event": event,
            "signal": signal_name,
            "count": self.received,
            "session_id": self.session,
            "phase": phase,
            "processed_s": processed_s,
            "total_s": total_s,
            "percent": if total_s > 0.0 { processed_s * 100.0 / total_s } else { 100.0 },
            "rate": rate,
            "eta_s": eta_s,
            "pid": std::process::id(),
            "last_progress_s_ago": self.last_progress.elapsed().as_secs_f64(),
            "decision": decision,
        }));
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::time::Duration;

    use super::SignalMonitor;

    static SIGNAL_TEST_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn signal_is_deferred_until_a_progress_checkpoint() {
        let _guard = SIGNAL_TEST_LOCK.lock().expect("serialize process signals");
        let mut monitor = SignalMonitor::new("signal-test", 4.0, Duration::from_secs(1))
            .expect("register signals");
        signal_hook::low_level::raise(signal_hook::consts::SIGTERM).expect("raise test signal");
        assert_eq!(monitor.received, 0, "signal is consumed at a checkpoint");

        monitor
            .progress("final_you", 1.0, 4.0)
            .expect("progress remains within stall timeout");

        assert_eq!(monitor.received, 1, "one signal was recorded");
        assert!(
            monitor.deferred,
            "the signal asks the pass to finish before exit"
        );
    }

    #[test]
    fn signal_after_stall_marks_the_run_for_resume() {
        let _guard = SIGNAL_TEST_LOCK.lock().expect("serialize process signals");
        let mut monitor = SignalMonitor::new("stalled-signal-test", 4.0, Duration::from_millis(1))
            .expect("register signals");
        monitor
            .progress("final_you", 1.0, 4.0)
            .expect("initial progress is healthy");
        signal_hook::low_level::raise(signal_hook::consts::SIGTERM).expect("raise test signal");
        std::thread::sleep(Duration::from_millis(2));

        let error = monitor
            .progress("final_you", 1.0, 4.0)
            .expect_err("a signal after a progress stall must stop the pass");

        assert_eq!(error.code, "transcription_stalled");
        assert_eq!(monitor.received, 1, "one termination signal was recorded");
    }
}
