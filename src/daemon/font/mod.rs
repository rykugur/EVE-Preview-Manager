//! Font rendering and discovery
//!
//! Refactored into sub-modules for better organization.

pub mod discovery;
pub mod rendering;

// Re-export common types
pub use discovery::{list_fonts, select_best_default_font};
pub use rendering::FontRenderer;
