//! The Groups pane: every group, and the members of the one selected.

use egui::{RichText, Ui};
use egui_extras::Column;

use crate::app::{App, Tab};
use crate::graph::groups::{NewGroup, NewGroupKind};
use crate::graph::models::{DirectoryObject, Group};
use crate::graph::{Graph, Result};
use crate::task::{Task, take_finished};
use crate::ui;
use crate::ui::shortcuts::{self, Command};

enum Change {
    Created(Group),
    Deleted(String),
    /// The members of this group changed; read them again.
    Members(String),
}

#[derive(Default)]
pub struct State {
    groups: Vec<Group>,
    loaded: bool,
    load_error: Option<String>,
    load: Option<Task<Vec<Group>>>,
    query: String,
    selected: Option<String>,
    members: Option<(String, Result<Vec<DirectoryObject>>)>,
    /// Carries the group's ID whether it worked or not, as in the Users
    /// pane.
    members_load: Option<Task<(String, Result<Vec<DirectoryObject>>)>>,
    action: Option<Task<(String, Change)>>,
    /// The sign-in name typed into the "add member" box.
    new_member: String,
    form: Option<(NewGroup, Option<String>)>,
    confirm_delete: Option<(String, String)>,
    confirm_remove: Option<(String, String, String)>,
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
        self.groups
            .iter()
            .enumerate()
            .filter(|(_, g)| {
                ui::matches_search(
                    &terms,
                    &[
                        g.name(),
                        g.mail.as_deref().unwrap_or(""),
                        g.description.as_deref().unwrap_or(""),
                        g.kind(),
                    ],
                )
            })
            .map(|(i, _)| i)
            .collect()
    }

    fn selected_group(&self) -> Option<&Group> {
        let id = self.selected.as_deref()?;
        self.groups.iter().find(|g| g.id == id)
    }
}

/// The keyboard shortcuts' commands; see [`shortcuts`].
pub fn command(app: &mut App, ctx: &egui::Context, command: Command) -> bool {
    if app.graph.is_none() {
        return false;
    }
    match command {
        Command::Find => ui::request_find(ctx),
        Command::Refresh => refresh(app),
        Command::New => new_group(app),
        Command::Delete => {
            let Some(group) = app.groups.selected_group() else { return false };
            let pending = (group.id.clone(), group.name().to_owned());
            ask_delete(app, pending);
        }
        Command::Deselect => {
            app.groups.new_member.clear();
            return app.groups.selected.take().is_some();
        }
    }
    true
}

/// Show one group on this tab, from a user's memberships.
pub fn show_group(app: &mut App, id: &str) {
    app.groups.selected = Some(id.to_owned());
    app.groups.query.clear();
    app.groups.new_member.clear();
    app.tab = Tab::Groups;
}

fn refresh(app: &mut App) {
    if app.groups.load.is_none() {
        app.groups.loaded = false;
        app.groups.members = None;
    }
}

fn new_group(app: &mut App) {
    if app.groups.action.is_none() {
        app.groups.form = Some((NewGroup::default(), None));
    }
}

fn ask_delete(app: &mut App, group: (String, String)) {
    if app.groups.action.is_none() {
        app.groups.confirm_delete = Some(group);
    }
}

fn run(
    app: &mut App,
    ctx: &egui::Context,
    label: &str,
    work: impl FnOnce(&Graph) -> Result<(String, Change)> + Send + 'static,
) {
    let Some(graph) = app.graph.clone() else { return };
    if app.groups.action.is_some() {
        return;
    }
    app.groups.action = Some(Task::spawn(ctx, label, move || work(&graph)));
}

fn load_members(app: &mut App, ctx: &egui::Context, group_id: String) {
    let Some(graph) = app.graph.clone() else { return };
    app.groups.members_load = Some(Task::spawn(ctx, "Loading members…", move || {
        let members = graph.group_members(&group_id);
        Ok((group_id, members))
    }));
}

pub fn poll(app: &mut App) {
    if let Some(result) = take_finished(&mut app.groups.load) {
        app.groups.loaded = true;
        match result {
            Ok(groups) => {
                app.groups.groups = groups;
                app.groups.load_error = None;
            }
            Err(err) => {
                app.groups.load_error = Some(err.clone());
                app.report_error(format!("Could not load groups: {err}"));
            }
        }
    }

    if let Some(result) = take_finished(&mut app.groups.members_load) {
        match result {
            Ok((id, members)) => app.groups.members = Some((id, members)),
            // The thread died; see the Users pane.
            Err(err) => {
                if let Some(id) = app.groups.selected.clone() {
                    app.groups.members = Some((id, Err(err)));
                }
            }
        }
    }

    if let Some(result) = take_finished(&mut app.groups.action) {
        match result {
            Ok((message, change)) => {
                match change {
                    Change::Created(group) => {
                        app.groups.selected = Some(group.id.clone());
                        app.groups.groups.push(group);
                        app.groups.groups.sort_by_key(|g| g.name().to_lowercase());
                    }
                    Change::Deleted(id) => {
                        app.groups.groups.retain(|g| g.id != id);
                        if app.groups.selected.as_deref() == Some(id.as_str()) {
                            app.groups.selected = None;
                        }
                    }
                    // Dropping what is held makes the details panel read the
                    // members again.
                    Change::Members(gid) => {
                        if app.groups.members.as_ref().is_some_and(|(id, _)| *id == gid) {
                            app.groups.members = None;
                        }
                        app.groups.new_member.clear();
                    }
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
        ui::pane_header(ui, "Groups", "");
        if ui::not_connected(ui) {
            app.tab = Tab::Connection;
        }
        return;
    }
    if !app.groups.loaded
        && app.groups.load.is_none()
        && let Some(graph) = app.graph.clone()
    {
        app.groups.load = Some(Task::spawn(&ctx, "Loading groups…", move || graph.list_groups()));
    }

    let shown = app.groups.shown();
    let subtitle = if !app.groups.loaded {
        "Loading…".to_owned()
    } else if app.groups.query.trim().is_empty() {
        format!("{} groups", app.groups.groups.len())
    } else {
        format!("{} of {} groups match", shown.len(), app.groups.groups.len())
    };
    ui::pane_header(ui, "Groups", &subtitle);

    ui.horizontal(|ui| {
        if ui::tool_button(ui, app.groups.load.is_none(), "Refresh")
            .on_hover_text(ui::shortcut_hint(ui, "Read the groups again", &shortcuts::REFRESH))
            .clicked()
        {
            refresh(app);
        }
        if ui::tool_button(ui, app.groups.action.is_none(), "+ New group")
            .on_hover_text(ui::shortcut_hint(ui, "Create a group", &shortcuts::NEW))
            .clicked()
        {
            new_group(app);
        }
    });
    ui.add_space(4.0);
    ui::search_box(ui, &mut app.groups.query, "Search by name, mail, description or type");
    ui.add_space(6.0);
    if let Some(err) = &app.groups.load_error {
        ui::error_text(ui, err);
    }

    if app.groups.selected_group().is_some() {
        egui::Panel::right("group-details")
            .resizable(true)
            .default_size(340.0)
            .min_size(280.0)
            .show(ui, |ui| details(app, ui, &ctx));
    }

    let selected_index = app
        .groups
        .selected
        .as_deref()
        .and_then(|id| shown.iter().position(|&i| app.groups.groups[i].id == id));
    let groups = &app.groups.groups;
    let idle = app.groups.action.is_none();
    let mut delete = None;
    let clicks = ui::select_table(
        ui,
        "groups",
        &[
            ("Name", Column::initial(240.0).at_least(80.0)),
            ("Type", Column::initial(160.0).at_least(60.0)),
            ("Membership", Column::initial(100.0).at_least(60.0)),
            ("Mail", Column::remainder().at_least(80.0)),
        ],
        shown.len(),
        selected_index,
        |row, column, ui| {
            let g = &groups[shown[row]];
            match column {
                0 => ui::cell_text(ui, g.name()),
                1 => ui::cell_text(ui, g.kind()),
                2 => ui::cell_text(ui, if g.is_dynamic() { "Dynamic" } else { "Assigned" }),
                _ => ui::cell_text(ui, g.mail.as_deref().unwrap_or("")),
            }
        },
        Some(&mut |row, ui| {
            let g = &groups[shown[row]];
            ui::copy_item(ui, "name", g.display_name.as_deref().unwrap_or(""));
            ui::copy_item(ui, "mail address", g.mail.as_deref().unwrap_or(""));
            ui::copy_item(ui, "object ID", &g.id);
            ui.separator();
            if ui::menu_item(ui, idle, "Delete group", &shortcuts::DELETE) {
                delete = Some((g.id.clone(), g.name().to_owned()));
            }
        }),
    );
    if let Some(row) = clicks.clicked {
        let id = app.groups.groups[shown[row]].id.clone();
        app.groups.selected = if app.groups.selected.as_deref() == Some(&id) {
            None
        } else {
            Some(id)
        };
        app.groups.new_member.clear();
    }
    if let Some(row) = clicks.right_clicked {
        let id = app.groups.groups[shown[row]].id.clone();
        if app.groups.selected.as_deref() != Some(&id) {
            app.groups.selected = Some(id);
            app.groups.new_member.clear();
        }
    }
    if let Some(group) = delete {
        ask_delete(app, group);
    }
}

fn details(app: &mut App, ui: &mut Ui, ctx: &egui::Context) {
    let Some(group) = app.groups.selected_group().cloned() else {
        return;
    };
    let have = app.groups.members.as_ref().map(|(id, _)| id.as_str());
    if have != Some(group.id.as_str()) && app.groups.members_load.is_none() {
        load_members(app, ctx, group.id.clone());
    }
    let idle = app.groups.action.is_none();

    egui::ScrollArea::vertical().show(ui, |ui| {
        ui.add_space(8.0);
        ui.heading(group.name());
        ui.add_space(6.0);
        if ui::tool_button(ui, idle, "Delete group")
            .on_hover_text(ui::shortcut_hint(ui, "Delete this group", &shortcuts::DELETE))
            .clicked()
        {
            ask_delete(app, (group.id.clone(), group.name().to_owned()));
        }
        ui.add_space(8.0);

        let s = |v: &Option<String>| v.clone().unwrap_or_default();
        ui::property(ui, "Type", group.kind());
        ui::property(ui, "Description", &s(&group.description));
        ui::property(ui, "Mail", &s(&group.mail));
        if let Some(rule) = &group.membership_rule {
            ui::property(ui, "Membership rule", rule);
        }
        ui::property(ui, "Object ID", &group.id);

        ui.add_space(8.0);
        ui.label(RichText::new("Members").strong());
        if group.is_dynamic() {
            ui.label(
                RichText::new("Members come from the rule above and cannot be changed by hand.")
                    .size(12.0)
                    .weak(),
            );
        } else {
            ui.horizontal(|ui| {
                let field = egui::TextEdit::singleline(&mut app.groups.new_member)
                    .hint_text("user@contoso.com")
                    .desired_width(ui.available_width() - 70.0);
                let response = ui::named(ui.add(field), "Sign-in name of the user to add");
                let enter = ui::submitted(&response);
                let can_add = idle && !app.groups.new_member.trim().is_empty();
                let clicked = ui::tool_button(ui, can_add, "Add").clicked();
                if clicked || (enter && can_add) {
                    let upn = app.groups.new_member.trim().to_owned();
                    let gid = group.id.clone();
                    let gname = group.name().to_owned();
                    run(app, ctx, "Adding member…", move |g| {
                        let uid = g.user_id_for(&upn)?;
                        g.add_member(&gid, &uid)?;
                        Ok((format!("Added {upn} to {gname}."), Change::Members(gid)))
                    });
                }
            });
        }
        ui.add_space(4.0);

        let mut show_user = None;
        match &app.groups.members {
            Some((id, Ok(members))) if *id == group.id => {
                if members.is_empty() {
                    ui.label(RichText::new("No members.").weak());
                }
                ui.label(RichText::new(format!("{} members", members.len())).size(12.0).weak());
                for m in members {
                    let row = ui::menu_row(ui, |ui| {
                        let remove = format!("Remove {} from the group", m.name());
                        if !group.is_dynamic()
                            && ui::named(
                                ui.add_enabled(idle, egui::Button::new("✕").small()),
                                &remove,
                            )
                            .on_hover_text(&remove)
                            .clicked()
                        {
                            app.groups.confirm_remove = Some((
                                group.id.clone(),
                                m.id.clone(),
                                format!("{} from {}", m.name(), group.name()),
                            ));
                        }
                        ui.vertical(|ui| {
                            ui.label(m.name());
                            let detail = if m.detail().is_empty() {
                                m.kind().to_owned()
                            } else {
                                format!("{} · {}", m.kind(), m.detail())
                            };
                            ui.label(RichText::new(detail).size(12.0).weak());
                        });
                    });
                    row.response.context_menu(|ui| {
                        if m.kind() == "user" && ui.button("Show in Users").clicked() {
                            show_user = Some(m.id.clone());
                        }
                        ui::copy_item(ui, "name", m.display_name.as_deref().unwrap_or(""));
                        ui::copy_item(ui, "sign-in name", m.user_principal_name.as_deref().unwrap_or(""));
                        ui::copy_item(ui, "mail address", m.mail.as_deref().unwrap_or(""));
                        ui::copy_item(ui, "object ID", &m.id);
                        if !group.is_dynamic() {
                            ui.separator();
                            if ui.add_enabled(idle, egui::Button::new("Remove from group")).clicked() {
                                app.groups.confirm_remove = Some((
                                    group.id.clone(),
                                    m.id.clone(),
                                    format!("{} from {}", m.name(), group.name()),
                                ));
                            }
                        }
                    });
                }
            }
            Some((id, Err(err))) if *id == group.id => ui::error_text(ui, err),
            _ => ui::busy(ui, "Loading…"),
        }
        if let Some(id) = show_user {
            ui::users::show_user(app, &id);
        }
    });
}

pub fn modals(app: &mut App, ctx: &egui::Context) {
    form_modal(app, ctx);

    if let Some((id, name)) = app.groups.confirm_delete.clone() {
        let answer = ui::confirm_modal(
            ctx,
            egui::Id::new("delete-group"),
            "Delete group?",
            &format!("{name} will be deleted. A Microsoft 365 group can be restored for 30 days; a security group cannot."),
            "Delete",
        );
        if answer != ui::Confirmation::Waiting {
            app.groups.confirm_delete = None;
        }
        if answer == ui::Confirmation::Confirmed {
            run(app, ctx, "Deleting group…", move |g| {
                g.delete_group(&id)?;
                Ok((format!("Deleted {name}."), Change::Deleted(id)))
            });
        }
    }

    if let Some((gid, mid, what)) = app.groups.confirm_remove.clone() {
        let answer = ui::confirm_modal(
            ctx,
            egui::Id::new("remove-member"),
            "Remove member?",
            &format!("Remove {what}?"),
            "Remove",
        );
        if answer != ui::Confirmation::Waiting {
            app.groups.confirm_remove = None;
        }
        if answer == ui::Confirmation::Confirmed {
            run(app, ctx, "Removing member…", move |g| {
                g.remove_member(&gid, &mid)?;
                Ok((format!("Removed {what}."), Change::Members(gid)))
            });
        }
    }
}

fn form_modal(app: &mut App, ctx: &egui::Context) {
    let Some((group, error)) = app.groups.form.as_mut() else {
        return;
    };
    let mut answer = None;
    let modal = egui::Modal::new(egui::Id::new("group-form")).show(ctx, |ui| {
        ui.set_width(420.0);
        ui.heading("New group");
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            for kind in [NewGroupKind::Security, NewGroupKind::Microsoft365] {
                ui.selectable_value(&mut group.kind, kind, kind.label());
            }
        });
        ui.add_space(6.0);
        let first = ui::labelled_field(ui, "Name", &mut group.display_name, "");
        ui::focus_on_open(ui, egui::Id::new("group-form"), &first);
        let mut enter = ui::submitted(&first);
        enter |= ui::submitted(&ui::labelled_field(
            ui,
            "Mail nickname (optional)",
            &mut group.mail_nickname,
            "from the name",
        ));
        enter |= ui::submitted(&ui::labelled_field(ui, "Description (optional)", &mut group.description, ""));
        if let Some(err) = error.as_ref() {
            ui::error_text(ui, err);
        }
        let can_create = !group.display_name.trim().is_empty();
        answer = ui::form_buttons(ui, "Create", can_create);
        if enter && can_create && answer.is_none() {
            answer = Some(true);
        }
    });
    if modal.should_close() && answer.is_none() {
        answer = Some(false);
    }
    match answer {
        Some(true) => {
            let Some((group, _)) = app.groups.form.take() else { return };
            run(app, ctx, "Creating group…", move |g| {
                let created = g.create_group(&group)?;
                Ok((format!("Created {}.", created.name()), Change::Created(created)))
            });
        }
        Some(false) => app.groups.form = None,
        None => {}
    }
}
