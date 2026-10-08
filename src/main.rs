// Mainstone Cloud System — users, groups and devices in Microsoft Entra ID.
// Copyright (c) 2026 Will Richards. Released under the MIT licence; see LICENSE.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod config;
mod csvio;
mod export;
mod graph;
mod logging;
mod secrets;
mod servers;
mod task;
mod theme;
mod ui;
mod update;

/// The name shown in the title bar, and the one the README uses. The binary
/// itself is `mainstone`.
pub const APP_NAME: &str = "Mainstone Cloud System";

fn main() -> eframe::Result {
    logging::init();
    update::clean_up();

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(APP_NAME)
            .with_app_id("mainstone")
            .with_inner_size([1180.0, 740.0])
            .with_min_inner_size([860.0, 520.0]),
        ..Default::default()
    };

    eframe::run_native(
        APP_NAME,
        options,
        Box::new(|cc| Ok(Box::new(app::App::new(cc)))),
    )
}
