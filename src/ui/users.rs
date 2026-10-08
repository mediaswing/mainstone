//! The Users pane: every user in the tenant, searchable, with a details panel
//! for the one selected, and bulk import and export through CSV files.

use std::sync::{Arc, Mutex};

use egui::{RichText, Ui};
use egui_extras::Column;

use crate::app::{App, Tab};
use crate::csvio::{self, ImportResult, ImportRow};
use crate::graph::licensing::Licensee;
use crate::graph::mailbox::{
    AutoReplyEdit, Audience, CONCEALED_HINT, MailboxSettings, ReplyStatus, UsageReport,
};
use crate::graph::models::{DirectoryObject, User, short_time};
use crate::graph::users::{NewUser, UserEdit, generate_password};
use crate::graph::{Graph, Result};
use crate::task::{Task, take_finished};
use crate::ui;
use crate::ui::shortcuts::{self, Command};

/// What a finished action changed, so the list can be patched in place rather
/// than read again from the start.
enum Change {
    /// Nothing the list shows has changed, such as after a password reset.
    Nothing,
    Upsert(Box<User>),
    Removed(String),
    /// Licences changed; read them again, here and on the Licensing tab.
    Licences,
    /// Mailbox settings changed; read the selected user's again.
    Mailbox,
}

/// The Automatic replies dialog.
struct ReplyForm {
    id: String,
    name: String,
    edit: AutoReplyEdit,
    error: Option<String>,
}

/// The Licences dialog: one user, and the products ticked for them.
struct LicenceForm {
    user: Licensee,
    /// SKU IDs, lower case, of the licences to hold directly.
    direct: std::collections::BTreeSet<String>,
}

enum Form {
    Create(NewUser),
    Edit { id: String, edit: UserEdit },
}

struct ResetForm {
    id: String,
    name: String,
    password: String,
    force_change: bool,
}

struct ImportPreview {
    file: String,
    rows: Vec<ImportRow>,
}

/// What can be done to one user, from the buttons in the details panel or
/// the row's right-click menu, which offer the same things in the same way.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Action {
    Edit,
    ToggleEnabled,
    ResetPassword,
    Delete,
    Licences,
    AutoReplies,
    SignIns,
}

impl Action {
    const ALL: [Self; 7] = [
        Self::Edit,
        Self::ToggleEnabled,
        Self::ResetPassword,
        Self::Delete,
        Self::Licences,
        Self::AutoReplies,
        Self::SignIns,
    ];

    fn label(self, user: &User) -> &'static str {
        match self {
            Self::Edit => "Edit",
            Self::ToggleEnabled if user.account_enabled == Some(false) => "Enable",
            Self::ToggleEnabled => "Disable",
            Self::ResetPassword => "Reset password",
            Self::Delete => "Delete",
            Self::Licences => "Licences…",
            Self::AutoReplies => "Automatic replies…",
            Self::SignIns => "Sign-ins",
        }
    }

    fn hover(self) -> Option<&'static str> {
        match self {
            Self::AutoReplies => Some("Out-of-office replies for this mailbox"),
            Self::SignIns => Some("This user's sign-ins, on the Logs tab"),
            _ => None,
        }
    }
}

#[derive(Default)]
pub struct State {
    users: Vec<User>,
    loaded: bool,
    load_error: Option<String>,
    load: Option<Task<Vec<User>>>,
    query: String,
    selected: Option<String>,
    memberships: Option<(String, Result<Vec<DirectoryObject>>)>,
    /// Carries the user's ID whether it worked or not, so an answer that
    /// arrives after the selection has moved on is filed under the right
    /// user.
    memberships_load: Option<Task<(String, Result<Vec<DirectoryObject>>)>>,
    action: Option<Task<(String, Change)>>,
    form: Option<(Form, Option<String>)>,
    reset: Option<ResetForm>,
    confirm_delete: Option<(String, String)>,
    /// The selected user's licences, carried with their ID as memberships
    /// are.
    licences: Option<(String, Result<Licensee>)>,
    licences_load: Option<Task<(String, Result<Licensee>)>>,
    licence_form: Option<LicenceForm>,
    /// The selected user's mailbox settings, carried with their ID as
    /// memberships are. `Ok(None)` is a user with no Exchange mailbox.
    mailbox: Option<(String, Result<Option<MailboxSettings>>)>,
    mailbox_load: Option<Task<(String, Result<Option<MailboxSettings>>)>>,
    reply_form: Option<ReplyForm>,
    /// Every mailbox's size, read alongside the users. A failure here is
    /// shown quietly in the details: the permission is optional, and the
    /// tenant may have no Exchange at all.
    usage: Option<Result<UsageReport>>,
    usage_load: Option<Task<UsageReport>>,
    preview: Option<ImportPreview>,
    import: Option<Task<Vec<ImportResult>>>,
    import_progress: Arc<Mutex<String>>,
    results: Option<Vec<ImportResult>>,
}

impl State {
    /// Licences have changed somewhere, so whatever is shown is read again.
    pub fn forget_licences(&mut self) {
        self.licences = None;
    }

    pub fn importing(&self) -> bool {
        self.import.is_some()
    }

    /// The last import's results, taken out so they can outlive the rest
    /// of the pane: they may hold the only copy of generated passwords.
    pub fn take_results(&mut self) -> Option<Vec<ImportResult>> {
        self.results.take()
    }

    pub fn restore_results(&mut self, results: Option<Vec<ImportResult>>) {
        self.results = results;
    }

    pub fn activity(&self) -> Option<String> {
        if self.import.is_some() {
            return self.import_progress.lock().ok().map(|p| p.clone());
        }
        self.load
            .as_ref()
            .map(|t| t.label.clone())
            .or_else(|| self.action.as_ref().map(|t| t.label.clone()))
    }

    /// The users that match the search, as indices into `users`.
    fn shown(&self) -> Vec<usize> {
        let terms = ui::search_terms(&self.query);
        self.users
            .iter()
            .enumerate()
            .filter(|(_, u)| {
                ui::matches_search(
                    &terms,
                    &[
                        u.name(),
                        u.upn(),
                        u.mail.as_deref().unwrap_or(""),
                        u.department.as_deref().unwrap_or(""),
                        u.job_title.as_deref().unwrap_or(""),
                    ],
                )
            })
            .map(|(i, _)| i)
            .collect()
    }

    fn selected_user(&self) -> Option<&User> {
        let id = self.selected.as_deref()?;
        self.users.iter().find(|u| u.id == id)
    }

    /// The usage report, when it has been read and its names can be matched.
    fn usage_report(&self) -> Option<&UsageReport> {
        match &self.usage {
            Some(Ok(report)) if !report.concealed => Some(report),
            _ => None,
        }
    }
}

/// The keyboard shortcuts' commands; see [`shortcuts`].
pub fn command(app: &mut App, ctx: &egui::Context, command: Command) -> bool {
    if app.graph.is_none() {
        return false;
    }
    match command {
        Command::Find => ui::request_find(ctx),
        Command::Refresh => {
            if app.users.import.is_none() {
                reload(app, ctx);
            }
        }
        Command::New => new_user(app),
        Command::Delete => {
            let Some(user) = app.users.selected_user().cloned() else { return false };
            perform(app, ctx, &user, Action::Delete);
        }
        Command::Deselect => return app.users.selected.take().is_some(),
    }
    true
}

/// Show one user on this tab, from a member or holder elsewhere.
pub fn show_user(app: &mut App, id: &str) {
    app.users.selected = Some(id.to_owned());
    app.users.query.clear();
    app.tab = Tab::Users;
}

fn new_user(app: &mut App) {
    if app.users.action.is_some() {
        return;
    }
    app.users.form = Some((
        Form::Create(NewUser {
            password: generate_password(),
            account_enabled: true,
            force_change_password: true,
            ..Default::default()
        }),
        None,
    ));
}

/// The selected user's licences, once read.
fn licensee_of(app: &App, user: &User) -> Option<Licensee> {
    match &app.users.licences {
        Some((id, Ok(l))) if *id == user.id => Some(l.clone()),
        _ => None,
    }
}

/// The selected user's automatic replies, once their mailbox has been read.
fn replies_of(app: &App, user: &User) -> Option<crate::graph::mailbox::AutoReplies> {
    match &app.users.mailbox {
        Some((id, Ok(Some(settings)))) if *id == user.id => Some(settings.auto_replies.clone()),
        _ => None,
    }
}

fn available(app: &App, user: &User, action: Action) -> bool {
    let idle = app.users.action.is_none();
    match action {
        Action::Licences => idle && licensee_of(app, user).is_some(),
        Action::AutoReplies => idle && replies_of(app, user).is_some(),
        Action::SignIns => !user.upn().is_empty(),
        _ => idle,
    }
}

fn perform(app: &mut App, ctx: &egui::Context, user: &User, action: Action) {
    if !available(app, user, action) {
        return;
    }
    match action {
        Action::Edit => {
            app.users.form = Some((
                Form::Edit {
                    id: user.id.clone(),
                    edit: UserEdit::from_user(user),
                },
                None,
            ));
        }
        Action::ToggleEnabled => {
            let enabled = user.account_enabled != Some(false);
            let id = user.id.clone();
            let name = user.name().to_owned();
            let mut updated = user.clone();
            run(app, ctx, "Updating user…", move |g| {
                g.set_user_enabled(&id, !enabled)?;
                updated.account_enabled = Some(!enabled);
                let verb = if enabled { "disabled" } else { "enabled" };
                Ok((format!("{name} {verb}."), Change::Upsert(Box::new(updated))))
            });
        }
        Action::ResetPassword => {
            app.users.reset = Some(ResetForm {
                id: user.id.clone(),
                name: user.name().to_owned(),
                password: generate_password(),
                force_change: true,
            });
        }
        Action::Delete => {
            app.users.confirm_delete = Some((user.id.clone(), user.name().to_owned()));
        }
        Action::Licences => {
            let Some(licensee) = licensee_of(app, user) else { return };
            let direct = licensee
                .license_assignment_states
                .iter()
                .filter(|s| s.assigned_by_group.is_none())
                .map(|s| s.sku_id.to_lowercase())
                .collect();
            app.users.licence_form = Some(LicenceForm {
                user: licensee,
                direct,
            });
        }
        Action::AutoReplies => {
            let Some(replies) = replies_of(app, user) else { return };
            app.users.reply_form = Some(ReplyForm {
                id: user.id.clone(),
                name: user.name().to_owned(),
                edit: AutoReplyEdit::from_settings(&replies),
                error: None,
            });
        }
        Action::SignIns => ui::logs::sign_ins_for(app, ctx, user.upn()),
    }
}

/// A row's right-click menu: what the details panel offers, and copying
/// what the user is known by.
fn row_menu(app: &App, ui: &mut Ui, user: &User, chosen: &mut Option<(Action, String)>) {
    for action in Action::ALL {
        if action == Action::Delete {
            continue;
        }
        if ui
            .add_enabled(available(app, user, action), egui::Button::new(action.label(user)))
            .clicked()
        {
            *chosen = Some((action, user.id.clone()));
        }
    }
    ui.separator();
    ui::copy_item(ui, "display name", user.display_name.as_deref().unwrap_or(""));
    ui::copy_item(ui, "sign-in name", user.upn());
    ui::copy_item(ui, "mail address", user.mail.as_deref().unwrap_or(""));
    ui::copy_item(ui, "object ID", &user.id);
    ui.separator();
    if ui::menu_item(ui, available(app, user, Action::Delete), "Delete", &shortcuts::DELETE) {
        *chosen = Some((Action::Delete, user.id.clone()));
    }
}

fn graph(app: &App) -> Option<Graph> {
    app.graph.clone()
}

/// Run a change against Graph in the background.
fn run(
    app: &mut App,
    ctx: &egui::Context,
    label: &str,
    work: impl FnOnce(&Graph) -> Result<(String, Change)> + Send + 'static,
) {
    let Some(graph) = graph(app) else { return };
    if app.users.action.is_some() {
        return;
    }
    app.users.action = Some(Task::spawn(ctx, label, move || work(&graph)));
}

fn reload(app: &mut App, ctx: &egui::Context) {
    let Some(graph) = graph(app) else { return };
    if app.users.load.is_some() {
        return;
    }
    if app.users.usage_load.is_none() {
        let graph = graph.clone();
        app.users.usage_load = Some(Task::spawn(ctx, "Loading mailbox sizes…", move || {
            graph.mailbox_usage()
        }));
    }
    app.users.load = Some(Task::spawn(ctx, "Loading users…", move || graph.list_users()));
}

pub fn poll(app: &mut App) {
    if let Some(result) = take_finished(&mut app.users.load) {
        app.users.loaded = true;
        match result {
            Ok(users) => {
                app.users.users = users;
                app.users.load_error = None;
            }
            Err(err) => {
                app.users.load_error = Some(err.clone());
                app.report_error(format!("Could not load users: {err}"));
            }
        }
    }

    if let Some(result) = take_finished(&mut app.users.memberships_load) {
        match result {
            Ok((id, groups)) => app.users.memberships = Some((id, groups)),
            // The thread itself died, so whose answer it was is unknown.
            // Filed under whoever is selected, so it is not asked again in
            // a loop.
            Err(err) => {
                if let Some(id) = app.users.selected.clone() {
                    app.users.memberships = Some((id, Err(err)));
                }
            }
        }
    }

    if let Some(result) = take_finished(&mut app.users.licences_load) {
        match result {
            Ok(answer) => app.users.licences = Some(answer),
            // As for memberships above.
            Err(err) => {
                if let Some(id) = app.users.selected.clone() {
                    app.users.licences = Some((id, Err(err)));
                }
            }
        }
    }

    if let Some(result) = take_finished(&mut app.users.mailbox_load) {
        match result {
            Ok(answer) => app.users.mailbox = Some(answer),
            // As for memberships above.
            Err(err) => {
                if let Some(id) = app.users.selected.clone() {
                    app.users.mailbox = Some((id, Err(err)));
                }
            }
        }
    }

    if let Some(result) = take_finished(&mut app.users.usage_load) {
        if let Err(err) = &result {
            log::warn!("mailbox usage report not read: {err}");
        }
        app.users.usage = Some(result);
    }

    if let Some(result) = take_finished(&mut app.users.action) {
        match result {
            Ok((message, change)) => {
                let users = &mut app.users.users;
                match change {
                    Change::Nothing => {}
                    Change::Mailbox => app.users.mailbox = None,
                    Change::Licences => {
                        app.users.licences = None;
                        app.licensing.invalidate();
                    }
                    Change::Upsert(user) => {
                        let user = *user;
                        // A new usage location changes what can be assigned.
                        app.users.licences = None;
                        match users.iter_mut().find(|u| u.id == user.id) {
                            Some(existing) => *existing = user,
                            None => {
                                app.users.selected = Some(user.id.clone());
                                users.push(user);
                                users.sort_by_key(|u| u.name().to_lowercase());
                            }
                        }
                    }
                    Change::Removed(id) => {
                        users.retain(|u| u.id != id);
                        if app.users.selected.as_deref() == Some(id.as_str()) {
                            app.users.selected = None;
                        }
                    }
                }
                app.report_ok(message);
            }
            Err(err) => {
                // A licence change can fail half-way, so read them again.
                app.users.licences = None;
                app.report_error(err);
            }
        }
    }

    if let Some(result) = take_finished(&mut app.users.import) {
        match result {
            Ok(results) => {
                let created = results.iter().filter(|r| r.outcome.is_ok()).count();
                let failed = results.len() - created;
                let message = format!("Import finished: {created} created, {failed} failed.");
                if failed == 0 {
                    app.report_ok(message);
                } else {
                    app.report_error(message);
                }
                app.users.results = Some(results);
                // Read the list again, so the new users are in it.
                app.users.loaded = false;
            }
            Err(err) => app.report_error(err),
        }
    }
}

pub fn show(app: &mut App, ui: &mut Ui) {
    let ctx = ui.ctx().clone();
    if app.graph.is_none() {
        ui::pane_header(ui, "Users", "");
        if ui::not_connected(ui) {
            app.tab = Tab::Connection;
        }
        return;
    }
    if !app.users.loaded && app.users.load.is_none() {
        reload(app, &ctx);
    }

    let shown = app.users.shown();
    let subtitle = if app.users.load.is_some() && !app.users.loaded {
        "Loading…".to_owned()
    } else if app.users.query.trim().is_empty() {
        format!("{} users", app.users.users.len())
    } else {
        format!("{} of {} users match", shown.len(), app.users.users.len())
    };
    ui::pane_header(ui, "Users", &subtitle);

    toolbar(app, ui, &ctx, &shown);
    ui.add_space(4.0);
    ui::search_box(ui, &mut app.users.query, "Search by name, sign-in name, mail, department or job title");
    ui.add_space(6.0);

    if let Some(err) = &app.users.load_error {
        ui::error_text(ui, err);
    }
    import_results(app, ui);

    if app.users.selected_user().is_some() {
        egui::Panel::right("user-details")
            .resizable(true)
            .default_size(320.0)
            .min_size(260.0)
            .show(ui, |ui| details(app, ui, &ctx));
    }

    let selected_index = app
        .users
        .selected
        .as_deref()
        .and_then(|id| shown.iter().position(|&i| app.users.users[i].id == id));
    let users = &app.users.users;
    // Mailbox sizes get a column once there are some to show.
    let usage = app.users.usage_report().filter(|r| r.len() > 0);
    let mut columns = vec![
        ("Display name", Column::initial(200.0).at_least(80.0)),
        ("User principal name", Column::initial(260.0).at_least(80.0)),
        ("Department", Column::initial(130.0).at_least(60.0)),
        ("Job title", Column::initial(130.0).at_least(60.0)),
    ];
    if usage.is_some() {
        columns.push(("Mailbox", Column::initial(90.0).at_least(60.0)));
    }
    columns.push(("Status", Column::remainder().at_least(70.0)));
    let mut chosen = None;
    let app_ref: &App = app;
    let clicks = ui::select_table(
        ui,
        "users",
        &columns,
        shown.len(),
        selected_index,
        |row, column, ui| {
            let u = &users[shown[row]];
            match (column, usage) {
                (0, _) => ui::cell_text(ui, u.name()),
                (1, _) => ui::cell_text(ui, u.upn()),
                (2, _) => ui::cell_text(ui, u.department.as_deref().unwrap_or("")),
                (3, _) => ui::cell_text(ui, u.job_title.as_deref().unwrap_or("")),
                (4, Some(report)) => match report.get(u.upn()) {
                    Some(m) => {
                        let size = m
                            .storage_used
                            .map(crate::graph::mailbox::human_bytes)
                            .unwrap_or_default();
                        if m.quota_state().is_some() {
                            ui.label(RichText::new(size).color(ui::warn_colour(ui)));
                        } else {
                            ui::cell_text(ui, &size);
                        }
                    }
                    None => ui::cell_text(ui, ""),
                },
                _ => {
                    if u.account_enabled == Some(false) {
                        ui.label(RichText::new("Disabled").color(ui::warn_colour(ui)));
                    } else {
                        ui::cell_text(ui, "Enabled");
                    }
                }
            }
        },
        Some(&mut |row, ui| row_menu(app_ref, ui, &users[shown[row]], &mut chosen)),
    );
    if let Some(row) = clicks.clicked {
        let id = app.users.users[shown[row]].id.clone();
        app.users.selected = if app.users.selected.as_deref() == Some(&id) {
            None
        } else {
            Some(id)
        };
    }
    if let Some(row) = clicks.right_clicked {
        app.users.selected = Some(app.users.users[shown[row]].id.clone());
    }
    if let Some((action, id)) = chosen
        && let Some(user) = app.users.users.iter().find(|u| u.id == id).cloned()
    {
        perform(app, &ctx, &user, action);
    }
}

fn toolbar(app: &mut App, ui: &mut Ui, ctx: &egui::Context, shown: &[usize]) {
    let idle = app.users.load.is_none() && app.users.import.is_none();
    ui.horizontal_wrapped(|ui| {
        if ui::tool_button(ui, idle, "Refresh")
            .on_hover_text(ui::shortcut_hint(ui, "Read the users again", &shortcuts::REFRESH))
            .clicked()
        {
            reload(app, ctx);
        }
        if ui::tool_button(ui, app.users.action.is_none(), "+ New user")
            .on_hover_text(ui::shortcut_hint(ui, "Create a user", &shortcuts::NEW))
            .clicked()
        {
            new_user(app);
        }
        ui.separator();
        if ui::tool_button(ui, idle, "Import CSV…").clicked() {
            pick_import(app);
        }
        let export_label = if app.users.query.trim().is_empty() {
            "Export CSV…".to_owned()
        } else {
            format!("Export {} shown…", shown.len())
        };
        if ui::tool_button(ui, !shown.is_empty(), &export_label).clicked() {
            export_csv(app, shown);
        }
        if ui::tool_button(ui, true, "Save CSV template…").clicked()
            && let Some(path) = rfd::FileDialog::new()
                .set_file_name("mainstone-users-template.csv")
                .add_filter("CSV", &["csv"])
                .save_file()
        {
            match csvio::write_template(&path) {
                Ok(()) => app.report_ok(format!("Template saved to {}.", crate::config::tilde(&path))),
                Err(err) => app.report_error(format!("Could not save the template: {err}")),
            }
        }
    });
}

fn pick_import(app: &mut App) {
    let Some(path) = rfd::FileDialog::new()
        .add_filter("CSV", &["csv"])
        .pick_file()
    else {
        return;
    };
    match csvio::read_import(&path) {
        Ok(rows) if rows.is_empty() => app.report_error("That file has no users in it."),
        Ok(rows) => {
            app.users.preview = Some(ImportPreview {
                file: crate::config::tilde(&path),
                rows,
            })
        }
        Err(err) => app.report_error(err),
    }
}

fn export_csv(app: &mut App, shown: &[usize]) {
    let Some(path) = rfd::FileDialog::new()
        .set_file_name("users.csv")
        .add_filter("CSV", &["csv"])
        .save_file()
    else {
        return;
    };
    let users: Vec<&User> = shown.iter().map(|&i| &app.users.users[i]).collect();
    let count = users.len();
    match csvio::write_users(&path, &users) {
        Ok(()) => app.report_ok(format!(
            "Exported {count} users to {}.",
            crate::config::tilde(&path)
        )),
        Err(err) => app.report_error(format!("Could not export: {err}")),
    }
}

/// What the last import did, until it is dismissed.
fn import_results(app: &mut App, ui: &mut Ui) {
    let Some(results) = &app.users.results else {
        return;
    };
    let created = results.iter().filter(|r| r.outcome.is_ok()).count();
    let failures: Vec<&ImportResult> = results.iter().filter(|r| r.outcome.is_err()).collect();
    let passwords = results.iter().any(|r| r.password.is_some() && r.outcome.is_ok());

    let mut dismiss = false;
    let mut save = false;
    egui::Frame::group(ui.style()).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            ui.label(RichText::new(format!(
                "Last import: {created} created, {} failed.",
                failures.len()
            )));
            if ui.button("Save results…").clicked() {
                save = true;
            }
            if ui.button("Dismiss").clicked() {
                dismiss = true;
            }
        });
        if passwords {
            ui.label(
                RichText::new(
                    "Some passwords were generated. They are only in the results file — save it before dismissing.",
                )
                .size(13.0)
                .color(ui::warn_colour(ui)),
            );
        }
        for f in failures.iter().take(8) {
            if let Err(err) = &f.outcome {
                ui::error_text(
                    ui,
                    &format!("Line {} ({}): {err}", f.line, f.user_principal_name),
                );
            }
        }
        if failures.len() > 8 {
            ui.label(format!("…and {} more in the results file.", failures.len() - 8));
        }
    });
    ui.add_space(6.0);

    if save
        && let Some(path) = rfd::FileDialog::new()
            .set_file_name("mainstone-import-results.csv")
            .add_filter("CSV", &["csv"])
            .save_file()
    {
        let results = app.users.results.as_deref().unwrap_or_default();
        match csvio::write_results(&path, results) {
            Ok(()) => app.report_ok(format!("Results saved to {}.", crate::config::tilde(&path))),
            Err(err) => app.report_error(format!("Could not save the results: {err}")),
        }
    }
    if dismiss {
        app.users.results = None;
    }
}

fn details(app: &mut App, ui: &mut Ui, ctx: &egui::Context) {
    let Some(user) = app.users.selected_user().cloned() else {
        return;
    };

    // Group memberships for whoever is selected, read once per selection.
    let have = app.users.memberships.as_ref().map(|(id, _)| id.as_str());
    if have != Some(user.id.as_str())
        && app.users.memberships_load.is_none()
        && let Some(graph) = graph(app)
    {
        let id = user.id.clone();
        app.users.memberships_load = Some(Task::spawn(ctx, "Loading memberships…", move || {
            let groups = graph.user_memberships(&id);
            Ok((id, groups))
        }));
    }
    let have = app.users.licences.as_ref().map(|(id, _)| id.as_str());
    if have != Some(user.id.as_str())
        && app.users.licences_load.is_none()
        && let Some(graph) = graph(app)
    {
        let id = user.id.clone();
        app.users.licences_load = Some(Task::spawn(ctx, "Loading licences…", move || {
            let licences = graph.licensee(&id);
            Ok((id, licences))
        }));
    }
    let have = app.users.mailbox.as_ref().map(|(id, _)| id.as_str());
    if have != Some(user.id.as_str())
        && app.users.mailbox_load.is_none()
        && let Some(graph) = graph(app)
    {
        let id = user.id.clone();
        app.users.mailbox_load = Some(Task::spawn(ctx, "Loading mailbox settings…", move || {
            let settings = graph.mailbox_settings(&id);
            Ok((id, settings))
        }));
    }
    // The product names come from the subscriptions list.
    ui::licensing::ensure_loaded(app, ctx);

    egui::ScrollArea::vertical().show(ui, |ui| {
        ui.add_space(8.0);
        ui.heading(user.name());
        ui.add_space(6.0);

        let mut chosen = None;
        ui.horizontal_wrapped(|ui| {
            for action in Action::ALL {
                let mut button = ui::tool_button(ui, available(app, &user, action), action.label(&user));
                if let Some(hover) = action.hover() {
                    button = button.on_hover_text(hover);
                }
                if action == Action::Delete {
                    button = button.on_hover_text(ui::shortcut_hint(ui, "Delete this user", &shortcuts::DELETE));
                }
                if button.clicked() {
                    chosen = Some(action);
                }
            }
        });
        if let Some(action) = chosen {
            perform(app, ctx, &user, action);
        }
        if user.on_premises_sync_enabled == Some(true) {
            ui.label(
                RichText::new("Synchronised from on-premises Active Directory: most changes have to be made there.")
                    .size(12.0)
                    .color(ui::warn_colour(ui)),
            );
        }
        ui.add_space(10.0);

        let s = |v: &Option<String>| v.clone().unwrap_or_default();
        ui::property(ui, "User principal name", user.upn());
        ui::property(ui, "Mail", &s(&user.mail));
        ui::property(ui, "Given name", &s(&user.given_name));
        ui::property(ui, "Surname", &s(&user.surname));
        ui::property(ui, "Job title", &s(&user.job_title));
        ui::property(ui, "Department", &s(&user.department));
        ui::property(ui, "Office", &s(&user.office_location));
        ui::property(ui, "Mobile phone", &s(&user.mobile_phone));
        ui::property(ui, "Usage location", &s(&user.usage_location));
        ui::property(ui, "User type", &s(&user.user_type));
        ui::property(ui, "Created (UTC)", &short_time(user.created_date_time.as_deref()));
        ui::property(ui, "Object ID", &user.id);

        ui.add_space(8.0);
        ui.label(RichText::new("Licences").strong());
        licences_list(app, ui, &user.id);

        ui.add_space(8.0);
        ui.label(RichText::new("Mailbox").strong());
        mailbox_section(app, ui, &user);

        ui.add_space(8.0);
        ui.label(RichText::new("Member of").strong());
        match &app.users.memberships {
            Some((id, Ok(groups))) if *id == user.id => {
                if groups.is_empty() {
                    ui.label(RichText::new("No groups.").weak());
                }
                let mut open = None;
                for g in groups {
                    let label = ui.add(
                        egui::Label::new(format!("{}  ·  {}", g.name(), g.kind())).sense(egui::Sense::click()),
                    );
                    label.context_menu(|ui| {
                        if g.kind() == "group" && ui.button("Show in Groups").clicked() {
                            open = Some(g.id.clone());
                        }
                        ui::copy_item(ui, "name", g.name());
                        ui::copy_item(ui, "object ID", &g.id);
                    });
                }
                if let Some(id) = open {
                    ui::groups::show_group(app, &id);
                }
            }
            Some((id, Err(err))) if *id == user.id => ui::error_text(ui, err),
            _ => ui::busy(ui, "Loading…"),
        }
    });
}

/// A product's name, from the subscriptions list if it has been read, or
/// its SKU ID if not.
fn product_name(app: &App, sku_id: &str) -> String {
    app.licensing
        .skus()
        .and_then(|skus| skus.iter().find(|s| s.sku_id.eq_ignore_ascii_case(sku_id)))
        .map_or_else(|| sku_id.to_owned(), |s| s.name().to_owned())
}

fn licences_list(app: &App, ui: &mut Ui, user_id: &str) {
    match &app.users.licences {
        Some((id, Ok(licensee))) if id == user_id => {
            // One line per product, however many ways it is held.
            let mut skus: Vec<&str> = licensee
                .license_assignment_states
                .iter()
                .map(|s| s.sku_id.as_str())
                .collect();
            skus.sort_unstable_by_key(|s| s.to_lowercase());
            skus.dedup_by_key(|s| s.to_lowercase());
            if skus.is_empty() {
                ui.label(RichText::new("No licences.").weak());
            }
            for sku in skus {
                let how = match (licensee.holds_directly(sku), licensee.groups_for(sku).len()) {
                    (true, 0) => "direct",
                    (true, _) => "direct and through a group",
                    (false, _) => "through a group",
                };
                ui.label(format!("{}  ·  {how}", product_name(app, sku)));
                if let Some(problem) = licensee.problem_with(sku) {
                    ui.label(RichText::new(problem).size(12.0).color(ui::bad_colour(ui)));
                }
            }
            if !licensee.usage_location.as_deref().is_some_and(|l| !l.is_empty()) {
                ui.label(
                    RichText::new("No usage location: set one with Edit before assigning a licence.")
                        .size(12.0)
                        .color(ui::warn_colour(ui)),
                );
            }
        }
        Some((id, Err(err))) if id == user_id => ui::error_text(ui, err),
        _ => ui::busy(ui, "Loading…"),
    }
}

/// Automatic replies, time zone and language from the mailbox settings, and
/// size and activity from the usage report.
fn mailbox_section(app: &App, ui: &mut Ui, user: &User) {
    let note = |ui: &mut Ui, text: &str| {
        ui.label(RichText::new(text).size(12.0).weak());
    };
    let settings = match &app.users.mailbox {
        Some((id, Ok(None))) if *id == user.id => {
            note(ui, "No Exchange Online mailbox.");
            return;
        }
        Some((id, Ok(Some(settings)))) if *id == user.id => settings,
        Some((id, Err(err))) if *id == user.id => {
            ui::error_text(ui, err);
            return;
        }
        _ => {
            ui::busy(ui, "Loading…");
            return;
        }
    };
    let usage = match &app.users.usage {
        Some(Ok(report)) if !report.concealed => report.get(user.upn()),
        _ => None,
    };

    let kind = settings
        .purpose
        .clone()
        .or_else(|| usage.map(|m| m.recipient_type.clone()))
        .filter(|k| !k.is_empty())
        .map(|k| {
            let mut chars = k.chars();
            chars
                .next()
                .map(|c| c.to_uppercase().chain(chars).collect::<String>())
                .unwrap_or_default()
        });
    ui::property(ui, "Automatic replies", &settings.auto_replies.summary());
    if let Some(kind) = kind {
        ui::property(ui, "Mailbox type", &kind);
    }
    ui::property(ui, "Time zone", settings.time_zone.as_deref().unwrap_or(""));
    ui::property(ui, "Language", settings.language.as_deref().unwrap_or(""));

    match (&app.users.usage, usage) {
        (_, Some(m)) => {
            ui::property(ui, "Size", &m.size_text());
            if let Some(state) = m.quota_state() {
                ui.label(RichText::new(state).size(12.0).color(ui::warn_colour(ui)));
                ui.add_space(4.0);
            }
            let items = m.item_count.map(|n| n.to_string()).unwrap_or_default();
            ui::property(ui, "Items", &items);
            ui::property(ui, "Last activity", &m.last_activity);
            let archive = match m.has_archive {
                Some(true) => "Yes",
                Some(false) => "No",
                None => "",
            };
            ui::property(ui, "Archive", archive);
            if let Some(Ok(report)) = &app.users.usage {
                note(ui, &format!("Size and activity as of {}, from Microsoft's usage report.", report.refresh_date));
            }
        }
        (Some(Ok(report)), None) if report.concealed => note(ui, CONCEALED_HINT),
        // Usually a mailbox made since the report was last refreshed.
        (Some(Ok(_)), None) => note(ui, "Not in the usage report yet, which Microsoft refreshes about once a day."),
        (Some(Err(err)), None) => note(ui, &format!("Mailbox sizes could not be read: {err}")),
        (None, None) => {}
    }
}

fn reply_modal(app: &mut App, ctx: &egui::Context) {
    let Some(form) = app.users.reply_form.as_mut() else {
        return;
    };
    let mut answer = None;
    let modal = egui::Modal::new(egui::Id::new("auto-replies")).show(ctx, |ui| {
        ui.set_width(500.0);
        ui.heading("Automatic replies");
        ui.label(RichText::new(&form.name).weak());
        ui.add_space(8.0);
        let edit = &mut form.edit;
        egui::ScrollArea::vertical()
            .max_height(ctx.content_rect().height() - 220.0)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    for status in ReplyStatus::ALL {
                        ui.radio_value(&mut edit.status, status, status.label());
                    }
                });
                ui.add_space(6.0);
                if edit.status == ReplyStatus::Scheduled {
                    let zone = edit.time_zone.clone();
                    ui::labelled_field(ui, &format!("Start ({zone})"), &mut edit.start, "2026-10-05 09:00");
                    ui::labelled_field(ui, &format!("End ({zone})"), &mut edit.end, "2026-10-12 17:00");
                }
                ui::labelled_multiline(
                    ui,
                    "Reply to senders inside the organisation",
                    &mut edit.internal,
                    "I'm away until…",
                );
                ui.label(RichText::new("Reply to senders outside the organisation").size(13.0));
                egui::ComboBox::from_id_salt("reply-audience")
                    .selected_text(edit.audience.label())
                    .width(300.0)
                    .show_ui(ui, |ui| {
                        for audience in Audience::ALL {
                            ui.selectable_value(&mut edit.audience, audience, audience.label());
                        }
                    });
                ui.add_space(6.0);
                if edit.audience != Audience::None {
                    ui::labelled_multiline(ui, "Message for outside senders", &mut edit.external, "");
                }
                if edit.messages_changed() {
                    ui.label(
                        RichText::new("Messages are saved as plain text, replacing any formatting set in Outlook.")
                            .size(12.0)
                            .weak(),
                    );
                }
            });
        if let Some(err) = &form.error {
            ui::error_text(ui, err);
        }
        answer = ui::form_buttons(ui, "Save", true);
    });
    if modal.should_close() && answer.is_none() {
        answer = Some(false);
    }
    match answer {
        Some(true) => {
            if let Err(err) = form.edit.validate() {
                form.error = Some(err);
                return;
            }
            let Some(form) = app.users.reply_form.take() else { return };
            run(app, ctx, "Saving automatic replies…", move |g| {
                g.set_auto_replies(&form.id, &form.edit)?;
                let state = match form.edit.status {
                    ReplyStatus::Off => "turned off",
                    ReplyStatus::On => "turned on",
                    ReplyStatus::Scheduled => "scheduled",
                };
                Ok((format!("Automatic replies {state} for {}.", form.name), Change::Mailbox))
            });
        }
        Some(false) => app.users.reply_form = None,
        None => {}
    }
}

fn licence_modal(app: &mut App, ctx: &egui::Context) {
    if app.users.licence_form.is_none() {
        return;
    }
    ui::licensing::ensure_loaded(app, ctx);
    let skus: Option<Vec<_>> = app
        .licensing
        .skus()
        .map(|s| s.iter().filter(|s| s.assignable()).cloned().collect());
    let Some(form) = app.users.licence_form.as_mut() else { return };
    let mut answer = None;

    let modal = egui::Modal::new(egui::Id::new("licence-form")).show(ctx, |ui| {
        ui.set_width(480.0);
        ui.heading("Licences");
        ui.label(RichText::new(form.user.upn()).weak());
        ui.add_space(8.0);
        let Some(skus) = &skus else {
            ui::busy(ui, "Loading subscriptions…");
            answer = ui::form_buttons(ui, "Save", false);
            return;
        };
        if skus.is_empty() {
            ui.label("The tenant has no licences that can be assigned to users.");
        }
        egui::ScrollArea::vertical()
            .max_height(ctx.content_rect().height() - 220.0)
            .show(ui, |ui| {
                for sku in skus {
                    let id = sku.sku_id.to_lowercase();
                    let had = form.user.holds_directly(&id);
                    let mut ticked = form.direct.contains(&id);
                    // Adding needs one free; keeping one already held does not.
                    let can_tick = had || ticked || sku.available() > 0;
                    let label = format!("{}  ({} available)", sku.name(), sku.available());
                    if ui
                        .add_enabled(can_tick, egui::Checkbox::new(&mut ticked, label))
                        .changed()
                    {
                        if ticked {
                            form.direct.insert(id.clone());
                        } else {
                            form.direct.remove(&id);
                        }
                    }
                    if !form.user.groups_for(&id).is_empty() {
                        ui.label(
                            RichText::new("Also held through a group, which this does not change.")
                                .size(12.0)
                                .weak(),
                        );
                    }
                }
            });
        if !form.user.usage_location.as_deref().is_some_and(|l| !l.is_empty()) {
            ui.add_space(4.0);
            ui.label(
                RichText::new("This user has no usage location, so licences can be removed but not added. Set one with Edit.")
                    .size(12.0)
                    .color(ui::warn_colour(ui)),
            );
        }
        answer = ui::form_buttons(ui, "Save", true);
    });
    if modal.should_close() && answer.is_none() {
        answer = Some(false);
    }
    match answer {
        Some(true) => {
            let Some(form) = app.users.licence_form.take() else { return };
            let held: std::collections::BTreeSet<String> = form
                .user
                .license_assignment_states
                .iter()
                .filter(|s| s.assigned_by_group.is_none())
                .map(|s| s.sku_id.to_lowercase())
                .collect();
            let add: Vec<String> = form.direct.difference(&held).cloned().collect();
            let remove: Vec<String> = held.difference(&form.direct).cloned().collect();
            if add.is_empty() && remove.is_empty() {
                return;
            }
            let user = form.user;
            run(app, ctx, "Changing licences…", move |g| {
                g.change_licences(&user, &add, &remove)?;
                let message = match (add.len(), remove.len()) {
                    (a, 0) => format!("Assigned {a} licence(s) to {}.", user.upn()),
                    (0, r) => format!("Removed {r} licence(s) from {}.", user.upn()),
                    (a, r) => format!("Assigned {a} and removed {r} licence(s) for {}.", user.upn()),
                };
                Ok((message, Change::Licences))
            });
        }
        Some(false) => app.users.licence_form = None,
        None => {}
    }
}

pub fn modals(app: &mut App, ctx: &egui::Context) {
    licence_modal(app, ctx);
    reply_modal(app, ctx);
    form_modal(app, ctx);
    reset_modal(app, ctx);
    delete_modal(app, ctx);
    preview_modal(app, ctx);
}

fn form_modal(app: &mut App, ctx: &egui::Context) {
    let Some((form, error)) = app.users.form.as_mut() else {
        return;
    };
    let mut answer = None;
    let mut enter = false;
    let modal = egui::Modal::new(egui::Id::new("user-form")).show(ctx, |ui| {
        ui.set_width(460.0);
        egui::ScrollArea::vertical()
            .max_height(ctx.content_rect().height() - 160.0)
            .show(ui, |ui| match form {
                Form::Create(u) => {
                    ui.heading("New user");
                    ui.add_space(6.0);
                    let first = ui::labelled_field(ui, "Display name", &mut u.display_name, "Jo Bloggs");
                    ui::focus_on_open(ui, egui::Id::new("user-form"), &first);
                    enter |= ui::submitted(&first);
                    enter |= ui::submitted(&ui::labelled_field(ui, "User principal name", &mut u.user_principal_name, "jo.bloggs@contoso.com"));
                    enter |= ui::submitted(&ui::labelled_field(ui, "Mail nickname (optional)", &mut u.mail_nickname, "from the user principal name"));
                    enter |= ui::submitted(&ui::labelled_field(ui, "Initial password", &mut u.password, ""));
                    ui.checkbox(&mut u.force_change_password, "Must change password at next sign-in");
                    ui.checkbox(&mut u.account_enabled, "Account enabled");
                    ui.add_space(6.0);
                    enter |= ui::submitted(&ui::labelled_field(ui, "Given name", &mut u.given_name, ""));
                    enter |= ui::submitted(&ui::labelled_field(ui, "Surname", &mut u.surname, ""));
                    enter |= ui::submitted(&ui::labelled_field(ui, "Job title", &mut u.job_title, ""));
                    enter |= ui::submitted(&ui::labelled_field(ui, "Department", &mut u.department, ""));
                    enter |= ui::submitted(&ui::labelled_field(ui, "Office", &mut u.office_location, ""));
                    enter |= ui::submitted(&ui::labelled_field(ui, "Mobile phone", &mut u.mobile_phone, ""));
                    enter |= ui::submitted(&ui::labelled_field(ui, "Usage location", &mut u.usage_location, "GB"));
                }
                Form::Edit { edit, .. } => {
                    ui.heading("Edit user");
                    ui.add_space(6.0);
                    let first = ui::labelled_field(ui, "Display name", &mut edit.display_name, "");
                    ui::focus_on_open(ui, egui::Id::new("user-form"), &first);
                    enter |= ui::submitted(&first);
                    enter |= ui::submitted(&ui::labelled_field(ui, "Given name", &mut edit.given_name, ""));
                    enter |= ui::submitted(&ui::labelled_field(ui, "Surname", &mut edit.surname, ""));
                    enter |= ui::submitted(&ui::labelled_field(ui, "Job title", &mut edit.job_title, ""));
                    enter |= ui::submitted(&ui::labelled_field(ui, "Department", &mut edit.department, ""));
                    enter |= ui::submitted(&ui::labelled_field(ui, "Office", &mut edit.office_location, ""));
                    enter |= ui::submitted(&ui::labelled_field(ui, "Mobile phone", &mut edit.mobile_phone, ""));
                    enter |= ui::submitted(&ui::labelled_field(ui, "Usage location", &mut edit.usage_location, "GB"));
                }
            });
        if let Some(err) = error.as_ref() {
            ui::error_text(ui, err);
        }
        let label = match form {
            Form::Create(_) => "Create",
            Form::Edit { .. } => "Save",
        };
        answer = ui::form_buttons(ui, label, true);
    });
    if enter && answer.is_none() {
        answer = Some(true);
    }
    if modal.should_close() && answer.is_none() {
        answer = Some(false);
    }

    match answer {
        Some(false) => app.users.form = None,
        Some(true) => {
            let Some((form, _)) = app.users.form.take() else { return };
            // Check here what can be checked here, and keep the form open.
            let check = match &form {
                Form::Create(u) => u.validate(),
                Form::Edit { .. } => Ok(()),
            };
            if let Err(err) = check {
                app.users.form = Some((form, Some(err)));
                return;
            }
            match form {
                Form::Create(new) => run(app, ctx, "Creating user…", move |g| {
                    let user = g.create_user(&new)?;
                    Ok((format!("Created {}.", user.upn()), Change::Upsert(Box::new(user))))
                }),
                Form::Edit { id, edit } => {
                    let mut updated = app
                        .users
                        .users
                        .iter()
                        .find(|u| u.id == id)
                        .cloned()
                        .unwrap_or_default();
                    run(app, ctx, "Saving user…", move |g| {
                        g.update_user(&id, &edit)?;
                        let opt = |s: &str| Some(s.trim().to_owned()).filter(|s| !s.is_empty());
                        updated.display_name = opt(&edit.display_name);
                        updated.given_name = opt(&edit.given_name);
                        updated.surname = opt(&edit.surname);
                        updated.job_title = opt(&edit.job_title);
                        updated.department = opt(&edit.department);
                        updated.office_location = opt(&edit.office_location);
                        updated.mobile_phone = opt(&edit.mobile_phone);
                        updated.usage_location = opt(&edit.usage_location.to_uppercase());
                        Ok((format!("Saved {}.", updated.name()), Change::Upsert(Box::new(updated))))
                    })
                }
            }
        }
        None => {}
    }
}

fn reset_modal(app: &mut App, ctx: &egui::Context) {
    let Some(reset) = app.users.reset.as_mut() else {
        return;
    };
    let mut answer = None;
    let modal = egui::Modal::new(egui::Id::new("reset-password")).show(ctx, |ui| {
        ui.set_width(400.0);
        ui.heading("Reset password");
        ui.label(RichText::new(&reset.name).weak());
        ui.add_space(8.0);
        let field = ui::labelled_field(ui, "New password", &mut reset.password, "");
        ui::focus_on_open(ui, egui::Id::new("reset-password"), &field);
        if ui.small_button("Generate another").clicked() {
            reset.password = generate_password();
        }
        ui.checkbox(&mut reset.force_change, "Must change password at next sign-in");
        ui.label(
            RichText::new("Copy the password before resetting: it is not shown again.")
                .size(12.0)
                .weak(),
        );
        answer = ui::form_buttons(ui, "Reset", !reset.password.is_empty());
        if ui::submitted(&field) && !reset.password.is_empty() && answer.is_none() {
            answer = Some(true);
        }
    });
    if modal.should_close() && answer.is_none() {
        answer = Some(false);
    }
    match answer {
        Some(true) => {
            let Some(r) = app.users.reset.take() else { return };
            run(app, ctx, "Resetting password…", move |g| {
                g.reset_password(&r.id, &r.password, r.force_change)?;
                Ok((format!("Password reset for {}.", r.name), Change::Nothing))
            });
        }
        Some(false) => app.users.reset = None,
        None => {}
    }
}

fn delete_modal(app: &mut App, ctx: &egui::Context) {
    let Some((id, name)) = app.users.confirm_delete.clone() else {
        return;
    };
    let answer = ui::confirm_modal(
        ctx,
        egui::Id::new("delete-user"),
        "Delete user?",
        &format!("{name} will be moved to deleted users, and can be restored from the Entra admin centre for 30 days."),
        "Delete",
    );
    if answer == ui::Confirmation::Waiting {
        return;
    }
    app.users.confirm_delete = None;
    if answer == ui::Confirmation::Confirmed {
        run(app, ctx, "Deleting user…", move |g| {
            g.delete_user(&id)?;
            Ok((format!("Deleted {name}."), Change::Removed(id)))
        });
    }
}

fn preview_modal(app: &mut App, ctx: &egui::Context) {
    let Some(preview) = &app.users.preview else {
        return;
    };
    let ready = preview.rows.iter().filter(|r| r.user.is_ok()).count();
    let generated = preview
        .rows
        .iter()
        .filter(|r| r.user.is_ok() && r.generated_password)
        .count();
    let mut answer = None;

    let modal = egui::Modal::new(egui::Id::new("import-preview")).show(ctx, |ui| {
        ui.set_width(560.0);
        ui.heading("Import users");
        ui.label(RichText::new(&preview.file).size(12.0).weak());
        ui.add_space(6.0);
        ui.label(format!(
            "{ready} of {} rows are ready to create.",
            preview.rows.len()
        ));
        if generated > 0 {
            ui.label(
                RichText::new(format!(
                    "{generated} rows have no password: one will be generated for each, and written to the results file."
                ))
                .size(13.0)
                .color(ui::warn_colour(ui)),
            );
        }
        ui.add_space(6.0);
        egui::ScrollArea::vertical().max_height(300.0).show(ui, |ui| {
            egui::Grid::new("import-rows")
                .num_columns(3)
                .striped(true)
                .spacing([12.0, 4.0])
                .show(ui, |ui| {
                    for row in &preview.rows {
                        ui.label(RichText::new(format!("Line {}", row.line)).weak());
                        match &row.user {
                            Ok(u) => {
                                ui.label(&u.display_name);
                                ui.label(&u.user_principal_name);
                            }
                            Err(err) => {
                                ui.label(RichText::new("Skipped").color(ui::bad_colour(ui)));
                                ui.label(RichText::new(err).color(ui::bad_colour(ui)));
                            }
                        }
                        ui.end_row();
                    }
                });
        });
        answer = ui::form_buttons(ui, &format!("Create {ready} users"), ready > 0);
    });
    if modal.should_close() && answer.is_none() {
        answer = Some(false);
    }

    match answer {
        Some(true) => {
            let Some(preview) = app.users.preview.take() else { return };
            let Some(graph) = graph(app) else { return };
            let progress = app.users.import_progress.clone();
            app.users.results = None;
            app.users.import = Some(Task::spawn(ctx, "Importing users…", move || {
                let rows: Vec<_> = preview
                    .rows
                    .into_iter()
                    .filter_map(|r| r.user.ok().map(|u| (r.line, u, r.generated_password)))
                    .collect();
                let total = rows.len();
                let mut results = Vec::with_capacity(total);
                for (n, (line, user, generated)) in rows.into_iter().enumerate() {
                    if let Ok(mut p) = progress.lock() {
                        *p = format!("Creating user {} of {total}…", n + 1);
                    }
                    let outcome = graph.create_user(&user).map(|u| u.id);
                    if let Err(err) = &outcome {
                        log::warn!("import line {line} ({}) failed: {err}", user.user_principal_name);
                    }
                    results.push(ImportResult {
                        line,
                        user_principal_name: user.user_principal_name.clone(),
                        password: generated.then(|| user.password.clone()),
                        outcome,
                    });
                }
                Ok(results)
            }));
        }
        Some(false) => app.users.preview = None,
        None => {}
    }
}
