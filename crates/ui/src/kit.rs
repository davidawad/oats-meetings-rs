//! The design's shared components (docs/design/drafting-table/design.md, "Components"): buttons
//! by kind, page header, list box and row, tags, toggle, banner, empty state, dropdown trigger,
//! text field, icons. Colours come from `theme::current`, so a view only picks the component.

use std::borrow::Cow;

use gpui::{
    div, prelude::*, px, relative, rgb, svg, AssetSource, Div, FontWeight, SharedString, Stateful,
    Svg,
};

use crate::theme::{self, MONO};

/// A button's place in the hierarchy: one `Primary` per screen; `Rec` starts / stops recording.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Primary,
    Secondary,
    Danger,
    Rec,
}

/// A button's height: `Sm` 26px, `Md` 32px, `Big` 44px (the record strip).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Size {
    Sm,
    Md,
    Big,
}

/// A button. Disabled ones are drawn at 40% and must get no `on_click` (the caller's rule).
pub fn button(
    id: impl Into<SharedString>,
    label: impl Into<SharedString>,
    kind: Kind,
    size: Size,
    enabled: bool,
) -> Stateful<Div> {
    button_with(id, None, label, kind, size, enabled)
}

/// A button with a glyph (a dot, a square) before its label.
pub fn button_with(
    id: impl Into<SharedString>,
    glyph: Option<Div>,
    label: impl Into<SharedString>,
    kind: Kind,
    size: Size,
    enabled: bool,
) -> Stateful<Div> {
    let t = theme::current();
    let (bg, border, fg) = match kind {
        Kind::Primary => (t.accent, t.accent, t.accent_ink),
        Kind::Secondary => (t.surface, t.line2, t.text),
        Kind::Danger => (t.surface, t.err, t.err),
        Kind::Rec => (t.rec, t.rec, 0xFFFFFF),
    };
    let (h, pad, text, radius) = match size {
        Size::Sm => (26., 10., 12., 7.),
        Size::Md => (32., 14., 13., 7.),
        Size::Big => (44., 20., 14., 9.),
    };
    let b = div()
        .id(id.into())
        .h(px(h))
        .px(px(pad))
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .gap(px(7.))
        .rounded(px(radius))
        .border_1()
        .border_color(rgb(border))
        .bg(rgb(bg))
        .text_color(rgb(fg))
        .text_size(px(text))
        .font_weight(FontWeight::MEDIUM)
        .whitespace_nowrap()
        .children(glyph.map(|g| g.flex_none().bg(rgb(fg))))
        .child(label.into());
    if !enabled {
        return b.opacity(0.4);
    }
    let hover = match kind {
        Kind::Secondary | Kind::Danger => Some(t.raised),
        Kind::Primary | Kind::Rec => None,
    };
    b.cursor_pointer()
        .when_some(hover, |b, h| b.hover(move |s| s.bg(rgb(h))))
}

/// The page header row: a 20/600 title, then whatever the caller adds (a spacer is `div().flex_1()`).
pub fn head(title: impl Into<SharedString>) -> Div {
    div()
        .flex()
        .flex_none()
        .items_center()
        .gap(px(12.))
        .pt(px(18.))
        .pb(px(14.))
        .px(px(28.))
        .child(
            div()
                .text_size(px(20.))
                .font_weight(FontWeight::SEMIBOLD)
                .child(title.into()),
        )
}

/// The page body under the header: the 28px gutter and 22px bottom.
pub fn pad() -> Div {
    div()
        .flex()
        .flex_col()
        .flex_1()
        .min_h_0()
        .px(px(28.))
        .pb(px(22.))
}

/// A bordered list / card surface.
pub fn panel() -> Div {
    let t = theme::current();
    div()
        .flex()
        .flex_col()
        .rounded(px(10.))
        .border_1()
        .border_color(rgb(t.line))
        .bg(rgb(t.surface))
        .overflow_hidden()
}

/// A list row: hairline bottom, raised on hover; selected rows get accent-soft and a 3px accent bar.
pub fn row(id: impl Into<SharedString>, selected: bool) -> Stateful<Div> {
    let t = theme::current();
    let r = div()
        .id(id.into())
        .flex()
        .flex_none()
        .items_center()
        .gap(px(14.))
        .px(px(16.))
        .border_b_1()
        .border_color(rgb(t.line))
        .cursor_pointer();
    if selected {
        r.relative().bg(rgb(t.accent_soft)).child(
            div()
                .absolute()
                .left_0()
                .top_0()
                .bottom_0()
                .w(px(3.))
                .bg(rgb(t.accent)),
        )
    } else {
        r.hover(move |s| s.bg(rgb(t.raised)))
    }
}

/// A 12/600 section label ("Today", a settings group).
pub fn section(text: impl Into<SharedString>) -> Div {
    let t = theme::current();
    div()
        .flex_none()
        .text_size(px(12.))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(rgb(t.faint))
        .pt(px(12.))
        .pb(px(6.))
        .px(px(2.))
        .child(text.into())
}

/// A 20px pill; `accent` for a tag that matches the search or marks the default.
pub fn tag(label: impl Into<SharedString>, accent: bool) -> Div {
    let t = theme::current();
    let (bg, fg) = if accent {
        (t.accent_soft, t.accent)
    } else {
        (t.raised, t.dim)
    };
    div()
        .flex()
        .flex_none()
        .items_center()
        .h(px(20.))
        .px(px(8.))
        .rounded(px(10.))
        .bg(rgb(bg))
        .text_color(rgb(fg))
        .text_size(px(11.))
        .font_weight(FontWeight::MEDIUM)
        .whitespace_nowrap()
        .child(label.into())
}

/// The 38x22 toggle pill.
pub fn toggle(id: impl Into<SharedString>, on: bool) -> Stateful<Div> {
    let t = theme::current();
    div()
        .id(id.into())
        .relative()
        .flex_none()
        .w(px(38.))
        .h(px(22.))
        .rounded_full()
        .cursor_pointer()
        .bg(rgb(if on { t.accent } else { t.toggle_off }))
        .child(
            div()
                .absolute()
                .top(px(3.))
                .left(px(if on { 19. } else { 3. }))
                .size(px(16.))
                .rounded_full()
                .bg(rgb(0xFFFFFF)),
        )
}

/// A banner: what happened in bold (`lead`), then what to do; the caller appends the action.
pub fn banner(error: bool, lead: impl Into<SharedString>, rest: impl Into<SharedString>) -> Div {
    let t = theme::current();
    let (bg, bar) = if error {
        (t.rec_soft, t.err)
    } else {
        (t.warn_soft, t.warn)
    };
    div()
        .flex()
        .flex_none()
        .items_center()
        .gap(px(12.))
        .px(px(14.))
        .py(px(10.))
        .rounded(px(9.))
        .border_l(px(4.))
        .border_color(rgb(bar))
        .bg(rgb(bg))
        .text_color(rgb(t.text))
        .text_size(px(13.))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_wrap()
                .gap(px(5.))
                .child(div().font_weight(FontWeight::SEMIBOLD).child(lead.into()))
                .child(div().child(rest.into())),
        )
}

/// The empty state: a centred 18/600 heading and one dim sentence saying what to do next.
pub fn empty(heading: impl Into<SharedString>, sentence: impl Into<SharedString>) -> Div {
    let t = theme::current();
    div().flex().flex_1().items_center().justify_center().child(
        div()
            .flex()
            .flex_col()
            .items_center()
            .gap(px(6.))
            .max_w(px(380.))
            .child(
                div()
                    .text_size(px(18.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(heading.into()),
            )
            .child(
                div()
                    .text_color(rgb(t.dim))
                    .text_center()
                    .whitespace_normal()
                    .child(sentence.into()),
            ),
    )
}

/// A one-line text box (32px); `focus` draws the accent border.
pub fn field(id: impl Into<SharedString>, focus: bool) -> Stateful<Div> {
    let t = theme::current();
    div()
        .id(id.into())
        .h(px(32.))
        .px(px(11.))
        .flex()
        .items_center()
        .gap(px(8.))
        .min_w_0()
        .overflow_hidden()
        .whitespace_nowrap()
        .rounded(px(7.))
        .border_1()
        .border_color(rgb(if focus { t.accent } else { t.line2 }))
        .bg(rgb(t.surface))
        .cursor_text()
}

/// A dropdown trigger: the current value and a chevron.
pub fn dropdown(
    id: impl Into<SharedString>,
    value: impl Into<SharedString>,
    open: bool,
) -> Stateful<Div> {
    let t = theme::current();
    div()
        .id(id.into())
        .h(px(30.))
        .min_w(px(200.))
        .px(px(10.))
        .flex()
        .flex_none()
        .items_center()
        .justify_between()
        .gap(px(12.))
        .rounded(px(7.))
        .border_1()
        .border_color(rgb(if open { t.accent } else { t.line2 }))
        .bg(rgb(t.bg))
        .text_size(px(13.))
        .cursor_pointer()
        .child(div().whitespace_nowrap().child(value.into()))
        .child(icon(Icon::Chevron, t.dim).size(px(12.)))
}

/// The open dropdown menu's box; items are `menu_item`.
pub fn menu() -> Div {
    let t = theme::current();
    div()
        .flex()
        .flex_col()
        .min_w(px(220.))
        .p(px(4.))
        .rounded(px(9.))
        .border_1()
        .border_color(rgb(t.line2))
        .bg(rgb(t.raised))
        .shadow(vec![gpui::BoxShadow {
            color: gpui::hsla(0., 0., 0., 0.35),
            offset: gpui::point(px(0.), px(12.)),
            blur_radius: px(32.),
            spread_radius: px(0.),
            inset: false,
        }])
}

pub fn menu_item(
    id: impl Into<SharedString>,
    label: impl Into<SharedString>,
    current: bool,
) -> Stateful<Div> {
    let t = theme::current();
    div()
        .id(id.into())
        .flex()
        .justify_between()
        .gap(px(12.))
        .px(px(10.))
        .py(px(7.))
        .rounded(px(6.))
        .text_size(px(13.))
        .cursor_pointer()
        .hover(move |s| s.bg(rgb(t.accent_soft)))
        .child(label.into())
        .when(current, |d| d.child("✓"))
}

/// Mono text (clock, timestamps, ids, numbers).
pub fn mono(text: impl Into<SharedString>) -> Div {
    div().font_family(MONO).child(text.into())
}

/// A radio dot: a 4px accent ring when on, a faint outline when off.
pub fn radio(on: bool) -> Div {
    let t = theme::current();
    let r = div().size(px(14.)).flex_none().rounded_full();
    if on {
        r.border(px(4.)).border_color(rgb(t.accent))
    } else {
        r.border(px(1.5)).border_color(rgb(t.faint))
    }
}

/// A 5px level meter track filled to `level` (0..1) in the ok colour.
pub fn meter(level: f32) -> Div {
    let t = theme::current();
    div()
        .h(px(5.))
        .flex_1()
        .rounded_full()
        .bg(rgb(t.line))
        .overflow_hidden()
        .child(
            div()
                .h_full()
                .rounded_full()
                .w(relative(level.clamp(0., 1.)))
                .bg(rgb(t.ok)),
        )
}

/// The stroke icons of the mockup (16px viewBox, 1.6 stroke), served from memory by `Assets`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Icon {
    Mic,
    List,
    Note,
    Template,
    Calendar,
    Chip,
    Gear,
    Trash,
    Search,
    Chevron,
}

impl Icon {
    fn path(self) -> &'static str {
        match self {
            Icon::Mic => "icons/mic.svg",
            Icon::List => "icons/list.svg",
            Icon::Note => "icons/note.svg",
            Icon::Template => "icons/template.svg",
            Icon::Calendar => "icons/calendar.svg",
            Icon::Chip => "icons/chip.svg",
            Icon::Gear => "icons/gear.svg",
            Icon::Trash => "icons/trash.svg",
            Icon::Search => "icons/search.svg",
            Icon::Chevron => "icons/chevron.svg",
        }
    }

    fn body(self) -> &'static str {
        match self {
            Icon::Mic => {
                r#"<rect x="5.5" y="1.5" width="5" height="8" rx="2.5"/><path d="M3 7.5a5 5 0 0010 0M8 12.5v2"/>"#
            }
            Icon::List => r#"<path d="M2 4h12M2 8h12M2 12h8"/>"#,
            Icon::Note => r#"<path d="M3.5 1.5h6l3 3v10h-9z M9.5 1.5v3h3 M5.5 8h5M5.5 11h5"/>"#,
            Icon::Template => {
                r#"<rect x="2" y="2" width="12" height="12" rx="2"/><path d="M2 6h12M6 6v8"/>"#
            }
            Icon::Calendar => {
                r#"<rect x="2" y="3" width="12" height="11" rx="2"/><path d="M2 7h12M5 1.5v3M11 1.5v3"/>"#
            }
            Icon::Chip => {
                r#"<rect x="4" y="4" width="8" height="8" rx="1.5"/><path d="M6.5 1.5v2.5M9.5 1.5v2.5M6.5 12v2.5M9.5 12v2.5M1.5 6.5H4M1.5 9.5H4M12 6.5h2.5M12 9.5h2.5"/>"#
            }
            Icon::Gear => {
                r#"<circle cx="8" cy="8" r="2.2"/><path d="M8 1.5v2M8 12.5v2M1.5 8h2M12.5 8h2M3.4 3.4l1.4 1.4M11.2 11.2l1.4 1.4M3.4 12.6l1.4-1.4M11.2 4.8l1.4-1.4"/>"#
            }
            Icon::Trash => r#"<path d="M2.5 4h11M6 4V2.5h4V4M4 4l.7 9.5h6.6L12 4"/>"#,
            Icon::Search => r#"<circle cx="7" cy="7" r="4.5"/><path d="M10.5 10.5L14 14"/>"#,
            Icon::Chevron => r#"<path d="M4 6l4 4 4-4"/>"#,
        }
    }

    const ALL: [Icon; 10] = [
        Icon::Mic,
        Icon::List,
        Icon::Note,
        Icon::Template,
        Icon::Calendar,
        Icon::Chip,
        Icon::Gear,
        Icon::Trash,
        Icon::Search,
        Icon::Chevron,
    ];

    fn svg(self) -> String {
        // GPUI tints an svg with its text colour by its alpha mask: stroke in black.
        format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16" fill="none" stroke="black" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round">{}</svg>"#,
            self.body()
        )
    }
}

/// A 16px icon in `colour`.
pub fn icon(i: Icon, colour: u32) -> Svg {
    svg()
        .path(i.path())
        .size(px(16.))
        .flex_none()
        .text_color(rgb(colour))
}

/// The app's assets: the icons above, rendered from their path data.
pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> gpui::Result<Option<Cow<'static, [u8]>>> {
        Ok(Icon::ALL
            .into_iter()
            .find(|i| i.path() == path)
            .map(|i| Cow::Owned(i.svg().into_bytes())))
    }

    fn list(&self, path: &str) -> gpui::Result<Vec<SharedString>> {
        Ok(Icon::ALL
            .into_iter()
            .map(Icon::path)
            .filter(|p| p.starts_with(path))
            .map(SharedString::from)
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_icon_is_an_svg_the_asset_source_serves() {
        for i in Icon::ALL {
            let b = Assets.load(i.path()).unwrap().expect("served");
            let s = std::str::from_utf8(&b).unwrap();
            assert!(s.starts_with("<svg") && s.ends_with("</svg>"), "{i:?}");
        }
        assert!(Assets.load("icons/nope.svg").unwrap().is_none());
        assert_eq!(Assets.list("icons/").unwrap().len(), Icon::ALL.len());
    }
}
