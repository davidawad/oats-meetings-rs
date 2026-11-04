//! Settings tab: every `ui = true` config key (`crate::settings`), in the design's sections. Each
//! row is a label and the key's doc on the left and its control on the right: a toggle for a bool,
//! a dropdown for an enum, a text box (opened on click, Enter saves) for strings, numbers and
//! durations. Every change goes through `config-set`, the call `ctl config-set` makes.

use gpui::{
    anchored, deferred, div, prelude::*, px, rgb, AnyElement, Context, FontWeight, SharedString,
    Window,
};
use serde_json::json;

use crate::kit::{self, Kind, Size};
use crate::settings::{Row, Widget};
use crate::theme::{self, MONO};
use crate::view::{edit_text, MainWindow};

fn set(this: &mut MainWindow, path: &str, value: serde_json::Value) {
    this.state.act(
        &this.engine,
        "config-set",
        json!({"key": path, "value": value}),
    );
}

/// A choice row's dropdown and, while open, its menu floating over the rows below.
fn choice(
    w: &MainWindow,
    path: &'static str,
    options: &[&'static str],
    current: &str,
    cx: &mut Context<MainWindow>,
) -> AnyElement {
    let open = w.menu == Some(path);
    let shown = if path == "ui.theme" && current == "system" {
        "Match system".to_string()
    } else {
        current.to_string()
    };
    let trigger = kit::dropdown(format!("set-{path}"), shown, open).on_click(cx.listener(
        move |this, _, _, cx| {
            this.menu = if this.menu == Some(path) {
                None
            } else {
                Some(path)
            };
            cx.notify();
        },
    ));
    let mut out = div().relative().flex_none().child(trigger);
    if open {
        let items = options.iter().map(|&o| {
            kit::menu_item(format!("set-{path}-{o}"), o, o == current).on_click(cx.listener(
                move |this, _, _, cx| {
                    this.menu = None;
                    set(this, path, json!(o));
                    cx.notify();
                },
            ))
        });
        out = out.child(
            div().absolute().top(px(34.)).left_0().child(deferred(
                anchored().child(
                    kit::menu()
                        .id(SharedString::from(format!("menu-{path}")))
                        .children(items),
                ),
            )),
        );
    }
    out.into_any_element()
}

fn open_edit(
    this: &mut MainWindow,
    path: &'static str,
    seed: &str,
    window: &mut Window,
    cx: &mut Context<MainWindow>,
) {
    this.menu = None;
    this.state.begin_edit(path, seed);
    if let Some(f) = &this.focus {
        f.focus(window, cx);
    }
    cx.notify();
}

fn setting_row(w: &MainWindow, row: &Row, last: bool, cx: &mut Context<MainWindow>) -> AnyElement {
    let t = theme::current();
    let path = row.path;
    let editing = w.state.edit.as_ref().filter(|e| e.path == path);
    let control = match &row.widget {
        Widget::Toggle(on) => {
            let next = !*on;
            kit::toggle(format!("set-{path}"), *on)
                .on_click(cx.listener(move |this, _, _, cx| {
                    set(this, path, json!(next));
                    cx.notify();
                }))
                .into_any_element()
        }
        Widget::Choice { options, current } => choice(w, path, options, current, cx),
        Widget::Text(_) | Widget::Number { .. } | Widget::Duration { .. } => {
            let seed = match &row.widget {
                Widget::Text(t) => t.clone(),
                Widget::Number { value, .. } => value.to_string(),
                Widget::Duration { text, .. } => text.clone(),
                _ => String::new(),
            };
            let value = match editing {
                Some(e) => edit_text(e).into_any_element(),
                None if seed.is_empty() => div()
                    .text_color(rgb(t.faint))
                    .child("(default)")
                    .into_any_element(),
                None => div().child(seed.clone()).into_any_element(),
            };
            let mut f = kit::field(format!("set-{path}"), editing.is_some())
                .w(px(240.))
                .flex_none()
                .text_size(px(13.))
                .child(value);
            if editing.is_none() {
                f =
                    f.on_click(cx.listener(move |this, _, window, cx| {
                        open_edit(this, path, &seed, window, cx)
                    }));
            }
            f.into_any_element()
        }
    };
    let mut right = div()
        .flex()
        .flex_none()
        .items_center()
        .gap(px(8.))
        .child(control);
    if editing.is_some() {
        right = right
            .child(
                kit::button("chip-save", "Save", Kind::Primary, Size::Sm, true).on_click(
                    cx.listener(|this, _, _, cx| {
                        this.state.commit_edit(&this.engine);
                        cx.notify();
                    }),
                ),
            )
            .child(
                kit::button("chip-cancel", "Cancel", Kind::Secondary, Size::Sm, true).on_click(
                    cx.listener(|this, _, _, cx| {
                        this.state.cancel_edit();
                        cx.notify();
                    }),
                ),
            );
    }
    let mut left = div()
        .flex()
        .flex_col()
        .flex_1()
        .min_w_0()
        .gap(px(2.))
        .child(
            div()
                .flex()
                .items_baseline()
                .gap(px(8.))
                .child(
                    div()
                        .font_weight(FontWeight::MEDIUM)
                        .child(row.label.clone()),
                )
                .child(
                    div()
                        .font_family(MONO)
                        .text_size(px(11.))
                        .text_color(rgb(t.faint))
                        .child(path),
                ),
        )
        .child(
            div()
                .text_size(px(12.))
                .text_color(rgb(t.dim))
                .whitespace_normal()
                .child(row.doc),
        );
    if let Some(err) = editing.and_then(|e| e.error.as_ref()) {
        left = left.child(
            div()
                .text_size(px(12.))
                .text_color(rgb(t.err))
                .child(err.clone()),
        );
    }
    if editing.is_some() && path.ends_with(".device") {
        left = left.child(device_picker(w, path, cx));
    }
    div()
        .id(SharedString::from(format!("row-{path}")))
        .flex()
        .flex_none()
        .items_center()
        .gap(px(20.))
        .min_h(px(52.))
        .px(px(16.))
        .py(px(8.))
        .when(!last, |d| d.border_b_1().border_color(rgb(t.line)))
        .child(left)
        .child(right)
        .into_any_element()
}

/// Devices from `devices-list`, one click to fill the input; the empty id is the default device.
fn device_picker(
    w: &MainWindow,
    path: &'static str,
    cx: &mut Context<MainWindow>,
) -> impl IntoElement {
    let opts = std::iter::once(("".to_string(), "(default)".to_string())).chain(
        w.state
            .device_opts
            .iter()
            .map(|d| (d.id.clone(), format!("{} [{}]", d.name, d.kind))),
    );
    div()
        .flex()
        .flex_wrap()
        .gap(px(4.))
        .pt(px(4.))
        .children(opts.map(|(id, label)| {
            kit::button(
                format!("dev-{path}-{id}"),
                label,
                Kind::Secondary,
                Size::Sm,
                true,
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                if let Some(e) = this.state.edit.as_mut() {
                    e.buf = id.clone();
                    e.error = None;
                }
                cx.notify();
            }))
        }))
}

pub fn render(w: &MainWindow, cx: &mut Context<MainWindow>) -> AnyElement {
    let t = theme::current();
    let saved = w.state.status.starts_with("Saved");
    let head = kit::head("Settings")
        .child(div().flex_1())
        .when(saved, |h| {
            h.child(
                div()
                    .text_size(px(12.))
                    .text_color(rgb(t.ok))
                    .child("Saved"),
            )
        });
    let mut col = div()
        .id("settings")
        .flex()
        .flex_col()
        .flex_1()
        .min_h_0()
        .overflow_y_scroll()
        .px(px(28.))
        .pb(px(22.));
    for g in &w.state.settings {
        let n = g.rows.len();
        col = col.child(
            div()
                .flex()
                .flex_col()
                .flex_none()
                .max_w(px(820.))
                .mb(px(22.))
                .child(kit::section(g.name.clone()).text_color(rgb(t.dim)).pt_0())
                .child(
                    kit::panel().children(
                        g.rows
                            .iter()
                            .enumerate()
                            .map(|(i, r)| setting_row(w, r, i + 1 == n, cx)),
                    ),
                ),
        );
    }
    div()
        .flex()
        .flex_col()
        .size_full()
        .child(head)
        .child(col)
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::Tab;
    use crate::testutil;
    use gpui::TestAppContext;

    #[test]
    fn settings_render_with_a_menu_open_and_an_editor_open_and_write_through_config_set() {
        let (engine, _d) = testutil::engine(&[]);
        let mut cx = TestAppContext::single();
        let (view, cx) = cx.add_window_view(|_, _| MainWindow::new(engine.clone()));
        view.update(cx, |v, cx| {
            v.state.tab = Tab::Settings;
            v.menu = Some("ui.theme");
            cx.notify();
        });
        cx.run_until_parked();
        view.update(cx, |v, cx| {
            v.menu = None;
            set(v, "ui.theme", json!("light"));
            v.state.begin_edit("audio.mic.device", "");
            cx.notify();
        });
        cx.run_until_parked();
        assert_eq!(
            engine.snapshot().config.ui.theme,
            oats_core::config::typed::Theme::Light
        );
        view.read_with(cx, |v, _| {
            assert_eq!(
                v.theme,
                crate::theme::Theme::LIGHT,
                "the next frame repaints light"
            );
            assert!(v.state.settings.iter().any(|g| g.name == "Appearance"));
        });
    }
}
