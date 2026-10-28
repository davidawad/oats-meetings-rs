//! The main window: Live transcript, Sessions list, Settings. Pure presentation over `State`;
//! every control dispatches an action id through `model::dispatch`.

use std::sync::Arc;

use gpui::{div, prelude::*, px, rgb, Context, IntoElement, Render, SharedString, Window};
use oats_core::control::engine::Engine;
use serde_json::json;

use crate::settings::{next_value, Row, Widget};
use crate::state::{Line, State, Tab};

const BG: u32 = 0x1e1e2e;
const PANEL: u32 = 0x313244;
const FG: u32 = 0xcdd6f4;
const DIM: u32 = 0x9399b2;
const ACCENT: u32 = 0x89b4fa;

pub struct MainWindow {
    engine: Arc<Engine>,
    pub state: State,
}

impl MainWindow {
    pub fn new(engine: Arc<Engine>) -> Self {
        let mut state = State::default();
        state.refresh(&engine);
        Self { engine, state }
    }

    /// Apply a pending event line from the engine bus and re-render.
    pub fn event(&mut self, line: &str, cx: &mut Context<Self>) {
        self.state.on_event(line);
        cx.notify();
    }

    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        self.state.refresh(&self.engine);
        cx.notify();
    }

    fn tab_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div().flex().gap_2().children(Tab::ALL.map(|t| {
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
                    cx.notify();
                }))
        }))
    }

    fn lines(lines: &[Line], empty: &str) -> impl IntoElement {
        let body = div().flex().flex_col().gap_1();
        if lines.is_empty() {
            return body.child(div().text_color(rgb(DIM)).child(empty.to_string()));
        }
        body.children(lines.iter().map(|l| {
            div()
                .flex()
                .gap_2()
                .child(div().w(px(80.)).text_color(rgb(ACCENT)).child(l.speaker.clone()))
                .child(div().child(l.text.clone()))
        }))
    }

    fn live(&self) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(div().text_sm().text_color(rgb(DIM)).child(format!("{} audio devices", self.state.devices)))
            .child(Self::lines(&self.state.live, "No recording. Start one from the Recording menu."))
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
        let detail = div().id("transcript").flex_1().overflow_y_scroll().child(Self::lines(
            &self.state.transcript,
            if self.state.selected.is_some() { "Empty transcript." } else { "Select a session." },
        ));
        div().flex().gap_4().size_full().child(list).child(detail)
    }

    fn setting_row(&self, row: &Row, cx: &mut Context<Self>) -> impl IntoElement {
        let (path, next) = (row.path, next_value(row));
        let value = match &row.widget {
            Widget::Toggle(b) => (if *b { "on" } else { "off" }).to_string(),
            Widget::Choice { current, .. } => current.clone(),
            Widget::Text(t) if t.is_empty() => "(default)".to_string(),
            Widget::Text(t) => t.clone(),
        };
        let mut ctl = div()
            .id(SharedString::from(format!("set-{path}")))
            .px_2()
            .rounded_md()
            .bg(rgb(PANEL))
            .child(value);
        if let Some(v) = next {
            ctl = ctl.cursor_pointer().on_click(cx.listener(move |this, _, _, cx| {
                this.state.act(&this.engine, "config-set", json!({"key": path, "value": v}));
                cx.notify();
            }));
        }
        div()
            .flex()
            .gap_3()
            .items_center()
            .child(div().w(px(260.)).child(path))
            .child(ctl)
            .child(div().text_color(rgb(DIM)).text_sm().child(row.doc))
    }

    fn settings(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let mut col = div().id("settings").flex().flex_col().gap_3().size_full().overflow_y_scroll();
        for g in &self.state.settings {
            col = col.child(div().text_color(rgb(ACCENT)).child(g.name.clone()));
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
            .child(self.tab_bar(cx))
            .child(div().flex_1().child(body))
            .child(div().text_sm().text_color(rgb(DIM)).child(self.state.status.clone()))
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
