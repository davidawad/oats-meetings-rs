//! Settings tab: every `ui = true` config key (`crate::settings`), in the design's sections. Each
//! row is a label and the key's doc on the left and its control on the right: a toggle for a bool,
//! a dropdown for an enum, a text box (opened on click, Enter saves) for strings, numbers and
//! durations, and for a keyboard shortcut its keys with Rebind (press a key), Reset and Clear
//! (`views::shortcuts`). Every change goes through `config-set`, the call `ctl config-set` makes.
//!
//! A search bar is pinned above the rows (dot-4uzgol.90). The `search` shortcut (`/`) focuses it
//! while nothing is being typed in; it filters live by label, key path, doc and current value,
//! highlights what matched and hides empty sections. Esc clears it, a second Esc leaves it; Enter
//! leaves it with the filter applied and jumps to the first match.

use gpui::{
    anchored, deferred, div, point, prelude::*, px, rgb, AnyElement, Context, FontWeight,
    HighlightStyle, SharedString, StyledText, Window,
};
use serde_json::json;

use crate::kit::{self, Icon, Kind, Size};
use crate::model;
use crate::settings::{self, Group, Row, Widget};
use crate::state::State;
use crate::theme::{self, MONO};
use crate::view::{edit_text, MainWindow};

/// The search bar's `Edit` path.
pub(crate) const SEARCH: &str = "settings:search";

/// Settings-tab state the rest of the window does not need.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Ui {
    /// The applied search; while the search bar is open its buffer filters live instead.
    pub query: String,
    /// The row Enter jumped to (the first match), marked until the query changes.
    pub target: Option<&'static str>,
    /// Enter asked for the rows to scroll back to the first match.
    pub jump: bool,
}

/// The search text in effect: the open search bar, else the applied query.
pub fn query(st: &State) -> &str {
    match &st.edit {
        Some(e) if e.path == SEARCH => &e.buf,
        _ => &st.settings_ui.query,
    }
}

/// The sections and rows the search leaves.
pub fn visible(st: &State) -> Vec<Group> {
    settings::filter(&st.settings, query(st))
}

/// Open the search bar on the applied query (the `search` shortcut, a click on the bar).
pub fn begin_search(st: &mut State) {
    let q = st.settings_ui.query.clone();
    st.begin_edit(SEARCH, &q);
    st.settings_ui.target = None;
}

/// Enter in the search bar: apply the filter, close the bar and jump to the first match.
pub fn commit(st: &mut State) {
    let Some(e) = st.edit.take() else { return };
    st.settings_ui.query = e.buf.trim().to_string();
    st.settings_ui.target = visible(st)
        .first()
        .and_then(|g| g.rows.first())
        .map(|r| r.path);
    st.settings_ui.jump = true;
}

/// Esc in the search bar: clear what was typed, and leave the bar once it is empty. True when it
/// cleared (the bar stays open).
pub fn escape(st: &mut State) -> bool {
    match st.edit.as_mut().filter(|e| e.path == SEARCH) {
        Some(e) if !e.buf.is_empty() => {
            e.buf.clear();
            e.all = false;
            true
        }
        _ => false,
    }
}

/// Clear the applied search (Esc with the bar closed, the clear chip).
pub fn clear(st: &mut State) {
    if st.edit.as_ref().is_some_and(|e| e.path == SEARCH) {
        st.edit = None;
    }
    st.settings_ui = Ui::default();
}

/// `text` with the search's matches highlighted.
fn marked(text: impl Into<SharedString>, q: &str) -> StyledText {
    let t = theme::current();
    let text: SharedString = text.into();
    let style = HighlightStyle {
        background_color: Some(rgb(t.accent_soft).into()),
        color: Some(rgb(t.text).into()),
        ..HighlightStyle::default()
    };
    let ranges = settings::highlights(&text, q);
    StyledText::new(text).with_highlights(ranges.into_iter().map(|r| (r, style)))
}

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
    let q = query(&w.state);
    let target = w.state.settings_ui.target == Some(path);
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
        Widget::Keys(keys) => match path.strip_prefix("ui.keys.").and_then(model::binding) {
            Some(b) => crate::views::shortcuts::control(w, b.command, keys, cx),
            None => div().into_any_element(),
        },
        Widget::Info { value, copy } => {
            let shown = div()
                .max_w(px(420.))
                .text_size(px(12.))
                .child(value.clone());
            let mut row = div().flex().items_center().gap(px(8.)).child(shown);
            if let Some(snippet) = copy.clone() {
                row = row.child(
                    kit::button("api-info-copy", "Copy", Kind::Secondary, Size::Sm, true).on_click(
                        cx.listener(move |_, _, _, cx| {
                            cx.write_to_clipboard(gpui::ClipboardItem::new_string(snippet.clone()));
                        }),
                    ),
                );
            }
            row.into_any_element()
        }
        Widget::Text(_) | Widget::Number { .. } | Widget::Duration { .. } => {
            let seed = match &row.widget {
                Widget::Text(t) => t.clone(),
                Widget::Number { value, .. } => value.to_string(),
                Widget::Duration { text, .. } => text.clone(),
                Widget::Toggle(_)
                | Widget::Choice { .. }
                | Widget::Keys(_)
                | Widget::Info { .. } => String::new(),
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
                        .child(marked(row.label.clone(), q)),
                )
                .child(
                    div()
                        .font_family(MONO)
                        .text_size(px(11.))
                        .text_color(rgb(t.faint))
                        .child(marked(path, q)),
                ),
        )
        .child(
            div()
                .text_size(px(12.))
                .text_color(rgb(t.dim))
                .whitespace_normal()
                .child(marked(row.doc, q)),
        );
    let refused = w
        .keys
        .note
        .as_ref()
        .filter(|(c, _)| path.strip_prefix("ui.keys.") == Some(*c))
        .map(|(_, why)| why);
    if let Some(err) = editing.and_then(|e| e.error.as_ref()).or(refused) {
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
        .when(target, |d| d.bg(rgb(t.accent_soft)))
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

/// The search bar: the open buffer with a caret, else the applied query (or a hint), with the
/// match count and a clear chip while a query is in effect.
fn search_bar(w: &MainWindow, shown: usize, cx: &mut Context<MainWindow>) -> AnyElement {
    let t = theme::current();
    let q = query(&w.state).to_string();
    let editing = w.state.edit.as_ref().filter(|e| e.path == SEARCH);
    let keys = crate::shortcuts::shown(w.keymap().keys("search"));
    let text = match editing {
        Some(e) => edit_text(e).into_any_element(),
        None if q.is_empty() => div()
            .text_color(rgb(t.faint))
            .child(format!(
                "Search settings by name, key, description or value ({keys})"
            ))
            .into_any_element(),
        None => div().child(q.clone()).into_any_element(),
    };
    let field = kit::field("settings-search", editing.is_some())
        .debug_selector(|| "settings-search".into())
        .flex_1()
        .child(kit::icon(Icon::Search, t.dim).size(px(14.)))
        .child(text)
        .on_click(cx.listener(|this, _, window, cx| {
            this.menu = None;
            if !this.state.edit.as_ref().is_some_and(|e| e.path == SEARCH) {
                begin_search(&mut this.state);
            }
            if let Some(f) = &this.focus {
                f.focus(window, cx);
            }
            cx.notify();
        }));
    let total: usize = w.state.settings.iter().map(|g| g.rows.len()).sum();
    div()
        .flex()
        .flex_none()
        .items_center()
        .gap(px(8.))
        .max_w(px(820.))
        .mx(px(28.))
        .mb(px(14.))
        .child(field)
        .when(!q.trim().is_empty(), |d| {
            d.child(
                div()
                    .flex_none()
                    .text_size(px(12.))
                    .text_color(rgb(t.dim))
                    .child(format!("{shown} of {total}")),
            )
            .child(
                kit::button(
                    "settings-search-clear",
                    "Clear",
                    Kind::Secondary,
                    Size::Sm,
                    true,
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    clear(&mut this.state);
                    cx.notify();
                })),
            )
        })
        .into_any_element()
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
    let groups = visible(&w.state);
    let shown: usize = groups.iter().map(|g| g.rows.len()).sum();
    let mut col = div()
        .id("settings")
        .flex()
        .flex_col()
        .flex_1()
        .min_h_0()
        .overflow_y_scroll()
        .track_scroll(&w.settings_scroll)
        .px(px(28.))
        .pb(px(22.));
    if groups.is_empty() {
        col = col.child(kit::empty(
            "No settings match",
            "Try fewer words, or press Esc to clear the search.",
        ));
    }
    for g in &groups {
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
        if g.name == "Calendar" {
            col = col.child(super::calendar_sources::panel(w, cx));
        }
        if g.name == "Hooks" {
            col = col.child(super::hooks::panel(w, cx));
        }
    }
    div()
        .flex()
        .flex_col()
        .size_full()
        .child(head)
        .child(search_bar(w, shown, cx))
        .child(col)
        .into_any_element()
}

/// Before a frame: Enter's jump scrolls the rows back to the top, where the first match now is.
pub(crate) fn prepare(w: &mut MainWindow) {
    super::hooks::prepare(&mut w.state, &w.engine);
    if std::mem::take(&mut w.state.settings_ui.jump) {
        w.settings_scroll.set_offset(point(px(0.), px(0.)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::Tab;
    use crate::testutil;
    use gpui::TestAppContext;
    use serde_json::Value;

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

    fn search_dump(view: &gpui::Entity<MainWindow>, cx: &mut gpui::VisualTestContext) -> Value {
        view.update(cx, |v, cx| v.dump(cx))["view"]["search"].clone()
    }

    fn window<'a>(
        engine: &std::sync::Arc<oats_core::control::engine::Engine>,
        cx: &'a mut TestAppContext,
    ) -> (gpui::Entity<MainWindow>, &'a mut gpui::VisualTestContext) {
        let (view, cx) = cx.add_window_view(|_, _| MainWindow::new(engine.clone()));
        cx.update(|_, cx| {
            let map = crate::shortcuts::Keymap::of(&engine.snapshot().config);
            crate::shortcuts::sync(cx, &map, false);
        });
        cx.run_until_parked();
        (view, cx)
    }

    fn keys(cx: &mut gpui::VisualTestContext, k: &str) {
        cx.simulate_keystrokes(k);
        cx.run_until_parked();
    }

    #[test]
    fn slash_focuses_the_search_bar_which_filters_live_and_esc_clears_then_leaves() {
        let (engine, _d) = testutil::engine(&[("ui.tz", json!("Europe/Lisbon"))]);
        let mut cx = TestAppContext::single();
        let (view, cx) = window(&engine, &mut cx);
        keys(cx, "7");
        view.read_with(cx, |v, _| assert_eq!(v.state.tab, Tab::Settings));
        assert!(cx.debug_bounds("settings-search").is_some(), "pinned bar");
        let all = search_dump(&view, cx)["matches"].as_u64().unwrap();
        assert!(all > 20, "{all}");
        // / opens the bar; what follows is typed into it and filters as it is typed.
        keys(cx, "/ l i s b o n");
        let s = search_dump(&view, cx);
        assert_eq!(s["query"], "lisbon");
        assert_eq!(s["open"], true);
        assert_eq!(s["shown"], json!(["ui.tz"]), "by current value");
        assert_eq!(s["matches"], 1);
        view.read_with(cx, |v, _| {
            let groups = visible(&v.state);
            assert_eq!(groups.len(), 1, "empty sections are hidden");
            assert_eq!(groups[0].name, "Appearance");
        });
        // By key path, a / typed into the bar is a /, not a shortcut.
        keys(cx, "escape");
        let s = search_dump(&view, cx);
        assert_eq!(
            (s["query"].as_str(), s["open"].as_bool()),
            (Some(""), Some(true))
        );
        assert_eq!(s["matches"].as_u64(), Some(all), "cleared, still open");
        keys(cx, "s t t . l o c a l . m o d e l");
        assert!(search_dump(&view, cx)["shown"]
            .as_array()
            .unwrap()
            .contains(&json!("stt.local.model")));
        keys(cx, "escape z / q");
        let s = search_dump(&view, cx);
        assert_eq!(s["query"], "z/q");
        assert_eq!(s["matches"], 0, "no settings match");
        view.read_with(cx, |v, _| assert!(visible(&v.state).is_empty()));
        // Esc clears, a second Esc leaves the bar; the tab stays.
        keys(cx, "escape escape");
        let s = search_dump(&view, cx);
        assert_eq!(
            (s["query"].as_str(), s["open"].as_bool()),
            (Some(""), Some(false))
        );
        view.read_with(cx, |v, _| {
            assert_eq!(v.state.tab, Tab::Settings);
            assert!(v.state.edit.is_none());
        });
        // Bare keys work again once it is closed: ? opens the overlay.
        keys(cx, "?");
        view.read_with(cx, |v, _| assert!(v.keys.help));
        keys(cx, "escape");
    }

    #[test]
    fn enter_jumps_to_the_first_match_and_esc_then_clears_the_applied_filter() {
        let (engine, _d) = testutil::engine(&[]);
        let mut cx = TestAppContext::single();
        let (view, cx) = window(&engine, &mut cx);
        keys(cx, "7 / d e v i c e enter");
        let s = search_dump(&view, cx);
        assert_eq!(s["open"], false);
        assert_eq!(s["query"], "device");
        let first = s["shown"][0].clone();
        assert_eq!(s["target"], first, "Enter marks the first match");
        assert_eq!(first, "audio.mic.device");
        view.read_with(cx, |v, _| {
            assert!(!v.state.settings_ui.jump, "the frame took the jump");
            assert_eq!(v.settings_scroll.offset().y, px(0.));
        });
        // The filter stays applied; Esc (back) clears it.
        keys(cx, "escape");
        let s = search_dump(&view, cx);
        assert_eq!(s["query"], "");
        assert_eq!(s["target"], Value::Null);
        // ctl drives the same: ui-invoke settings.search.
        view.update(cx, |v, cx| {
            let d = v
                .drive(
                    &json!({"op": "invoke", "name": "settings.search", "args": {"query": "theme"}}),
                    cx,
                )
                .unwrap();
            assert_eq!(d["view"]["search"]["query"], "theme");
            assert!(d["view"]["search"]["matches"].as_u64().unwrap() >= 1);
        });
    }

    #[test]
    fn a_slash_typed_into_a_settings_field_is_a_slash_and_the_binding_is_rebindable() {
        let (engine, _d) = testutil::engine(&[]);
        let mut cx = TestAppContext::single();
        let (view, cx) = window(&engine, &mut cx);
        view.update(cx, |v, cx| {
            v.switch_tab(Tab::Settings, cx);
            v.state.begin_edit("ui.tz", "");
        });
        keys(cx, "E u r o p e / L i s b o n");
        view.read_with(cx, |v, _| {
            let e = v.state.edit.as_ref().unwrap();
            assert_eq!((e.path, e.buf.as_str()), ("ui.tz", "Europe/Lisbon"));
        });
        assert_eq!(search_dump(&view, cx)["open"], false);
        keys(cx, "escape");
        // Rebound to f: f focuses the bar, / no longer does.
        model::dispatch(
            &engine,
            "config-set",
            json!({"key": "ui.keys.search", "value": "f"}),
        )
        .unwrap();
        keys(cx, "/");
        assert_eq!(search_dump(&view, cx)["open"], false);
        keys(cx, "f");
        assert_eq!(search_dump(&view, cx)["open"], true);
    }
}
