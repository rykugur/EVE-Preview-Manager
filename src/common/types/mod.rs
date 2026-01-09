//! Domain types for type safety and clarity
//!
//! Refactored into sub-modules for better organization.

pub mod domain;
pub mod geometry;

// Re-export specific types to maintain compatibility
pub use domain::{CharacterSettings, EveWindowType, PreviewMode, ThumbnailState};
pub use geometry::{Dimensions, Position, TextOffset};
