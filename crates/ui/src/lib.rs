//! GPUI front end (viewer) over the control engine. See `host` for how GUI mode hosts the engine.

pub mod capture;
pub mod host;
pub mod model;
pub mod settings;
pub mod state;
pub mod tray;
pub mod view;

#[cfg(test)]
mod testutil;

pub use host::{run, run_env};
