//! Configuration management
//!
//! Handles profile-based configuration with JSON persistence.
//! Supports multiple profiles, each with visual settings, hotkey bindings,
//! and per-character thumbnail positions.

pub mod daemon_state;
pub mod hotkey_binding;
pub mod profile;

pub use daemon_state::{DisplayConfig, PersistentState};
pub use hotkey_binding::HotkeyBinding;
