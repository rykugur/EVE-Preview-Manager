//! EVE window detection and thumbnail creation logic

use anyhow::{Context, Result};
use tracing::{debug, error, info, warn};
use x11rb::protocol::xproto::*;

use crate::config::DaemonConfig;
use crate::constants::{self, paths, wine};
use crate::types::Dimensions;
use crate::x11::{is_window_eve, is_window_minimized, AppContext};

use super::session_state::SessionState;
use super::thumbnail::Thumbnail;

pub fn check_and_create_window<'a>(
    ctx: &AppContext<'a>,
    daemon_config: &DaemonConfig,
    window: Window,
    state: &mut SessionState,
) -> Result<Option<Thumbnail<'a>>> {
    let pid_atom = ctx.conn.intern_atom(false, b"_NET_WM_PID")
        .context("Failed to intern _NET_WM_PID atom")?
        .reply()
        .context("Failed to get reply for _NET_WM_PID atom")?
        .atom;
    if let Ok(prop) = ctx.conn
        .get_property(false, window, pid_atom, AtomEnum::CARDINAL, 0, 1)
        .context(format!("Failed to query _NET_WM_PID property for window {}", window))?
        .reply()
    {
        if !prop.value.is_empty() {
            let pid = u32::from_ne_bytes(prop.value[0..constants::x11::PID_PROPERTY_SIZE].try_into()
                .context("Invalid PID property format (expected 4 bytes)")?);
            
            // Skip our own thumbnail windows
            if pid == std::process::id() {
                return Ok(None);
            }
            
            if !std::fs::read_link(paths::PROC_EXE_FORMAT.replace("{}", &pid.to_string()))
                .map(|x| {
                    x.to_string_lossy().contains(wine::WINE64_PRELOADER)
                        || x.to_string_lossy().contains(wine::WINE_PRELOADER)
                })
                .inspect_err(|e| {
                    error!(
                        pid = pid,
                        error = ?e,
                        "Cannot read /proc/{pid}/exe, assuming wine process"
                    );
                })
                .unwrap_or(true)
            {
                return Ok(None); // Return if we can determine that the window is not running through wine.
            }
        } else {
            warn!(
                window = window,
                "_NET_WM_PID not set, assuming wine process"
            );
        }
    }

    ctx.conn.change_window_attributes(
        window,
        &ChangeWindowAttributesAux::new().event_mask(EventMask::PROPERTY_CHANGE),
    )
    .context(format!("Failed to set event mask for window {}", window))?;

    if let Some(eve_window) = is_window_eve(ctx.conn, window, ctx.atoms)
        .context(format!("Failed to check if window {} is EVE client", window))? {
        let character_name = eve_window.character_name().to_string();

        // Track last known character for this window (for logged-out cycling feature)
        state.update_last_character(window, &character_name);

        ctx.conn.change_window_attributes(
            window,
            &ChangeWindowAttributesAux::new()
                .event_mask(EventMask::PROPERTY_CHANGE | EventMask::FOCUS_CHANGE),
        )
        .context(format!("Failed to set focus event mask for EVE window {} ('{}')", window, character_name))?;

        // Skip thumbnail creation if thumbnails are disabled (daemon still runs for hotkeys)
        if !ctx.config.enabled {
            debug!(
                window = window,
                character = %character_name,
                "Skipping thumbnail creation (thumbnails disabled in config)"
            );
            return Ok(None);
        }

        // Get saved position and dimensions for this character/window
        let position = state.get_position(
            &character_name,
            window,
            &daemon_config.character_thumbnails,
            daemon_config.profile.thumbnail_preserve_position_on_swap,
        );

        // Get dimensions from CharacterSettings or use auto-detected defaults
        let dimensions = if let Some(settings) = daemon_config.character_thumbnails.get(&character_name) {
            // If dimensions are 0 (not yet saved), auto-detect
            if settings.dimensions.width == 0 || settings.dimensions.height == 0 {
                let (w, h) = daemon_config.default_thumbnail_size(
                    ctx.screen.width_in_pixels,
                    ctx.screen.height_in_pixels,
                );
                Dimensions::new(w, h)
            } else {
                settings.dimensions
            }
        } else {
            // Character not in settings yet - auto-detect
            let (w, h) = daemon_config.default_thumbnail_size(
                ctx.screen.width_in_pixels,
                ctx.screen.height_in_pixels,
            );
            Dimensions::new(w, h)
        };

        let mut thumbnail = Thumbnail::new(ctx, character_name.clone(), window, ctx.font_renderer, position, dimensions)
            .context(format!("Failed to create thumbnail for '{}' (window {})", character_name, window))?;
        if is_window_minimized(ctx.conn, window, ctx.atoms)
            .context(format!("Failed to query minimized state for window {}", window))?
        {
            debug!(window = window, character = %character_name, "Window minimized at startup");
            thumbnail
                .minimized()
                .context(format!("Failed to set minimized state for '{}'", character_name))?;
        }
        info!(
            window = window,
            character = %character_name,
            "Created thumbnail for EVE window"
        );
        Ok(Some(thumbnail))
    } else {
        Ok(None)
    }
}
