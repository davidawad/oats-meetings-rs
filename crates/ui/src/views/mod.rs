//! One file per main-window tab. Each exposes `render(&MainWindow, cx) -> AnyElement`, dispatched
//! from the table in `view.rs` (Live renders in `view.rs` itself), so a view's work only touches its own file. `shortcuts` is the
//! window's keyboard (shortcut commands, the `?` overlay, the Settings rebind controls).

pub mod calendar;
pub mod models;
pub mod notes;
pub mod sessions;
pub mod settings;
pub mod shortcuts;
pub mod templates;
pub mod trash;
