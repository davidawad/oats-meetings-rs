//! What the level meters and the record clock show, held apart from the window (dot-4uzgol.97).
//!
//! A tick that moves only a meter or the clock updates this entity and notifies it, never the
//! window: the window then redraws its chrome (record strip, status line) around the cached tab
//! body (`view::Body`), so an idle monitor never re-lays out or re-wraps a long transcript.
//! Repaints are throttled to `ui.meter_interval` and skipped while the change is below one of
//! `ui.meter_steps` visible steps.
//!
//! dot-4uzgol.103: a level repaint redraws the meter bars alone. Each bar is a [`Gauge`] view the
//! window paints over a [`slot`] the cached chrome leaves for it ([`layer`]), as it does the tab
//! content, so a level change re-renders neither the chrome (rail, record strip, status line) nor
//! the tab content; the clock and a source change redraw the chrome only. While the window is
//! occluded or minimised the meters do not repaint at all (`MainWindow::show_meters`).

use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::{
    canvas, prelude::*, AnyElement, AnyView, AvailableSpace, Bounds, Canvas, Context, Entity,
    Pixels, Size, StyleRefinement, Window,
};

use crate::kit;
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

/// A place the chrome leaves for a view drawn over it: a meter bar (the Live record strip's pair
/// or the status line's) or the tab content.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Slot {
    StripMic,
    StripSystem,
    StatusMic,
    StatusSystem,
    Body,
}

impl Slot {
    pub const GAUGES: [Slot; 4] = [
        Slot::StripMic,
        Slot::StripSystem,
        Slot::StatusMic,
        Slot::StatusSystem,
    ];

    fn mic(self) -> bool {
        matches!(self, Slot::StripMic | Slot::StatusMic)
    }
}

/// Where the chrome laid each slot out in its last render (`None`: not shown, e.g. the record
/// strip off Live). The chrome clears it as it renders and its [`slot`]s fill it in.
pub type Slots = Rc<RefCell<[Option<Bounds<Pixels>>; 5]>>;

/// An empty placeholder, styled by the caller, that records where it was laid out for the view
/// [`layer`] draws over it.
pub fn place(slots: &Slots, s: Slot) -> Canvas<()> {
    let slots = Rc::clone(slots);
    canvas(
        move |b, _, _| slots.borrow_mut()[s as usize] = Some(b),
        |_, _, _, _| {},
    )
}

/// A placeholder the size of a meter bar: 5px high, the width it is given.
pub fn slot(slots: &Slots, s: Slot) -> impl IntoElement {
    place(slots, s).h(gpui::px(5.)).flex_1()
}

/// One meter bar as its own view: it re-renders when the meters repaint (or the window is
/// notified, for a theme change), and nothing else does.
pub struct Gauge {
    meters: Entity<Meters>,
    slot: Slot,
    /// How many times this bar rendered: `ui-state` reports the sum as `meters.frames`.
    pub renders: u64,
}

impl Gauge {
    pub fn new<T: 'static>(
        meters: &Entity<Meters>,
        main: &Entity<T>,
        slot: Slot,
        cx: &mut Context<Self>,
    ) -> Self {
        cx.observe(meters, |_, _, cx| cx.notify()).detach();
        cx.observe(main, |_, _, cx| cx.notify()).detach();
        Self {
            meters: meters.clone(),
            slot,
            renders: 0,
        }
    }
}

impl Render for Gauge {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.renders += 1;
        let (mic, sys) = self.meters.read(cx).fills();
        kit::meter(if self.slot.mic() { mic } else { sys }).w_full()
    }
}

/// The views over the chrome (the tab content, then the meter bars): laid out and painted after
/// it, each at the bounds its slot took in the chrome's last layout. Cached, so each redraws only
/// when it is notified or its slot moves.
pub fn layer(slots: &Slots, views: Vec<(Slot, AnyView)>) -> impl IntoElement {
    let slots = Rc::clone(slots);
    canvas(
        move |_, window, cx| {
            // The chrome before it in the tree has prepainted: its slots are this frame's.
            let at = *slots.borrow();
            views
                .into_iter()
                .filter_map(|(s, view)| {
                    let b = at[s as usize]?;
                    let mut e = view
                        .cached(StyleRefinement::default().size_full())
                        .into_any_element();
                    e.layout_as_root(Size::<AvailableSpace>::from(b.size), window, cx);
                    e.prepaint_at(b.origin, window, cx);
                    Some(e)
                })
                .collect::<Vec<AnyElement>>()
        },
        |_, mut bars, window, cx| {
            for e in &mut bars {
                e.paint(window, cx);
            }
        },
    )
    .absolute()
    .top_0()
    .left_0()
    .size_full()
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
