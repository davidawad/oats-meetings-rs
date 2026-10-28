//! The main window: Live transcript, Sessions list, Settings. Pure presentation over `State`;
//! every control dispatches an action id through `model::dispatch`.

use std::sync::Arc;

use std::time::Instant;

use gpui::{div, prelude::*, px, rgb, Context, FocusHandle, IntoElement, Render, ScrollHandle, SharedString, Window};
use oats_core::control::engine::Engine;
use serde_json::json;

use crate::settings::{next_value, Row, Widget};
use crate::state::{badge_text, Control, Controls, EditKey, Line, Rec, State, Tab};

const ERR: u32 = 0xf38ba8;

const BG: u32 = 0x1e1e2e;
const PANEL: u32 = 0x313244;
const FG: u32 = 0xcdd6f4;
const DIM: u32 = 0x9399b2;
const ACCENT: u32 = 0x89b4fa;
const WARN: u32 = 0xf9e2af;
const REC_RED: u32 = 0xf38ba8;
const PAUSE_AMBER: u32 = 0xf9e2af;
const OK_GREEN: u32 = 0xa6e3a1;

pub struct MainWindow {
    engine: Arc<Engine>,
    pub state: State,
    /// Keyboard focus for the settings text editor; created on first render.
    focus: Option<FocusHandle>,
    live_scroll: ScrollHandle,
}

impl MainWindow {
    pub fn new(engine: Arc<Engine>) -> Self {
        let mut state = State::default();
        state.refresh(&engine);
        state.sync_rec(&engine);
        Self { engine, state, focus: None, live_scroll: ScrollHandle::new() }
    }

    /// Apply a pending event line from the engine bus and re-render.
    pub fn event(&mut self, line: &str, cx: &mut Context<Self>) {
        let before = self.state.live.len();
        self.state.on_event(&self.engine, line);
        self.after_live_change(before);
        cx.notify();
    }

    fn after_live_change(&mut self, before: usize) {
        if self.engine.snapshot().config.ui.live_follow && self.state.live.len() >= before {
            self.live_scroll.scroll_to_bottom();
        }
    }

    /// Per host tick: reconcile with the recorder and keep the elapsed clock moving.
    pub fn tick(&mut self, cx: &mut Context<Self>) {
        let before = self.state.rec.clone();
        if self.state.pending.is_none() {
            self.state.sync_rec(&self.engine);
        }
        if self.state.rec != before || matches!(self.state.rec, Rec::Recording { .. }) {
            cx.notify();
        }
    }

    /// A recording button was pressed: the same engine action as `ctl` and the shortcuts. It runs off
    /// the UI thread (a start loads the ASR model) while the buttons show what is pending.
    pub fn press(&mut self, c: Control, cx: &mut Context<Self>) {
        let allowed = if self.state.pending.is_some() { Controls::BUSY } else { Controls::of(&self.state.rec) };
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
            let res = cx.background_executor().spawn(async move { crate::model::dispatch(&eng, c.action(), json!({})) }).await;
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

    fn button(&self, id: &'static str, label: &'static str, on: bool, c: Control, cx: &mut Context<Self>) -> impl IntoElement {
        let b = div().id(id).px_3().py_1().rounded_md().child(label);
        if on {
            b.cursor_pointer().bg(rgb(ACCENT)).text_color(rgb(BG)).on_click(cx.listener(move |this, _, _, cx| this.press(c, cx)))
        } else {
            b.bg(rgb(PANEL)).text_color(rgb(DIM))
        }
    }

    fn record_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let cfg = self.engine.snapshot();
        let ui = &cfg.config.ui;
        let c = if self.state.pending.is_some() { Controls::BUSY } else { Controls::of(&self.state.rec) };
        let colour = match self.state.rec {
            Rec::Idle => DIM,
            Rec::Recording { .. } => REC_RED,
            Rec::Paused { .. } => PAUSE_AMBER,
        };
        div()
            .id("record-bar")
            .flex()
            .gap_2()
            .items_center()
            .child(self.button("rec-start", "Start", c.start, Control::Start, cx))
            .child(self.button("rec-pause", c.pause_label, c.pause, Control::Pause, cx))
            .child(self.button("rec-stop", "Stop", c.stop, Control::Stop, cx))
            .child(div().id("rec-badge").flex().gap_2().items_center().ml_2().child(div().size(px(10.)).rounded_full().bg(rgb(colour))).child(
                div().text_color(rgb(colour)).child(match self.state.pending {
                    Some(p) => p.busy_label().to_string(),
                    None => badge_text(&self.state.rec, Instant::now(), ui.show_elapsed),
                }),
            ))
    }

    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        self.state.refresh(&self.engine);
        cx.notify();
    }

    fn tab_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div().flex().gap_2().flex_none().children(Tab::ALL.map(|t| {
            let active = self.state.tab == t;
            div()
                .id(SharedString::from(format!("tab-{}", t.label())))
                .px_3()
                .py_1()
                .rounded_md()
                .cursor_pointer()
                .bg(rgb(if active { ACCENT } else { PANEL }))
                .text_color(rgb(if active { BG } else { FG }))
                .child(t.label())
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.state.tab = t;
                    this.state.cancel_edit();
                    cx.notify();
                }))
        }))
    }

    fn lines(lines: &[Line], empty: &str, speaker_w: f32) -> impl IntoElement {
        let body = div().flex().flex_col().gap_1().w_full();
        if lines.is_empty() {
            return body.child(div().text_color(rgb(DIM)).child(empty.to_string()));
        }
        body.children(lines.iter().map(|l| {
            div()
                .flex()
                .items_start()
                .gap_2()
                .w_full()
                .child(div().w(px(speaker_w)).flex_none().text_color(rgb(ACCENT)).child(l.speaker.clone()))
                // flex_1 + min_w_0 lets long text wrap inside the row instead of running off the window.
                .child(div().flex_1().min_w_0().whitespace_normal().child(l.text.clone()))
        }))
    }

    /// The transcription-unavailable banner, with a button when an action fixes it.
    fn banner(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let p = self.state.stt_problem.as_ref()?;
        let mut row = div().flex().gap_3().items_center().px_3().py_2().rounded_md().bg(rgb(PANEL)).text_color(rgb(WARN)).child(p.message.clone());
        if let Some(fix) = p.fix.clone() {
            row = row.child(
                div()
                    .id("stt-fix")
                    .px_3()
                    .rounded_md()
                    .cursor_pointer()
                    .bg(rgb(ACCENT))
                    .text_color(rgb(BG))
                    .child("Download model")
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.state.act(&this.engine, &fix, json!({}));
                        cx.notify();
                    })),
            );
        }
        Some(row)
    }

    fn speaker_width(&self) -> f32 {
        self.engine.snapshot().config.ui.speaker_width as f32
    }

    fn live(&self) -> impl IntoElement {
        let w = self.speaker_width();
        let empty = match self.state.rec {
            Rec::Idle if self.state.live_session.is_some() => "Empty transcript.",
            Rec::Idle => "No recording. Press Start (or ctrl+shift+R, or `ctl record-start`).",
            _ => "Listening... lines appear as speech is recognised.",
        };
        let header = match (&self.state.rec, &self.state.live_session) {
            (Rec::Idle, Some(s)) => format!("Last recording: {s} (saved; see Sessions)"),
            (r, _) if r.session().is_some() => format!("Recording {}", r.session().unwrap_or_default()),
            _ => format!("{} audio devices", self.state.devices),
        };
        div()
            .flex()
            .flex_col()
            .gap_2()
            .size_full()
            .child(div().text_sm().text_color(rgb(DIM)).flex_none().child(header))
            .child(
                div()
                    .id("live")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .track_scroll(&self.live_scroll)
                    .child(Self::lines(&self.state.live, empty, w)),
            )
    }

    fn sessions(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let list = div().id("sessions").flex().flex_col().gap_1().w(px(280.)).overflow_y_scroll().children(
            self.state.sessions.iter().map(|s| {
                let id = s.id.clone();
                let sel = self.state.selected.as_deref() == Some(s.id.as_str());
                let title = if s.title.is_empty() { s.id.clone() } else { s.title.clone() };
                div()
                    .id(SharedString::from(format!("session-{}", s.id)))
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .cursor_pointer()
                    .bg(rgb(if sel { ACCENT } else { PANEL }))
                    .text_color(rgb(if sel { BG } else { FG }))
                    .child(format!("{title}  ({} lines)", s.utterances))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.state.select(&this.engine, &id);
                        cx.notify();
                    }))
            }),
        );
        let detail = div().id("transcript").flex_1().min_w_0().overflow_y_scroll().child(Self::lines(
            &self.state.transcript,
            if self.state.selected.is_some() { "Empty transcript." } else { "Select a session." },
            self.speaker_width(),
        ));
        div().flex().gap_4().size_full().child(list).child(detail)
    }

    fn setting_row(&self, row: &Row, cx: &mut Context<Self>) -> impl IntoElement {
        let (path, next) = (row.path, next_value(row));
        let text_seed = match &row.widget {
            Widget::Text(t) => Some(t.clone()),
            Widget::Number { value, .. } => Some(value.to_string()),
            Widget::Duration { text, .. } => Some(text.clone()),
            _ => None,
        };
        let editing = self.state.edit.as_ref().filter(|e| e.path == path);
        let value = match (&row.widget, editing) {
            (_, Some(e)) => format!("{}|", e.buf),
            (Widget::Toggle(b), _) => (if *b { "on" } else { "off" }).to_string(),
            (Widget::Choice { current, .. }, _) => current.clone(),
            (Widget::Text(t), _) if t.is_empty() => "(default)".to_string(),
            (Widget::Text(t), _) => t.clone(),
            (Widget::Number { value, .. }, _) => value.to_string(),
            (Widget::Duration { text, .. }, _) => text.clone(),
        };
        let mut ctl = div()
            .id(SharedString::from(format!("set-{path}")))
            .px_2()
            .rounded_md()
            .min_w(px(160.))
            .bg(rgb(PANEL))
            .border_1()
            .border_color(rgb(if editing.is_some() { ACCENT } else { PANEL }))
            .child(value);
        if let Some(v) = next {
            ctl = ctl.cursor_pointer().on_click(cx.listener(move |this, _, _, cx| {
                this.state.act(&this.engine, "config-set", json!({"key": path, "value": v}));
                cx.notify();
            }));
        } else if let Some(seed) = text_seed {
            ctl = ctl.cursor_text().on_click(cx.listener(move |this, _, window, cx| {
                this.state.begin_edit(path, &seed);
                if let Some(f) = &this.focus {
                    f.focus(window, cx);
                }
                cx.notify();
            }));
        }
        let mut top = div().flex().gap_3().items_center().child(div().w(px(260.)).flex_none().child(path)).child(ctl);
        if let Some(e) = editing {
            top = top
                .child(self.chip("save", cx, move |this, _, cx| {
                    this.state.commit_edit(&this.engine);
                    cx.notify();
                }))
                .child(self.chip("cancel", cx, move |this, _, cx| {
                    this.state.cancel_edit();
                    cx.notify();
                }));
            let _ = e;
        }
        let mut out = div().flex().flex_col().flex_none().gap_1().w_full().child(top);
        if let Some(err) = editing.and_then(|e| e.error.as_ref()) {
            out = out.child(div().text_sm().text_color(rgb(ERR)).child(err.clone()));
        }
        if editing.is_some() && path.ends_with(".device") {
            out = out.child(self.device_picker(path, cx));
        }
        out.child(div().w_full().text_color(rgb(DIM)).text_sm().child(row.doc))
    }

    fn chip(
        &self,
        label: &str,
        cx: &mut Context<Self>,
        f: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
    ) -> impl IntoElement {
        div()
            .id(SharedString::from(format!("chip-{label}")))
            .px_2()
            .rounded_md()
            .cursor_pointer()
            .bg(rgb(PANEL))
            .text_sm()
            .child(label.to_string())
            .on_click(cx.listener(move |this, _, window, cx| f(this, window, cx)))
    }

    /// Devices from `devices-list`, one click to fill the input; the empty id is the default device.
    fn device_picker(&self, path: &'static str, cx: &mut Context<Self>) -> impl IntoElement {
        let opts = std::iter::once(("".to_string(), "(default)".to_string()))
            .chain(self.state.device_opts.iter().map(|d| (d.id.clone(), format!("{} [{}]", d.name, d.kind))));
        div().flex().flex_wrap().gap_1().children(opts.map(|(id, label)| {
            div()
                .id(SharedString::from(format!("dev-{path}-{id}")))
                .px_2()
                .rounded_md()
                .cursor_pointer()
                .bg(rgb(PANEL))
                .text_sm()
                .child(label)
                .on_click(cx.listener(move |this, _, _, cx| {
                    if let Some(e) = this.state.edit.as_mut() {
                        e.buf = id.clone();
                        e.error = None;
                    }
                    cx.notify();
                }))
        }))
    }

    fn settings(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let mut col = div().id("settings").flex().flex_col().gap_3().size_full().overflow_y_scroll();
        for g in &self.state.settings {
            col = col.child(div().flex_none().text_color(rgb(ACCENT)).child(g.name.clone()));
            for r in &g.rows {
                col = col.child(self.setting_row(r, cx));
            }
        }
        col
    }
}

impl Render for MainWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        window.set_window_title("Oats");
        let focus = self.focus.get_or_insert_with(|| cx.focus_handle()).clone();
        let controls = self.engine.snapshot().config.ui.record_controls;
        let body = match self.state.tab {
            Tab::Live => self.live().into_any_element(),
            Tab::Sessions => self.sessions(cx).into_any_element(),
            Tab::Settings => self.settings(cx).into_any_element(),
        };
        div()
            .flex()
            .flex_col()
            .gap_3()
            .p_4()
            .size_full()
            .bg(rgb(BG))
            .text_color(rgb(FG))
            .track_focus(&focus)
            .on_key_down(cx.listener(|this, ev: &gpui::KeyDownEvent, _, cx| {
                let k = &ev.keystroke;
                let command = k.modifiers.control || k.modifiers.alt || k.modifiers.platform;
                match this.state.edit_key(&k.key, k.key_char.as_deref(), command) {
                    EditKey::Commit => this.state.commit_edit(&this.engine),
                    EditKey::Cancel => this.state.cancel_edit(),
                    EditKey::Handled => {}
                    EditKey::Ignored => return,
                }
                cx.stop_propagation();
                cx.notify();
            }))
            .child(self.tab_bar(cx))
            .children(self.banner(cx))
            .children(controls.then(|| self.record_bar(cx)))
            .child(div().flex_1().min_h_0().overflow_hidden().child(body))
            .child(div().text_sm().text_color(rgb(if self.state.status.starts_with("Saved") { OK_GREEN } else { DIM })).flex_none().child(self.state.status.clone()))
    }
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
        std::fs::write(s.join("session.json"), r#"{"id":"s1","title":"T","startedAt":"2026-10-27T10:00:00Z"}"#).unwrap();
        std::fs::write(s.join("transcript.final.jsonl"), "{\"speaker\":\"you\",\"text\":\"hello\"}\n").unwrap();
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
        for (line, _, _) in &steps {
            view.update(cx, |v, cx| {
                v.state.tab = Tab::Live;
                v.event(line, cx);
            });
            cx.run_until_parked();
            view.read_with(cx, |v, _| {
                let c = Controls::of(&v.state.rec);
                assert_eq!(c.start, matches!(v.state.rec, Rec::Idle));
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
        view.read_with(cx, |v, _| assert!(matches!(v.state.rec, Rec::Recording { .. })));
        press(Control::Start, cx); // already recording: ignored, no error
        press(Control::Pause, cx);
        assert!(engine.recording().flatten().is_some_and(|r| r.1));
        view.read_with(cx, |v, _| assert!(matches!(v.state.rec, Rec::Paused { .. })));
        press(Control::Pause, cx);
        assert!(engine.recording().flatten().is_some_and(|r| !r.1));
        press(Control::Stop, cx);
        assert!(engine.recording() == Some(None));
        view.read_with(cx, |v, _| assert_eq!(v.state.rec, Rec::Idle));
    }

    #[test]
    fn main_window_renders_with_controls_hidden() {
        render_each_tab(&[("ui.record_controls", json!(false)), ("ui.live_follow", json!(false)), ("ui.show_elapsed", json!(false))]);
    }

    #[test]
    fn main_window_renders_every_tab() {
        render_each_tab(&[]);
    }

    #[test]
    fn main_window_renders_with_menubar_on_and_off() {
        render_each_tab(&[("app.menubar", json!(true))]);
        render_each_tab(&[("app.menubar", json!(false))]);
    }
}
