//! What the level meters and the record clock show, held apart from the window (dot-4uzgol.97).
//!
//! A tick that moves only a meter or the clock updates this entity and notifies it, never the
//! window: the window then redraws its chrome (record strip, status line) around the cached tab
//! body (`view::Body`), so an idle monitor never re-lays out or re-wraps a long transcript.
//! Repaints are throttled to `ui.meter_interval` and skipped while the change is below one of
//! `ui.meter_steps` visible steps.

use std::time::{Duration, Instant};

use crate::view::meter_fill;

#[derive(Debug, Clone, Default)]
pub struct Meters {
    /// The `(mic, system)` fills shown, in steps of `steps`.
    shown: (u32, u32),
    steps: u32,
    /// Where the levels come from: "recording", "monitor", or "none" (meters at rest).
    pub source: &'static str,
    /// The record clock's whole seconds, so it repaints once a second while recording.
    pub clock: u64,
    /// When the meters last repainted, for the `ui.meter_interval` throttle.
    last: Option<Instant>,
    /// How many times the meters asked for a repaint: `ui-state` reports it.
    pub repaints: u64,
}

/// One tick's reading for [`Meters::set`].
#[derive(Debug, Clone, Copy)]
pub struct Reading {
    pub levels: (f32, f32),
    pub source: &'static str,
    pub clock: u64,
    pub steps: u32,
    pub interval: Duration,
}

impl Meters {
    /// Take a tick's reading; true when what the meters show changed, so they need a repaint. A
    /// source, scale or clock change always shows; a level change shows once it moves a meter by
    /// a whole step and `interval` has passed since the last repaint.
    pub fn set(&mut self, r: Reading, now: Instant) -> bool {
        let steps = r.steps.max(1);
        let q = |rms: f32| (meter_fill(rms) * steps as f32).round() as u32;
        let shown = (q(r.levels.0), q(r.levels.1));
        let forced = r.source != self.source || r.clock != self.clock || steps != self.steps;
        if !forced
            && (shown == self.shown
                || self
                    .last
                    .is_some_and(|t| now.duration_since(t) < r.interval))
        {
            return false;
        }
        self.shown = shown;
        self.steps = steps;
        self.source = r.source;
        self.clock = r.clock;
        self.last = Some(now);
        self.repaints += 1;
        true
    }

    /// The `(mic, system)` meter fills, 0 to 1.
    pub fn fills(&self) -> (f32, f32) {
        if self.steps == 0 {
            return (0., 0.);
        }
        let s = self.steps as f32;
        (self.shown.0 as f32 / s, self.shown.1 as f32 / s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reading(levels: (f32, f32), source: &'static str, clock: u64) -> Reading {
        Reading {
            levels,
            source,
            clock,
            steps: 60,
            interval: Duration::from_millis(67),
        }
    }

    #[test]
    fn a_change_below_one_step_or_inside_the_interval_does_not_repaint() {
        let mut m = Meters::default();
        let t0 = Instant::now();
        assert!(m.set(reading((0.1, 0.1), "monitor", 0), t0));
        // Same reading, and a change well under one dB: no repaint.
        assert!(!m.set(
            reading((0.1, 0.1), "monitor", 0),
            t0 + Duration::from_secs(1)
        ));
        assert!(!m.set(
            reading((0.1005, 0.1), "monitor", 0),
            t0 + Duration::from_secs(1)
        ));
        // A visible change inside the interval waits for it.
        assert!(!m.set(
            reading((0.5, 0.1), "monitor", 0),
            t0 + Duration::from_millis(10)
        ));
        assert!(m.set(
            reading((0.5, 0.1), "monitor", 0),
            t0 + Duration::from_millis(70)
        ));
        assert!((m.fills().0 - meter_fill(0.5)).abs() <= 1. / 60.);
        assert_eq!(m.repaints, 2);
    }

    #[test]
    fn a_source_or_clock_change_repaints_at_once() {
        let mut m = Meters::default();
        let t0 = Instant::now();
        assert!(m.set(reading((0.5, 0.5), "monitor", 0), t0));
        assert!(m.set(reading((0., 0.), "none", 0), t0));
        assert_eq!(m.fills(), (0., 0.));
        assert!(m.set(reading((0., 0.), "recording", 1), t0));
        assert!(m.set(reading((0., 0.), "recording", 2), t0));
    }

    #[test]
    fn at_most_one_repaint_per_interval_however_fast_the_levels_move() {
        let mut m = Meters::default();
        let t0 = Instant::now();
        let mut n = 0;
        // 1000 readings over one second, every one a visible change.
        for i in 0..1000u64 {
            let lv = if i % 2 == 0 { 0.9 } else { 0.01 };
            if m.set(
                reading((lv, lv), "monitor", 0),
                t0 + Duration::from_millis(i),
            ) {
                n += 1;
            }
        }
        assert!(n <= 15, "{n} repaints in one second");
    }
}
