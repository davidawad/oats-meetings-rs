//! GPUI front end (viewer). Hello window only in the skeleton.
use gpui::{
    div, prelude::*, px, rgb, size, App, Application, Bounds, Context, SharedString, Window,
    WindowBounds, WindowOptions,
};

struct Hello {
    text: SharedString,
}

impl Render for Hello {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .size_full()
            .justify_center()
            .items_center()
            .bg(rgb(0x1e1e2e))
            .text_color(rgb(0xcdd6f4))
            .text_xl()
            .child(self.text.clone())
    }
}

pub fn run_hello() {
    Application::new().run(|cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(480.), px(240.)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            |_, cx| {
                cx.new(|_| Hello {
                    text: oats_core::version_line().into(),
                })
            },
        )
        .expect("open window");
        cx.activate(true);
    });
}
