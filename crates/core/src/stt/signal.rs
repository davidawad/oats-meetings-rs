//! Signal-aware progress records for a transcription pass.

use std::time::Instant;

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
}

impl SignalMonitor {
    pub(super) fn new(session: &str, total_s: f64) -> std::io::Result<Self> {
        let monitor = Self {
            signals: Signals::new([SIGTERM, SIGINT, SIGHUP, SIGQUIT])?,
            session: session.to_owned(),
            started: Instant::now(),
            last_progress: Instant::now(),
            received: 0,
            deferred: false,
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

    pub(super) fn progress(&mut self, phase: &str, processed_s: f64, total_s: f64) {
        self.last_progress = Instant::now();
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
            self.log_progress("signal_decision", signal_name, phase, processed_s, total_s);
        }
        if self.deferred {
            self.log_progress("transcription_progress", "", phase, processed_s, total_s);
        }
    }

    pub(super) fn finish(&mut self, phase: &str, total_s: f64) {
        self.progress(phase, total_s, total_s);
        crate::log::line(json!({
            "event": "transcription_end",
            "session_id": self.session,
            "phase": phase,
            "signal_count": self.received,
            "decision": "completed",
            "processed_s": total_s,
            "total_s": total_s,
            "pid": std::process::id(),
        }));
    }

    fn log_progress(
        &self,
        event: &str,
        signal_name: &str,
        phase: &str,
        processed_s: f64,
        total_s: f64,
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
            "decision": "deferring_exit",
        }));
    }
}

#[cfg(test)]
mod tests {
    use super::SignalMonitor;

    #[test]
    fn signal_is_deferred_until_a_progress_checkpoint() {
        let mut monitor = SignalMonitor::new("signal-test", 4.0).expect("register signals");
        signal_hook::low_level::raise(signal_hook::consts::SIGTERM).expect("raise test signal");
        assert_eq!(monitor.received, 0, "signal is consumed at a checkpoint");

        monitor.progress("final_you", 1.0, 4.0);

        assert_eq!(monitor.received, 1, "one signal was recorded");
        assert!(
            monitor.deferred,
            "the signal asks the pass to finish before exit"
        );
    }
}
