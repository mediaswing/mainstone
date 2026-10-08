//! The Export pane: a copy of users, groups, memberships and devices in a
//! MariaDB server. Laid out like watchspend's Database pane.

use std::sync::{Arc, Mutex};

use egui::{RichText, Ui};

use crate::app::{App, Tab};
use crate::config::Config;
use crate::export::{Choices, MariaDbSettings};
use crate::secrets;
use crate::task::{Task, take_finished};
use crate::ui;

pub struct State {
    pub settings: MariaDbSettings,
    pub port: String,
    pub remember_password: bool,
    pub choices: Choices,
    pub error: Option<String>,
    pub last_result: Option<String>,
    test: Option<Task<String>>,
    export: Option<Task<String>>,
    progress: Arc<Mutex<String>>,
}

impl State {
    pub fn from_config(config: &Config) -> Self {
        let mut settings = config.mariadb.clone();
        settings.password = secrets::load().mariadb_password;
        Self {
            port: settings.port.to_string(),
            remember_password: !settings.password.is_empty(),
            settings,
            choices: Choices::default(),
            error: None,
            last_result: None,
            test: None,
            export: None,
            progress: Arc::default(),
        }
    }

    pub fn activity(&self) -> Option<String> {
        if self.export.is_some() {
            return self.progress.lock().ok().map(|p| p.clone());
        }
        self.test.as_ref().map(|t| t.label.clone())
    }

    fn busy(&self) -> bool {
        self.test.is_some() || self.export.is_some()
    }
}

/// The settings as typed, with the port read out of its text box.
fn settings_of(state: &State) -> Result<MariaDbSettings, String> {
    let mut settings = state.settings.clone();
    settings.port = state
        .port
        .trim()
        .parse()
        .ok()
        .filter(|&port: &u16| port != 0)
        .ok_or_else(|| "The port is a number from 1 to 65535.".to_owned())?;
    Ok(settings)
}

/// Whether a connection to this host stays on this computer, where going
/// without TLS gives nothing away.
fn is_local(host: &str) -> bool {
    matches!(
        host.trim().to_ascii_lowercase().as_str(),
        "" | "localhost" | "127.0.0.1" | "::1" | "[::1]"
    )
}

/// Remember the server (and, if asked, the password) once it has worked.
fn remember(app: &mut App, settings: &MariaDbSettings) {
    app.config.mariadb = settings.clone();
    let mut stored = secrets::load();
    stored.mariadb_password = if app.export.remember_password {
        settings.password.clone()
    } else {
        String::new()
    };
    if let Err(err) = secrets::save(&stored) {
        app.report_error(err);
    }
    if let Err(err) = app.config.save() {
        app.report_error(format!("The settings could not be saved: {err}"));
    }
}

pub fn poll(app: &mut App) {
    for (slot_is_export, result) in [
        (false, take_finished(&mut app.export.test)),
        (true, take_finished(&mut app.export.export)),
    ] {
        let Some(result) = result else { continue };
        match result {
            Ok(message) => {
                app.export.error = None;
                if slot_is_export {
                    app.export.last_result = Some(format!(
                        "{} ({})",
                        message,
                        chrono::Local::now().format("%Y-%m-%d %H:%M")
                    ));
                }
                if let Ok(settings) = settings_of(&app.export) {
                    remember(app, &settings);
                }
                app.report_ok(message);
            }
            Err(err) => {
                app.export.error = Some(err.clone());
                app.report_error(err);
            }
        }
    }
}

pub fn show(app: &mut App, ui: &mut Ui) {
    ui::pane_header(
        ui,
        "Export",
        "Copy the directory into a MariaDB or MySQL server. The tables are created if they are not there, and a later export updates them in place.",
    );
    let ctx = ui.ctx().clone();
    let busy = app.export.busy();
    let mut test_clicked = false;
    let mut export_clicked = false;

    egui::ScrollArea::vertical().show(ui, |ui| {
        ui.set_max_width(640.0);
        let state = &mut app.export;
        let s = &mut state.settings;
        ui::labelled_field(ui, "Host", &mut s.host, "localhost");
        ui::labelled_field(ui, "Port", &mut state.port, "3306");
        ui::labelled_field(ui, "Database", &mut s.database, "gcm");
        ui::labelled_field(ui, "User", &mut s.username, "");
        ui::labelled_password(ui, "Password", &mut s.password);
        ui.checkbox(&mut s.use_tls, "Use TLS");
        if !s.use_tls && !is_local(&s.host) {
            ui.label(
                RichText::new(
                    "Without TLS, the password and every exported record cross the network unencrypted.",
                )
                .size(13.0)
                .color(ui::warn_colour(ui)),
            );
        }
        if s.use_tls {
            ui.checkbox(
                &mut s.tls_skip_verify,
                "Accept a certificate that does not match the host name",
            );
            if s.tls_skip_verify {
                ui.label(
                    RichText::new(
                        "Any server with a certificate from a trusted authority will be accepted, not only this one.",
                    )
                    .size(13.0)
                    .color(ui::warn_colour(ui)),
                );
            }
            ui::labelled_field(
                ui,
                "Extra certificate authority (optional)",
                &mut s.ca_cert_path,
                "path to a .pem or .der file",
            );
        }
        ui.checkbox(
            &mut state.remember_password,
            format!(
                "Remember the password in {}",
                crate::config::tilde(&secrets::path())
            ),
        );
        ui.add_space(4.0);
        ui.label(
            RichText::new("The account needs CREATE, SELECT, INSERT, UPDATE and DELETE on this database.")
                .size(12.0)
                .weak(),
        );
        ui.add_space(8.0);
        test_clicked = ui
            .add_enabled_ui(!busy, |ui| ui::wide_button(ui, "Test connection"))
            .inner
            .clicked();

        ui.add_space(18.0);
        ui.heading("What to export");
        ui.add_space(4.0);
        let c = &mut state.choices;
        ui.checkbox(&mut c.users, "Users > gcm_users");
        ui.checkbox(&mut c.groups, "Groups > gcm_groups");
        ui.checkbox(&mut c.members, "Group memberships > gcm_group_members (one Graph call per group)");
        ui.checkbox(&mut c.devices, "Devices, Entra and Intune > gcm_devices");
        ui.checkbox(&mut c.mailboxes, "Mailbox sizes, from the usage report > gcm_mailboxes");
        ui.add_space(4.0);
        ui.checkbox(
            &mut c.mirror,
            "Mirror: remove rows for objects no longer in the tenant",
        );
        ui.add_space(10.0);

        let nothing = !(c.users || c.groups || c.members || c.devices || c.mailboxes);
        let signed_in = app.graph.is_some();
        let label = if app.export.export.is_some() {
            "Exporting…"
        } else {
            "Export to MariaDB"
        };
        export_clicked = ui
            .add_enabled_ui(!busy && !nothing && signed_in, |ui| ui::wide_button(ui, label))
            .inner
            .clicked();
        if !signed_in {
            ui.horizontal(|ui| {
                ui.label(RichText::new("Sign in first to export.").weak());
                if ui.link("Connection").clicked() {
                    app.tab = Tab::Connection;
                }
            });
        }
        if app.export.export.is_some()
            && let Ok(p) = app.export.progress.lock()
        {
            ui::busy(ui, &p);
        }
        if let Some(err) = &app.export.error {
            ui.add_space(6.0);
            ui::error_text(ui, err);
        }
        if let Some(result) = &app.export.last_result {
            ui.add_space(6.0);
            ui.label(RichText::new(result).color(ui::good_colour(ui)));
        }

        ui.add_space(12.0);
        egui::CollapsingHeader::new("Table definitions").show(ui, |ui| {
            for statement in crate::export::SCHEMA {
                ui.add(
                    egui::Label::new(RichText::new(*statement).monospace().size(12.0))
                        .selectable(true),
                );
                ui.add_space(6.0);
            }
        });
    });

    if test_clicked || export_clicked {
        let settings = match settings_of(&app.export) {
            Ok(s) => s,
            Err(err) => {
                app.export.error = Some(err.clone());
                app.report_error(err);
                return;
            }
        };
        app.export.error = None;
        if test_clicked {
            app.export.test = Some(Task::spawn(&ctx, "Testing MariaDB connection…", move || {
                settings.test()
            }));
        } else if let Some(graph) = app.graph.clone() {
            // Rows are filed under the tenant's GUID, so a tenant signed in
            // to by one of its domain names lands in the same rows.
            let tenant = app
                .session
                .as_ref()
                .and_then(|s| s.tenant_guid.clone())
                .unwrap_or_else(|| graph.tenant_id().to_owned());
            let choices = app.export.choices;
            let progress = app.export.progress.clone();
            app.export.export = Some(Task::spawn(&ctx, "Exporting…", move || {
                crate::export::run(&graph, &tenant, &settings, choices, &progress)
            }));
        }
    }
}
