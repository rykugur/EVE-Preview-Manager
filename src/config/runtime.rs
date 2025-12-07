//! Runtime configuration for the preview daemon
//!
//! Loads the selected profile and global settings at startup,
//! then maintains character positions synchronized with the config file.

use anyhow::{Context, Result};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use tracing::{error, info};
use x11rb::protocol::render::Color;

use crate::color::{HexColor, Opacity};
use crate::config::profile::SaveStrategy;
use crate::types::{CharacterSettings, Position, TextOffset};


/// Shared display configuration for all thumbnails
/// Immutable after creation - can be borrowed without RefCell
#[derive(Debug, Clone)]
pub struct DisplayConfig {
    pub opacity: u32,
    pub border_size: u16,
    pub border_color: Color,
    pub text_offset: TextOffset,
    pub text_color: u32,
    pub hide_when_no_focus: bool,
}

/// Daemon runtime configuration - holds selected profile + global settings
/// Built from the JSON config at runtime, not serialized directly
#[derive(Debug)]
pub struct DaemonConfig {
    pub profile: crate::config::profile::Profile,
    pub global: crate::config::profile::GlobalSettings,
    pub character_positions: HashMap<String, CharacterSettings>,
}

impl DaemonConfig {
    fn config_path() -> PathBuf {
        let mut path = dirs::config_dir().unwrap_or_else(|| PathBuf::from("."));
        path.push(crate::constants::config::APP_DIR);
        path.push(crate::constants::config::FILENAME);
        path
    }

    /// Get default thumbnail dimensions from global settings
    pub fn default_thumbnail_size(&self, _screen_width: u16, _screen_height: u16) -> (u16, u16) {
        (self.global.default_thumbnail_width, self.global.default_thumbnail_height)
    }

    /// Build DisplayConfig from current settings
    pub fn build_display_config(&self) -> DisplayConfig {
        let border_color = HexColor::parse(&self.profile.border_color)
            .map(|c| c.to_x11_color())
            .unwrap_or_else(|| {
                error!(border_color = %self.profile.border_color, "Invalid border_color hex, using default");
                HexColor::from_argb32(0xFFFF0000).to_x11_color()
            });

        let text_color = HexColor::parse(&self.profile.text_color)
            .map(|c| c.argb32())
            .unwrap_or_else(|| {
                error!(text_color = %self.profile.text_color, "Invalid text_color hex, using default");
                HexColor::from_argb32(0xFF_FF_FF_FF).argb32()
            });

        let opacity = Opacity::from_percent(self.profile.opacity_percent).to_argb32();

        DisplayConfig {
            opacity,
            border_size: self.profile.border_size,
            border_color,
            text_offset: TextOffset::from_border_edge(self.profile.text_x, self.profile.text_y),
            text_color,
            hide_when_no_focus: self.global.hide_when_no_focus,
        }
    }

    pub fn load() -> Self {
        let config_path = Self::config_path();
        if let Ok(contents) = fs::read_to_string(&config_path) {
            match serde_json::from_str::<crate::config::profile::Config>(&contents) {
                Ok(profile_config) => {
                    info!("Loading daemon config from profile-based format");
                    return Self::from_profile_config(profile_config);
                }
                Err(e) => {
                    error!(path = %config_path.display(), error = %e, "Failed to parse config file");
                    error!(path = %config_path.display(), "Please fix the syntax errors in your config file.");
                    std::process::exit(1);
                }
            }
        }

        error!(path = %config_path.display(), "No config file found. Please run the GUI manager first to create a profile.");
        error!("Run: eve-preview-manager");
        std::process::exit(1);
    }

    fn from_profile_config(config: crate::config::profile::Config) -> Self {
        let profile = config.profiles
            .iter()
            .find(|p| p.name == config.global.selected_profile)
            .or_else(|| config.profiles.first())
            .expect("Config must have at least one profile")
            .clone();

        info!(profile = %profile.name, "Using profile for daemon settings");

        DaemonConfig {
            profile: profile.clone(),
            global: config.global.clone(),
            character_positions: profile.character_positions.clone(),
        }
    }

    /// Load config with screen size available (for future smart defaults)
    pub fn load_with_screen(_screen_width: u16, _screen_height: u16) -> Self {
        Self::load()
    }

    /// Save character positions to the profile config
    pub fn save(&self) -> Result<()> {
        let config_path = Self::config_path();
        let mut profile_config = if let Ok(contents) = fs::read_to_string(&config_path) {
            serde_json::from_str::<crate::config::profile::Config>(&contents)
                .context("Failed to parse profile config for save")?
        } else {
            crate::config::profile::Config::default()
        };

        let selected_name = profile_config.global.selected_profile.clone();
        let profile_idx = profile_config.profiles
            .iter()
            .position(|p| p.name == selected_name)
            .unwrap_or(0);

        let profile_positions = &mut profile_config.profiles[profile_idx].character_positions;
        for (char_name, char_settings) in &self.character_positions {
            profile_positions.insert(char_name.clone(), *char_settings);
        }

        profile_config.save_with_strategy(SaveStrategy::OverwriteCharacterPositions)
    }

    /// Handle character name change (login/logout)
    /// Returns new position if the new character has a saved position
    pub fn handle_character_change(
        &mut self,
        old_name: &str,
        new_name: &str,
        current_position: Position,
        current_width: u16,
        current_height: u16,
    ) -> Result<Option<Position>> {
        info!(old = %old_name, new = %new_name, "Character change");

        if !old_name.is_empty() && self.profile.auto_save_thumbnail_positions {
            let settings = CharacterSettings::new(
                current_position.x,
                current_position.y,
                current_width,
                current_height
            );
            self.character_positions.insert(old_name.to_string(), settings);

            self.save()
                .context(format!("Failed to save config after character change from '{}' to '{}'", old_name, new_name))?;
        } else if !old_name.is_empty() {
            let settings = CharacterSettings::new(
                current_position.x,
                current_position.y,
                current_width,
                current_height
            );
            self.character_positions.insert(old_name.to_string(), settings);
        }

        if !new_name.is_empty()
            && let Some(settings) = self.character_positions.get(new_name) {
                info!(character = %new_name, x = settings.x, y = settings.y, "Moving to saved position for character");
                return Ok(Some(settings.position()));
            }

        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config(
        opacity_percent: u8,
        border_size: u16,
        border_color: &str,
        text_x: i16,
        text_y: i16,
        text_color: &str,
        hide_when_no_focus: bool,
        snap_threshold: u16,
    ) -> DaemonConfig {
        use crate::config::profile::{GlobalSettings, Profile};

        DaemonConfig {
            profile: Profile {
                name: "Test Profile".to_string(),
                description: String::new(),
                opacity_percent,
                border_enabled: true,
                border_size,
                border_color: border_color.to_string(),
                text_size: 18,
                text_x,
                text_y,
                text_color: text_color.to_string(),
                text_font_family: String::new(),
                cycle_forward_keys: None,
                cycle_backward_keys: None,
                selected_hotkey_device: None,
                cycle_group: vec![],
                include_logged_out_in_cycle: false,
                hotkey_require_eve_focus: true,
                auto_save_thumbnail_positions: true,
                character_positions: HashMap::new(),
            },
            global: GlobalSettings {
                selected_profile: "Test Profile".to_string(),
                window_width: 1020,
                window_height: 745,
                window_x: None,
                window_y: None,
                minimize_clients_on_switch: false,
                hotkey_require_eve_focus: true,
                hide_when_no_focus,
                snap_threshold,
                preserve_thumbnail_position_on_swap: false,
                default_thumbnail_width: 480,
                default_thumbnail_height: 270,
            },
            character_positions: HashMap::new(),
        }
    }

    #[test]
    fn test_build_display_config_valid_colors() {
        let state = test_config(
            75, 3, "#FF00FF00", 15, 25, "#FFFFFFFF", true, 20,
        );

        let config = state.build_display_config();
        assert_eq!(config.border_size, 3);
        assert_eq!(config.text_offset.x, 15);
        assert_eq!(config.text_offset.y, 25);
        assert_eq!(config.hide_when_no_focus, true);
        assert_eq!(config.opacity, 0xBF000000);
        assert_eq!(config.border_color.red, 0);
        assert_eq!(config.border_color.green, 65535);
        assert_eq!(config.border_color.blue, 0);
        assert_eq!(config.border_color.alpha, 65535);
    }

    #[test]
    fn test_build_display_config_invalid_colors_fallback() {
        let state = test_config(
            100, 5, "invalid", 10, 20, "also_invalid", false, 15,
        );

        let config = state.build_display_config();
        assert_eq!(config.opacity, 0xFF000000);
        assert_eq!(config.border_color.red, 65535);
        assert_eq!(config.border_color.blue, 0);
        assert_eq!(config.border_color.alpha, 65535);
    }

    #[test]
    fn test_handle_character_change_both_names() {
        let mut state = test_config(
            75, 3, "#FF00FF00", 10, 20, "#FFFFFFFF", false, 15,
        );

        state.character_positions.insert("NewChar".to_string(), CharacterSettings::new(500, 600, 240, 135));

        let current_pos = Position::new(100, 200);
        let result = state.handle_character_change("OldChar", "NewChar", current_pos, 480, 270);

        let old_settings = state.character_positions.get("OldChar").unwrap();
        assert_eq!(old_settings.x, 100);
        assert_eq!(old_settings.y, 200);
        assert_eq!(old_settings.dimensions.width, 480);
        assert_eq!(old_settings.dimensions.height, 270);

        if let Ok(Some(new_pos)) = result {
            assert_eq!(new_pos.x, 500);
            assert_eq!(new_pos.y, 600);
        }

        let new_settings = state.character_positions.get("NewChar").unwrap();
        assert_eq!(new_settings.x, 500);
        assert_eq!(new_settings.y, 600);
    }

    #[test]
    fn test_handle_character_change_logout() {
        let mut state = test_config(
            75, 3, "#FF00FF00", 10, 20, "#FFFFFFFF", false, 15,
        );

        let current_pos = Position::new(300, 400);
        let result = state.handle_character_change("LoggingOut", "", current_pos, 480, 270);

        let settings = state.character_positions.get("LoggingOut").unwrap();
        assert_eq!(settings.x, 300);
        assert_eq!(settings.y, 400);
        assert_eq!(settings.dimensions.width, 480);
        assert_eq!(settings.dimensions.height, 270);

        if let Ok(new_pos) = result {
            assert_eq!(new_pos, None);
        }
    }

    #[test]
    fn test_handle_character_change_new_character_no_saved_position() {
        let mut state = test_config(
            75, 3, "#FF00FF00", 10, 20, "#FFFFFFFF", false, 15,
        );

        let current_pos = Position::new(700, 800);
        let result = state.handle_character_change("", "BrandNewChar", current_pos, 480, 270);

        assert!(state.character_positions.is_empty());

        if let Ok(new_pos) = result {
            assert_eq!(new_pos, None);
        }
    }
}
