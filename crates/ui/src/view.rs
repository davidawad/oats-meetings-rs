//! The main window in the drafting-table design (docs/design/drafting-table): the left tab rail,
//! the active tab's view (see `views`; Live is here), the banner and the status line, over the
//! blueprint grid, with a red top edge while recording. Pure presentation over `State`; every
//! control dispatches an action id through `model::dispatch`.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use std::time::Instant;

use gpui::{
    canvas, div, fill, prelude::*, px, relative, rgb, rgba, Bounds, Context, FocusHandle,
    FontWeight, IntoElement, Render, ScrollHandle, SharedString, Subscription, Window,
};
use oats_core::control::engine::Engine;
use serde_json::json;

use crate::kit::{self, Icon, Kind, Size};
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
    /// Where the view state goes when the window closes, for the next window to resume.
    keep: Option<Rc<RefCell<Option<State>>>>,
    /// The `(mic, system)` RMS levels of the recording, read each tick for the meters.
    pub(crate) levels: (f32, f32),
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
            keep: None,
            levels: (0., 0.),
            menu: None,
            theme: Theme::DARK,
            appearance: None,
            frames: 0,
            keys: views::shortcuts::Keys::default(),
            focus_lost: None,
        }
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
        self.levels = match self.state.rec {
            Rec::Recording { .. } => self.engine.recording_levels().unwrap_or(self.levels),
            _ => (0., 0.),
        };
        // Until a listing arrives: the worker fills the engine's cache after the first refresh.
        let listing = self.state.devices_note.is_some();
        if listing {
            self.state.sync_devices(&self.engine);
        }
        if listing || self.state.rec != before || matches!(self.state.rec, Rec::Recording { .. }) {
            cx.notify();
        }
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
    fn record_strip(&self, cx: &mut Context<Self>) -> impl IntoElement {
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
        let clock = fmt_clock(self.state.rec.elapsed_ms(Instant::now()));
        let (mic, sys) = (meter_fill(self.levels.0), meter_fill(self.levels.1));
        let meter = |label: &'static str, level: f32| {
            div()
                .flex()
                .items_center()
                .gap(px(10.))
                .text_size(px(11.))
                .text_color(rgb(t.dim))
                .child(div().w(px(66.)).flex_none().child(label))
                .child(kit::meter(if idle { 0. } else { level }))
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
    /// same speaker as the one before hides its timestamp and tag (the columns stay).
    pub(crate) fn lines(lines: &[Line], empty: &str, speaker_w: f32) -> impl IntoElement {
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
                        .child(div().min_w_0().overflow_hidden().child(l.speaker.clone())),
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

    /// The transcription-unavailable banner, with a button when an action fixes it.
    pub(crate) fn banner(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let p = self.state.stt_problem.as_ref()?;
        let (lead, rest) = match p.message.split_once(": ") {
            Some((a, b)) => (format!("{a}."), b.to_string()),
            None => (p.message.clone(), String::new()),
        };
        let mut row = kit::banner(false, lead, rest);
        if let Some(fix) = p.fix.clone() {
            row = row.child(
                kit::button("stt-fix", "Download model", Kind::Primary, Size::Sm, true).on_click(
                    cx.listener(move |this, _, _, cx| {
                        this.state.act(&this.engine, &fix, json!({}));
                        cx.notify();
                    }),
                ),
            );
        }
        Some(row)
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

    fn inputs(&self) -> usize {
        let cfg = self.engine.snapshot();
        let a = &cfg.config.audio;
        usize::from(a.mic.enabled) + usize::from(a.system.enabled)
    }

    fn live(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let t = self.theme;
        let w = self.speaker_width();
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
        let body = if self.state.live.is_empty() {
            let (h, s) = match self.state.rec {
                Rec::Idle if self.state.live_session.is_some() => (
                    "Empty transcript",
                    "The last recording has no lines. Start recording to try again.".to_string(),
                ),
                Rec::Idle => {
                    let map = self.keymap();
                    let k = |c: &str| crate::shortcuts::shown(map.keys(c));
                    (
                        "Ready when you are",
                        format!(
                            "Start recording and the transcript appears here as people speak. Your \
                             voice shows as you, everyone else as remote_1, remote_2. (Also {} or \
                             {}, or `ctl record-start`; {} lists every shortcut.)",
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
                .child(Self::lines(&self.state.live, "", w))
                .into_any_element()
        };
        div()
            .flex()
            .flex_col()
            .size_full()
            .child(head)
            .children(controls.then(|| self.record_strip(cx)))
            .children(
                self.banner(cx)
                    .map(|b| div().flex_none().mx(px(28.)).mt(px(12.)).child(b)),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .text_color(rgb(t.text))
                    .child(body),
            )
    }

    /// The status line: what the window is doing (the last action's result, else the session),
    /// and on Live the model and runtime and the capture devices.
    fn status_bar(&self) -> impl IntoElement {
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
        bar
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
        // One line per tab; each view lives in its own file under `views/`.
        let body = match self.state.tab {
            Tab::Live => self.live(cx).into_any_element(),
            Tab::Sessions => {
                views::sessions::sync_hits(&mut self.state, &self.engine);
                views::sessions::render(self, cx)
            }
            Tab::Notes => views::notes::render(self, cx),
            Tab::Templates => views::templates::render(self, cx),
            Tab::Calendar => views::calendar::render(self, cx),
            Tab::Trash => views::trash::render(self, cx),
            Tab::Models => views::models::render(self, cx),
            Tab::Settings => views::settings::render(self, cx),
        };
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
                        .child(div().flex().flex_col().flex_1().min_h_0().child(body))
                        .child(self.status_bar()),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil;
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
