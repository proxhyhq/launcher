#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

mod app;
mod log;
mod process;
mod update;

use app::App;
use eframe::egui;
use log::MAX_LOG_LINES;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use update::{
    ProxhyState, UpdateState, spawn_ensure_binary, spawn_gui_update_check,
    spawn_proxhy_update_check,
};

fn main() -> eframe::Result {
    let state = Arc::new(Mutex::new(UpdateState::default()));
    let log = Arc::new(Mutex::new(VecDeque::with_capacity(MAX_LOG_LINES)));
    let proxhy_state = Arc::new(Mutex::new(ProxhyState::default()));

    let icon_data = {
        #[cfg(target_os = "macos")]
        let bytes: &[u8] = include_bytes!("../assets/icons/Proxhy_padded.png");
        #[cfg(not(target_os = "macos"))]
        let bytes: &[u8] = include_bytes!("../assets/icons/Proxhy.png");
        eframe::icon_data::from_png_bytes(bytes).expect("failed to load icon")
    };

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Proxhy")
            .with_icon(icon_data)
            .with_inner_size([800.0, 500.0]),
        ..Default::default()
    };

    eframe::run_native(
        "Proxhy",
        options,
        Box::new(move |cc| {
            let ctx = cc.egui_ctx.clone();
            spawn_ensure_binary(Arc::clone(&log), Arc::clone(&proxhy_state), ctx);
            spawn_gui_update_check(Arc::clone(&state));
            spawn_proxhy_update_check(Arc::clone(&proxhy_state));
            Ok(Box::new(App::new(cc, state, proxhy_state, log)))
        }),
    )
}
