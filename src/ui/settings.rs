//! The Settings pane: light, dark, or follow the system, updates, and the
//! debug log.

use egui::{RichText, Ui};

use crate::app::App;
use crate::config::Appearance;
use crate::ui;

pub fn show(app: &mut App, ui: &mut Ui) {
    ui::pane_header(ui, "Settings", "How the app looks, updates, and a log for when something goes wrong.");

    egui::ScrollArea::vertical().show(ui, |ui| {
        ui.set_max_width(640.0);
        ui.label(RichText::new("Appearance").strong());
        ui.add_space(6.0);
        let mut changed = false;
        for appearance in Appearance::ALL {
            let selected = app.config.appearance == appearance;
            let button = egui::Button::selectable(
                selected,
                ui::centred(RichText::new(appearance.label()).size(15.0)),
            )
            .corner_radius(6.0)
            .frame_when_inactive(true)
            .min_size(egui::vec2(ui.available_width(), 38.0));
            if ui.add(button).clicked() && !selected {
                app.config.appearance = appearance;
                changed = true;
            }
            ui.label(RichText::new(appearance.description()).size(12.0).weak());
            ui.add_space(8.0);
        }
        ui.add_space(8.0);
        ui.label(RichText::new("Troubleshooting").strong());
        ui.add_space(6.0);
        let forced = crate::logging::forced_by_environment();
        let mut debug = app.config.debug_logging;
        if ui
            .add_enabled(!forced, egui::Checkbox::new(&mut debug, "Write a debug log"))
            .changed()
        {
            match crate::logging::set_debug(debug) {
                Ok(()) => {
                    app.config.debug_logging = debug;
                    changed = true;
                }
                Err(err) => app.report_error(err),
            }
        }
        let log_path = crate::logging::path();
        ui.label(
            RichText::new(format!(
                "Records each request to Microsoft Graph and MariaDB, with its result and Microsoft's request ID, in {}. Secrets, tokens and passwords are never written to it.",
                crate::config::tilde(&log_path)
            ))
            .size(12.0)
            .weak(),
        );
        if forced {
            ui.label(
                RichText::new("On for this run, because GCM_DEBUG is set.")
                    .size(12.0)
                    .color(ui::warn_colour(ui)),
            );
        }
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            if ui.button("Show the log file").clicked()
                && let Err(err) = crate::logging::reveal()
            {
                app.report_error(err);
            }
            if ui.button("Copy its location").clicked() {
                ui.ctx().copy_text(log_path.display().to_string());
                app.report_ok("Log file location copied.");
            }
        });

        ui.add_space(16.0);
        ui.label(RichText::new("Updates").strong());
        ui.add_space(6.0);
        if ui
            .checkbox(&mut app.config.check_for_updates, "Check for updates when the app starts")
            .changed()
        {
            changed = true;
        }
        ui.label(
            RichText::new(
                "Asks GitHub for the latest release of mediaswing/mainstone. When there is a newer one, a banner offers to download it, check it against GitHub's published checksum, install it and restart.",
            )
            .size(12.0)
            .weak(),
        );
        ui.add_space(4.0);
        let checking = app.update.checking();
        if ui
            .add_enabled(
                !checking,
                egui::Button::new(if checking { "Checking…" } else { "Check now" }),
            )
            .clicked()
        {
            let ctx = ui.ctx().clone();
            crate::ui::update::check(app, &ctx, true);
        }

        if changed && let Err(err) = app.config.save() {
            app.report_error(format!("The setting could not be saved: {err}"));
        }

        ui.add_space(16.0);
        ui.label(
            RichText::new(format!(
                "Settings are kept in {}.",
                crate::config::tilde(&crate::config::config_path())
            ))
            .size(12.0)
            .weak(),
        );
        ui.label(
            RichText::new(concat!(
                "Mainstone Cloud System ",
                env!("CARGO_PKG_VERSION"),
                " · MIT licence"
            ))
            .size(12.0)
            .weak(),
        );
    });
}
