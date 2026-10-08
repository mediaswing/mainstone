//! The update banner across the top of the window, and the check behind it.

use std::sync::{Arc, Mutex};

use egui::{RichText, Ui};

use crate::app::App;
use crate::task::{Task, take_finished};
use crate::ui;
use crate::update::{self, Release};

#[derive(Default)]
pub struct State {
    check: Option<Task<Option<Release>>>,
    /// Whether the last check was asked for from Settings, so that "you are
    /// up to date" is said then, and not at every startup.
    asked: bool,
    found: Option<Release>,
    install: Option<Task<()>>,
    progress: Arc<Mutex<String>>,
    error: Option<String>,
}

impl State {
    pub fn activity(&self) -> Option<String> {
        if self.install.is_some() {
            return self.progress.lock().ok().map(|p| p.clone());
        }
        self.check.as_ref().map(|t| t.label.clone())
    }

    pub fn checking(&self) -> bool {
        self.check.is_some()
    }
}

/// Ask GitHub for the latest release. `asked` is true when the user pressed
/// the button rather than this being the check at startup.
pub fn check(app: &mut App, ctx: &egui::Context, asked: bool) {
    if app.update.check.is_some() || app.update.install.is_some() {
        return;
    }
    app.update.asked = asked;
    app.update.check = Some(Task::spawn(ctx, "Checking for updates…", update::check));
}

pub fn poll(app: &mut App, ctx: &egui::Context) {
    if let Some(result) = take_finished(&mut app.update.check) {
        let asked = std::mem::take(&mut app.update.asked);
        match result {
            Ok(Some(release)) => {
                let skipped = app.config.skipped_update.as_deref() == Some(release.version.as_str());
                if asked || !skipped {
                    app.update.found = Some(release);
                    app.update.error = None;
                }
            }
            Ok(None) if asked => app.report_ok(format!("Mainstone Cloud System {} is the latest version.", update::CURRENT)),
            Ok(None) => {}
            // At startup a failed check says nothing: being offline is not
            // something to be told about.
            Err(err) if asked => app.report_error(err),
            Err(err) => log::info!("update check failed: {err}"),
        }
    }

    if let Some(result) = take_finished(&mut app.update.install) {
        match result {
            Ok(()) => {
                log::info!("update installed; closing so the new version can take over");
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            Err(err) => {
                app.update.error = Some(err.clone());
                app.report_error(err);
            }
        }
    }
}

/// The banner, when there is something to offer. Drawn at the top of the
/// central panel, above whichever tab is showing.
pub fn banner(app: &mut App, ui: &mut Ui) {
    let Some(release) = app.update.found.clone() else {
        return;
    };
    let installing = app.update.install.is_some();
    // An import half done would be lost if the app closed under it.
    let importing = app.users.importing();

    let mut install = false;
    let mut skip = false;
    let mut later = false;
    egui::Frame::group(ui.style())
        .fill(ui.visuals().faint_bg_color)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal_wrapped(|ui| {
                ui.label(
                    RichText::new(format!(
                        "Mainstone Cloud System {} is available. You have {}.",
                        release.version,
                        update::CURRENT
                    ))
                    .strong(),
                );
                if ui.link("What's new").clicked() {
                    ui.ctx().open_url(egui::OpenUrl::new_tab(&release.page));
                }
            });
            ui.horizontal_wrapped(|ui| {
                if installing {
                    let progress = app.update.progress.lock().map(|p| p.clone()).unwrap_or_default();
                    ui::busy(ui, &progress);
                    return;
                }
                if release.package.is_some() {
                    install = ui::tool_button(ui, !importing, "Install and restart").clicked();
                } else if ui::tool_button(ui, true, "Download…").clicked() {
                    ui.ctx().open_url(egui::OpenUrl::new_tab(&release.page));
                }
                later = ui.button("Not now").clicked();
                skip = ui.button("Skip this version").clicked();
            });
            if release.package.is_none() && !installing {
                ui.label(
                    RichText::new(
                        "This copy was not installed from a release package, so it cannot update itself. Download the new version from the release page.",
                    )
                    .size(12.0)
                    .weak(),
                );
            }
            if importing && !installing {
                ui.label(
                    RichText::new("A CSV import is running. Installing waits until it has finished.")
                        .size(12.0)
                        .color(ui::warn_colour(ui)),
                );
            }
            if let Some(err) = &app.update.error {
                ui::error_text(ui, err);
            }
        });
    ui.add_space(6.0);

    if install && let Some(package) = release.package.clone() {
        app.update.error = None;
        let progress = app.update.progress.clone();
        if let Ok(mut p) = progress.lock() {
            *p = "Downloading the update…".into();
        }
        let ctx = ui.ctx().clone();
        app.update.install = Some(Task::spawn(&ctx, "Updating…", move || {
            update::install(&package, &progress)
        }));
    }
    if later {
        app.update.found = None;
    }
    if skip {
        app.config.skipped_update = Some(release.version.clone());
        app.update.found = None;
        if let Err(err) = app.config.save() {
            app.report_error(format!("The setting could not be saved: {err}"));
        }
    }
}
