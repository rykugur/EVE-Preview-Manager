//! GUI module - egui-based management interface with system tray control

pub mod components;
mod key_capture;
mod app;
pub mod state;
pub mod utils;
pub mod x11_utils;

pub use app::run_gui;
