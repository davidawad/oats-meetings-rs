//! Sessions tab: the session list and the selected session's transcript.

use gpui::{div, prelude::*, px, rgb, AnyElement, Context, SharedString};

use crate::view::{MainWindow, ACCENT, BG, FG, PANEL};

pub fn render(w: &MainWindow, cx: &mut Context<MainWindow>) -> AnyElement {
    let list = div()
        .id("sessions")
        .flex()
        .flex_col()
        .gap_1()
        .w(px(280.))
        .overflow_y_scroll()
        .children(w.state.sessions.iter().map(|s| {
            let id = s.id.clone();
            let sel = w.state.selected.as_deref() == Some(s.id.as_str());
            let title = if s.title.is_empty() {
                s.id.clone()
            } else {
                s.title.clone()
            };
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
        }));
    let detail = div()
        .id("transcript")
        .flex_1()
        .min_w_0()
        .overflow_y_scroll()
        .child(MainWindow::lines(
            &w.state.transcript,
            if w.state.selected.is_some() {
                "Empty transcript."
            } else {
                "Select a session."
            },
            w.speaker_width(),
        ));
    div()
        .flex()
        .gap_4()
        .size_full()
        .child(list)
        .child(detail)
        .into_any_element()
}
