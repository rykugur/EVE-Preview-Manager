//! X11 window state queries and operations

use anyhow::{Context, Result};
use tracing::debug;
use x11rb::connection::Connection;
use x11rb::errors::ReplyError;
use x11rb::protocol::xproto::*;
use x11rb::rust_connection::RustConnection;

use crate::constants::{eve, x11};
use crate::types::EveWindowType;

use super::CachedAtoms;

/// Check if a window is an EVE Online client
pub fn is_window_eve(conn: &RustConnection, window: Window, atoms: &CachedAtoms) -> Result<Option<EveWindowType>> {
    let cookie = conn
        .get_property(false, window, atoms.wm_name, AtomEnum::STRING, 0, 1024)
        .context(format!("Failed to query WM_NAME property for window {}", window))?;
    let name_prop = match cookie.reply() {
        Ok(reply) => reply,
        Err(ReplyError::X11Error(err))
            if err.error_kind == x11rb::protocol::ErrorKind::Window =>
        {
            debug!(window = window, "Window destroyed before WM_NAME reply, skipping");
            return Ok(None);
        }
        Err(err) => {
            return Err(err).context(format!("Failed to get WM_NAME reply for window {}", window));
        }
    };
    let title = String::from_utf8_lossy(&name_prop.value).into_owned();
    Ok(if let Some(name) = title.strip_prefix(eve::WINDOW_TITLE_PREFIX) {
        Some(EveWindowType::LoggedIn(name.to_string()))
    } else if title == eve::LOGGED_OUT_TITLE {
        Some(EveWindowType::LoggedOut)
    } else {
        None
    })
}

/// Check whether the given EVE client window is currently minimized/iconified
pub fn is_window_minimized(
    conn: &RustConnection,
    window: Window,
    atoms: &CachedAtoms,
) -> Result<bool> {
    let net_state_cookie = conn
        .get_property(false, window, atoms.net_wm_state, AtomEnum::ATOM, 0, 1024)
        .context(format!("Failed to query _NET_WM_STATE for window {}", window))?;
    match net_state_cookie.reply() {
        Ok(reply) => {
            if let Some(mut values) = reply.value32()
                && values.any(|state| state == atoms.net_wm_state_hidden) {
                    return Ok(true);
                }
        }
        Err(ReplyError::X11Error(err))
            if err.error_kind == x11rb::protocol::ErrorKind::Window =>
        {
            debug!(window = window, "Window destroyed before _NET_WM_STATE reply");
            return Ok(false);
        }
        Err(err) => {
            return Err(err)
                .context(format!("Failed to get _NET_WM_STATE reply for window {}", window));
        }
    }

    // Fallback to ICCCM WM_STATE / IconicState detection
    let wm_state_cookie = conn
        .get_property(
            false,
            window,
            atoms.wm_state,
            atoms.wm_state,
            0,
            2,
        )
        .context(format!("Failed to query WM_STATE for window {}", window))?;
    match wm_state_cookie.reply() {
        Ok(reply) => {
            if let Some(mut values) = reply.value32()
                && let Some(state) = values.next()
                    && state == x11::ICONIC_STATE {
                        return Ok(true);
                    }
        }
        Err(ReplyError::X11Error(err))
            if err.error_kind == x11rb::protocol::ErrorKind::Window =>
        {
            debug!(window = window, "Window destroyed before WM_STATE reply");
            return Ok(false);
        }
        Err(err) => {
            return Err(err).context(format!("Failed to get WM_STATE reply for window {}", window));
        }
    }

    Ok(false)
}

/// Check if the currently focused window is an EVE client
pub fn is_eve_window_focused(conn: &RustConnection, screen: &Screen, atoms: &CachedAtoms) -> Result<bool> {
    let active_window_prop = conn
        .get_property(
            false,
            screen.root,
            atoms.net_active_window,
            AtomEnum::WINDOW,
            0,
            1,
        )
        .context("Failed to query _NET_ACTIVE_WINDOW property")?
        .reply()
        .context("Failed to get reply for _NET_ACTIVE_WINDOW query")?;

    if active_window_prop.value.len() >= 4 {
        let active_window = u32::from_ne_bytes(active_window_prop.value[0..4].try_into()
            .context("Invalid _NET_ACTIVE_WINDOW property format")?);
        Ok(is_window_eve(conn, active_window, atoms)
            .context(format!("Failed to check if active window {} is EVE client", active_window))?.is_some())
    } else {
        Ok(false)
    }
}

/// Activate (focus) an X11 window using _NET_ACTIVE_WINDOW
pub fn activate_window(
    conn: &RustConnection,
    screen: &Screen,
    atoms: &CachedAtoms,
    window: Window,
) -> Result<()> {
    conn.configure_window(
        window,
        &ConfigureWindowAux::new().stack_mode(StackMode::ABOVE),
    )
    .context(format!("Failed to raise window {} to top of stack", window))?;

    let event = ClientMessageEvent {
        response_type: CLIENT_MESSAGE_EVENT,
        format: 32,
        sequence: 0,
        window,
        type_: atoms.net_active_window,
        data: ClientMessageData::from([
            x11::ACTIVE_WINDOW_SOURCE_PAGER,
            x11rb::CURRENT_TIME,
            0,
            0,
            0,
        ]),
    };

    conn.send_event(
        false,
        screen.root,
        EventMask::SUBSTRUCTURE_NOTIFY | EventMask::SUBSTRUCTURE_REDIRECT,
        event,
    )
    .context(format!("Failed to send _NET_ACTIVE_WINDOW event for window {}", window))?;

    conn.flush()
        .context("Failed to flush X11 connection after window activation")?;
    Ok(())
}

/// Minimize (hide) an X11 window using _NET_WM_STATE
pub fn minimize_window(
    conn: &RustConnection,
    screen: &Screen,
    atoms: &CachedAtoms,
    window: Window,
) -> Result<()> {
    let event = ClientMessageEvent {
        response_type: CLIENT_MESSAGE_EVENT,
        format: 32,
        sequence: 0,
        window,
        type_: atoms.net_wm_state,
        data: ClientMessageData::from([
            x11::NET_WM_STATE_ADD,
            atoms.net_wm_state_hidden,
            0,
            x11::ACTIVE_WINDOW_SOURCE_PAGER,
            0,
        ]),
    };

    conn.send_event(
        false,
        screen.root,
        EventMask::SUBSTRUCTURE_NOTIFY | EventMask::SUBSTRUCTURE_REDIRECT,
        event,
    )
    .context(format!("Failed to send _NET_WM_STATE minimize event for window {}", window))?;

    // Fallback for WMs that expect ICCCM-style iconify requests
    let change_state_event = ClientMessageEvent {
        response_type: CLIENT_MESSAGE_EVENT,
        format: 32,
        sequence: 0,
        window,
        type_: atoms.wm_change_state,
        data: ClientMessageData::from([
            x11::ICONIC_STATE,
            0,
            0,
            0,
            0,
        ]),
    };

    conn.send_event(
        false,
        screen.root,
        EventMask::SUBSTRUCTURE_NOTIFY | EventMask::SUBSTRUCTURE_REDIRECT,
        change_state_event,
    )
    .context(format!("Failed to send WM_CHANGE_STATE iconify event for window {}", window))?;

    conn.flush()
        .context("Failed to flush X11 connection after window minimize")?;
    Ok(())
}
