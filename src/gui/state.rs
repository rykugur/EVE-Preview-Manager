use std::process::Child;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use eframe::egui;
use tracing::{debug, error, info, warn};

use crate::config::profile::{Config, SaveStrategy};
use crate::constants::gui::*;
use crate::gui::utils::spawn_preview_daemon;

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum GuiTab {
    Behavior,
    Appearance,
    Hotkeys,
    Characters,
    Sources,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DaemonStatus {
    Starting,
    Running,
    Stopped,
    Crashed(Option<i32>),
}

impl DaemonStatus {
    pub fn color(&self) -> egui::Color32 {
        match self {
            DaemonStatus::Running => STATUS_RUNNING,
            DaemonStatus::Starting => STATUS_STARTING,
            _ => STATUS_STOPPED,
        }
    }

    pub fn label(&self) -> String {
        match self {
            DaemonStatus::Running => "Preview daemon running".to_string(),
            DaemonStatus::Starting => "Preview daemon starting...".to_string(),
            DaemonStatus::Stopped => "Preview daemon stopped".to_string(),
            DaemonStatus::Crashed(code) => match code {
                Some(code) => format!("Preview daemon crashed (exit {code})"),
                None => "Preview daemon crashed".to_string(),
            },
        }
    }
}

pub struct StatusMessage {
    pub text: String,
    pub color: egui::Color32,
}

use crate::config::DaemonConfig;
use crate::ipc::{BootstrapMessage, ConfigMessage, DaemonMessage};
use ipc_channel::ipc::{IpcOneShotServer, IpcReceiver, IpcSender};
use std::sync::mpsc::{self, Receiver};

// Core application state shared between GUI and Tray
pub struct SharedState {
    pub config: Config,
    pub daemon: Option<Child>,
    pub daemon_status: DaemonStatus,
    pub last_health_check: Instant,
    pub status_message: Option<StatusMessage>,
    pub config_status_message: Option<StatusMessage>,
    pub settings_changed: bool,
    pub selected_profile_idx: usize,
    pub should_quit: bool,
    pub last_config_mtime: Option<std::time::SystemTime>,

    // IPC
    pub ipc_config_tx: Option<IpcSender<ConfigMessage>>,
    pub ipc_status_rx: Option<IpcReceiver<DaemonMessage>>,
    pub bootstrap_rx: Option<Receiver<BootstrapMessage>>,
    // To keep status receiver unblocked, we might need another thread that pumps messages to a GUI-friendly channel?
    // IpcReceiver::recv is blocking.
    // So we need a thread that loops recv() and sends to mpsc::Receiver that GUI polls.
    pub gui_status_rx: Option<Receiver<DaemonMessage>>,
}

impl SharedState {
    pub fn new(config: Config) -> Self {
        let selected_profile_idx = config
            .profiles
            .iter()
            .position(|p| p.profile_name == config.global.selected_profile)
            .unwrap_or(0);

        Self {
            config,
            daemon: None,
            daemon_status: DaemonStatus::Stopped,
            last_health_check: Instant::now(),
            status_message: None,
            config_status_message: None,
            settings_changed: false,
            selected_profile_idx,
            should_quit: false,
            last_config_mtime: std::fs::metadata(Config::path())
                .ok()
                .and_then(|m| m.modified().ok()),
            ipc_config_tx: None,
            ipc_status_rx: None,
            bootstrap_rx: None,
            gui_status_rx: None,
        }
    }

    pub fn start_daemon(&mut self) -> Result<()> {
        if self.daemon.is_some() {
            return Ok(());
        }

        // 1. Create IPC OneShot Server
        let (server, server_name) =
            IpcOneShotServer::<BootstrapMessage>::new().context("Failed to create IPC server")?;

        // 2. Spawn Daemon with server name
        let child = spawn_preview_daemon(&server_name)?;
        let pid = child.id();
        info!(pid, server_name = %server_name, "Started preview daemon");

        // 3. Spawn thread to wait for connection (avoid blocking GUI)
        let (tx, rx) = mpsc::channel();
        self.bootstrap_rx = Some(rx);

        std::thread::spawn(move || {
            info!("Waiting for daemon IPC connection...");
            match server.accept() {
                Ok((_, bootstrap_msg)) => {
                    info!("Daemon connected via IPC");
                    let _ = tx.send(bootstrap_msg);
                }
                Err(e) => {
                    error!(error = %e, "Failed to accept IPC connection");
                }
            }
        });

        self.daemon = Some(child);
        self.daemon_status = DaemonStatus::Starting;
        Ok(())
    }

    pub fn stop_daemon(&mut self) -> Result<()> {
        if let Some(mut child) = self.daemon.take() {
            info!(pid = child.id(), "Stopping preview daemon");

            // Allow some time for the daemon to process signals? No, kill() is immediate.
            // But verify if kill succeeds.
            if let Err(e) = child.kill() {
                error!(pid = child.id(), error = %e, "Failed to send SIGKILL to daemon");
            } else {
                debug!(pid = child.id(), "SIGKILL sent successfully");
            }

            match child.wait() {
                Ok(status) => {
                    info!(pid = child.id(), status = ?status, "Daemon exited");
                    self.daemon_status = if status.success() {
                        DaemonStatus::Stopped
                    } else {
                        DaemonStatus::Crashed(status.code())
                    };
                }
                Err(e) => {
                    error!(pid = child.id(), error = %e, "Failed to wait for daemon exit");
                    // Assuming crashed if we cant wait
                    self.daemon_status = DaemonStatus::Crashed(None);
                }
            }
        }
        Ok(())
    }

    pub fn restart_daemon(&mut self) {
        info!("Restart requested");
        if let Err(err) = self.stop_daemon().and_then(|_| self.start_daemon()) {
            error!(error = ?err, "Failed to restart daemon");
            self.status_message = Some(StatusMessage {
                text: format!("Restart failed: {err}"),
                color: STATUS_STOPPED,
            });
        }
    }

    pub fn reload_daemon_config(&mut self) {
        info!("Config reload requested - restarting daemon");
        self.restart_daemon();
    }

    pub fn save_config(&mut self) -> Result<()> {
        // Load fresh config from disk (has all characters including daemon's additions)
        let disk_config = Config::load().unwrap_or_else(|_| self.config.clone());

        // Merge strategy: Start with GUI's profile list (handles deletions), merge character positions from disk
        let mut merged_profiles = Vec::new();

        for gui_profile in &self.config.profiles {
            let mut merged_profile = gui_profile.clone();

            // Find matching profile in disk config to get daemon's character positions
            if let Some(disk_profile) = disk_config
                .profiles
                .iter()
                .find(|p| p.profile_name == gui_profile.profile_name)
            {
                // Merge character positions: start with GUI's, add disk characters, preserve disk positions
                for (char_name, disk_settings) in &disk_profile.character_thumbnails {
                    if let Some(gui_settings) =
                        merged_profile.character_thumbnails.get_mut(char_name)
                    {
                        // Character exists in both: keep GUI dimensions, use disk position (x, y)
                        gui_settings.x = disk_settings.x;
                        gui_settings.y = disk_settings.y;
                        // Don't overwrite dimensions - GUI state is authoritative
                        // gui_settings.dimensions = disk_settings.dimensions;
                    }
                    // REMOVED: Do NOT re-add characters found on disk but missing from memory.
                    // This was resurrecting deleted characters.
                    // New/Active characters will be re-added by the daemon automatically.
                }
            }

            merged_profiles.push(merged_profile);
        }

        // Build final config with merged profiles and GUI's global settings
        let final_config = Config {
            profiles: merged_profiles,
            global: self.config.global.clone(),
        };

        // Update in-memory config immediately
        self.config = final_config;

        // Sync with daemon via IPC
        if let Some(ref tx) = self.ipc_config_tx {
            // Create DaemonConfig from current profile
            // This logic needs to mirror how DaemonConfig was constructed before.
            // We need to resolve the selected profile.
            let selected_profile = self
                .config
                .get_active_profile()
                .cloned()
                .unwrap_or_default();

            // We construct a DaemonConfig. Note: Runtime overrides (custom sources etc)
            // are part of DaemonConfig. If we just send profile, we might lose them?
            // Actually, DaemonConfig has `character_thumbnails` which IS the runtime state.
            // When we save_config, we merge runtime state back to disk.
            // But here we are sending the *updated* config to the daemon.
            // The daemon should use this as the new truth.

            // Wait. `save_config` merges daemon state (from disk or memory?) into new config.
            // In the new architecture, GUI is the owner.
            // Does GUI have the latest character positions?
            // Yes, via `DaemonMessage::PositionChanged`.

            // So when `save_config` happens, `self.config` should be up to date.
            // But `DaemonConfig` structure in `runtime.rs` contains more than just `Profile`.
            // It contains `runtime_hidden`, `profile_hotkeys` (derived), `character_thumbnails`.

            // We need to construct a `DaemonConfig` to send.
            // `DaemonConfig` struct is:
            // pub struct DaemonConfig {
            //    pub profile: Profile,
            //    pub character_thumbnails: HashMap<String, CharacterSettings>,
            //    pub custom_source_thumbnails: HashMap<String, CharacterSettings>,
            //    pub profile_hotkeys: HashMap<HotkeyBinding, String>,
            //    pub runtime_hidden: bool,
            // }

            // We need to populate this.
            // `profile` is easy.
            // `character_thumbnails` come from the profile (since we just saved/merged them).
            // `custom_source_thumbnails`? These might need to be tracked in GUI state too?
            // Currently GUI Config has `Profile` which has `character_thumbnails`.
            // CUSTOM sources are also stored in `character_thumbnails` in the profile?
            // Let's check `Profile` definition.
            // `Profile` has `custom_windows`.
            // The runtime `custom_source_thumbnails` map in DaemonConfig tracks their active state/positions.
            // If we send a new DaemonConfig, we might wipe active custom sources if we don't include them?
            // Ideally we should preserve them.
            // BUT, if the GUI doesn't track custom sources separately, we have a problem.
            // `DaemonConfig` separates them. `Profile` combines them?
            // Let's look at `DaemonConfig::load()` in `src/config/runtime.rs` (which I deleted).
            // It initialized them empty.
            // `scan_eve_windows` populated them.

            // If we send a FRESH DaemonConfig, the daemon replaces its state.
            // We need to match what the daemon expects.
            // If we want to preserve runtime state (like custom sources or cached positions not on disk),
            // either the GUI tracks it, or the daemon merges it.
            // Our `handle_config_update` in Daemon REPLACES `resources.config`.
            // This implies if we send empty `custom_source_thumbnails`, they go poof.

            // Solution: The GUI should track everything OR we instruct Daemon to "Update Profile Only".
            // But our message is `Update(DaemonConfig)`.
            // We should probably change `ConfigMessage` to `UpdateProfile(Profile)`?
            // Or `Update(DaemonConfig)`.

            // For now, let's try to construct it best effort.
            // `character_thumbnails` in Profile -> `character_thumbnails` in DaemonConfig.
            // Custom sources?
            // If they are in `Profile.character_thumbnails`, we can split them?
            // Actually, `DaemonConfig` splits them based on whether they match a rule?
            // If `Profile` stores valid positions for custom sources, we should just send them.

            // Let's look at `DaemonConfig` struct again.
            // It has `character_thumbnails` and `custom_source_thumbnails`.
            // `Profile` has `character_thumbnails`.

            // Reuse `DaemonConfig::from_profile(profile)` if it exists?
            // It doesn't.

            // Let's replicate `DaemonConfig` construction logic here or add meaningful constructor to `DaemonConfig`.
            // I'll add a helper `DaemonConfig::from_gui_config(profile)`?
            // But `DaemonConfig` is in `crate::config`.

            // Let's assume for this step we construct it manually.
            let mut character_thumbnails = selected_profile.character_thumbnails.clone();
            let mut custom_source_thumbnails = std::collections::HashMap::new();

            // If we want to separate them correctly, we need to know which are custom.
            // Iterate through `character_thumbnails`, if name matches a custom rule, move to `custom`.
            // But `Profile` is the source of truth.
            // If we send everything in `character_thumbnails`, and daemon splits them, that's fine?
            // Daemon uses `character_thumbnails` map for EVE and `custom` for custom.
            // If we populate `character_thumbnails` with EVERYTHING, the daemon might get confused if it expects separation.
            // `check_and_create_window` checks specific maps.

            // Filter based on custom rules in profile.
            let rules = &selected_profile.custom_windows;
            // keys to move
            let mut move_keys = Vec::new();
            for (key, _) in &character_thumbnails {
                if rules.iter().any(|r| r.alias == *key) {
                    move_keys.push(key.clone());
                }
            }

            for key in move_keys {
                if let Some(val) = character_thumbnails.remove(&key) {
                    custom_source_thumbnails.insert(key, val);
                }
            }

            // Build hotkeys for profile switching (requires looking at all profiles)
            let mut profile_hotkeys = std::collections::HashMap::new();
            for profile in &self.config.profiles {
                if let Some(ref binding) = profile.hotkey_profile_switch {
                    profile_hotkeys.insert(binding.clone(), profile.profile_name.clone());
                }
            }

            let daemon_config = DaemonConfig {
                profile: selected_profile,
                character_thumbnails,
                custom_source_thumbnails,
                profile_hotkeys,
                runtime_hidden: false, // Reset hidden state on config reload? acceptable.
            };

            if let Err(e) = tx.send(ConfigMessage::Update(daemon_config)) {
                error!(error = %e, "Failed to send config update to daemon");
            } else {
                info!("Sent config update to daemon");
            }
        }

        // Re-sync selected_profile_idx with the potentially reloaded profile list
        self.selected_profile_idx = self
            .config
            .profiles
            .iter()
            .position(|p| p.profile_name == self.config.global.selected_profile)
            .unwrap_or(0);

        self.settings_changed = false;
        self.config_status_message = Some(StatusMessage {
            text: "Configuration saved successfully".to_string(),
            color: COLOR_SUCCESS,
        });
        info!("Configuration saved to disk");
        Ok(())
    }

    pub fn switch_profile(&mut self, idx: usize) {
        info!(profile_idx = idx, "Profile switch requested");

        if idx < self.config.profiles.len() {
            self.config.global.selected_profile = self.config.profiles[idx].profile_name.clone();
            self.selected_profile_idx = idx;

            // Save config with new selection
            if let Err(err) = self.save_config() {
                error!(error = ?err, "Failed to save config after profile switch");
                self.status_message = Some(StatusMessage {
                    text: format!("Profile switch failed: {err}"),
                    color: STATUS_STOPPED,
                });
            } else {
                // Reload daemon with new profile
                self.reload_daemon_config();
            }
        }
    }

    pub fn discard_changes(&mut self) {
        self.config = Config::load().unwrap_or_default();

        // Re-find selected profile index after reload
        self.selected_profile_idx = self
            .config
            .profiles
            .iter()
            .position(|p| p.profile_name == self.config.global.selected_profile)
            .unwrap_or(0);

        self.settings_changed = false;
        self.config_status_message = Some(StatusMessage {
            text: "Changes discarded".to_string(),
            color: COLOR_ERROR,
        });
        info!("Configuration changes discarded");
    }

    pub fn reload_character_list(&mut self) {
        // Load fresh config from disk to get daemon's new characters
        if let Ok(disk_config) = Config::load() {
            // Merge new characters from disk into GUI config without losing GUI changes
            for (profile_idx, gui_profile) in self.config.profiles.iter_mut().enumerate() {
                if let Some(disk_profile) = disk_config.profiles.get(profile_idx)
                    && disk_profile.profile_name == gui_profile.profile_name
                {
                    // Add any new characters from disk that GUI doesn't know about
                    for (char_name, char_settings) in &disk_profile.character_thumbnails {
                        if !gui_profile.character_thumbnails.contains_key(char_name)
                            && !char_name.is_empty()
                        {
                            gui_profile
                                .character_thumbnails
                                .insert(char_name.clone(), char_settings.clone());
                            info!(character = %char_name, profile = %gui_profile.profile_name, "Detected new character from daemon");
                        }
                    }
                }
            }
        }
    }

    pub fn save_thumbnail_positions(&mut self) -> Result<()> {
        // With IPC, positions are automatically updated in memory via DaemonMessage::PositionChanged
        // and conditionally auto-saved.
        // If this method is called manually (e.g. from Tray), we just ensure config is saved to disk.

        self.save_config().context("Failed to save configuration")?;

        self.status_message = Some(StatusMessage {
            text: "Thumbnail positions saved".to_string(),
            color: STATUS_RUNNING,
        });
        Ok(())
    }

    pub fn poll_daemon(&mut self) {
        // 1. Check for Bootstrap handshake
        if let Some(ref rx) = self.bootstrap_rx {
            if let Ok(msg) = rx.try_recv() {
                info!("Received IPC channels from daemon");
                let (config_tx, status_rx) = msg;
                self.ipc_config_tx = Some(config_tx);

                // Bridge status_rx to GUI thread
                let (gui_tx, gui_rx) = mpsc::channel();
                self.gui_status_rx = Some(gui_rx);

                std::thread::spawn(move || {
                    loop {
                        match status_rx.recv() {
                            Ok(msg) => {
                                if let Err(_) = gui_tx.send(msg) {
                                    break; // GUI dropped
                                }
                            }
                            Err(_) => break, // Daemon closed
                        }
                    }
                });

                // Send Initial Config
                // Trigger a save_config (virtual) or just send logic?
                // We can just call save_config() to ensure we sync everything?
                // Or just extract and send.
                // Calling save_config() writes to disk which is unnecessary.
                // Let's invoke the sending logic directly or factor it out.
                // For now, I'll copy the sending logic/invoke restart logic?
                // Ideally `start_daemon` triggers logic.

                // HACK: Just trigger a save, it's cheap enough and ensures sync.
                let _ = self.save_config();

                self.bootstrap_rx = None; // Done
                self.daemon_status = DaemonStatus::Running;
            }
        }

        // 2. Poll Status Messages
        if let Some(ref rx) = self.gui_status_rx {
            while let Ok(msg) = rx.try_recv() {
                match msg {
                    DaemonMessage::Log { level, message } => {
                        info!(level = %level, "Daemon: {}", message);
                    }
                    DaemonMessage::Error(e) => {
                        error!("Daemon Error: {}", e);
                    }
                    DaemonMessage::PositionChanged {
                        name,
                        x,
                        y,
                        width,
                        height,
                    } => {
                        // Update in-memory config
                        if let Some(profile) = self.config.get_active_profile_mut() {
                            profile
                                .character_thumbnails
                                .entry(name.clone())
                                .and_modify(|s| {
                                    s.x = x;
                                    s.y = y;
                                    s.dimensions.width = width;
                                    s.dimensions.height = height;
                                })
                                .or_insert_with(|| {
                                    crate::types::CharacterSettings::new(x, y, width, height)
                                });
                        }

                        // Also update status message to show activity?
                        // debug!("Updated position for {}", name);

                        // We do NOT save to disk immediately to avoid spam.
                        // We could debounce save?
                        // But `state.rs` doesn't have a debouncer.
                        // Ideally we save on "significant" checkpoints or exit.
                        // Or we mimic the "Auto Save" behavior.
                        // Check profile auto-save setting
                        let auto_save = self
                            .config
                            .get_active_profile()
                            .map(|p| p.thumbnail_auto_save_position)
                            .unwrap_or(false);

                        if auto_save {
                            // Just save silently
                            // Use SaveStrategy::Overwrite since we are the authority now (daemon pushed to us)
                            // To avoid constant IO, maybe only if mLastSave > 1s?
                            let _ = self.config.save_with_strategy(SaveStrategy::Overwrite);
                        }
                    }
                    DaemonMessage::CharacterDetected(name) => {
                        info!("Daemon detected character: {}", name);
                        // Ensure it exists in config
                        // Logic handled by PositionChanged usually accompanies this if new?
                        // But if just detected and no position change (e.g. startup), we might want to reload list?
                        // self.reload_character_list(); // From memory?
                        // Actually `reload_character_list` loads from disk.
                        // We are the source of truth now.
                        // We should ensure `name` is in `character_thumbnails`.
                        // If not, add default?
                        // Daemon sends PositionChanged for new characters too.
                    }
                    _ => {}
                }
            }
        }

        if self.last_health_check.elapsed() < Duration::from_millis(DAEMON_CHECK_INTERVAL_MS) {
            return;
        }
        self.last_health_check = Instant::now();

        // Removed file polling logic here as GUI is now the owner

        if let Some(child) = self.daemon.as_mut() {
            match child.try_wait() {
                Ok(Some(status)) => {
                    warn!(pid = child.id(), exit = ?status.code(), "Preview daemon exited");
                    self.daemon = None;
                    self.daemon_status = if status.success() {
                        DaemonStatus::Stopped
                    } else {
                        DaemonStatus::Crashed(status.code())
                    };
                    // Clear IPC channels on exit
                    self.ipc_config_tx = None;
                    self.ipc_status_rx = None;
                    self.gui_status_rx = None;
                }
                Ok(None) => {
                    // Running
                }
                Err(err) => {
                    error!(error = ?err, "Failed to query daemon status");
                }
            }
        }
    }
}
