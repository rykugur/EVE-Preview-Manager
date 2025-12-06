//! Preview daemon - runs in background showing EVE window thumbnails

mod cycle_state;
mod daemon;
mod event_handler;
pub mod font;
mod font_discovery;
mod session_state;
mod snapping;
mod thumbnail;
mod window_detection;

pub use daemon::run_preview_daemon;
pub use font_discovery::{find_font_path, list_fonts, select_best_default_font};
