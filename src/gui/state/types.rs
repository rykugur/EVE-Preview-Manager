use crate::constants::gui::*;
use eframe::egui;

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
