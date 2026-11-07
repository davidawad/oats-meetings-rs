//! The main window in the drafting-table design (docs/design/drafting-table): the left tab rail,
//! the active tab's view (see `views`; Live is here), the banner and the status line, over the
//! blueprint grid, with a red top edge while recording. Pure presentation over `State`; every
//! control dispatches an action id through `model::dispatch`.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::Arc;

use std::time::Instant;

use gpui::{
    canvas, div, fill, prelude::*, px, relative, rgb, rgba, Bounds, Context, Entity, FocusHandle,
    FontWeight, IntoElement, Render, ScrollHandle, SharedString, StyleRefinement, Subscription,
    WeakEntity, Window,
};
use oats_core::control::engine::Engine;
use serde_json::json;

use crate::kit::{self, Icon, Kind, Size};
use crate::meters::{Meters, Reading};
use crate::state::{fmt_clock, Control, Controls, Dot, Edit, Line, Rec, State, Tab};
use crate::theme::{self, Theme, MONO, SANS};
use crate::views;

/// An open text input's buffer: highlighted while ctrl/cmd+a has it all selected, else with a caret.
pub(crate) fn edit_text(e: &Edit) -> gpui::Div {
    let t = theme::current();
    if e.all {
        div()
            .bg(rgb(t.accent))
            .text_color(rgb(t.accent_ink))
            .child(e.buf.clone())
    } else {
        div().child(format!("{}|", e.buf))
    }
}

/// A meter's fill for an RMS level: -60 dBFS (and silence) is empty, 0 dBFS full.
pub(crate) fn meter_fill(rms: f32) -> f32 {
    if rms <= 0. {
        return 0.;
    }
    ((20. * rms.log10() + 60.) / 60.).clamp(0., 1.)
}

pub struct MainWindow {
    pub(crate) engine: Arc<Engine>,
    pub state: State,
    /// Trash tab listing (`views::trash`), reloaded with the session list.
    pub(crate) trash: views::trash::Trash,
    /// Keyboard focus for the settings text editor; created on first render.
    pub(crate) focus: Option<FocusHandle>,
    live_scroll: ScrollHandle,
    /// The Settings rows' scroll, reset when the search's Enter jumps to the first match.
    pub(crate) settings_scroll: ScrollHandle,
    /// Where the view state goes when the window closes, for the next window to resume.
    keep: Option<Rc<RefCell<Option<State>>>>,
    /// The `(mic, system)` RMS levels for the meters, read each tick: the recording's, else the
    /// input monitor's (`ui.monitor`, dot-4uzgol.92).
    pub(crate) levels: (f32, f32),
    /// Where `levels` come from: "recording", "monitor", or "none" (meters at rest).
    pub(crate) meter_source: &'static str,
    /// Why the input monitor could not open its devices, if it tried.
    pub(crate) monitor_error: Option<String>,
    /// Why the recording may be missing its system audio (`Engine::recording_warning`: the macOS
    /// tap delivering only silence before System Audio Recording is allowed), read each tick.
    pub(crate) rec_warning: Option<String>,
    /// The settings row whose dropdown menu is open.
    pub(crate) menu: Option<&'static str>,
    /// The palette of the last frame (`ui.theme`, else the OS appearance).
    pub(crate) theme: Theme,
    appearance: Option<Subscription>,
    /// How many times this view rendered: `ui-state` reports it, so a frozen frame shows up as a
    /// count that stops moving while the state changes (dot-4uzgol.79).
    pub(crate) frames: u64,
    /// The keyboard: the `?` overlay and a Settings Rebind in progress (`views::shortcuts`).
    pub(crate) keys: views::shortcuts::Keys,
    /// Puts the keyboard back on the window when a focused editor goes away, so bare keys work.
    focus_lost: Option<Subscription>,
    /// What the meters and the record clock show: a tick that moves only them notifies this,
    /// never the window (dot-4uzgol.97). Created on first use.
    meters: Option<Entity<Meters>>,
    /// The active tab's content as a cached view: it re-renders only when this window is
    /// notified, so a meter repaint never re-lays out the transcript. Created on first render.
    body: Option<Entity<Body>>,
}

/// The active tab's content (Live: its transcript, under the record strip the window draws), as
/// a view the window embeds cached (dot-4uzgol.97). It observes the window: whatever notifies the
/// window re-renders it, while a repaint of the meters alone reuses its last layout.
pub struct Body {
    main: WeakEntity<MainWindow>,
    /// How many times the tab content rendered: `ui-state` reports it as `body_frames`.
    pub(crate) renders: u64,
}

impl Body {
    fn new(main: &Entity<MainWindow>, cx: &mut Context<Self>) -> Self {
        cx.observe(main, |_, _, cx| cx.notify()).detach();
        Self {
            main: main.downgrade(),
            renders: 0,
        }
    }
}

impl Render for Body {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.renders += 1;
        self.main
            .update(cx, |m, cx| m.tab_body(window, cx))
            .unwrap_or_else(|_| div().into_any_element())
    }
}

impl MainWindow {
    pub fn new(engine: Arc<Engine>) -> Self {
        Self::with_state(engine, State::default())
    }

    /// A window that picks up the state the last one left in `keep` (hide then show keeps the tab,
    /// the Live view and the selection) and leaves its own there when it closes.
    pub fn resume(engine: Arc<Engine>, keep: Rc<RefCell<Option<State>>>) -> Self {
        let state = keep.borrow_mut().take().unwrap_or_default();
        let mut w = Self::with_state(engine, state);
        w.keep = Some(keep);
        w
    }

    fn with_state(engine: Arc<Engine>, mut state: State) -> Self {
        // An open input or a pending press belonged to the old window (its focus, its task).
        state.cancel_edit();
        state.pending = None;
        state.refresh(&engine);
        state.sync_rec(&engine);
        let mut trash = views::trash::Trash::default();
        trash.load(&engine);
        Self {
            engine,
            state,
            trash,
            focus: None,
            live_scroll: ScrollHandle::new(),
            settings_scroll: ScrollHandle::new(),
            keep: None,
            levels: (0., 0.),
            meter_source: "none",
            monitor_error: None,
            rec_warning: None,
            menu: None,
            theme: Theme::DARK,
            appearance: None,
            frames: 0,
            keys: views::shortcuts::Keys::default(),
            focus_lost: None,
            meters: None,
            body: None,
        }
    }

    /// The meters' entity (created on first use: a window is built before it has a context).
    pub(crate) fn meters(&mut self, cx: &mut Context<Self>) -> Entity<Meters> {
        self.meters
            .get_or_insert_with(|| cx.new(|_| Meters::default()))
            .clone()
    }

    /// How many times the tab content rendered (0 before the first frame).
    pub(crate) fn body_renders(&self, cx: &gpui::App) -> u64 {
        self.body.as_ref().map_or(0, |b| b.read(cx).renders)
    }

    /// Apply a pending event line from the engine bus and re-render.
    pub fn event(&mut self, line: &str, cx: &mut Context<Self>) {
        let before = self.state.live.len();
        self.state.on_event(&self.engine, line);
        crate::views::calendar::on_event(line, cx);
        self.after_live_change(before);
        cx.notify();
    }

    fn after_live_change(&mut self, before: usize) {
        if self.engine.snapshot().config.ui.live_follow && self.state.live.len() >= before {
            self.live_scroll.scroll_to_bottom();
        }
    }

    /// Per host tick: reconcile with the recorder and keep the elapsed clock and meters moving.
    pub fn tick(&mut self, cx: &mut Context<Self>) {
        let before = self.state.rec.clone();
        if self.state.pending.is_none() {
            self.state.sync_rec(&self.engine);
        }
        let was = (self.levels, self.meter_source);
        (self.levels, self.meter_source) = match self.state.rec {
            Rec::Recording { .. } => (
                self.engine.recording_levels().unwrap_or(self.levels),
                "recording",
            ),
            Rec::Paused { .. } => ((0., 0.), "recording"),
            // A worker holding the monitor this tick: keep the last reading.
            Rec::Idle => match self.engine.monitor_state() {
                Some(m) => {
                    self.monitor_error = m.error;
                    if m.open {
                        (m.levels, "monitor")
                    } else {
                        ((0., 0.), "none")
                    }
                }
                None => was,
            },
        };
        let warned = self.rec_warning.clone();
        self.rec_warning = match self.state.rec {
            Rec::Idle => None,
            // A start or stop holding the recorder this tick: keep the last reading.
            _ => self
                .engine
                .recording_warning()
                .or_else(|| warned.clone().filter(|_| self.engine.recording().is_none())),
        };
        // Until a listing arrives: the worker fills the engine's cache after the first refresh.
        let listing = self.state.devices_note.is_some();
        if listing {
            self.state.sync_devices(&self.engine);
        }
        // The meters and the clock repaint through their own entity; the window (and with it the
        // tab content) only on a state change (dot-4uzgol.97), or when the recording banner changes.
        self.show_meters(cx);
        if listing || self.state.rec != before || self.rec_warning != warned {
            cx.notify();
        }
    }

    /// Hand this tick's levels and clock to the meters; they repaint only on a visible change, at
    /// most once per `ui.meter_interval`.
    pub(crate) fn show_meters(&mut self, cx: &mut Context<Self>) {
        let ui = &self.engine.snapshot().config.ui;
        let r = Reading {
            levels: self.levels,
            source: self.meter_source,
            clock: self.state.rec.elapsed_ms(Instant::now()) / 1000,
            steps: ui.meter_steps,
            interval: ui.meter_interval.as_duration(),
        };
        self.meters(cx).update(cx, |m, cx| {
            if m.set(r, Instant::now()) {
                cx.notify();
            }
        });
    }

    /// A recording button was pressed: the same engine action as `ctl` and the shortcuts. It runs off
    /// the UI thread (a start loads the ASR model) while the buttons show what is pending.
    pub fn press(&mut self, c: Control, cx: &mut Context<Self>) {
        let allowed = if self.state.pending.is_some() {
            Controls::BUSY
        } else {
            Controls::of(&self.state.rec)
        };
        let ok = match c {
            Control::Start => allowed.start,
            Control::Pause => allowed.pause,
            Control::Stop => allowed.stop,
        };
        if !ok {
            return;
        }
        self.state.pending = Some(c);
        cx.notify();
        let engine = Arc::clone(&self.engine);
        cx.spawn(async move |this, cx| {
            let eng = Arc::clone(&engine);
            let res = cx
                .background_executor()
                .spawn(async move { crate::model::dispatch(&eng, c.action(), json!({})) })
                .await;
            let _ = this.update(cx, |v, cx| {
                v.state.pending = None;
                v.state.status = res.err().map(|e| e.to_string()).unwrap_or_default();
                v.state.refresh(&v.engine);
                v.state.sync_rec(&v.engine);
                cx.notify();
            });
        })
        .detach();
    }

    /// The New session button: Live is cleared to an empty session, the finished one stays in
    /// Sessions. False when it does not apply (recording, paused, busy, or Live already empty).
    pub fn new_session(&mut self, cx: &mut Context<Self>) -> bool {
        if !self.state.new_session() {
            return false;
        }
        cx.notify();
        true
    }

    fn button(
        &self,
        id: &'static str,
        label: &'static str,
        kind: Kind,
        on: bool,
        c: Control,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let glyph = match c {
            Control::Start => Some(div().size(px(10.)).rounded_full()),
            Control::Stop => Some(div().size(px(10.)).rounded(px(2.))),
            Control::Pause => None,
        };
        let b = kit::button_with(id, glyph, label, kind, Size::Big, on);
        if on {
            b.on_click(cx.listener(move |this, _, _, cx| this.press(c, cx)))
        } else {
            b
        }
    }

    /// The Live tab's record strip: lamp and state word, clock, the two level meters, controls.
    fn record_strip(&self, fills: (f32, f32), cx: &mut Context<Self>) -> impl IntoElement {
        let t = self.theme;
        let ui = &self.engine.snapshot().config.ui;
        let c = if self.state.pending.is_some() {
            Controls::BUSY
        } else {
            Controls::of(&self.state.rec)
        };
        let recording = matches!(self.state.rec, Rec::Recording { .. });
        let lamp = match self.state.rec {
            Rec::Idle => t.faint,
            Rec::Recording { .. } => t.rec,
            Rec::Paused { .. } => t.pause,
        };
        let word = match (self.state.pending, &self.state.rec) {
            (Some(p), _) => p.busy_label(),
            (None, Rec::Idle) => "Not recording",
            (None, Rec::Recording { .. }) => "Recording",
            (None, Rec::Paused { .. }) => "Paused",
        };
        let idle = matches!(self.state.rec, Rec::Idle);
        // Idle, the meters show the input monitor (nothing recorded); recording, the recorded
        // streams.
        let clock = fmt_clock(self.state.rec.elapsed_ms(Instant::now()));
        let (mic, sys) = fills;
        let meter = |label: &'static str, level: f32| {
            div()
                .flex()
                .items_center()
                .gap(px(10.))
                .text_size(px(11.))
                .text_color(rgb(t.dim))
                .child(div().w(px(66.)).flex_none().child(label))
                .child(kit::meter(level))
        };
        let controls = if idle && self.state.pending.is_none() {
            let fresh = self.state.can_new_session();
            let new = kit::button_with(
                "rec-new",
                None,
                "New session",
                Kind::Secondary,
                Size::Big,
                fresh,
            );
            div()
                .flex()
                .gap(px(8.))
                .child(if fresh {
                    new.on_click(cx.listener(|this, _, _, cx| {
                        this.new_session(cx);
                    }))
                } else {
                    new
                })
                .child(self.button(
                    "rec-start",
                    "Start recording",
                    Kind::Rec,
                    c.start,
                    Control::Start,
                    cx,
                ))
        } else {
            div()
                .flex()
                .gap(px(8.))
                .child(self.button(
                    "rec-pause",
                    c.pause_label,
                    Kind::Secondary,
                    c.pause,
                    Control::Pause,
                    cx,
                ))
                .child(self.button("rec-stop", "Stop", Kind::Rec, c.stop, Control::Stop, cx))
        };
        div()
            .id("record-bar")
            .flex()
            .flex_none()
            .items_center()
            .gap(px(20.))
            .mx(px(28.))
            .px(px(18.))
            .py(px(14.))
            .rounded(px(12.))
            .border_1()
            .border_color(rgb(if recording { t.rec_line } else { t.line }))
            .bg(rgb(if recording { t.rec_soft } else { t.surface }))
            .child(
                div()
                    .id("rec-badge")
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(px(10.))
                    .min_w(px(150.))
                    .child(div().size(px(14.)).flex_none().rounded_full().bg(rgb(lamp)))
                    .child(
                        div()
                            .text_size(px(15.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(word),
                    ),
            )
            .when(ui.show_elapsed, |d| {
                d.child(
                    kit::mono(clock)
                        .flex_none()
                        .min_w(px(170.))
                        .text_size(px(38.))
                        .line_height(relative(1.))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(rgb(if idle { t.faint } else { t.text })),
                )
            })
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .gap(px(6.))
                    .child(meter("You", mic))
                    .child(meter("Remote", sys)),
            )
            .child(controls)
    }

    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        self.state.refresh(&self.engine);
        self.trash.load(&self.engine);
        cx.notify();
    }

    /// The left rail: brand, the daily tabs, a divider, the setup tabs, the version.
    fn rail(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let t = self.theme;
        let lit = self.state.tab.rail();
        let dot = self.state.rec.dot();
        let tab = |tab: Tab, cx: &mut Context<Self>| {
            let active = lit == tab;
            let icon = match tab {
                Tab::Live => Icon::Mic,
                Tab::Sessions => Icon::List,
                Tab::Notes => Icon::Note,
                Tab::Templates => Icon::Template,
                Tab::Calendar => Icon::Calendar,
                Tab::Models => Icon::Chip,
                Tab::Settings => Icon::Gear,
                Tab::Trash => Icon::Trash,
            };
            let fg = if active { t.text } else { t.dim };
            div()
                .id(SharedString::from(format!("tab-{}", tab.label())))
                .relative()
                .h(px(34.))
                .px(px(10.))
                .flex()
                .flex_none()
                .items_center()
                .gap(px(10.))
                .rounded(px(7.))
                .cursor_pointer()
                .text_color(rgb(fg))
                .font_weight(FontWeight::MEDIUM)
                .when(active, |d| {
                    d.bg(rgb(t.accent_soft)).child(
                        div()
                            .absolute()
                            .left(px(-10.))
                            .top(px(7.))
                            .bottom(px(7.))
                            .w(px(3.))
                            .rounded_r(px(3.))
                            .bg(rgb(t.accent)),
                    )
                })
                .when(!active, |d| {
                    d.hover(move |s| s.bg(rgb(t.raised)).text_color(rgb(t.text)))
                })
                .child(kit::icon(icon, fg))
                .child(tab.label())
                .when_some(dot.filter(|_| tab == Tab::Live), |d, dot| {
                    d.child(
                        div()
                            .id("live-dot")
                            .ml_auto()
                            .size(px(8.))
                            .rounded_full()
                            .bg(rgb(match dot {
                                Dot::Rec => t.rec,
                                Dot::Paused => t.pause,
                            })),
                    )
                })
                .on_click(cx.listener(move |this, _, _, cx| this.switch_tab(tab, cx)))
        };
        let mut rail = div()
            .w(px(176.))
            .flex_none()
            .flex()
            .flex_col()
            .gap(px(2.))
            .px(px(10.))
            .py(px(14.))
            .bg(rgb(t.side))
            .border_r_1()
            .border_color(rgb(t.line))
            .child(
                div()
                    .flex()
                    .px(px(10.))
                    .pt(px(2.))
                    .pb(px(14.))
                    .font_family(MONO)
                    .text_size(px(20.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("oats")
                    .child(div().text_color(rgb(t.accent)).child("_")),
            );
        for (i, group) in Tab::RAIL.iter().enumerate() {
            if i > 0 {
                rail = rail.child(div().h(px(1.)).mx(px(10.)).my(px(8.)).bg(rgb(t.line)));
            }
            for &g in group.iter() {
                rail = rail.child(tab(g, cx));
            }
        }
        rail.child(
            kit::mono(concat!("v", env!("CARGO_PKG_VERSION")))
                .mt_auto()
                .p(px(10.))
                .text_size(px(11.))
                .text_color(rgb(t.faint)),
        )
    }

    /// A tab-bar click: views that cache engine reads reload them on entry.
    pub(crate) fn switch_tab(&mut self, t: Tab, cx: &mut Context<Self>) {
        self.state.tab = t;
        self.state.cancel_edit();
        self.menu = None;
        match t {
            Tab::Trash => self.trash.load(&self.engine),
            // Templates may have been created, imported or deleted since the picker was read.
            Tab::Notes => cx.default_global::<views::notes::Notes>().loaded = false,
            _ => {}
        }
        cx.notify();
    }

    /// Transcript lines: a mono timestamp, the speaker tag in its colour, the text. A line by the
    /// same speaker as the one before hides its timestamp and tag (the columns stay). The tag is
    /// the speaker id's label (`labels`, `state::speaker_labels`).
    pub(crate) fn lines(
        lines: &[Line],
        labels: &BTreeMap<String, String>,
        empty: &str,
        speaker_w: f32,
    ) -> impl IntoElement {
        let t = theme::current();
        let body = div().flex().flex_col().gap(px(2.)).w_full().max_w(px(860.));
        if lines.is_empty() {
            return body.child(div().text_color(rgb(t.dim)).child(empty.to_string()));
        }
        body.children(lines.iter().enumerate().map(|(i, l)| {
            let cont = i > 0 && lines[i - 1].speaker == l.speaker;
            let c = t.speaker(&l.speaker);
            let ts = l
                .start
                .map(|s| crate::state::fmt_elapsed((s.max(0.) * 1000.) as u64))
                .unwrap_or_default();
            div()
                .flex()
                .items_start()
                .gap(px(10.))
                .w_full()
                .px(px(12.))
                .py(px(7.))
                .rounded(px(8.))
                .child(
                    kit::mono(ts)
                        .w(px(48.))
                        .flex_none()
                        .pt(px(5.))
                        .text_size(px(11.))
                        .text_color(rgb(t.faint))
                        .when(cont, |d| d.invisible()),
                )
                .child(
                    div()
                        .w(px(speaker_w))
                        .flex_none()
                        .flex()
                        .items_center()
                        .gap(px(7.))
                        .pt(px(4.))
                        .font_family(MONO)
                        .text_size(px(12.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(rgb(c))
                        .when(cont, |d| d.invisible())
                        .child(
                            div()
                                .w(px(3.))
                                .h(px(14.))
                                .flex_none()
                                .rounded(px(2.))
                                .bg(rgb(c)),
                        )
                        .child(
                            div()
                                .min_w_0()
                                .overflow_hidden()
                                .child(labels.get(&l.speaker).unwrap_or(&l.speaker).clone()),
                        ),
                )
                // flex_1 + min_w_0 lets long text wrap inside the row instead of running off the window.
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .whitespace_normal()
                        .text_size(px(17.))
                        .line_height(relative(1.5))
                        .child(l.text.clone()),
                )
        }))
    }

    /// The banners: system audio silent while recording (the macOS tap without its grant),
    /// transcription unavailable (with a button when an action fixes it), and the last failed
    /// hook run (dismissable; its output is in Settings > Hooks).
    pub(crate) fn banner(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let mut rows = vec![];
        if let Some(w) = self.rec_warning.as_ref() {
            let (lead, rest) = match w.split_once(": ") {
                Some((a, b)) => (format!("{}.", capitalised(a)), b.to_string()),
                None => (w.clone(), String::new()),
            };
            rows.push(kit::banner(true, lead, rest));
        }
        if let Some(p) = self.state.stt_problem.as_ref() {
            let (lead, rest) = match p.message.split_once(": ") {
                Some((a, b)) => (format!("{a}."), b.to_string()),
                None => (p.message.clone(), String::new()),
            };
            let mut row = kit::banner(false, lead, rest);
            if let Some(fix) = p.fix.clone() {
                row = row.child(
                    kit::button("stt-fix", "Download model", Kind::Primary, Size::Sm, true)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.state.act(&this.engine, &fix, json!({}));
                            cx.notify();
                        })),
                );
            }
            rows.push(row);
        }
        if let Some(msg) = self.state.hook_failure.as_ref() {
            let (lead, rest) = match msg.split_once(": ") {
                Some((a, b)) => (
                    format!("{a}."),
                    format!("{b} (Settings > Hooks has its output)"),
                ),
                None => (msg.clone(), String::new()),
            };
            rows.push(kit::banner(true, lead, rest).child(
                kit::button("hook-dismiss", "Dismiss", Kind::Secondary, Size::Sm, true).on_click(
                    cx.listener(|this, _, _, cx| {
                        this.state.hook_failure = None;
                        cx.notify();
                    }),
                ),
            ));
        }
        (!rows.is_empty()).then(|| div().flex().flex_col().gap(px(8.)).children(rows))
    }

    pub(crate) fn speaker_width(&self) -> f32 {
        self.engine.snapshot().config.ui.speaker_width as f32
    }

    /// The configured capture devices by name, `mic · system`, for the Live status line.
    fn devices_label(&self) -> String {
        let cfg = self.engine.snapshot();
        let a = &cfg.config.audio;
        let name = |id: &str, fallback: &str| {
            if id.is_empty() {
                return fallback.to_string();
            }
            self.state
                .device_opts
                .iter()
                .find(|d| d.id == id)
                .map_or_else(|| id.to_string(), |d| d.name.clone())
        };
        let mut out = vec![];
        if a.mic.enabled {
            out.push(name(&a.mic.device, "Default microphone"));
        }
        if a.system.enabled {
            out.push(name(&a.system.device, "System audio"));
        }
        if out.is_empty() {
            return "No inputs enabled".into();
        }
        out.join(" · ")
    }

    /// The Live lines' speaker labels.
    pub(crate) fn live_labels(&self) -> BTreeMap<String, String> {
        let cfg = self.engine.snapshot();
        crate::state::speaker_labels(
            &cfg.config.speakers,
            &self.state.live,
            &self.state.live_names,
        )
    }

    /// The selected session's speaker labels.
    pub(crate) fn transcript_labels(&self) -> BTreeMap<String, String> {
        let cfg = self.engine.snapshot();
        crate::state::speaker_labels(
            &cfg.config.speakers,
            &self.state.transcript,
            &self.state.transcript_names,
        )
    }

    fn inputs(&self) -> usize {
        let cfg = self.engine.snapshot();
        let a = &cfg.config.audio;
        usize::from(a.mic.enabled) + usize::from(a.system.enabled)
    }

    /// The Live tab's head, record strip and banner: the window draws them, around the cached
    /// transcript (`live_body`), so the strip's meters and clock repaint without it.
    fn live_chrome(&self, fills: (f32, f32), cx: &mut Context<Self>) -> impl IntoElement {
        let controls = self.engine.snapshot().config.ui.record_controls;
        let n = self.inputs();
        let mut head = kit::head("Live").child(div().flex_1());
        if let Some((name, _)) = &self.state.model {
            head = head.child(kit::tag(name.clone(), true));
        }
        head = head.child(kit::tag(
            format!("{n} input{}", if n == 1 { "" } else { "s" }),
            false,
        ));
        div()
            .flex()
            .flex_col()
            .flex_none()
            .child(head)
            .children(controls.then(|| self.record_strip(fills, cx)))
            .children(
                self.banner(cx)
                    .map(|b| div().flex_none().mx(px(28.)).mt(px(12.)).child(b)),
            )
    }

    /// The Live transcript (or its empty state), in the cached tab content.
    fn live_body(&self) -> impl IntoElement {
        let t = self.theme;
        let w = self.speaker_width();
        let body = if self.state.live.is_empty() {
            let (h, s) = match self.state.rec {
                Rec::Idle if self.state.live_session.is_some() => (
                    "Empty transcript",
                    "The last recording has no lines. Start recording to try again.".to_string(),
                ),
                Rec::Idle => {
                    let map = self.keymap();
                    let k = |c: &str| crate::shortcuts::shown(map.keys(c));
                    let cfg = self.engine.snapshot();
                    let sp = &cfg.config.speakers;
                    let n = |i| oats_core::speakers::fill(&sp.system.template, i);
                    (
                        "Ready when you are",
                        format!(
                            "Start recording and the transcript appears here as people speak. Your \
                             voice shows as {}, the other side as {} ({}, {} ... when several \
                             speak). (Also {} or {}, or `ctl record-start`; {} lists every \
                             shortcut.)",
                            sp.mic.label,
                            sp.system.label_single,
                            n(1),
                            n(2),
                            k("record-toggle"),
                            k("record-start"),
                            k("help")
                        ),
                    )
                }
                _ => (
                    "Listening",
                    "Lines appear here as speech is recognised.".to_string(),
                ),
            };
            kit::empty(h, s).into_any_element()
        } else {
            div()
                .id("live")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .track_scroll(&self.live_scroll)
                .pt(px(18.))
                .px(px(16.))
                .child(Self::lines(&self.state.live, &self.live_labels(), "", w))
                .into_any_element()
        };
        div()
            .flex()
            .flex_col()
            .size_full()
            .text_color(rgb(t.text))
            .child(body)
    }

    /// The status line: what the window is doing (the last action's result, else the session),
    /// and on Live the model and runtime and the capture devices.
    fn status_bar(&self, meters: &Meters) -> impl IntoElement {
        let t = self.theme;
        let st = &self.state;
        // Every segment ellipsizes (dot-4uzgol.81): at 1100px the devices ran past the edge.
        let mut left = div()
            .flex()
            .items_center()
            .min_w_0()
            .gap(px(6.))
            .debug_selector(|| "status-left".into());
        if !st.status.is_empty() {
            let colour = if st.status.starts_with("Saved") {
                t.ok
            } else {
                t.dim
            };
            left = left
                .text_color(rgb(colour))
                .child(kit::ellipsis(st.status.clone()));
        } else if st.tab == Tab::Live {
            left = match (&st.rec, &st.live_session) {
                (r, _) if r.session().is_some() => left
                    .child(div().flex_none().text_color(rgb(t.ok)).child("●"))
                    .child(kit::ellipsis(format!(
                        "Saving to {}",
                        r.session().unwrap_or_default()
                    ))),
                (Rec::Idle, Some(s)) => left.child(kit::ellipsis(format!(
                    "Last recording: {s} (saved; see Sessions)"
                ))),
                _ => left.child(kit::ellipsis(match &st.devices_note {
                    Some(note) => note.clone(),
                    None => kit::count(st.devices as u64, "audio device"),
                })),
            };
        } else if st.tab == Tab::Sessions {
            left = left.child(kit::ellipsis(format!(
                "{} · click one to read it",
                kit::count(st.sessions.len() as u64, "session")
            )));
        }
        let mut bar = div()
            .id("status")
            .h(px(32.))
            .flex_none()
            .flex()
            .items_center()
            .gap(px(18.))
            .px(px(28.))
            .border_t_1()
            .border_color(rgb(t.line))
            .bg(rgb(t.side))
            .text_size(px(12.))
            .text_color(rgb(t.dim))
            .overflow_hidden()
            .whitespace_nowrap()
            .debug_selector(|| "status-bar".into())
            .child(left);
        if st.tab == Tab::Live {
            if let Some((name, runtime)) = &st.model {
                bar = bar.child(
                    kit::ellipsis(if runtime.is_empty() {
                        name.clone()
                    } else {
                        format!("{name} · {runtime}")
                    })
                    .font_family(MONO)
                    .debug_selector(|| "status-model".into()),
                );
            }
            bar = bar.child(
                kit::ellipsis(self.devices_label())
                    .font_family(MONO)
                    .ml_auto()
                    .debug_selector(|| "status-devices".into()),
            );
        }
        if meters.source != "none" {
            bar = bar.child(
                self.status_meters(meters.fills())
                    .when(st.tab != Tab::Live, |d| d.ml_auto()),
            );
        }
        bar
    }

    /// The compact meters at the end of the status line, on every tab: mic and system audio
    /// while monitoring or recording.
    fn status_meters(&self, (mic, sys): (f32, f32)) -> gpui::Div {
        let t = self.theme;
        let meter = |label: &'static str, level: f32| {
            div()
                .flex()
                .flex_none()
                .items_center()
                .gap(px(5.))
                .child(label)
                .child(div().flex().w(px(36.)).child(kit::meter(level)))
        };
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(10.))
            .text_color(rgb(t.faint))
            .debug_selector(|| "status-meters".into())
            .child(meter("You", mic))
            .child(meter("Remote", sys))
    }

    /// The blueprint grid behind the content: 1px lines every 24px in the grid colour.
    fn grid(&self) -> impl IntoElement {
        let colour = self.theme.grid;
        canvas(
            |_, _, _| (),
            move |b: Bounds<gpui::Pixels>, _, window, _| {
                let step = px(24.);
                let mut x = b.origin.x;
                while x < b.origin.x + b.size.width {
                    window.paint_quad(fill(
                        Bounds::new(
                            gpui::point(x, b.origin.y),
                            gpui::size(px(1.), b.size.height),
                        ),
                        rgba(colour),
                    ));
                    x += step;
                }
                let mut y = b.origin.y;
                while y < b.origin.y + b.size.height {
                    window.paint_quad(fill(
                        Bounds::new(gpui::point(b.origin.x, y), gpui::size(b.size.width, px(1.))),
                        rgba(colour),
                    ));
                    y += step;
                }
            },
        )
        .absolute()
        .top_0()
        .left_0()
        .size_full()
    }
}

impl MainWindow {
    /// The active tab's content, rendered by `Body`; one line per tab, each view in its own file
    /// under `views/`.
    fn tab_body(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> gpui::AnyElement {
        match self.state.tab {
            Tab::Live => self.live_body().into_any_element(),
            Tab::Sessions => {
                views::sessions::sync_hits(&mut self.state, &self.engine);
                views::sessions::render(self, cx)
            }
            Tab::Notes => views::notes::render(self, cx),
            Tab::Templates => views::templates::render(self, cx),
            Tab::Calendar => views::calendar::render(self, cx),
            Tab::Trash => views::trash::render(self, cx),
            Tab::Models => views::models::render(self, cx),
            Tab::Settings => {
                views::settings::prepare(self);
                views::settings::render(self, cx)
            }
        }
    }
}

impl Drop for MainWindow {
    fn drop(&mut self) {
        if let Some(k) = &self.keep {
            *k.borrow_mut() = Some(std::mem::take(&mut self.state));
        }
    }
}

impl Render for MainWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.frames += 1;
        window.set_window_title("Oats");
        let pref = self.engine.snapshot().config.ui.theme;
        self.theme = Theme::resolve(pref, window.appearance());
        theme::set(self.theme);
        if self.appearance.is_none() {
            self.appearance = Some(cx.observe_window_appearance(window, |_, _, cx| cx.notify()));
        }
        let t = self.theme;
        let focus = self.focus.get_or_insert_with(|| cx.focus_handle()).clone();
        // Keys reach the window's handlers only through a focused element in it: take the keyboard
        // when nothing has it (a new window, or the focused editor was removed).
        if self.focus_lost.is_none() {
            self.focus_lost = Some(cx.on_focus_lost(window, |this, window, cx| {
                if let Some(f) = &this.focus {
                    f.focus(window, cx);
                }
            }));
        }
        if window.focused(cx).is_none() {
            let f = focus.clone();
            window.defer(cx, move |window, cx| f.focus(window, cx));
        }
        // Reading the meters here is what makes their notify redraw this window's chrome; the tab
        // content below is cached and re-renders only when this window is notified.
        let meters = self.meters(cx).read(cx).clone();
        let main = cx.entity();
        let tab = self
            .body
            .get_or_insert_with(|| cx.new(|cx| Body::new(&main, cx)))
            .clone()
            .cached(StyleRefinement::default().w_full().flex_1().min_h_0());
        let body = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .when(self.state.tab == Tab::Live, |d| {
                d.child(self.live_chrome(meters.fills(), cx))
            })
            .child(tab);
        // Live shows the banner under its record strip; the other tabs above their content.
        let banner = (self.state.tab != Tab::Live)
            .then(|| self.banner(cx))
            .flatten()
            .map(|b| div().flex_none().mx(px(28.)).mt(px(12.)).child(b));
        let recording = matches!(self.state.rec, Rec::Recording { .. });
        div()
            .id("window")
            .flex()
            .flex_col()
            .size_full()
            .bg(rgb(t.bg))
            .text_color(rgb(t.text))
            .font_family(SANS)
            .text_size(px(14.))
            // The recording edge; transparent while idle so the layout does not jump.
            .border_t(px(3.))
            .border_color(if recording {
                rgb(t.rec).into()
            } else {
                gpui::transparent_black()
            })
            .track_focus(&focus)
            // After a focused editor (Notes, a template, a calendar field) had the key: a Rebind
            // listening, the open text box, else a bare shortcut (views::shortcuts).
            .on_key_down(cx.listener(|this, ev: &gpui::KeyDownEvent, _, cx| {
                if this.key_down(&ev.keystroke, cx) {
                    cx.stop_propagation();
                    cx.notify();
                }
            }))
            // A chord (a GPUI key binding) or a menu item: a window command runs here; an engine
            // action too, so the buttons show it pending. Anything else goes on to the app.
            .on_action(cx.listener(|this, a: &crate::host::Do, _, cx| {
                if !this.command(&a.id, cx) {
                    cx.propagate();
                }
            }))
            .child(
                div().flex().flex_1().min_h_0().child(self.rail(cx)).child(
                    div()
                        .relative()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .min_w_0()
                        .child(self.grid())
                        .children(banner)
                        .child(body)
                        .child(self.status_bar(&meters)),
                ),
            )
            .children(self.keys.help.then(|| views::shortcuts::help(self, cx)))
    }
}

/// What the dump reports of the recording chrome: the red window edge and the Live-tab dot.
pub(crate) fn chrome(rec: &Rec) -> serde_json::Value {
    json!({
        "edge": matches!(rec, Rec::Recording { .. }),
        "live_dot": rec.dot().map(|d| match d {
            Dot::Rec => "rec",
            Dot::Paused => "paused",
        }),
    })
}

/// `s` with its first letter upper-case.
fn capitalised(s: &str) -> String {
    let mut c = s.chars();
    c.next()
        .map(|f| f.to_uppercase().chain(c).collect())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{model, testutil};
    use gpui::TestAppContext;

    fn render_each_tab(pairs: &[(&str, serde_json::Value)]) {
        let (engine, dir) = testutil::engine(pairs);
        let s = dir.path().join("sessions/s1");
        std::fs::create_dir_all(&s).unwrap();
        std::fs::write(
            s.join("session.json"),
            r#"{"id":"s1","title":"T","startedAt":"2026-10-27T10:00:00Z"}"#,
        )
        .unwrap();
        std::fs::write(
            s.join("transcript.final.jsonl"),
            "{\"speaker\":\"you\",\"text\":\"hello\"}\n",
        )
        .unwrap();
        let mut cx = TestAppContext::single();
        let (view, cx) = cx.add_window_view(|_, _| MainWindow::new(engine.clone()));
        for tab in Tab::ALL {
            view.update(cx, |v, cx| {
                v.state.tab = tab;
                if tab == Tab::Sessions {
                    v.state.select(&v.engine, "s1");
                }
                cx.notify();
            });
            cx.run_until_parked();
        }
        view.read_with(cx, |v, _| {
            assert_eq!(v.state.sessions.len(), 1);
            assert_eq!(v.state.transcript.len(), 1);
            assert!(!v.state.settings.is_empty());
        });
    }

    fn ev(name: &str, data: serde_json::Value) -> String {
        json!({"event": name, "data": data}).to_string()
    }

    #[test]
    fn live_view_renders_each_recording_state_and_controls_follow_them() {
        let (engine, _d) = testutil::engine(&[]);
        let mut cx = TestAppContext::single();
        let (view, cx) = cx.add_window_view(|_, _| MainWindow::new(engine.clone()));
        let long = "word ".repeat(200);
        let steps = [
            (ev("recording-started", json!({"session": "a"})), true, false),
            (json!({"event":"segment","data":{"session":"a","kind":"live","speaker":"you","text":long,"start":0.0}}).to_string(), false, false),
            (ev("recording-paused", json!({"session": "a", "paused": true, "duration_ms": 4000})), false, true),
            (ev("recording-paused", json!({"session": "a", "paused": false, "duration_ms": 4000})), true, false),
            (ev("recording-stopped", json!({"session": "a", "duration_ms": 9000})), false, false),
        ];
        // The red edge shows only while recording; the Live-tab dot is red, then amber when paused.
        let chrome_of = [
            (true, Some("rec")),
            (true, Some("rec")),
            (false, Some("paused")),
            (true, Some("rec")),
            (false, None),
        ];
        for ((line, _, _), (edge, dot)) in steps.iter().zip(chrome_of) {
            view.update(cx, |v, cx| {
                v.state.tab = Tab::Live;
                v.event(line, cx);
            });
            cx.run_until_parked();
            view.update(cx, |v, cx| {
                let c = Controls::of(&v.state.rec);
                assert_eq!(c.start, matches!(v.state.rec, Rec::Idle));
                let d = v.dump(cx);
                assert_eq!(d["recording"]["chrome"]["edge"], edge, "{line}");
                assert_eq!(d["recording"]["chrome"]["live_dot"], json!(dot), "{line}");
            });
        }
        view.read_with(cx, |v, _| {
            assert_eq!(v.state.rec, Rec::Idle);
            assert_eq!(v.state.live_session.as_deref(), Some("a"));
        });
    }

    #[test]
    fn pressing_the_buttons_records_pauses_resumes_and_stops_through_the_engine() {
        use oats_audio::fake::FakeBackend;
        let (engine, _d) = testutil::engine(&[]);
        engine.set_audio_factory(Box::new(|_| Ok(Box::new(FakeBackend::default()))));
        let mut cx = TestAppContext::single();
        let (view, cx) = cx.add_window_view(|_, _| MainWindow::new(engine.clone()));
        let press = |c: Control, cx: &mut gpui::VisualTestContext| {
            view.update(cx, |v, cx| v.press(c, cx));
            cx.run_until_parked();
        };
        // Pause / Stop do nothing while idle.
        press(Control::Stop, cx);
        assert!(engine.recording() == Some(None));
        press(Control::Start, cx);
        assert!(engine.recording().flatten().is_some_and(|r| !r.1));
        view.read_with(cx, |v, _| {
            assert!(matches!(v.state.rec, Rec::Recording { .. }))
        });
        press(Control::Start, cx); // already recording: ignored, no error
        press(Control::Pause, cx);
        assert!(engine.recording().flatten().is_some_and(|r| r.1));
        view.read_with(cx, |v, _| {
            assert!(matches!(v.state.rec, Rec::Paused { .. }))
        });
        press(Control::Pause, cx);
        assert!(engine.recording().flatten().is_some_and(|r| !r.1));
        press(Control::Stop, cx);
        assert!(engine.recording() == Some(None));
        view.read_with(cx, |v, _| assert_eq!(v.state.rec, Rec::Idle));
    }

    #[test]
    fn main_window_renders_with_controls_hidden() {
        render_each_tab(&[
            ("ui.record_controls", json!(false)),
            ("ui.live_follow", json!(false)),
            ("ui.show_elapsed", json!(false)),
        ]);
    }

    #[test]
    fn main_window_renders_every_tab() {
        render_each_tab(&[]);
    }

    #[test]
    fn main_window_renders_every_tab_in_dark_and_light() {
        render_each_tab(&[("ui.theme", json!("dark"))]);
        render_each_tab(&[("ui.theme", json!("light"))]);
    }

    #[test]
    fn ui_theme_picks_the_palette_and_the_dump_reports_it() {
        let (engine, _d) = testutil::engine(&[("ui.theme", json!("light"))]);
        let mut cx = TestAppContext::single();
        let (view, cx) = cx.add_window_view(|_, _| MainWindow::new(engine.clone()));
        cx.run_until_parked();
        view.update(cx, |v, cx| {
            assert_eq!(v.theme, Theme::LIGHT);
            assert_eq!(v.dump(cx)["theme"], "light");
            v.state.act(
                &v.engine,
                "config-set",
                json!({"key": "ui.theme", "value": "dark"}),
            );
            cx.notify();
        });
        cx.run_until_parked();
        view.update(cx, |v, cx| assert_eq!(v.dump(cx)["theme"], "dark"));
    }

    /// dot-4uzgol.81, at the carbon window size and the minimum window width (host.rs): the Default chip ends before
    /// the Size column (the long model name ellipsizes) and every footer segment stays inside
    /// the footer.
    #[test]
    fn model_names_and_footer_segments_stay_inside_their_columns() {
        let (engine, _d) = testutil::engine(&[(
            "audio.mic.device",
            json!("MacBook Pro Microphone (Built-in, Aggregate Device)"),
        )]);
        let selected = views::models::load(&engine)
            .rows
            .into_iter()
            .find(|r| r.selected)
            .expect("a default model");
        let size_sel: &'static str = Box::leak(format!("model-size-{}", selected.id).into());
        let name_sel: &'static str = Box::leak(format!("model-name-{}", selected.id).into());
        let mut cx = TestAppContext::single();
        let (view, cx) = cx.add_window_view(|_, _| MainWindow::new(engine.clone()));
        for width in [1100., 860.] {
            cx.simulate_resize(gpui::size(px(width), px(752.)));
            view.update(cx, |v, cx| {
                v.state.tab = Tab::Models;
                cx.notify();
            });
            cx.run_until_parked();
            let name = cx.debug_bounds(name_sel).expect("name");
            let chip = cx.debug_bounds("model-default-chip").expect("chip");
            let size = cx.debug_bounds(size_sel).expect("size");
            assert!(name.right() <= chip.left(), "{width}: {name:?} {chip:?}");
            assert!(chip.right() <= size.left(), "{width}: {chip:?} {size:?}");
            view.update(cx, |v, cx| {
                v.state.tab = Tab::Live;
                v.state.status.clear();
                v.state.model = Some((selected.name.clone(), "sherpa-onnx".into()));
                cx.notify();
            });
            cx.run_until_parked();
            // Inside the bar's 28px right padding, as the left edge is.
            let bar = cx.debug_bounds("status-bar").expect("bar");
            for seg in ["status-left", "status-model", "status-devices"] {
                if let Some(b) = cx.debug_bounds(seg) {
                    assert!(
                        b.right() <= bar.right() - px(28.),
                        "{width} {seg}: {b:?} {bar:?}"
                    );
                }
            }
            assert!(cx.debug_bounds("status-devices").is_some());
        }
    }

    /// dot-4uzgol.92: idle, the Live meters and the status-line pair show the input monitor while
    /// the window is shown; hidden (window_visible) they rest and the devices are released.
    #[test]
    fn idle_meters_follow_the_input_monitor_and_rest_when_hidden() {
        let (engine, _dir) = testutil::engine(&[]);
        engine.enable_monitor();
        model::dispatch(&engine, "window-show", json!({})).unwrap();
        let mut cx = TestAppContext::single();
        let (view, cx) = cx.add_window_view(|_, _| MainWindow::new(engine.clone()));
        let settle = |cx: &mut gpui::VisualTestContext, want: &'static str| {
            let deadline = Instant::now() + std::time::Duration::from_secs(30);
            loop {
                view.update(cx, |v, cx| v.tick(cx));
                cx.run_until_parked();
                let (src, lv) = view.read_with(cx, |v, _| (v.meter_source, v.levels));
                if src == want && (want == "none" || (lv.0 > 0.3 && lv.1 > 0.3)) {
                    return;
                }
                assert!(
                    Instant::now() < deadline,
                    "meters {src} {lv:?}, want {want}"
                );
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        };
        settle(cx, "monitor");
        assert!(cx.debug_bounds("status-meters").is_some());
        let dump = view.update(cx, |v, cx| v.dump(cx));
        assert_eq!(dump["meters"]["source"], "monitor");
        assert_eq!(dump["meters"]["monitor"], "window_visible");
        assert_eq!(dump["recording"]["state"], "idle");
        model::dispatch(&engine, "window-hide", json!({})).unwrap();
        settle(cx, "none");
        assert!(cx.debug_bounds("status-meters").is_none());
        assert!(!engine.monitor_state().unwrap().open);
    }

    /// A window over a session of `n` long lines, selected in Sessions; Live holds them too.
    fn long_session_window(
        n: usize,
        pairs: &[(&str, serde_json::Value)],
    ) -> (
        Arc<Engine>,
        tempfile::TempDir,
        Entity<MainWindow>,
        &'static mut gpui::VisualTestContext,
    ) {
        let (engine, dir) = testutil::engine(pairs);
        let s = dir.path().join("sessions/s1");
        std::fs::create_dir_all(&s).unwrap();
        std::fs::write(
            s.join("session.json"),
            r#"{"id":"s1","title":"T","startedAt":"2026-10-27T10:00:00Z"}"#,
        )
        .unwrap();
        let line = "and then we went over the quarterly numbers again, line by line, because ";
        let jsonl: String = (0..n)
            .map(|i| {
                let speaker = if i % 3 == 0 { "you" } else { "them" };
                let text = format!("{i}: {}", line.repeat(3));
                format!(
                    "{}\n",
                    json!({"speaker": speaker, "text": text, "start": i as f64 * 4.})
                )
            })
            .collect();
        std::fs::write(s.join("transcript.final.jsonl"), jsonl).unwrap();
        let cx = Box::leak(Box::new(TestAppContext::single()));
        let (view, cx) = cx.add_window_view(|_, _| MainWindow::new(engine.clone()));
        view.update(cx, |v, cx| {
            v.state.tab = Tab::Sessions;
            v.state.select(&v.engine, "s1");
            v.state.live = v.state.transcript.clone();
            cx.notify();
        });
        cx.run_until_parked();
        assert_eq!(view.read_with(cx, |v, _| v.state.transcript.len()), n);
        (engine, dir, view, cx)
    }

    /// dot-4uzgol.97: idle meter ticks repaint the meters and the window chrome only. Over N
    /// visible meter changes the transcript (the cached tab content) renders 0 times, on Sessions
    /// and on Live, while the meters themselves do redraw; a window notify still re-renders it.
    #[test]
    fn meter_ticks_never_render_the_transcript() {
        let (engine, _dir, view, cx) = long_session_window(400, &[]);
        engine.enable_monitor();
        model::dispatch(&engine, "window-show", json!({})).unwrap();
        // Settle first: the monitor opens and the device listing arrives (a state change, which
        // does re-render the content once).
        let deadline = Instant::now() + std::time::Duration::from_secs(30);
        loop {
            view.update(cx, |v, cx| v.tick(cx));
            cx.run_until_parked();
            let (src, listing) =
                view.read_with(cx, |v, _| (v.meter_source, v.state.devices_note.is_some()));
            if src == "monitor" && !listing {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "monitor {src}, listing {listing}"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        for tab in [Tab::Sessions, Tab::Live] {
            view.update(cx, |v, cx| {
                v.state.tab = tab;
                cx.notify();
            });
            cx.run_until_parked();
            let read = |cx: &mut gpui::VisualTestContext| {
                view.update(cx, |v, cx| {
                    (v.body_renders(cx), v.frames, v.meters(cx).read(cx).repaints)
                })
            };
            let (body, frames, repaints) = read(cx);
            assert!(body > 0, "{tab:?}: the tab content never rendered");
            const N: usize = 20;
            for i in 0..N {
                // The real tick (monitor levels), then a visible swing so every tick repaints.
                view.update(cx, |v, cx| {
                    v.tick(cx);
                    v.levels = if i % 2 == 0 { (0.9, 0.01) } else { (0.01, 0.9) };
                    v.meter_source = "monitor";
                    v.show_meters(cx);
                });
                cx.run_until_parked();
                std::thread::sleep(std::time::Duration::from_millis(70));
            }
            let (body2, frames2, repaints2) = read(cx);
            assert_eq!(body2, body, "{tab:?}: meter ticks rendered the transcript");
            assert!(
                repaints2 - repaints >= (N / 2) as u64,
                "{tab:?}: meters never repainted"
            );
            assert!(
                frames2 - frames >= (N / 2) as u64,
                "{tab:?}: the meters drew no frame"
            );
            assert!(cx.debug_bounds("status-meters").is_some());
            // The window's own state changes still reach the tab content.
            view.update(cx, |_, cx| cx.notify());
            cx.run_until_parked();
            assert_eq!(
                read(cx).0,
                body2 + 1,
                "{tab:?}: a window notify missed the content"
            );
        }
    }

    /// dot-4uzgol.97 perf guard: one idle second of ticks far faster than `ui.tick`, each one a
    /// visible level swing, draws at most 15 meter frames (`ui.meter_interval`) and lays the long
    /// transcript out not once.
    #[test]
    fn an_idle_second_draws_at_most_15_meter_frames_and_no_transcript_layout() {
        let (_engine, _dir, view, cx) = long_session_window(400, &[]);
        let read = |cx: &mut gpui::VisualTestContext| {
            view.update(cx, |v, cx| (v.body_renders(cx), v.frames))
        };
        let (body, frames) = read(cx);
        let start = Instant::now();
        let mut i = 0;
        while start.elapsed() < std::time::Duration::from_secs(1) {
            view.update(cx, |v, cx| {
                v.levels = if i % 2 == 0 { (0.9, 0.9) } else { (0.01, 0.01) };
                v.meter_source = "monitor";
                v.show_meters(cx);
            });
            cx.run_until_parked();
            i += 1;
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let (body2, frames2) = read(cx);
        assert_eq!(body2, body, "the transcript was laid out while idle");
        let drawn = frames2 - frames;
        // 15 per second, plus the one a throttle window straddling the second's end allows.
        assert!(
            drawn >= 5,
            "{drawn} frames: the meters stopped drawing ({i} ticks)"
        );
        assert!(drawn <= 16, "{drawn} frames in one idle second ({i} ticks)");
    }

    #[test]
    fn meters_map_rms_onto_a_60_db_scale() {
        assert_eq!(meter_fill(0.), 0.);
        assert_eq!(meter_fill(0.001), 0.);
        assert!((meter_fill(0.1) - 2. / 3.).abs() < 1e-4);
        assert_eq!(meter_fill(1.), 1.);
        assert_eq!(meter_fill(2.), 1.);
    }

    #[test]
    fn main_window_renders_with_menubar_on_and_off() {
        render_each_tab(&[("app.menubar", json!(true))]);
        render_each_tab(&[("app.menubar", json!(false))]);
    }
}
