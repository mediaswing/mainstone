//! The Logs pane: the tenant's sign-ins and its directory audit log, read
//! for a chosen stretch of time, searchable, with a details panel for the
//! entry selected and an export of what is shown.

use egui::{RichText, Ui};
use egui_extras::Column;

use crate::app::{App, Tab};
use crate::csvio;
use crate::graph::logs::{DirectoryAudit, Entries, LogQuery, MAX_ENTRIES, Range, SignIn, log_time};
use crate::task::{Task, take_finished};
use crate::ui;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Kind {
    #[default]
    SignIns,
    Audit,
}

impl Kind {
    fn label(self) -> &'static str {
        match self {
            Self::SignIns => "Sign-ins",
            Self::Audit => "Audit log",
        }
    }
}

/// What a load brought back, and what it was asked for.
struct Loaded<T> {
    entries: Entries<T>,
    asked: LogQuery,
}

enum Fetched {
    SignIns(Loaded<SignIn>),
    Audits(Loaded<DirectoryAudit>),
}

#[derive(Default)]
pub struct State {
    kind: Kind,
    /// The filters as set in the toolbar, applied by the next load.
    query: LogQuery,
    sign_ins: Option<Loaded<SignIn>>,
    audits: Option<Loaded<DirectoryAudit>>,
    sign_ins_error: Option<String>,
    audits_error: Option<String>,
    load: Option<Task<Fetched>>,
    /// Which log `load` is reading, since the view can be switched to the
    /// other one while it runs.
    loading: Kind,
    search: String,
    /// The entry selected, by ID, in whichever log is showing.
    selected: Option<String>,
}

impl State {
    pub fn activity(&self) -> Option<String> {
        self.load.as_ref().map(|t| t.label.clone())
    }

    fn error(&self) -> Option<&String> {
        match self.kind {
            Kind::SignIns => self.sign_ins_error.as_ref(),
            Kind::Audit => self.audits_error.as_ref(),
        }
    }

    fn loaded(&self) -> bool {
        match self.kind {
            Kind::SignIns => self.sign_ins.is_some() || self.sign_ins_error.is_some(),
            Kind::Audit => self.audits.is_some() || self.audits_error.is_some(),
        }
    }

    fn shown_sign_ins(&self) -> Vec<&SignIn> {
        let terms = ui::search_terms(&self.search);
        self.sign_ins
            .iter()
            .flat_map(|l| &l.entries.rows)
            .filter(|s| {
                ui::matches_search(
                    &terms,
                    &[
                        s.user(),
                        s.user_display_name.as_deref().unwrap_or(""),
                        s.app(),
                        &s.outcome(),
                        s.ip_address.as_deref().unwrap_or(""),
                        &s.place(),
                        s.device_detail.operating_system.as_deref().unwrap_or(""),
                    ],
                )
            })
            .collect()
    }

    fn shown_audits(&self) -> Vec<&DirectoryAudit> {
        let terms = ui::search_terms(&self.search);
        self.audits
            .iter()
            .flat_map(|l| &l.entries.rows)
            .filter(|a| {
                ui::matches_search(
                    &terms,
                    &[
                        a.activity(),
                        a.category.as_deref().unwrap_or(""),
                        a.logged_by_service.as_deref().unwrap_or(""),
                        &a.initiator(),
                        &a.target_resources
                            .iter()
                            .filter_map(|t| t.name())
                            .collect::<Vec<_>>()
                            .join(" "),
                        a.result.as_deref().unwrap_or(""),
                    ],
                )
            })
            .collect()
    }
}

/// Read the log that is showing, with the filters as they are set.
fn load(app: &mut App, ctx: &egui::Context) {
    let Some(graph) = app.graph.clone() else { return };
    if app.logs.load.is_some() {
        return;
    }
    let query = app.logs.query.clone();
    app.logs.selected = None;
    app.logs.loading = app.logs.kind;
    app.logs.load = Some(match app.logs.kind {
        Kind::SignIns => Task::spawn(ctx, "Reading sign-ins…", move || {
            let entries = graph.sign_ins(&query)?;
            Ok(Fetched::SignIns(Loaded {
                entries,
                asked: query,
            }))
        }),
        Kind::Audit => Task::spawn(ctx, "Reading the audit log…", move || {
            let entries = graph.directory_audits(&query)?;
            Ok(Fetched::Audits(Loaded {
                entries,
                asked: query,
            }))
        }),
    });
}

/// Show one user's sign-ins: from the Users pane's details panel.
pub fn sign_ins_for(app: &mut App, ctx: &egui::Context, upn: &str) {
    if app.logs.load.is_some() {
        app.report_error("The logs are still loading. Try again in a moment.");
        return;
    }
    app.logs.kind = Kind::SignIns;
    app.logs.query.user = upn.to_owned();
    app.logs.query.failures_only = false;
    app.logs.search.clear();
    app.tab = Tab::Logs;
    load(app, ctx);
}

pub fn poll(app: &mut App) {
    let Some(result) = take_finished(&mut app.logs.load) else {
        return;
    };
    let logs = &mut app.logs;
    match result {
        Ok(Fetched::SignIns(loaded)) => {
            logs.sign_ins_error = None;
            logs.sign_ins = Some(loaded);
        }
        Ok(Fetched::Audits(loaded)) => {
            logs.audits_error = None;
            logs.audits = Some(loaded);
        }
        Err(err) => {
            match logs.loading {
                Kind::SignIns => logs.sign_ins_error = Some(err.clone()),
                Kind::Audit => logs.audits_error = Some(err.clone()),
            }
            app.inform("Could not read the log", err);
        }
    }
}

pub fn show(app: &mut App, ui: &mut Ui) {
    let ctx = ui.ctx().clone();
    if app.graph.is_none() {
        ui::pane_header(ui, "Logs", "");
        if ui::not_connected(ui) {
            app.tab = Tab::Connection;
        }
        return;
    }
    if !app.logs.loaded() && app.logs.load.is_none() {
        load(app, &ctx);
    }

    let subtitle = subtitle(&app.logs);
    ui::pane_header(ui, "Logs", &subtitle);
    toolbar(app, ui, &ctx);
    ui.add_space(4.0);
    ui::search_box(
        ui,
        &mut app.logs.search,
        "Search what is loaded: user, app, activity, IP address, location",
    );
    ui.add_space(6.0);

    if let Some(err) = app.logs.error() {
        ui::error_text(ui, err);
        let help = match app.logs.kind {
            Kind::SignIns => {
                "Reading sign-ins needs the AuditLog.Read.All permission, and an Entra ID P1 or P2 licence in the tenant."
            }
            Kind::Audit => "Reading the audit log needs the AuditLog.Read.All permission.",
        };
        ui.label(RichText::new(help).size(13.0).weak());
        ui.add_space(6.0);
    }

    match app.logs.kind {
        Kind::SignIns => sign_ins_view(app, ui),
        Kind::Audit => audits_view(app, ui),
    }
}

fn subtitle(logs: &State) -> String {
    let (count, shown, asked, truncated) = match logs.kind {
        Kind::SignIns => match &logs.sign_ins {
            Some(l) => (l.entries.rows.len(), logs.shown_sign_ins().len(), &l.asked, l.entries.truncated),
            None => return loading_or_blank(logs),
        },
        Kind::Audit => match &logs.audits {
            Some(l) => (l.entries.rows.len(), logs.shown_audits().len(), &l.asked, l.entries.truncated),
            None => return loading_or_blank(logs),
        },
    };
    let mut text = if logs.search.trim().is_empty() {
        format!("{count} entries: {}", asked.describe())
    } else {
        format!("{shown} of {count} entries match: {}", asked.describe())
    };
    if truncated {
        text.push_str(&format!(
            ". Only the newest {MAX_ENTRIES} are loaded; choose a shorter range or one user to see the rest."
        ));
    }
    text
}

fn loading_or_blank(logs: &State) -> String {
    if logs.load.is_some() {
        "Loading…".to_owned()
    } else {
        String::new()
    }
}

fn toolbar(app: &mut App, ui: &mut Ui, ctx: &egui::Context) {
    let idle = app.logs.load.is_none();
    let mut reload = false;
    ui.horizontal_wrapped(|ui| {
        for kind in [Kind::SignIns, Kind::Audit] {
            if ui
                .selectable_label(app.logs.kind == kind, kind.label())
                .clicked()
                && app.logs.kind != kind
            {
                app.logs.kind = kind;
                app.logs.selected = None;
            }
        }
        ui.separator();
        egui::ComboBox::from_id_salt("log-range")
            .selected_text(app.logs.query.range.label())
            .show_ui(ui, |ui| {
                for range in Range::ALL {
                    ui.selectable_value(&mut app.logs.query.range, range, range.label());
                }
            });
        let hint = match app.logs.kind {
            Kind::SignIns => "Everyone, or one sign-in name",
            Kind::Audit => "Changes by anyone, or by one sign-in name",
        };
        let user = ui.add(
            egui::TextEdit::singleline(&mut app.logs.query.user)
                .hint_text(hint)
                .desired_width(260.0),
        );
        let user = ui::named(user, "Sign-in name to filter by");
        if user.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
            reload = true;
        }
        ui.checkbox(&mut app.logs.query.failures_only, "Failures only");
        if ui::tool_button(ui, idle, "Load").clicked() {
            reload = true;
        }
        ui.separator();
        let shown = match app.logs.kind {
            Kind::SignIns => app.logs.shown_sign_ins().len(),
            Kind::Audit => app.logs.shown_audits().len(),
        };
        if ui::tool_button(ui, shown > 0, &format!("Export {shown} shown…")).clicked() {
            export_csv(app);
        }
    });
    if reload && idle {
        load(app, ctx);
    }
}

fn export_csv(app: &mut App) {
    let name = match app.logs.kind {
        Kind::SignIns => "sign-ins.csv",
        Kind::Audit => "audit-log.csv",
    };
    let Some(path) = rfd::FileDialog::new()
        .set_file_name(name)
        .add_filter("CSV", &["csv"])
        .save_file()
    else {
        return;
    };
    let (written, count) = match app.logs.kind {
        Kind::SignIns => {
            let rows = app.logs.shown_sign_ins();
            (csvio::write_sign_ins(&path, &rows), rows.len())
        }
        Kind::Audit => {
            let rows = app.logs.shown_audits();
            (csvio::write_audits(&path, &rows), rows.len())
        }
    };
    match written {
        Ok(()) => app.report_ok(format!(
            "Exported {count} entries to {}.",
            crate::config::tilde(&path)
        )),
        Err(err) => app.report_error(format!("Could not export: {err}")),
    }
}

/// Clicking the selected row again clears the selection, as in the other
/// panes.
fn toggle(selected: &mut Option<String>, id: String) {
    *selected = if selected.as_deref() == Some(id.as_str()) {
        None
    } else {
        Some(id)
    };
}

fn outcome_label(ui: &mut Ui, ok: bool, text: &str) {
    if ok {
        ui::cell_text(ui, text);
    } else {
        ui.add(
            egui::Label::new(RichText::new(text).color(ui::bad_colour(ui)))
                .truncate()
                .selectable(false),
        );
    }
}

fn sign_ins_view(app: &mut App, ui: &mut Ui) {
    let selected = app
        .logs
        .selected
        .as_deref()
        .and_then(|id| app.logs.shown_sign_ins().into_iter().find(|s| s.id == id).cloned());
    if let Some(entry) = &selected {
        egui::Panel::right("sign-in-details")
            .resizable(true)
            .default_size(340.0)
            .min_size(280.0)
            .show(ui, |ui| sign_in_details(ui, entry));
    }

    let rows = app.logs.shown_sign_ins();
    let selected_index = selected
        .as_ref()
        .and_then(|s| rows.iter().position(|r| r.id == s.id));
    let clicked = ui::select_table(
        ui,
        "sign-ins",
        &[
            ("Time (UTC)", Column::initial(150.0).at_least(80.0)),
            ("User", Column::initial(220.0).at_least(80.0)),
            ("App", Column::initial(170.0).at_least(60.0)),
            ("Result", Column::initial(200.0).at_least(60.0)),
            ("IP address", Column::initial(120.0).at_least(60.0)),
            ("Location", Column::remainder().at_least(80.0)),
        ],
        rows.len(),
        selected_index,
        |row, column, ui| {
            let s = rows[row];
            match column {
                0 => ui::cell_text(ui, &log_time(s.created_date_time.as_deref())),
                1 => ui::cell_text(ui, s.user()),
                2 => ui::cell_text(ui, s.app()),
                3 => outcome_label(ui, s.succeeded(), &s.outcome()),
                4 => ui::cell_text(ui, s.ip_address.as_deref().unwrap_or("")),
                _ => ui::cell_text(ui, &s.place()),
            }
        },
    );
    let clicked = clicked.map(|row| rows[row].id.clone());
    if let Some(id) = clicked {
        toggle(&mut app.logs.selected, id);
    }
}

fn sign_in_details(ui: &mut Ui, s: &SignIn) {
    let t = |v: &Option<String>| v.clone().unwrap_or_default();
    let yes_no = |v: Option<bool>| match v {
        Some(true) => "Yes",
        Some(false) => "No",
        None => "",
    };
    egui::ScrollArea::vertical().show(ui, |ui| {
        ui.add_space(8.0);
        ui.heading(s.user());
        ui.add_space(4.0);
        let colour = if s.succeeded() {
            ui::good_colour(ui)
        } else {
            ui::bad_colour(ui)
        };
        ui.label(RichText::new(s.outcome()).color(colour));
        if let Some(more) = s.status.additional_details.as_deref().filter(|d| !d.is_empty()) {
            ui.label(RichText::new(more).size(13.0).weak());
        }
        ui.add_space(8.0);
        ui::property(ui, "Time (UTC)", &log_time(s.created_date_time.as_deref()));
        ui::property(ui, "Name", &t(&s.user_display_name));
        ui::property(ui, "App", s.app());
        ui::property(ui, "Resource", &t(&s.resource_display_name));
        ui::property(ui, "Client app", &t(&s.client_app_used));
        ui::property(ui, "Interactive", yes_no(s.is_interactive));
        ui::property(ui, "Conditional Access", &t(&s.conditional_access_status));
        ui::property(ui, "Risk during sign-in", &t(&s.risk_level_during_sign_in));
        ui::property(ui, "IP address", &t(&s.ip_address));
        ui::property(ui, "Location", &s.place());

        ui.add_space(8.0);
        ui.label(RichText::new("Device").strong());
        let d = &s.device_detail;
        ui::property(ui, "Name", &t(&d.display_name));
        ui::property(ui, "Operating system", &t(&d.operating_system));
        ui::property(ui, "Browser", &t(&d.browser));
        ui::property(ui, "Compliant", yes_no(d.is_compliant));
        ui::property(ui, "Managed", yes_no(d.is_managed));
        ui::property(ui, "Join type", &t(&d.trust_type));

        ui.add_space(8.0);
        ui::property(ui, "Correlation ID", &t(&s.correlation_id));
        ui::property(ui, "Sign-in ID", &s.id);
    });
}

fn audits_view(app: &mut App, ui: &mut Ui) {
    let selected = app
        .logs
        .selected
        .as_deref()
        .and_then(|id| app.logs.shown_audits().into_iter().find(|a| a.id == id).cloned());
    if let Some(entry) = &selected {
        egui::Panel::right("audit-details")
            .resizable(true)
            .default_size(360.0)
            .min_size(280.0)
            .show(ui, |ui| audit_details(ui, entry));
    }

    let rows = app.logs.shown_audits();
    let selected_index = selected
        .as_ref()
        .and_then(|a| rows.iter().position(|r| r.id == a.id));
    let clicked = ui::select_table(
        ui,
        "audits",
        &[
            ("Time (UTC)", Column::initial(150.0).at_least(80.0)),
            ("Activity", Column::initial(210.0).at_least(80.0)),
            ("Category", Column::initial(140.0).at_least(60.0)),
            ("Initiated by", Column::initial(200.0).at_least(60.0)),
            ("Target", Column::initial(200.0).at_least(60.0)),
            ("Result", Column::remainder().at_least(60.0)),
        ],
        rows.len(),
        selected_index,
        |row, column, ui| {
            let a = rows[row];
            match column {
                0 => ui::cell_text(ui, &log_time(a.activity_date_time.as_deref())),
                1 => ui::cell_text(ui, a.activity()),
                2 => ui::cell_text(ui, a.category.as_deref().unwrap_or("")),
                3 => ui::cell_text(ui, &a.initiator()),
                4 => ui::cell_text(ui, &a.target()),
                _ => outcome_label(ui, a.succeeded(), a.result.as_deref().unwrap_or("")),
            }
        },
    );
    let clicked = clicked.map(|row| rows[row].id.clone());
    if let Some(id) = clicked {
        toggle(&mut app.logs.selected, id);
    }
}

fn audit_details(ui: &mut Ui, a: &DirectoryAudit) {
    let t = |v: &Option<String>| v.clone().unwrap_or_default();
    egui::ScrollArea::vertical().show(ui, |ui| {
        ui.add_space(8.0);
        ui.heading(a.activity());
        ui.add_space(4.0);
        let colour = if a.succeeded() {
            ui::good_colour(ui)
        } else {
            ui::bad_colour(ui)
        };
        let result = match a.result_reason.as_deref().filter(|r| !r.is_empty()) {
            Some(reason) => format!("{}: {reason}", t(&a.result)),
            None => t(&a.result),
        };
        ui.label(RichText::new(result).color(colour));
        ui.add_space(8.0);
        ui::property(ui, "Time (UTC)", &log_time(a.activity_date_time.as_deref()));
        ui::property(ui, "Category", &t(&a.category));
        ui::property(ui, "Service", &t(&a.logged_by_service));
        ui::property(ui, "Operation", &t(&a.operation_type));
        ui::property(ui, "Initiated by", &a.initiator());
        if let Some(ip) = a.initiated_by.user.as_ref().and_then(|u| u.ip_address.clone()) {
            ui::property(ui, "From IP address", &ip);
        }

        for target in &a.target_resources {
            ui.add_space(8.0);
            let kind = target.kind.as_deref().unwrap_or("Target");
            ui.label(RichText::new(format!("{kind}: {}", target.name().unwrap_or("(no name)"))).strong());
            if let Some(id) = &target.id {
                ui.label(RichText::new(id).size(12.0).weak());
            }
            for p in &target.modified_properties {
                ui.add_space(2.0);
                ui.label(RichText::new(p.display_name.as_deref().unwrap_or("?")).size(12.0).weak());
                let old = p.old_value.as_deref().filter(|v| !v.is_empty() && *v != "[]");
                let new = p.new_value.as_deref().filter(|v| !v.is_empty() && *v != "[]");
                let text = format!("{} → {}", old.unwrap_or("—"), new.unwrap_or("—"));
                ui.add(egui::Label::new(RichText::new(text).size(13.0)).selectable(true).wrap());
            }
        }

        let details: Vec<_> = a
            .additional_details
            .iter()
            .filter(|d| d.value.as_deref().is_some_and(|v| !v.is_empty()))
            .collect();
        if !details.is_empty() {
            ui.add_space(8.0);
            ui.label(RichText::new("Details").strong());
            for d in details {
                ui::property(ui, d.key.as_deref().unwrap_or(""), d.value.as_deref().unwrap_or(""));
            }
        }

        ui.add_space(8.0);
        ui::property(ui, "Correlation ID", &t(&a.correlation_id));
        ui::property(ui, "Audit ID", &a.id);
    });
}
