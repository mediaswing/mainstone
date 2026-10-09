//! The Apps pane: every app connected to the tenant, what it may do, and
//! what about it is worth a second look. It only reads; nothing here changes
//! the tenant.

use std::collections::HashSet;

use egui::{RichText, Ui};
use egui_extras::Column;

use crate::app::{App, Tab};
use crate::csvio;
use crate::graph::apps::{AppDetails, ConnectedApp, Flag, Inventory, Kind, Permission, Privilege};
use crate::graph::models::short_time;
use crate::graph::Result;
use crate::task::{Task, take_finished};
use crate::ui;
use crate::ui::shortcuts::{self, Command};

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum Filter {
    /// Microsoft's own apps number in the hundreds and are rarely what
    /// anyone is looking for, so they are left out unless asked for.
    #[default]
    NotMicrosoft,
    Attention,
    Privileged,
    ThirdParty,
    ThisTenant,
    Expiring,
    Unused,
    Microsoft,
    All,
}

impl Filter {
    const ALL: [Self; 9] = [
        Self::NotMicrosoft,
        Self::Attention,
        Self::Privileged,
        Self::ThirdParty,
        Self::ThisTenant,
        Self::Expiring,
        Self::Unused,
        Self::Microsoft,
        Self::All,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::NotMicrosoft => "Not Microsoft",
            Self::Attention => "Needs attention",
            Self::Privileged => "High privilege",
            Self::ThirdParty => "Third-party",
            Self::ThisTenant => "This tenant's",
            Self::Expiring => "Credentials expiring",
            Self::Unused => "Unused",
            Self::Microsoft => "Microsoft",
            Self::All => "All apps",
        }
    }

    fn keeps(self, app: &ConnectedApp) -> bool {
        let has = |flag| app.flags.contains(&flag);
        match self {
            Self::NotMicrosoft => app.kind != Kind::Microsoft,
            Self::Attention => !app.flags.is_empty(),
            Self::Privileged => has(Flag::Critical) || has(Flag::HighPrivilege),
            Self::ThirdParty => app.kind == Kind::ThirdParty,
            Self::ThisTenant => app.kind == Kind::ThisTenant,
            Self::Expiring => has(Flag::CredentialExpiring) || has(Flag::CredentialExpired),
            Self::Unused => has(Flag::Unused),
            Self::Microsoft => app.kind == Kind::Microsoft,
            Self::All => true,
        }
    }
}

#[derive(Default)]
pub struct State {
    inventory: Inventory,
    loaded: bool,
    load_error: Option<String>,
    load: Option<Task<Inventory>>,
    query: String,
    filter: Filter,
    /// The selected app's object ID.
    selected: Option<String>,
    details: Option<(String, Result<AppDetails>)>,
    /// Carries the app's ID whether it worked or not, as in the Groups pane.
    details_load: Option<Task<(String, Result<AppDetails>)>>,
}

impl State {
    pub fn activity(&self) -> Option<String> {
        self.load.as_ref().map(|t| t.label.clone())
    }

    fn shown(&self) -> Vec<usize> {
        let terms = ui::search_terms(&self.query);
        self.inventory
            .apps
            .iter()
            .enumerate()
            .filter(|(_, a)| self.filter.keeps(a))
            .filter(|(_, a)| {
                let flags: Vec<&str> = a.flags.iter().map(|f| f.label()).collect();
                ui::matches_search(
                    &terms,
                    &[a.name(), a.publisher(), &a.sp.app_id, a.kind.label(), &flags.join(" ")],
                )
            })
            .map(|(i, _)| i)
            .collect()
    }

    fn selected_app(&self) -> Option<&ConnectedApp> {
        let id = self.selected.as_deref()?;
        self.inventory.apps.iter().find(|a| a.sp.id == id)
    }
}

/// The keyboard shortcuts' commands; see [`shortcuts`]. Nothing can be
/// created or deleted here, so New and Delete mean nothing.
pub fn command(app: &mut App, ctx: &egui::Context, command: Command) -> bool {
    if app.graph.is_none() {
        return false;
    }
    match command {
        Command::Find => ui::request_find(ctx),
        Command::Refresh => refresh(app),
        Command::Deselect => return app.apps.selected.take().is_some(),
        Command::New | Command::Delete => return false,
    }
    true
}

fn refresh(app: &mut App) {
    if app.apps.load.is_none() {
        app.apps.loaded = false;
        app.apps.details = None;
    }
}

pub fn poll(app: &mut App) {
    if let Some(result) = take_finished(&mut app.apps.load) {
        app.apps.loaded = true;
        match result {
            Ok(inventory) => {
                app.apps.inventory = inventory;
                app.apps.load_error = None;
                if app.apps.selected_app().is_none() {
                    app.apps.selected = None;
                }
            }
            Err(err) => {
                app.apps.load_error = Some(err.clone());
                app.report_error(format!("Could not load apps: {err}"));
            }
        }
    }

    if let Some(result) = take_finished(&mut app.apps.details_load) {
        match result {
            Ok((id, details)) => {
                if let Ok(d) = &details {
                    for r in &d.new_resources {
                        app.apps.inventory.resources.insert(r.id.clone(), r.clone());
                    }
                }
                app.apps.details = Some((id, details));
            }
            // The thread died; see the Users pane.
            Err(err) => {
                if let Some(id) = app.apps.selected.clone() {
                    app.apps.details = Some((id, Err(err)));
                }
            }
        }
    }
}

fn load_details(app: &mut App, ctx: &egui::Context, selected: ConnectedApp) {
    let Some(graph) = app.graph.clone() else { return };
    let known: HashSet<String> = app.apps.inventory.resources.keys().cloned().collect();
    app.apps.details_load = Some(Task::spawn(ctx, "Loading app details…", move || {
        let details = graph.app_details(&selected, &known);
        Ok((selected.sp.id.clone(), details))
    }));
}

pub fn show(app: &mut App, ui: &mut Ui) {
    let ctx = ui.ctx().clone();
    if app.graph.is_none() {
        ui::pane_header(ui, "Apps", "");
        if ui::not_connected(ui) {
            app.tab = Tab::Connection;
        }
        return;
    }
    if !app.apps.loaded
        && app.apps.load.is_none()
        && let Some(graph) = app.graph.clone()
    {
        let tenant = app.session.as_ref().and_then(|s| s.tenant_guid.clone());
        app.apps.load = Some(Task::spawn(&ctx, "Loading apps…", move || {
            graph.list_apps(tenant.as_deref())
        }));
    }

    let shown = app.apps.shown();
    let subtitle = if !app.apps.loaded {
        "Loading…".to_owned()
    } else {
        let apps = &app.apps.inventory.apps;
        let attention = apps
            .iter()
            .filter(|a| a.kind != Kind::Microsoft && !a.flags.is_empty())
            .count();
        format!(
            "{} shown of {} apps, {attention} not Microsoft's need attention",
            shown.len(),
            apps.len()
        )
    };
    ui::pane_header(ui, "Apps", &subtitle);

    ui.horizontal(|ui| {
        if ui::tool_button(ui, app.apps.load.is_none(), "Refresh")
            .on_hover_text(ui::shortcut_hint(ui, "Read the apps again", &shortcuts::REFRESH))
            .clicked()
        {
            refresh(app);
        }
        let export_label = format!("Export {} shown…", shown.len());
        if ui::tool_button(ui, !shown.is_empty(), &export_label).clicked() {
            export_csv(app, &shown);
        }
    });
    ui.add_space(4.0);
    ui.horizontal_wrapped(|ui| {
        for filter in Filter::ALL {
            ui.selectable_value(&mut app.apps.filter, filter, filter.label());
        }
    });
    ui.add_space(4.0);
    ui::search_box(ui, &mut app.apps.query, "Search by name, publisher, app ID, kind or flag");
    ui.add_space(6.0);
    if let Some(err) = &app.apps.load_error {
        ui::error_text(ui, err);
    }
    for note in &app.apps.inventory.notes {
        ui.label(RichText::new(note).size(13.0).color(ui::warn_colour(ui)));
    }

    if app.apps.selected_app().is_some() {
        egui::Panel::right("app-details")
            .resizable(true)
            .default_size(380.0)
            .min_size(300.0)
            .show(ui, |ui| details(app, ui, &ctx));
    }

    let selected_index = app
        .apps
        .selected
        .as_deref()
        .and_then(|id| shown.iter().position(|&i| app.apps.inventory.apps[i].sp.id == id));
    let inventory = &app.apps.inventory;
    let clicks = ui::select_table(
        ui,
        "apps",
        &[
            ("Name", Column::initial(220.0).at_least(80.0)),
            ("Publisher", Column::initial(170.0).at_least(60.0)),
            ("Kind", Column::initial(110.0).at_least(60.0)),
            ("Permissions", Column::initial(130.0).at_least(60.0)),
            ("Last sign-in", Column::initial(100.0).at_least(60.0)),
            ("Flags", Column::remainder().at_least(80.0)),
        ],
        shown.len(),
        selected_index,
        |row, column, ui| {
            let a = &inventory.apps[shown[row]];
            match column {
                0 if a.is_self => ui::cell_text(ui, &format!("{} (this app)", a.name())),
                0 => ui::cell_text(ui, a.name()),
                1 => match a.verified_publisher() {
                    Some(p) => ui::cell_text(ui, &format!("✓ {p}")),
                    None => ui::cell_text(ui, a.publisher()),
                },
                2 => ui::cell_text(ui, a.kind.label()),
                3 => ui::cell_text(ui, &permission_summary(a, inventory)),
                4 => ui::cell_text(ui, &last_sign_in(a, inventory)),
                _ => flags_cell(ui, a),
            }
        },
        Some(&mut |row, ui| {
            let a = &inventory.apps[shown[row]];
            ui::copy_item(ui, "name", a.sp.display_name.as_deref().unwrap_or(""));
            ui::copy_item(ui, "app ID", &a.sp.app_id);
            ui::copy_item(ui, "object ID", &a.sp.id);
        }),
    );
    if let Some(row) = clicks.clicked {
        let id = app.apps.inventory.apps[shown[row]].sp.id.clone();
        app.apps.selected = if app.apps.selected.as_deref() == Some(&id) {
            None
        } else {
            Some(id)
        };
    }
    if let Some(row) = clicks.right_clicked {
        app.apps.selected = Some(app.apps.inventory.apps[shown[row]].sp.id.clone());
    }
}

/// `3 app · 5 delegated`, or only the first when the grants are not known.
fn permission_summary(a: &ConnectedApp, inventory: &Inventory) -> String {
    let application = a.app_roles.len();
    if inventory.grants_known {
        format!("{application} app · {} delegated", a.delegated_scopes().len())
    } else {
        format!("{application} app")
    }
}

/// The day of the last sign-in, `Never`, or nothing when it is not known.
fn last_sign_in(a: &ConnectedApp, inventory: &Inventory) -> String {
    if !inventory.activity_known {
        return String::new();
    }
    match (a.last_sign_in_time(), &a.last_sign_in) {
        (Some(t), _) => t.format("%Y-%m-%d").to_string(),
        (None, Some(raw)) => raw.clone(),
        (None, None) => "Never".to_owned(),
    }
}

fn flag_colour(ui: &Ui, flag: Flag) -> egui::Color32 {
    if flag.severe() {
        ui::bad_colour(ui)
    } else {
        ui::warn_colour(ui)
    }
}

fn flags_cell(ui: &mut Ui, a: &ConnectedApp) {
    let Some(worst) = a.worst() else { return };
    let text: Vec<&str> = a.flags.iter().map(|f| f.label()).collect();
    let colour = flag_colour(ui, worst);
    ui.add(egui::Label::new(RichText::new(text.join(", ")).color(colour)).truncate());
}

fn privilege_colour(ui: &Ui, privilege: Privilege) -> Option<egui::Color32> {
    match privilege {
        Privilege::Critical => Some(ui::bad_colour(ui)),
        Privilege::High => Some(ui::warn_colour(ui)),
        Privilege::Normal => None,
    }
}

/// Permissions grouped under the API they are on.
fn permission_list(ui: &mut Ui, mut permissions: Vec<Permission>) {
    permissions.sort_by(|a, b| (&a.resource, &a.value).cmp(&(&b.resource, &b.value)));
    let mut resource = None;
    for p in &permissions {
        if resource != Some(&p.resource) {
            resource = Some(&p.resource);
            ui.add_space(2.0);
            let name = if p.resource.is_empty() { "Unknown API" } else { &p.resource };
            ui.label(RichText::new(name).size(12.0).weak());
        }
        let mut text = RichText::new(&p.value).size(14.0);
        if let Some(colour) = privilege_colour(ui, p.privilege) {
            text = text.color(colour);
        }
        let response = ui.add(egui::Label::new(text).selectable(true));
        if !p.description.is_empty() {
            response.on_hover_text(&p.description);
        }
    }
}

fn section(ui: &mut Ui, title: &str) {
    ui.add_space(10.0);
    ui.label(RichText::new(title).strong());
}

fn quiet(ui: &mut Ui, text: &str) {
    ui.label(RichText::new(text).size(12.0).weak());
}

fn details(app: &mut App, ui: &mut Ui, ctx: &egui::Context) {
    let Some(selected) = app.apps.selected_app().cloned() else {
        return;
    };
    let have = app.apps.details.as_ref().map(|(id, _)| id.as_str());
    if have != Some(selected.sp.id.as_str()) && app.apps.details_load.is_none() {
        load_details(app, ctx, selected.clone());
    }
    let inventory = &app.apps.inventory;
    let loaded = match &app.apps.details {
        Some((id, result)) if *id == selected.sp.id => Some(result),
        _ => None,
    };
    let mut show_user = None;
    let mut show_group = None;

    egui::ScrollArea::vertical().show(ui, |ui| {
        ui.add_space(8.0);
        ui.heading(selected.name());
        if selected.is_self {
            quiet(ui, "This is the app registration Mainstone signs in as.");
        }
        ui.add_space(6.0);

        for flag in &selected.flags {
            ui.label(RichText::new(flag.label()).strong().color(flag_colour(ui, *flag)));
            quiet(ui, flag.explanation());
            ui.add_space(2.0);
        }
        if !selected.flags.is_empty() {
            ui.add_space(6.0);
        }

        let s = |v: &Option<String>| v.clone().unwrap_or_default();
        let yes_no = |b: bool| if b { "Yes" } else { "No" }.to_owned();
        ui::property(ui, "Kind", selected.kind.label());
        ui::property(ui, "Publisher", selected.publisher());
        ui::property(
            ui,
            "Verified publisher",
            &selected.verified_publisher().map_or_else(|| "No".to_owned(), |p| format!("Yes, {p}")),
        );
        ui::property(ui, "Sign-in enabled", &yes_no(selected.enabled()));
        ui::property(
            ui,
            "Users must be assigned",
            &yes_no(selected.sp.app_role_assignment_required.unwrap_or(false)),
        );
        if inventory.activity_known {
            ui::property(ui, "Last sign-in", &last_sign_in(&selected, inventory));
        }
        if let Some(r) = &selected.registration {
            ui::property(ui, "Registered (UTC)", &short_time(r.created_date_time.as_deref()));
            ui::property(ui, "Who can sign in", &audience(r.sign_in_audience.as_deref()));
        }
        ui::property(ui, "Homepage", &s(&selected.sp.homepage));
        if !selected.sp.reply_urls.is_empty() {
            ui::property(ui, "Reply URLs", &selected.sp.reply_urls.join("\n"));
        }
        ui::property(ui, "App ID", &selected.sp.app_id);
        ui::property(ui, "Object ID", &selected.sp.id);

        let credentials = selected.credentials();
        if !credentials.is_empty() {
            section(ui, "Secrets and certificates");
            let now = chrono::Utc::now();
            for c in &credentials {
                let ends = c
                    .end
                    .map(|t| t.format("%Y-%m-%d").to_string())
                    .or(c.raw_end.map(str::to_owned))
                    .unwrap_or_default();
                let name = if c.name.is_empty() { c.kind.to_owned() } else { format!("{} “{}”", c.kind, c.name) };
                let mut text = RichText::new(format!("{name}, ends {ends}"));
                if let Some(end) = c.end {
                    if end < now {
                        text = text.color(ui::bad_colour(ui));
                    } else if end < now + chrono::Duration::days(30) {
                        text = text.color(ui::warn_colour(ui));
                    }
                }
                ui.label(text);
                quiet(ui, &format!("On the {}", c.on.to_lowercase()));
            }
        }

        section(ui, "Application permissions");
        quiet(ui, "What the app can do on its own, with nobody signed in.");
        match loaded {
            Some(Ok(d)) => {
                if d.app_roles.is_empty() {
                    quiet(ui, "None.");
                }
                permission_list(ui, d.app_roles.iter().map(|a| inventory.app_permission(a)).collect());
            }
            Some(Err(err)) => ui::error_text(ui, err),
            None => ui::busy(ui, "Loading…"),
        }

        section(ui, "Delegated permissions");
        quiet(ui, "What the app can do as a signed-in user, and only what that user can.");
        if !inventory.grants_known {
            quiet(ui, "Not known: reading them needs Directory.Read.All.");
        } else if selected.grants.is_empty() {
            quiet(ui, "None.");
        } else {
            let admin = inventory.delegated_permissions(&selected, true);
            if !admin.is_empty() {
                ui.add_space(2.0);
                ui.label("Consented by an administrator for everyone");
                permission_list(ui, admin);
            }
            let user = inventory.delegated_permissions(&selected, false);
            if !user.is_empty() {
                ui.add_space(6.0);
                let count = selected.consenting_users().len();
                ui.label(format!(
                    "Consented by {count} {} for themselves",
                    if count == 1 { "user" } else { "users" }
                ));
                permission_list(ui, user);
                match loaded {
                    Some(Ok(AppDetails { consenters: Some(people), .. })) => {
                        ui.add_space(2.0);
                        for person in people {
                            let row = ui::menu_row(ui, |ui| {
                                ui.label(person.name());
                                quiet(ui, person.detail());
                            });
                            row.response.context_menu(|ui| {
                                if ui.button("Show in Users").clicked() {
                                    show_user = Some(person.id.clone());
                                }
                                ui::copy_item(ui, "sign-in name", person.user_principal_name.as_deref().unwrap_or(""));
                                ui::copy_item(ui, "object ID", &person.id);
                            });
                        }
                    }
                    Some(Ok(_)) => quiet(ui, "Who they are could not be looked up."),
                    _ => {}
                }
            }
        }

        section(ui, "Assigned users and groups");
        if !selected.sp.app_role_assignment_required.unwrap_or(false) {
            quiet(ui, "Assignment is not required, so anyone in the tenant can sign in to it.");
        }
        match loaded {
            Some(Ok(d)) => {
                if d.assigned.is_empty() {
                    quiet(ui, "None.");
                }
                for a in &d.assigned {
                    let kind = a.principal_type.as_deref().unwrap_or("");
                    let row = ui::menu_row(ui, |ui| {
                        ui.label(a.principal_display_name.as_deref().unwrap_or("(no name)"));
                        quiet(ui, &kind.to_lowercase());
                    });
                    row.response.context_menu(|ui| {
                        match kind {
                            "User" if ui.button("Show in Users").clicked() => {
                                show_user = Some(a.principal_id.clone());
                            }
                            "Group" if ui.button("Show in Groups").clicked() => {
                                show_group = Some(a.principal_id.clone());
                            }
                            _ => {}
                        }
                        ui::copy_item(ui, "name", a.principal_display_name.as_deref().unwrap_or(""));
                        ui::copy_item(ui, "object ID", &a.principal_id);
                    });
                }
            }
            Some(Err(_)) => {}
            None => ui::busy(ui, "Loading…"),
        }

        section(ui, "Owners");
        match loaded {
            Some(Ok(d)) => {
                if d.owners.is_empty() {
                    quiet(ui, "None.");
                }
                for o in &d.owners {
                    let row = ui::menu_row(ui, |ui| {
                        ui.label(o.name());
                        quiet(ui, o.detail());
                    });
                    row.response.context_menu(|ui| {
                        if o.kind() == "user" && ui.button("Show in Users").clicked() {
                            show_user = Some(o.id.clone());
                        }
                        ui::copy_item(ui, "name", o.display_name.as_deref().unwrap_or(""));
                        ui::copy_item(ui, "object ID", &o.id);
                    });
                }
            }
            Some(Err(_)) => {}
            None => ui::busy(ui, "Loading…"),
        }
        ui.add_space(10.0);
    });

    if let Some(id) = show_user {
        ui::users::show_user(app, &id);
    } else if let Some(id) = show_group {
        ui::groups::show_group(app, &id);
    }
}

/// An app registration's sign-in audience, in words.
fn audience(value: Option<&str>) -> String {
    match value {
        Some("AzureADMyOrg") => "This tenant only".to_owned(),
        Some("AzureADMultipleOrgs") => "Any Entra tenant".to_owned(),
        Some("AzureADandPersonalMicrosoftAccount") => {
            "Any Entra tenant and personal Microsoft accounts".to_owned()
        }
        Some("PersonalMicrosoftAccount") => "Personal Microsoft accounts only".to_owned(),
        Some(other) => other.to_owned(),
        None => String::new(),
    }
}

fn export_csv(app: &mut App, shown: &[usize]) {
    let Some(path) = rfd::FileDialog::new()
        .set_file_name("apps.csv")
        .add_filter("CSV", &["csv"])
        .save_file()
    else {
        return;
    };
    let inventory = &app.apps.inventory;
    let apps: Vec<&ConnectedApp> = shown.iter().map(|&i| &inventory.apps[i]).collect();
    let count = apps.len();
    match csvio::write_apps(&path, &apps, inventory) {
        Ok(()) => app.report_ok(format!(
            "Exported {count} apps to {}.",
            crate::config::tilde(&path)
        )),
        Err(err) => app.report_error(format!("Could not export: {err}")),
    }
}
