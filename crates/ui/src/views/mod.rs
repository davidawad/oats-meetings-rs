//! One file per main-window tab. Each exposes `render(&MainWindow, cx) -> AnyElement`, dispatched
//! from the table in `view.rs`, so a view's work only touches its own file.

pub mod calendar;
pub mod models;
pub mod notes;
pub mod sessions;
pub mod templates;
pub mod trash;

use gpui::{div, prelude::*, rgb, AnyElement};

use crate::view::DIM;

/// Body for a tab whose view has not been built yet.
pub(crate) fn placeholder(id: &'static str, what: &str) -> AnyElement {
    div().id(id).text_color(rgb(DIM)).child(format!("{what}: not available yet.")).into_any_element()
}
