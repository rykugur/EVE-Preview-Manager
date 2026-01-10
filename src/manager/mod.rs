//! GUI module - egui-based management interface with system tray control

mod app;
pub mod components;
mod key_capture;
pub mod state;
pub mod utils;
pub mod x11_utils;

pub use app::run_gui;
