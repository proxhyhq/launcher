use eframe::egui;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

pub const MAX_LOG_LINES: usize = 1000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LogLevel {
    Info,
    Warn,
    Error,
    Debug,
    Trace,
    Gui,
    Unknown,
}

#[derive(Clone, Debug)]
pub struct LogLine {
    pub level: LogLevel,
    pub text: String,
}

impl LogLine {
    fn parse(raw: &str) -> Self {
        let upper = raw.to_uppercase();
        let level = if raw.starts_with("[gui]") {
            LogLevel::Gui
        } else if upper.contains("[ERROR]")
            || upper.contains("ERROR:")
            || upper.contains("EXCEPTION")
            || upper.contains("TRACEBACK")
        {
            LogLevel::Error
        } else if upper.contains("[WARN]")
            || upper.contains("WARNING:")
            || upper.contains("DEPRECATIONWARNING")
            || upper.contains("WARNING,")
        {
            LogLevel::Warn
        } else if upper.contains("[DEBUG]") {
            LogLevel::Debug
        } else if upper.contains("[TRACE]") {
            LogLevel::Trace
        } else if upper.contains("[INFO]") {
            LogLevel::Info
        } else {
            LogLevel::Unknown
        };
        Self {
            level,
            text: raw.to_string(),
        }
    }

    pub const fn color(&self) -> egui::Color32 {
        match self.level {
            LogLevel::Error => egui::Color32::from_rgb(255, 85, 85),
            LogLevel::Warn => egui::Color32::from_rgb(255, 184, 76),
            LogLevel::Info => egui::Color32::from_rgb(100, 220, 140),
            LogLevel::Debug => egui::Color32::from_rgb(130, 160, 255),
            LogLevel::Trace => egui::Color32::from_rgb(160, 130, 200),
            LogLevel::Gui => egui::Color32::from_rgb(100, 160, 255),
            LogLevel::Unknown => egui::Color32::from_rgb(180, 180, 180),
        }
    }

    pub const fn badge_text(&self) -> Option<&'static str> {
        match self.level {
            LogLevel::Error => Some("ERR"),
            LogLevel::Warn => Some("WRN"),
            LogLevel::Info => Some("INF"),
            LogLevel::Debug => Some("DBG"),
            LogLevel::Trace => Some("TRC"),
            LogLevel::Gui => Some("GUI"),
            LogLevel::Unknown => None,
        }
    }
}

pub fn push_log(log: &Arc<Mutex<VecDeque<LogLine>>>, raw: &str, ctx: &egui::Context) {
    let mut l = log.lock().unwrap();
    if l.len() >= MAX_LOG_LINES {
        l.pop_front();
    }
    l.push_back(LogLine::parse(raw));
    drop(l);
    ctx.request_repaint();
}
