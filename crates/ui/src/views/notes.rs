//! Notes tab.

use gpui::{AnyElement, Context};

use crate::view::MainWindow;

pub fn render(_w: &MainWindow, _cx: &mut Context<MainWindow>) -> AnyElement {
    super::placeholder("notes", "Notes")
}
