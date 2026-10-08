//! The Devices pane: Entra devices joined with their Intune records, and the
//! remote actions Intune can send.

use egui::{RichText, Ui};
use egui_extras::Column;

use crate::app::{App, Tab};
use crate::graph::devices::{DeviceList, IntuneAction};
use crate::graph::models::{DeviceRow, short_time};
use crate::graph::{Graph, Result};
use crate::task::{Task, take_finished};
use crate::ui;
use crate::ui::shortcuts::{self, Command};

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum Filter {
    #[default]
    All,
    Intune,
    NotIntune,
    Noncompliant,
}

impl Filter {
    const ALL: [Self; 4] = [Self::All, Self::Intune, Self::NotIntune, Self::Noncompliant];

    fn label(self) -> &'static str {
        match self {
            Self::All => "All devices",
            Self::Intune => "Managed by Intune",
            Self::NotIntune => "Not in Intune",
            Self::Noncompliant => "Not compliant",
        }
    }

    fn keeps(self, row: &DeviceRow) -> bool {
        match self {
            Self::All => true,
            Self::Intune => row.intune.is_some(),
            Self::NotIntune => row.intune.is_none(),
            Self::Noncompliant => {
                let c = row.compliance();
                !c.is_empty() && c != "compliant"
            }
        }
    }
}

/// What a finished action means for the list.
enum Change {
    Nothing,
    EntraEnabled(String, bool),
    /// The Entra half has gone. An Intune half, if there was one, stays.
    EntraRemoved(String),
    /// The Intune half has gone. An Entra half, if there was one, stays.
    IntuneRemoved(String),
}

/// A row is selected by whichever ID it has, since either half can be absent.
#[derive(Clone, PartialEq, Eq)]
enum Key {
    Entra(String),
    Intune(String),
}

fn key_of(row: &DeviceRow) -> Key {
    match (&row.entra, &row.intune) {
        (Some(e), _) => Key::Entra(e.id.clone()),
        (None, Some(i)) => Key::Intune(i.id.clone()),
        (None, None) => Key::Entra(String::new()),
    }
}

/// An action waiting for "are you sure".
enum Pending {
    Intune {
        id: String,
        name: String,
        action: IntuneAction,
    },
    DeleteEntra {
        id: String,
        name: String,
    },
}

/// What can be done to one device, from the buttons in the details panel or
/// the row's right-click menu.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Action {
    Intune(IntuneAction),
    ToggleEntra,
    DeleteEntra,
}

/// The actions that apply to a device, in the order they are offered: the
/// Intune ones first, as the details panel shows them.
fn actions(row: &DeviceRow) -> Vec<Action> {
    let mut actions = Vec::new();
    if let Some(i) = &row.intune {
        actions.extend(IntuneAction::ALL.into_iter().filter(|a| a.applies_to(i)).map(Action::Intune));
    }
    if row.entra.is_some() {
        actions.extend([Action::ToggleEntra, Action::DeleteEntra]);
    }
    actions
}

fn label(row: &DeviceRow, action: Action) -> &'static str {
    match action {
        Action::Intune(a) => a.label(),
        Action::ToggleEntra if row.entra.as_ref().is_some_and(|e| e.account_enabled == Some(false)) => "Enable",
        Action::ToggleEntra => "Disable",
        Action::DeleteEntra => "Delete from Entra ID",
    }
}

#[derive(Default)]
pub struct State {
    rows: Vec<DeviceRow>,
    intune_error: Option<String>,
    loaded: bool,
    load_error: Option<String>,
    load: Option<Task<DeviceList>>,
    query: String,
    filter: Filter,
    selected: Option<Key>,
    action: Option<Task<(String, Change)>>,
    confirm: Option<Pending>,
}

impl State {
    pub fn activity(&self) -> Option<String> {
        self.load
            .as_ref()
            .map(|t| t.label.clone())
            .or_else(|| self.action.as_ref().map(|t| t.label.clone()))
    }

    fn shown(&self) -> Vec<usize> {
        let terms = ui::search_terms(&self.query);
        self.rows
            .iter()
            .enumerate()
            .filter(|(_, r)| self.filter.keeps(r))
            .filter(|(_, r)| {
                let serial = r
                    .intune
                    .as_ref()
                    .and_then(|i| i.serial_number.as_deref())
                    .unwrap_or("");
                ui::matches_search(&terms, &[r.name(), &r.os(), r.user(), serial])
            })
            .map(|(i, _)| i)
            .collect()
    }

    fn selected_row(&self) -> Option<&DeviceRow> {
        let key = self.selected.as_ref()?;
        self.rows.iter().find(|r| key_of(r) == *key)
    }
}

/// The keyboard shortcuts' commands; see [`shortcuts`]. A device has two
/// kinds of delete, so Delete is left to the buttons, which say which.
pub fn command(app: &mut App, ctx: &egui::Context, command: Command) -> bool {
    if app.graph.is_none() {
        return false;
    }
    match command {
        Command::Find => ui::request_find(ctx),
        Command::Refresh => {
            if app.devices.load.is_none() {
                app.devices.loaded = false;
            }
        }
        Command::Deselect => return app.devices.selected.take().is_some(),
        Command::New | Command::Delete => return false,
    }
    true
}

fn perform(app: &mut App, ctx: &egui::Context, row: &DeviceRow, action: Action) {
    if app.devices.action.is_some() {
        return;
    }
    match action {
        Action::Intune(action) => {
            let Some(i) = &row.intune else { return };
            let pending = Pending::Intune {
                id: i.id.clone(),
                name: row.name().to_owned(),
                action,
            };
            if action.is_drastic() {
                app.devices.confirm = Some(pending);
            } else {
                start(app, ctx, pending);
            }
        }
        Action::ToggleEntra => {
            let Some(e) = &row.entra else { return };
            let enabled = e.account_enabled != Some(false);
            let id = e.id.clone();
            let name = row.name().to_owned();
            run(app, ctx, "Updating device…", move |g| {
                g.set_device_enabled(&id, !enabled)?;
                let verb = if enabled { "disabled" } else { "enabled" };
                Ok((format!("{name} {verb} in Entra ID."), Change::EntraEnabled(id, !enabled)))
            });
        }
        Action::DeleteEntra => {
            let Some(e) = &row.entra else { return };
            app.devices.confirm = Some(Pending::DeleteEntra {
                id: e.id.clone(),
                name: row.name().to_owned(),
            });
        }
    }
}

fn run(
    app: &mut App,
    ctx: &egui::Context,
    label: &str,
    work: impl FnOnce(&Graph) -> Result<(String, Change)> + Send + 'static,
) {
    let Some(graph) = app.graph.clone() else { return };
    if app.devices.action.is_some() {
        return;
    }
    app.devices.action = Some(Task::spawn(ctx, label, move || work(&graph)));
}

pub fn poll(app: &mut App) {
    if let Some(result) = take_finished(&mut app.devices.load) {
        app.devices.loaded = true;
        match result {
            Ok(list) => {
                app.devices.rows = list.rows;
                app.devices.intune_error = list.intune_error;
                app.devices.load_error = None;
            }
            Err(err) => {
                app.devices.load_error = Some(err.clone());
                app.report_error(format!("Could not load devices: {err}"));
            }
        }
    }

    if let Some(result) = take_finished(&mut app.devices.action) {
        match result {
            Ok((message, change)) => {
                let rows = &mut app.devices.rows;
                match change {
                    Change::Nothing => {}
                    Change::EntraEnabled(id, enabled) => {
                        if let Some(e) = rows
                            .iter_mut()
                            .filter_map(|r| r.entra.as_mut())
                            .find(|e| e.id == id)
                        {
                            e.account_enabled = Some(enabled);
                        }
                    }
                    Change::EntraRemoved(id) => {
                        for r in rows.iter_mut() {
                            if r.entra.as_ref().is_some_and(|e| e.id == id) {
                                r.entra = None;
                                // The row is now known by its Intune ID, so
                                // the selection follows it there.
                                if let Some(i) = &r.intune
                                    && app.devices.selected == Some(Key::Entra(id.clone()))
                                {
                                    app.devices.selected = Some(Key::Intune(i.id.clone()));
                                }
                            }
                        }
                        rows.retain(|r| r.entra.is_some() || r.intune.is_some());
                    }
                    Change::IntuneRemoved(id) => {
                        for r in rows.iter_mut() {
                            if r.intune.as_ref().is_some_and(|i| i.id == id) {
                                r.intune = None;
                            }
                        }
                        rows.retain(|r| r.entra.is_some() || r.intune.is_some());
                    }
                }
                if app.devices.selected_row().is_none() {
                    app.devices.selected = None;
                }
                app.report_ok(message);
            }
            Err(err) => app.report_error(err),
        }
    }
}

pub fn show(app: &mut App, ui: &mut Ui) {
    let ctx = ui.ctx().clone();
    if app.graph.is_none() {
        ui::pane_header(ui, "Devices", "");
        if ui::not_connected(ui) {
            app.tab = Tab::Connection;
        }
        return;
    }
    if !app.devices.loaded
        && app.devices.load.is_none()
        && let Some(graph) = app.graph.clone()
    {
        app.devices.load = Some(Task::spawn(&ctx, "Loading devices…", move || {
            graph.list_devices()
        }));
    }

    let shown = app.devices.shown();
    let subtitle = if !app.devices.loaded {
        "Loading…".to_owned()
    } else {
        let managed = app.devices.rows.iter().filter(|r| r.intune.is_some()).count();
        format!(
            "{} shown of {} devices, {managed} managed by Intune",
            shown.len(),
            app.devices.rows.len()
        )
    };
    ui::pane_header(ui, "Devices", &subtitle);

    ui.horizontal(|ui| {
        if ui::tool_button(ui, app.devices.load.is_none(), "Refresh")
            .on_hover_text(ui::shortcut_hint(ui, "Read the devices again", &shortcuts::REFRESH))
            .clicked()
        {
            app.devices.loaded = false;
        }
        ui.separator();
        for filter in Filter::ALL {
            ui.selectable_value(&mut app.devices.filter, filter, filter.label());
        }
    });
    ui.add_space(4.0);
    ui::search_box(ui, &mut app.devices.query, "Search by name, operating system, user or serial number");
    ui.add_space(6.0);
    if let Some(err) = &app.devices.load_error {
        ui::error_text(ui, err);
    }
    if let Some(err) = &app.devices.intune_error {
        ui.label(
            RichText::new(format!("Intune devices could not be read, so only Entra devices are shown: {err}"))
                .size(13.0)
                .color(ui::warn_colour(ui)),
        );
    }

    if app.devices.selected_row().is_some() {
        egui::Panel::right("device-details")
            .resizable(true)
            .default_size(340.0)
            .min_size(280.0)
            .show(ui, |ui| details(app, ui, &ctx));
    }

    let selected_index = app.devices.selected.as_ref().and_then(|key| {
        shown
            .iter()
            .position(|&i| key_of(&app.devices.rows[i]) == *key)
    });
    let rows = &app.devices.rows;
    let idle = app.devices.action.is_none();
    let mut chosen = None;
    let clicks = ui::select_table(
        ui,
        "devices",
        &[
            ("Name", Column::initial(200.0).at_least(80.0)),
            ("Operating system", Column::initial(170.0).at_least(60.0)),
            ("Primary user", Column::initial(200.0).at_least(60.0)),
            ("Compliance", Column::initial(110.0).at_least(60.0)),
            ("Intune", Column::initial(60.0).at_least(40.0)),
            ("Last seen (UTC)", Column::remainder().at_least(80.0)),
        ],
        shown.len(),
        selected_index,
        |row, column, ui| {
            let r = &rows[shown[row]];
            match column {
                0 => ui::cell_text(ui, r.name()),
                1 => ui::cell_text(ui, &r.os()),
                2 => ui::cell_text(ui, r.user()),
                3 => compliance_label(ui, r.compliance()),
                4 => ui::cell_text(ui, if r.intune.is_some() { "Yes" } else { "No" }),
                _ => ui::cell_text(ui, &short_time(r.last_seen())),
            }
        },
        Some(&mut |row, ui| {
            let r = &rows[shown[row]];
            let actions = actions(r);
            let mut entra_started = false;
            for action in &actions {
                // The Entra actions under a line of their own, as in the
                // details panel.
                if !matches!(action, Action::Intune(_)) && !entra_started {
                    entra_started = true;
                    if actions.iter().any(|a| matches!(a, Action::Intune(_))) {
                        ui.separator();
                    }
                }
                let mut button = ui.add_enabled(idle, egui::Button::new(label(r, *action)));
                if let Action::Intune(a) = action {
                    button = button.on_hover_text(a.explanation());
                }
                if button.clicked() {
                    chosen = Some((*action, key_of(r)));
                }
            }
            if !actions.is_empty() {
                ui.separator();
            }
            let serial = r.intune.as_ref().and_then(|i| i.serial_number.as_deref()).unwrap_or("");
            ui::copy_item(ui, "name", r.name());
            ui::copy_item(ui, "primary user", r.user());
            ui::copy_item(ui, "serial number", serial);
            ui::copy_item(ui, "Entra object ID", r.entra.as_ref().map_or("", |e| e.id.as_str()));
            ui::copy_item(ui, "Intune device ID", r.intune.as_ref().map_or("", |i| i.id.as_str()));
        }),
    );
    if let Some(row) = clicks.clicked {
        let key = key_of(&app.devices.rows[shown[row]]);
        app.devices.selected = if app.devices.selected.as_ref() == Some(&key) {
            None
        } else {
            Some(key)
        };
    }
    if let Some(row) = clicks.right_clicked {
        app.devices.selected = Some(key_of(&app.devices.rows[shown[row]]));
    }
    if let Some((action, key)) = chosen
        && let Some(row) = app.devices.rows.iter().find(|r| key_of(r) == key).cloned()
    {
        perform(app, &ctx, &row, action);
    }
}

fn compliance_label(ui: &mut Ui, state: &str) {
    match state {
        "compliant" => {
            ui.label(RichText::new("Compliant").color(ui::good_colour(ui)));
        }
        "" => {}
        other => {
            ui.label(RichText::new(other).color(ui::warn_colour(ui)));
        }
    }
}

fn details(app: &mut App, ui: &mut Ui, ctx: &egui::Context) {
    let Some(row) = app.devices.selected_row().cloned() else {
        return;
    };
    let idle = app.devices.action.is_none();
    let mut chosen = None;

    egui::ScrollArea::vertical().show(ui, |ui| {
        ui.add_space(8.0);
        ui.heading(row.name());
        ui.add_space(8.0);

        let s = |v: &Option<String>| v.clone().unwrap_or_default();

        if let Some(i) = &row.intune {
            ui.label(RichText::new("Intune").strong());
            ui.add_space(4.0);
            ui.horizontal_wrapped(|ui| {
                for action in IntuneAction::ALL {
                    if !action.applies_to(i) {
                        continue;
                    }
                    if ui::tool_button(ui, idle, action.label())
                        .on_hover_text(action.explanation())
                        .clicked()
                    {
                        chosen = Some(Action::Intune(action));
                    }
                }
            });
            ui.add_space(6.0);
            ui::property(ui, "Primary user", &s(&i.user_principal_name));
            ui::property(ui, "Compliance", &s(&i.compliance_state));
            ui::property(ui, "Ownership", &s(&i.managed_device_owner_type));
            ui::property(
                ui,
                "Model",
                format!("{} {}", s(&i.manufacturer), s(&i.model)).trim(),
            );
            ui::property(ui, "Serial number", &s(&i.serial_number));
            ui::property(ui, "Management agent", &s(&i.management_agent));
            ui::property(ui, "Enrolled (UTC)", &short_time(i.enrolled_date_time.as_deref()));
            ui::property(ui, "Last sync (UTC)", &short_time(i.last_sync_date_time.as_deref()));
            ui::property(ui, "Intune device ID", &i.id);
            ui.add_space(8.0);
        } else {
            ui.label(RichText::new("Not enrolled in Intune.").weak());
            ui.add_space(8.0);
        }

        if let Some(e) = &row.entra {
            ui.label(RichText::new("Entra ID").strong());
            ui.add_space(4.0);
            let enabled = e.account_enabled != Some(false);
            ui.horizontal_wrapped(|ui| {
                for action in [Action::ToggleEntra, Action::DeleteEntra] {
                    if ui::tool_button(ui, idle, label(&row, action)).clicked() {
                        chosen = Some(action);
                    }
                }
            });
            ui.add_space(6.0);
            ui::property(ui, "Enabled", if enabled { "Yes" } else { "No" });
            ui::property(ui, "Join type", &trust_label(e.trust_type.as_deref()));
            ui::property(
                ui,
                "Operating system",
                &format!("{} {}", s(&e.operating_system), s(&e.operating_system_version)),
            );
            ui::property(
                ui,
                "Registered (UTC)",
                &short_time(e.registration_date_time.as_deref()),
            );
            ui::property(
                ui,
                "Last sign-in (UTC)",
                &short_time(e.approximate_last_sign_in_date_time.as_deref()),
            );
            ui::property(ui, "Device ID", &s(&e.device_id));
            ui::property(ui, "Object ID", &e.id);
        }
    });
    if let Some(action) = chosen {
        perform(app, ctx, &row, action);
    }
}

/// Entra's join types, in the words the admin centre uses.
fn trust_label(trust: Option<&str>) -> String {
    match trust {
        Some("AzureAd") => "Microsoft Entra joined".into(),
        Some("ServerAd") => "Microsoft Entra hybrid joined".into(),
        Some("Workplace") => "Microsoft Entra registered".into(),
        Some(other) => other.into(),
        None => String::new(),
    }
}

fn start(app: &mut App, ctx: &egui::Context, pending: Pending) {
    match pending {
        Pending::Intune { id, name, action } => {
            run(app, ctx, &format!("{}…", action.label()), move |g| {
                g.intune_action(&id, action)?;
                let change = if action == IntuneAction::Delete {
                    Change::IntuneRemoved(id)
                } else {
                    Change::Nothing
                };
                let message = match action {
                    IntuneAction::Delete => format!("{name} deleted from Intune."),
                    _ => format!("{} sent to {name}.", action.label()),
                };
                Ok((message, change))
            });
        }
        Pending::DeleteEntra { id, name } => run(app, ctx, "Deleting device…", move |g| {
            g.delete_device(&id)?;
            Ok((format!("{name} deleted from Entra ID."), Change::EntraRemoved(id)))
        }),
    }
}

pub fn modals(app: &mut App, ctx: &egui::Context) {
    let Some(pending) = &app.devices.confirm else {
        return;
    };
    let (title, body, confirm) = match pending {
        Pending::Intune { name, action, .. } => (
            format!("{}?", action.label()),
            format!("{name}: {}", action.explanation()),
            action.label().to_owned(),
        ),
        Pending::DeleteEntra { name, .. } => (
            "Delete device?".to_owned(),
            format!(
                "{name} will be removed from Entra ID. It will no longer be able to authenticate, and this cannot be undone."
            ),
            "Delete".to_owned(),
        ),
    };
    let answer = ui::confirm_modal(ctx, egui::Id::new("device-confirm"), &title, &body, &confirm);
    if answer == ui::Confirmation::Waiting {
        return;
    }
    let Some(pending) = app.devices.confirm.take() else { return };
    if answer == ui::Confirmation::Confirmed {
        start(app, ctx, pending);
    }
}
