//! The Servers pane: a snapshot of a server over SSH. The connection form and
//! the snapshots taken so far are down the left; the one selected fills the
//! rest, with its packages and the updates waiting for them.
//!
//! Snapshots last as long as the window does. **Save CSV…** and **Save
//! JSON…** keep one.

use std::path::PathBuf;

use egui::{RichText, Ui};
use egui_extras::Column;

use crate::app::App;
use crate::config::Config;
use crate::servers::{Auth, Failure, ServerSettings, Snapshot, Target};
use crate::task::{Task, take_finished};
use crate::ui;
use crate::ui::shortcuts::{self, Command};

pub struct State {
    settings: ServerSettings,
    port: String,
    password: String,
    passphrase: String,
    snapshots: Vec<Snapshot>,
    selected: Option<usize>,
    query: String,
    updates_only: bool,
    error: Option<String>,
    take: Option<Task<Result<Snapshot, Failure>>>,
    /// What the running snapshot was asked to reach, kept so that it can be
    /// tried again once the user has agreed to trust the server's key.
    in_flight: Option<Target>,
    /// A server not in `known_hosts`, and its key's fingerprint, waiting for
    /// the user to say whether to trust it.
    pending_trust: Option<(Target, String)>,
}

impl State {
    pub fn from_config(config: &Config) -> Self {
        Self {
            port: config.server.port.to_string(),
            settings: config.server.clone(),
            password: String::new(),
            passphrase: String::new(),
            snapshots: Vec::new(),
            selected: None,
            query: String::new(),
            updates_only: false,
            error: None,
            take: None,
            in_flight: None,
            pending_trust: None,
        }
    }

    pub fn activity(&self) -> Option<String> {
        self.take.as_ref().map(|t| t.label.clone())
    }
}

/// `~/…` as the home directory, the way a shell would read it.
fn expand_home(path: &str) -> PathBuf {
    match (path.strip_prefix("~/").or(path.strip_prefix("~\\")), dirs::home_dir()) {
        (Some(rest), Some(home)) => home.join(rest),
        _ => PathBuf::from(path),
    }
}

/// The form as typed, checked.
fn target_of(state: &State) -> Result<Target, String> {
    let s = &state.settings;
    if s.host.trim().is_empty() {
        return Err("Enter the server's host name or address.".to_owned());
    }
    if s.user.trim().is_empty() {
        return Err("Enter the user name to sign in as.".to_owned());
    }
    let port = state
        .port
        .trim()
        .parse()
        .ok()
        .filter(|&port: &u16| port != 0)
        .ok_or_else(|| "The port is a number from 1 to 65535.".to_owned())?;
    let auth = if s.use_key {
        if s.key_path.trim().is_empty() {
            return Err("Choose the private key file to sign in with.".to_owned());
        }
        Auth::Key {
            path: expand_home(s.key_path.trim()),
            passphrase: state.passphrase.clone(),
        }
    } else {
        if state.password.is_empty() {
            return Err("Enter the password.".to_owned());
        }
        Auth::Password(state.password.clone())
    };
    Ok(Target {
        host: s.host.trim().to_owned(),
        port,
        user: s.user.trim().to_owned(),
        auth,
        trust: None,
    })
}

/// The keyboard shortcuts' commands; see [`shortcuts`]. Refresh takes the
/// snapshot again, as the button does.
pub fn command(app: &mut App, ctx: &egui::Context, command: Command) -> bool {
    match command {
        Command::Find if app.servers.selected.is_some() => ui::request_find(ctx),
        Command::Refresh => take_snapshot(app, ctx),
        _ => return false,
    }
    true
}

/// Take a snapshot of the server in the form, or say what is missing.
fn take_snapshot(app: &mut App, ctx: &egui::Context) {
    if app.servers.take.is_some() {
        return;
    }
    match target_of(&app.servers) {
        Ok(target) => start(app, ctx, target),
        Err(err) => {
            app.servers.error = Some(err.clone());
            app.report_error(err);
        }
    }
}

fn start(app: &mut App, ctx: &egui::Context, target: Target) {
    app.servers.error = None;
    app.servers.in_flight = Some(target.clone());
    let label = format!("Taking a snapshot of {}…", target.host);
    app.servers.take = Some(Task::spawn(ctx, label, move || {
        Ok(crate::servers::snapshot(&target))
    }));
}

pub fn poll(app: &mut App) {
    let Some(result) = take_finished(&mut app.servers.take) else {
        return;
    };
    let target = app.servers.in_flight.take();
    let error = match result {
        Ok(Ok(snapshot)) => {
            let message = format!(
                "Snapshot of {} taken: {} packages, {} updates waiting ({} security).",
                snapshot.address,
                snapshot.packages.len(),
                snapshot.upgradable.len(),
                snapshot.security_updates()
            );
            // A second snapshot of the same server replaces the first.
            let state = &mut app.servers;
            let index = match state.snapshots.iter().position(|s| s.address == snapshot.address) {
                Some(i) => {
                    state.snapshots[i] = snapshot;
                    i
                }
                None => {
                    state.snapshots.push(snapshot);
                    state.snapshots.len() - 1
                }
            };
            state.selected = Some(index);
            app.config.server = app.servers.settings.clone();
            if let Err(err) = app.config.save() {
                app.report_error(format!("The settings could not be saved: {err}"));
            }
            app.report_ok(message);
            return;
        }
        Ok(Err(Failure::UnknownHost { fingerprint })) => match target {
            Some(target) => {
                app.servers.pending_trust = Some((target, fingerprint));
                return;
            }
            None => "The server's key could not be checked.".to_owned(),
        },
        Ok(Err(Failure::Other(err))) | Err(err) => err,
    };
    app.servers.error = Some(error.clone());
    app.report_error(error);
}

pub fn show(app: &mut App, ui: &mut Ui) {
    ui::pane_header(
        ui,
        "Servers",
        "A snapshot of an Ubuntu or Debian server over SSH: what it runs, the packages installed, and the updates waiting. Nothing on the server is changed.",
    );
    let ctx = ui.ctx().clone();
    egui::Panel::left("server-form")
        .resizable(true)
        .default_size(320.0)
        .min_size(260.0)
        .show(ui, |ui| form(app, ui, &ctx));
    details(app, ui);
}

fn form(app: &mut App, ui: &mut Ui, ctx: &egui::Context) {
    let busy = app.servers.take.is_some();
    let mut go = false;
    let mut forget = None;
    egui::ScrollArea::vertical().show(ui, |ui| {
        ui.add_space(4.0);
        let state = &mut app.servers;
        let s = &mut state.settings;
        // Enter in any box of the form takes the snapshot.
        let mut enter = ui::submitted(&ui::labelled_field(ui, "Host", &mut s.host, "server.example.com or 192.0.2.10"));
        enter |= ui::submitted(&ui::labelled_field(ui, "Port", &mut state.port, "22"));
        enter |= ui::submitted(&ui::labelled_field(ui, "User", &mut s.user, "ubuntu"));

        ui.label(RichText::new("Sign in with").size(13.0));
        ui.horizontal(|ui| {
            ui.radio_value(&mut s.use_key, false, "Password");
            ui.radio_value(&mut s.use_key, true, "Key file");
        });
        ui.add_space(6.0);
        if s.use_key {
            enter |= ui::submitted(&ui::labelled_field(ui, "Private key file", &mut s.key_path, "~/.ssh/id_ed25519"));
            if ui.button("Browse…").clicked() {
                let mut dialog = rfd::FileDialog::new();
                if let Some(ssh) = dirs::home_dir().map(|h| h.join(".ssh")).filter(|d| d.is_dir()) {
                    dialog = dialog.set_directory(ssh);
                }
                if let Some(path) = dialog.pick_file() {
                    s.key_path = crate::config::tilde(&path);
                }
            }
            ui.add_space(6.0);
            enter |= ui::submitted(&ui::labelled_password(ui, "Passphrase, if the key has one", &mut state.passphrase));
        } else {
            enter |= ui::submitted(&ui::labelled_password(ui, "Password", &mut state.password));
        }
        ui.label(
            RichText::new(
                "Passwords and passphrases are never saved. The server's key is checked against ~/.ssh/known_hosts, the same file ssh uses.",
            )
            .size(12.0)
            .weak(),
        );
        ui.add_space(8.0);
        let label = if busy { "Connecting…" } else { "Take snapshot" };
        let button = ui
            .add_enabled_ui(!busy, |ui| ui::wide_button(ui, label))
            .inner
            .on_hover_text(ui::shortcut_hint(ui, "Connect and read the server", &shortcuts::REFRESH));
        go = button.clicked() || (enter && !busy);
        if let Some(err) = &state.error {
            ui.add_space(6.0);
            ui::error_text(ui, err);
        }

        if !state.snapshots.is_empty() {
            ui.add_space(16.0);
            ui.label(RichText::new("Snapshots").strong());
            ui.add_space(4.0);
            for (i, snapshot) in state.snapshots.iter().enumerate() {
                let name = if snapshot.hostname.is_empty() {
                    snapshot.address.clone()
                } else {
                    format!("{} ({})", snapshot.hostname, snapshot.address)
                };
                let text = format!(
                    "{name}\n{} · {} updates · {}",
                    if snapshot.os.is_empty() { "Unknown system" } else { &snapshot.os },
                    snapshot.upgradable.len(),
                    snapshot.taken
                );
                let button = egui::Button::selectable(state.selected == Some(i), text)
                    .corner_radius(6.0)
                    .min_size(egui::vec2(ui.available_width(), 0.0));
                let response = ui.add(button);
                if response.clicked() || response.secondary_clicked() {
                    state.selected = Some(i);
                }
                response.context_menu(|ui| {
                    ui::copy_item(ui, "host name", &snapshot.hostname);
                    ui::copy_item(ui, "address", &snapshot.address);
                    ui.separator();
                    if ui.button("Remove from the list").clicked() {
                        forget = Some(i);
                    }
                });
            }
        }
    });

    if let Some(i) = forget {
        let state = &mut app.servers;
        state.snapshots.remove(i);
        state.selected = match state.selected {
            Some(s) if s == i => None,
            Some(s) if s > i => Some(s - 1),
            other => other,
        };
    }
    if go {
        take_snapshot(app, ctx);
    }
}

fn details(app: &mut App, ui: &mut Ui) {
    let state = &mut app.servers;
    let Some(snapshot) = state.selected.and_then(|i| state.snapshots.get(i)) else {
        ui.add_space(20.0);
        ui.label(RichText::new("Take a snapshot to see it here.").weak());
        return;
    };

    ui.add_space(4.0);
    ui.heading(if snapshot.hostname.is_empty() {
        &snapshot.address
    } else {
        &snapshot.hostname
    });
    ui.label(
        RichText::new(format!("{} · taken {}", snapshot.address, snapshot.taken))
            .size(13.0)
            .weak(),
    );
    ui.add_space(6.0);
    let mut save_csv = false;
    let mut save_json = false;
    ui.horizontal(|ui| {
        save_csv = ui::tool_button(ui, !snapshot.packages.is_empty(), "Save CSV…").clicked();
        save_json = ui::tool_button(ui, true, "Save JSON…").clicked();
    });
    ui.add_space(8.0);

    let or_unknown = |v: &Option<String>| v.clone().unwrap_or_else(|| "Unknown".to_owned());
    ui.columns(3, |cols| {
        ui::property(&mut cols[0], "Operating system", &snapshot.os);
        ui::property(&mut cols[0], "Kernel", &snapshot.kernel);
        ui::property(&mut cols[0], "Architecture", &snapshot.architecture);
        ui::property(
            &mut cols[1],
            "Updates waiting",
            &format!(
                "{} ({} security)",
                snapshot.upgradable.len(),
                snapshot.security_updates()
            ),
        );
        ui::property(&mut cols[1], "Update list refreshed", &or_unknown(&snapshot.lists_updated));
        ui::property(&mut cols[1], "Last upgrade", &or_unknown(&snapshot.last_upgrade));
        ui::property(&mut cols[2], "Packages installed", &snapshot.packages.len().to_string());
        ui::property(
            &mut cols[2],
            "Up for",
            &snapshot
                .uptime_days
                .map(|d| format!("{d} days"))
                .unwrap_or_default(),
        );
        ui::property(
            &mut cols[2],
            "Reboot needed",
            if snapshot.reboot_required { "Yes" } else { "No" },
        );
    });
    if snapshot.reboot_required {
        let mut why = Vec::new();
        if let Some(kernel) = &snapshot.newer_kernel {
            why.push(format!("kernel {kernel} is installed but {} is running", snapshot.kernel));
        }
        if !snapshot.reboot_packages.is_empty() {
            why.push(format!("asked for by {}", snapshot.reboot_packages.join(", ")));
        }
        if !why.is_empty() {
            ui.label(
                RichText::new(format!("A reboot is needed: {}.", why.join("; ")))
                    .size(13.0)
                    .color(ui::warn_colour(ui)),
            );
        }
    }
    for note in &snapshot.notes {
        ui.label(RichText::new(note).size(13.0).color(ui::warn_colour(ui)));
    }

    ui.add_space(10.0);
    ui.horizontal(|ui| {
        ui.label(RichText::new("Packages").strong());
        ui.checkbox(&mut state.updates_only, "Only those with updates waiting");
    });
    ui.add_space(4.0);
    ui::search_box(ui, &mut state.query, "Search by package name or version");
    ui.add_space(6.0);

    let terms = ui::search_terms(&state.query);
    let shown: Vec<usize> = snapshot
        .packages
        .iter()
        .enumerate()
        .filter(|(_, p)| !state.updates_only || snapshot.upgrade_for(&p.name).is_some())
        .filter(|(_, p)| ui::matches_search(&terms, &[&p.name, &p.version]))
        .map(|(i, _)| i)
        .collect();
    let bad = ui::bad_colour(ui);
    ui::select_table(
        ui,
        "server-packages",
        &[
            ("Package", Column::initial(240.0).at_least(80.0)),
            ("Installed", Column::initial(200.0).at_least(60.0)),
            ("Update", Column::initial(200.0).at_least(60.0)),
            ("", Column::remainder().at_least(60.0)),
        ],
        shown.len(),
        None,
        |row, column, ui| {
            let package = &snapshot.packages[shown[row]];
            let upgrade = snapshot.upgrade_for(&package.name);
            match column {
                0 => ui::cell_text(ui, &package.name),
                1 => ui::cell_text(ui, &package.version),
                2 => ui::cell_text(ui, upgrade.map_or("", |u| u.available.as_str())),
                _ => {
                    if upgrade.is_some_and(|u| u.security) {
                        ui.label(RichText::new("Security").color(bad));
                    }
                }
            }
        },
        Some(&mut |row, ui| {
            let package = &snapshot.packages[shown[row]];
            let upgrade = snapshot.upgrade_for(&package.name);
            ui::copy_item(ui, "package name", &package.name);
            ui::copy_item(ui, "installed version", &package.version);
            ui::copy_item(ui, "update's version", upgrade.map_or("", |u| u.available.as_str()));
        }),
    );

    let snapshot = snapshot.clone();
    let stem = if snapshot.hostname.is_empty() {
        snapshot.address.clone()
    } else {
        snapshot.hostname.clone()
    };
    if save_csv
        && let Some(path) = rfd::FileDialog::new()
            .set_file_name(format!("{stem}-packages.csv"))
            .add_filter("CSV", &["csv"])
            .save_file()
    {
        match crate::csvio::write_packages(&path, &snapshot) {
            Ok(()) => app.report_ok(format!("Packages saved to {}.", crate::config::tilde(&path))),
            Err(err) => app.report_error(format!("Could not save the packages: {err}")),
        }
    }
    if save_json
        && let Some(path) = rfd::FileDialog::new()
            .set_file_name(format!("{stem}.json"))
            .add_filter("JSON", &["json"])
            .save_file()
    {
        let written = serde_json::to_string_pretty(&snapshot)
            .map_err(|e| e.to_string())
            .and_then(|json| std::fs::write(&path, json).map_err(|e| e.to_string()));
        match written {
            Ok(()) => app.report_ok(format!("Snapshot saved to {}.", crate::config::tilde(&path))),
            Err(err) => app.report_error(format!("Could not save the snapshot: {err}")),
        }
    }
}

/// Ask before trusting a server that is not in `known_hosts` yet.
pub fn modals(app: &mut App, ctx: &egui::Context) {
    let Some((target, fingerprint)) = &app.servers.pending_trust else {
        return;
    };
    let body = format!(
        "{} is not in ~/.ssh/known_hosts yet. Its key's fingerprint is\n\n{fingerprint}\n\nIf you can, check that this matches what the server says, for example with `ssh-keygen -lf /etc/ssh/ssh_host_ed25519_key.pub` run on it. Trusting it adds it to known_hosts, and a different key there later will be refused.",
        target.host
    );
    match ui::confirm_modal(
        ctx,
        egui::Id::new("trust-server"),
        "Trust this server?",
        &body,
        "Trust and connect",
    ) {
        ui::Confirmation::Waiting => {}
        ui::Confirmation::Dismissed => app.servers.pending_trust = None,
        ui::Confirmation::Confirmed => {
            if let Some((mut target, fingerprint)) = app.servers.pending_trust.take() {
                target.trust = Some(fingerprint);
                start(app, ctx, target);
            }
        }
    }
}
