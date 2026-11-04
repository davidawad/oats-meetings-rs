//! GPUI front end (viewer) over the control engine. See `host` for how GUI mode hosts the engine.

pub mod capture;
pub mod drive;
pub mod host;
pub mod kit;
pub mod model;
pub mod settings;
pub mod state;
pub mod theme;
pub mod tray;
pub mod view;
pub mod views;

#[cfg(test)]
mod testutil;

pub use host::{run, run_env};
