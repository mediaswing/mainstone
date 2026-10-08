//! The Licensing pane: every subscription the tenant has, how many of each
//! are in use, and who holds the one selected, with assigning by sign-in
//! name and removing.
//!
//! The list of subscriptions is shared with the Users pane, whose Licences
//! dialog offers the same products for one user.

use egui::{RichText, Ui};
use egui_extras::Column;

use crate::app::{App, Tab};
use crate::graph::licensing::{Holders, Sku};
use crate::graph::{Graph, Result};
use crate::task::{Task, take_finished};
use crate::ui;
use crate::ui::shortcuts::{self, Command};

#[derive(Default)]
pub struct State {
    skus: Vec<Sku>,
    loaded: bool,
    load_error: Option<String>,
    load: Option<Task<Vec<Sku>>>,
    query: String,
    selected: Option<String>,
    /// Who holds a licence, by SKU ID, carried with the answer as in the
    /// other panes so it is filed under the right one.
    holders: Option<(String, Result<Holders>)>,
    holders_load: Option<Task<(String, Result<Holders>)>>,
    action: Option<Task<String>>,
    /// The sign-in name typed into the "assign to" box.
    assign_to: String,
    /// User ID, sign-in name, SKU ID and product name, awaiting "are you
    /// sure".
    confirm_remove: Option<(String, String, String, String)>,
}

impl State {
    pub fn activity(&self) -> Option<String> {
        self.load
            .as_ref()
            .map(|t| t.label.clone())
            .or_else(|| self.action.as_ref().map(|t| t.label.clone()))
    }

    /// The subscriptions, once read.
    pub fn skus(&self) -> Option<&[Sku]> {
        self.loaded.then_some(self.skus.as_slice())
    }

    /// Counts and holders change when a licence is assigned or removed
    /// anywhere in the app, so both are read again when next shown.
    pub fn invalidate(&mut self) {
        self.loaded = false;
        self.holders = None;
    }

    fn shown(&self) -> Vec<usize> {
        let terms = ui::search_terms(&self.query);
        self.skus
            .iter()
            .enumerate()
            .filter(|(_, s)| ui::matches_search(&terms, &[s.name(), &s.sku_part_number]))
            .map(|(i, _)| i)
            .collect()
    }

    fn selected_sku(&self) -> Option<&Sku> {
        let id = self.selected.as_deref()?;
        self.skus.iter().find(|s| s.sku_id == id)
    }
}

/// Read the subscriptions if they have not been, for this pane or for the
/// Users pane's dialog.
pub fn ensure_loaded(app: &mut App, ctx: &egui::Context) {
    if app.licensing.loaded || app.licensing.load.is_some() {
        return;
    }
    let Some(graph) = app.graph.clone() else { return };
    app.licensing.load = Some(Task::spawn(ctx, "Loading subscriptions…", move || {
        graph.list_skus()
    }));
}

/// The keyboard shortcuts' commands; see [`shortcuts`].
pub fn command(app: &mut App, ctx: &egui::Context, command: Command) -> bool {
    if app.graph.is_none() {
        return false;
    }
    match command {
        Command::Find => ui::request_find(ctx),
        Command::Refresh => {
            if app.licensing.load.is_none() {
                app.licensing.invalidate();
            }
        }
        Command::Deselect => {
            app.licensing.assign_to.clear();
            return app.licensing.selected.take().is_some();
        }
        Command::New | Command::Delete => return false,
    }
    true
}

fn run(
    app: &mut App,
    ctx: &egui::Context,
    label: &str,
    work: impl FnOnce(&Graph) -> Result<String> + Send + 'static,
) {
    let Some(graph) = app.graph.clone() else { return };
    if app.licensing.action.is_some() {
        return;
    }
    app.licensing.action = Some(Task::spawn(ctx, label, move || work(&graph)));
}

pub fn poll(app: &mut App) {
    if let Some(result) = take_finished(&mut app.licensing.load) {
        app.licensing.loaded = true;
        match result {
            Ok(skus) => {
                app.licensing.skus = skus;
                app.licensing.load_error = None;
            }
            Err(err) => {
                app.licensing.load_error = Some(err.clone());
                app.report_error(format!("Could not load subscriptions: {err}"));
            }
        }
    }

    if let Some(result) = take_finished(&mut app.licensing.holders_load) {
        match result {
            Ok(answer) => app.licensing.holders = Some(answer),
            Err(err) => {
                if let Some(id) = app.licensing.selected.clone() {
                    app.licensing.holders = Some((id, Err(err)));
                }
            }
        }
    }

    if let Some(result) = take_finished(&mut app.licensing.action) {
        match result {
            Ok(message) => {
                app.licensing.assign_to.clear();
                app.report_ok(message);
            }
            Err(err) => app.report_error(err),
        }
        // Read again whether it worked or not: a failure can still have
        // changed something, and the counts are worth having fresh anyway.
        app.licensing.invalidate();
        app.users.forget_licences();
    }
}

pub fn show(app: &mut App, ui: &mut Ui) {
    let ctx = ui.ctx().clone();
    if app.graph.is_none() {
        ui::pane_header(ui, "Licensing", "");
        if ui::not_connected(ui) {
            app.tab = Tab::Connection;
        }
        return;
    }
    ensure_loaded(app, &ctx);

    let shown = app.licensing.shown();
    let subtitle = if !app.licensing.loaded {
        "Loading…".to_owned()
    } else {
        let assignable = app.licensing.skus.iter().filter(|s| s.assignable()).count();
        format!(
            "{} subscriptions, {assignable} of them assignable to users",
            app.licensing.skus.len()
        )
    };
    ui::pane_header(ui, "Licensing", &subtitle);

    ui.horizontal(|ui| {
        if ui::tool_button(ui, app.licensing.load.is_none(), "Refresh")
            .on_hover_text(ui::shortcut_hint(ui, "Read the subscriptions again", &shortcuts::REFRESH))
            .clicked()
        {
            app.licensing.invalidate();
        }
    });
    ui.add_space(4.0);
    ui::search_box(ui, &mut app.licensing.query, "Search by product name or part number");
    ui.add_space(6.0);
    if let Some(err) = &app.licensing.load_error {
        ui::error_text(ui, err);
        ui.label(
            RichText::new(
                "Listing subscriptions needs LicenseAssignment.ReadWrite.All, or Organization.Read.All.",
            )
            .size(13.0)
            .weak(),
        );
    }

    if app.licensing.selected_sku().is_some() {
        egui::Panel::right("licence-details")
            .resizable(true)
            .default_size(380.0)
            .min_size(300.0)
            .show(ui, |ui| details(app, ui, &ctx));
    }

    let selected_index = app
        .licensing
        .selected
        .as_deref()
        .and_then(|id| shown.iter().position(|&i| app.licensing.skus[i].sku_id == id));
    let skus = &app.licensing.skus;
    let clicks = ui::select_table(
        ui,
        "skus",
        &[
            ("Product", Column::initial(300.0).at_least(100.0)),
            ("Assigned", Column::initial(80.0).at_least(50.0)),
            ("Available", Column::initial(80.0).at_least(50.0)),
            ("Total", Column::initial(70.0).at_least(50.0)),
            ("Status", Column::remainder().at_least(70.0)),
        ],
        shown.len(),
        selected_index,
        |row, column, ui| {
            let s = &skus[shown[row]];
            match column {
                0 => ui::cell_text(ui, s.name()),
                1 => ui::cell_text(ui, &s.consumed_units.to_string()),
                2 => {
                    let available = s.available();
                    if available == 0 && s.assignable() {
                        ui.label(RichText::new("0").color(ui::warn_colour(ui)));
                    } else {
                        ui::cell_text(ui, &available.to_string());
                    }
                }
                3 => ui::cell_text(ui, &s.total().to_string()),
                _ => status_label(ui, s),
            }
        },
        Some(&mut |row, ui| {
            let s = &skus[shown[row]];
            ui::copy_item(ui, "product name", s.name());
            ui::copy_item(ui, "part number", &s.sku_part_number);
            ui::copy_item(ui, "SKU ID", &s.sku_id);
        }),
    );
    if let Some(row) = clicks.clicked {
        let id = app.licensing.skus[shown[row]].sku_id.clone();
        app.licensing.selected = if app.licensing.selected.as_deref() == Some(&id) {
            None
        } else {
            Some(id)
        };
        app.licensing.assign_to.clear();
    }
    if let Some(row) = clicks.right_clicked {
        let id = app.licensing.skus[shown[row]].sku_id.clone();
        if app.licensing.selected.as_deref() != Some(&id) {
            app.licensing.selected = Some(id);
            app.licensing.assign_to.clear();
        }
    }
}

fn status_label(ui: &mut Ui, sku: &Sku) {
    let status = sku.capability_status.as_deref().unwrap_or("");
    if !sku.assignable() {
        ui::cell_text(ui, "Tenant-wide");
    } else if status == "Enabled" {
        ui::cell_text(ui, "Active");
    } else {
        ui.label(RichText::new(status).color(ui::warn_colour(ui)));
    }
}

fn details(app: &mut App, ui: &mut Ui, ctx: &egui::Context) {
    let Some(sku) = app.licensing.selected_sku().cloned() else {
        return;
    };
    let have = app.licensing.holders.as_ref().map(|(id, _)| id.as_str());
    if sku.assignable()
        && have != Some(sku.sku_id.as_str())
        && app.licensing.holders_load.is_none()
        && let Some(graph) = app.graph.clone()
    {
        let id = sku.sku_id.clone();
        app.licensing.holders_load = Some(Task::spawn(ctx, "Loading licence holders…", move || {
            let holders = graph.sku_holders(&id);
            Ok((id, holders))
        }));
    }
    let idle = app.licensing.action.is_none();

    egui::ScrollArea::vertical().show(ui, |ui| {
        ui.add_space(8.0);
        ui.heading(sku.name());
        ui.label(RichText::new(&sku.sku_part_number).size(12.0).weak());
        ui.add_space(8.0);

        ui::property(
            ui,
            "In use",
            &format!(
                "{} of {}, {} available",
                sku.consumed_units,
                sku.total(),
                sku.available()
            ),
        );
        let p = &sku.prepaid_units;
        if p.warning > 0 || p.suspended > 0 || p.locked_out > 0 {
            ui.label(
                RichText::new(format!(
                    "{} in their warning period, {} suspended, {} locked out. Renew in the Microsoft 365 admin centre.",
                    p.warning, p.suspended, p.locked_out
                ))
                .size(12.0)
                .color(ui::warn_colour(ui)),
            );
            ui.add_space(4.0);
        }
        ui::property(ui, "Status", sku.capability_status.as_deref().unwrap_or(""));
        ui::property(ui, "SKU ID", &sku.sku_id);

        if !sku.assignable() {
            ui.add_space(6.0);
            ui.label(
                RichText::new("This subscription covers the whole tenant, and is not assigned to people.")
                    .size(13.0)
                    .weak(),
            );
        } else {
            ui.add_space(8.0);
            ui.label(RichText::new("Assign to a user").strong());
            ui.horizontal(|ui| {
                let field = egui::TextEdit::singleline(&mut app.licensing.assign_to)
                    .hint_text("user@contoso.com")
                    .desired_width(ui.available_width() - 80.0);
                let response = ui::named(ui.add(field), "Sign-in name of the user to assign to");
                let enter = ui::submitted(&response);
                let can = idle && sku.available() > 0 && !app.licensing.assign_to.trim().is_empty();
                let clicked = ui::tool_button(ui, can, "Assign").clicked();
                if clicked || (enter && can) {
                    let upn = app.licensing.assign_to.trim().to_owned();
                    let sku_id = sku.sku_id.clone();
                    let name = sku.name().to_owned();
                    run(app, ctx, "Assigning licence…", move |g| {
                        let user = g.licensee(&g.user_id_for(&upn)?)?;
                        if user.holds(&sku_id) {
                            return Err(format!("{upn} already has {name}."));
                        }
                        g.change_licences(&user, std::slice::from_ref(&sku_id), &[])?;
                        Ok(format!("Assigned {name} to {upn}."))
                    });
                }
            });
            if sku.available() == 0 {
                ui.label(
                    RichText::new("None left to assign. Buy more, or remove one from someone first.")
                        .size(12.0)
                        .color(ui::warn_colour(ui)),
                );
            }

            ui.add_space(8.0);
            ui.label(RichText::new("Assigned to").strong());
            holders_list(app, ui, &sku, idle);
        }

        ui.add_space(8.0);
        egui::CollapsingHeader::new(format!("Service plans ({})", sku.service_plans.len()))
            .id_salt("service-plans")
            .show(ui, |ui| {
                for plan in &sku.service_plans {
                    ui.horizontal(|ui| {
                        ui.label(&plan.service_plan_name);
                        if let Some(status) = plan.provisioning_status.as_deref()
                            && status != "Success"
                        {
                            ui.label(RichText::new(status).size(12.0).weak());
                        }
                    });
                }
            });
    });
}

fn holders_list(app: &mut App, ui: &mut Ui, sku: &Sku, idle: bool) {
    let mut remove = None;
    let mut show_user = None;
    match &app.licensing.holders {
        Some((id, Ok(holders))) if *id == sku.sku_id => {
            if holders.users.is_empty() {
                ui.label(RichText::new("Nobody.").weak());
            }
            for user in &holders.users {
                let removable = user.holds_directly(&sku.sku_id);
                let removal = || {
                    (
                        user.id.clone(),
                        user.upn().to_owned(),
                        sku.sku_id.clone(),
                        sku.name().to_owned(),
                    )
                };
                let row = ui::menu_row(ui, |ui| {
                    if removable {
                        let label = format!("Remove {} from {}", user.name(), sku.name());
                        if ui::named(ui.add_enabled(idle, egui::Button::new("✕").small()), &label)
                            .on_hover_text(&label)
                            .clicked()
                        {
                            remove = Some(removal());
                        }
                    } else {
                        // Keeps the names lined up with the removable ones.
                        ui.add_space(ui.spacing().interact_size.y);
                    }
                    ui.vertical(|ui| {
                        ui.label(user.name());
                        let mut how: Vec<String> = Vec::new();
                        if user.holds_directly(&sku.sku_id) {
                            how.push("direct".to_owned());
                        }
                        for group in user.groups_for(&sku.sku_id) {
                            how.push(format!("through {}", holders.group_name(&group)));
                        }
                        ui.label(
                            RichText::new(format!("{} · {}", user.upn(), how.join(", ")))
                                .size(12.0)
                                .weak(),
                        );
                        if let Some(problem) = user.problem_with(&sku.sku_id) {
                            ui.label(RichText::new(problem).size(12.0).color(ui::bad_colour(ui)));
                        }
                    });
                });
                row.response.context_menu(|ui| {
                    if ui.button("Show in Users").clicked() {
                        show_user = Some(user.id.clone());
                    }
                    ui::copy_item(ui, "name", user.display_name.as_deref().unwrap_or(""));
                    ui::copy_item(ui, "sign-in name", user.upn());
                    ui.separator();
                    // A licence held through a group is removed from the group.
                    if ui.add_enabled(idle && removable, egui::Button::new("Remove licence")).clicked() {
                        remove = Some(removal());
                    }
                });
            }
        }
        Some((id, Err(err))) if *id == sku.sku_id => ui::error_text(ui, err),
        _ => ui::busy(ui, "Loading…"),
    }
    if remove.is_some() {
        app.licensing.confirm_remove = remove;
    }
    if let Some(id) = show_user {
        ui::users::show_user(app, &id);
    }
}

pub fn modals(app: &mut App, ctx: &egui::Context) {
    let Some((user_id, upn, sku_id, name)) = app.licensing.confirm_remove.clone() else {
        return;
    };
    let answer = ui::confirm_modal(
        ctx,
        egui::Id::new("remove-licence"),
        "Remove licence?",
        &format!(
            "{upn} will lose {name}. What it gave them access to, such as a mailbox, may be deleted after a grace period."
        ),
        "Remove",
    );
    if answer == ui::Confirmation::Waiting {
        return;
    }
    app.licensing.confirm_remove = None;
    if answer == ui::Confirmation::Confirmed {
        run(app, ctx, "Removing licence…", move |g| {
            let user = g.licensee(&user_id)?;
            g.change_licences(&user, &[], std::slice::from_ref(&sku_id))?;
            Ok(format!("Removed {name} from {upn}."))
        });
    }
}
