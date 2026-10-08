//! Application state and the two-pane frame: tabs down the left, the selected
//! pane on the right, and a status bar along the bottom — the same frame as
//! watchspend.

use eframe::CreationContext;
use egui::{Align, Layout, RichText};

use crate::config::Config;
use crate::graph::{Graph, Session};
use crate::ui;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tab {
    Connection,
    Users,
    Groups,
    Devices,
    Licensing,
    Logs,
    Servers,
    Export,
    Settings,
}

impl Tab {
    pub const ALL: [Self; 9] = [
        Self::Connection,
        Self::Users,
        Self::Groups,
        Self::Devices,
        Self::Licensing,
        Self::Logs,
        Self::Servers,
        Self::Export,
        Self::Settings,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Self::Connection => "Connection",
            Self::Users => "Users",
            Self::Groups => "Groups",
            Self::Devices => "Devices",
            Self::Licensing => "Licensing",
            Self::Logs => "Logs",
            Self::Servers => "Servers",
            Self::Export => "Export",
            Self::Settings => "Settings",
        }
    }
}

/// The last thing that happened, shown along the bottom of the window.
pub struct Status {
    pub message: String,
    pub good: bool,
}

/// Something the user should read and acknowledge, such as a missing
/// permission or licence, shown in a box over the window rather than in the
/// status bar, where it is easily missed and soon replaced.
pub struct Notice {
    pub title: String,
    pub message: String,
}

pub struct App {
    pub config: Config,
    pub tab: Tab,
    pub status: Option<Status>,
    pub notice: Option<Notice>,
    /// The signed-in client, once a sign-in has worked.
    pub graph: Option<Graph>,
    pub session: Option<Session>,
    pub connection: ui::connection::State,
    pub users: ui::users::State,
    pub groups: ui::groups::State,
    pub devices: ui::devices::State,
    pub licensing: ui::licensing::State,
    pub logs: ui::logs::State,
    pub servers: ui::servers::State,
    pub export: ui::export::State,
    pub update: ui::update::State,
    cues: crate::sound::Cues,
}

impl App {
    pub fn new(cc: &CreationContext<'_>) -> Self {
        let config = Config::load();
        let mut status = None;
        if config.debug_logging
            && let Err(err) = crate::logging::set_debug(true)
        {
            status = Some(Status {
                message: err,
                good: false,
            });
        }
        crate::theme::apply(&cc.egui_ctx);
        crate::theme::apply_appearance(&cc.egui_ctx, config.appearance);

        let mut app = Self {
            tab: Tab::Connection,
            status,
            notice: None,
            graph: None,
            session: None,
            connection: ui::connection::State::from_config(&config),
            users: ui::users::State::default(),
            groups: ui::groups::State::default(),
            devices: ui::devices::State::default(),
            licensing: ui::licensing::State::default(),
            logs: ui::logs::State::default(),
            servers: ui::servers::State::from_config(&config),
            export: ui::export::State::from_config(&config),
            update: ui::update::State::default(),
            cues: crate::sound::Cues::default(),
            config,
        };
        // A remembered secret means the user asked to be signed in without
        // being asked again, so do that now, in the background.
        ui::connection::sign_in_at_start(&mut app, &cc.egui_ctx);
        if app.config.check_for_updates {
            ui::update::check(&mut app, &cc.egui_ctx, false);
        }
        app
    }

    /// A new tenant, or none: everything loaded from the old one goes,
    /// except the last import's results, which may hold the only copy of
    /// the passwords it generated and stay until they are dismissed.
    pub fn forget_directory(&mut self) {
        let results = self.users.take_results();
        self.users = ui::users::State::default();
        self.users.restore_results(results);
        self.groups = ui::groups::State::default();
        self.devices = ui::devices::State::default();
        self.licensing = ui::licensing::State::default();
        self.logs = ui::logs::State::default();
    }

    pub fn report_ok(&mut self, message: impl Into<String>) {
        let message = message.into();
        log::info!("{message}");
        self.cue(crate::sound::Cue::Success);
        self.status = Some(Status {
            message,
            good: true,
        });
    }

    pub fn report_error(&mut self, message: impl Into<String>) {
        let message = message.into();
        log::warn!("{message}");
        self.cue(crate::sound::Cue::Failure);
        self.status = Some(Status {
            message,
            good: false,
        });
    }

    /// Play the success or failure sound, unless sounds are switched off.
    fn cue(&self, cue: crate::sound::Cue) {
        if self.config.sounds {
            self.cues.play(cue);
        }
    }

    /// Show `message` in an information box until the user dismisses it.
    pub fn inform(&mut self, title: impl Into<String>, message: impl Into<String>) {
        let message = message.into();
        log::warn!("{message}");
        self.notice = Some(Notice {
            title: title.into(),
            message,
        });
    }

    fn notice_modal(&mut self, ctx: &egui::Context) {
        let Some(notice) = &self.notice else {
            return;
        };
        let id = egui::Id::new("notice");
        let mut close = false;
        let modal = egui::Modal::new(id).show(ctx, |ui| {
            ui.set_width(380.0);
            ui.heading(&notice.title);
            ui.add_space(4.0);
            ui.label(RichText::new(&notice.message).size(13.0));
            ui.add_space(14.0);
            let ok = ui.add_sized([ui.available_width(), 36.0], egui::Button::new(ui::centred("OK")));
            ui::focus_on_open(ui, id, &ok);
            close = ok.clicked();
        });
        if close || modal.should_close() {
            self.notice = None;
        }
    }

    /// Whatever is running in the background, for the status bar.
    fn activity(&self) -> Option<String> {
        self.connection
            .activity()
            .or_else(|| self.users.activity())
            .or_else(|| self.groups.activity())
            .or_else(|| self.devices.activity())
            .or_else(|| self.licensing.activity())
            .or_else(|| self.logs.activity())
            .or_else(|| self.servers.activity())
            .or_else(|| self.export.activity())
            .or_else(|| self.update.activity())
    }

    fn tab_strip(&mut self, ui: &mut egui::Ui) {
        ui.add_space(18.0);

        for tab in Tab::ALL {
            let selected = self.tab == tab;
            let button = egui::Button::selectable(selected, ui::centred(tab.title()))
                .corner_radius(6.0)
                .min_size(egui::vec2(ui.available_width(), 44.0));
            if ui.add(button).clicked() && self.tab != tab {
                log::debug!("tab: {}", tab.title());
                self.tab = tab;
            }
            ui.add_space(6.0);
        }

        ui.with_layout(Layout::bottom_up(Align::Min), |ui| {
            ui.add_space(12.0);
            ui.label(
                RichText::new(concat!("mainstone ", env!("CARGO_PKG_VERSION")))
                    .size(12.0)
                    .weak(),
            );
        });
    }

    fn status_bar(&self, ui: &mut egui::Ui) {
        ui.add_space(4.0);
        egui::Sides::new().shrink_left().show(
            ui,
            |ui| {
                let where_it_is = match (&self.graph, &self.session) {
                    (Some(graph), Some(session)) => format!(
                        "Signed in to {} ({})",
                        session.organisation.as_deref().unwrap_or("tenant"),
                        graph.tenant_id()
                    ),
                    _ => "Not signed in".to_owned(),
                };
                ui.add(egui::Label::new(RichText::new(where_it_is).size(12.0).weak()).truncate());
            },
            |ui| {
                if let Some(activity) = self.activity() {
                    ui.label(RichText::new(activity).size(12.0).weak());
                    ui.spinner();
                } else if let Some(status) = &self.status {
                    let colour = if status.good {
                        ui::good_colour(ui)
                    } else {
                        ui::bad_colour(ui)
                    };
                    ui.label(RichText::new(&status.message).size(12.0).color(colour));
                }
            },
        );
        ui.add_space(4.0);
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        crate::theme::apply_appearance(&ctx, self.config.appearance);
        ui::shortcuts::handle(self, &ctx);

        // Answers are collected whichever tab is showing, so a load started on
        // one tab is not lost by moving to another.
        ui::connection::poll(self, &ctx);
        ui::update::poll(self, &ctx);
        ui::users::poll(self);
        ui::groups::poll(self);
        ui::devices::poll(self);
        ui::licensing::poll(self);
        ui::logs::poll(self);
        ui::servers::poll(self);
        ui::export::poll(self);

        egui::Panel::left("tabs")
            .resizable(false)
            .exact_size(190.0)
            .show(ui, |ui| self.tab_strip(ui));

        egui::Panel::bottom("status").show(ui, |ui| self.status_bar(ui));

        egui::CentralPanel::default().show(ui, |ui| {
            ui::update::banner(self, ui);
            match self.tab {
                Tab::Connection => ui::connection::show(self, ui),
                Tab::Users => ui::users::show(self, ui),
                Tab::Groups => ui::groups::show(self, ui),
                Tab::Devices => ui::devices::show(self, ui),
                Tab::Licensing => ui::licensing::show(self, ui),
                Tab::Logs => ui::logs::show(self, ui),
                Tab::Servers => ui::servers::show(self, ui),
                Tab::Export => ui::export::show(self, ui),
                Tab::Settings => ui::settings::show(self, ui),
            }
        });

        // Modals are drawn last, over everything.
        ui::users::modals(self, &ctx);
        ui::groups::modals(self, &ctx);
        ui::devices::modals(self, &ctx);
        ui::licensing::modals(self, &ctx);
        ui::servers::modals(self, &ctx);
        self.notice_modal(&ctx);

        // Anything copied from a right-click menu this frame.
        if let Some(message) = ui::take_copied(&ctx) {
            self.report_ok(message);
        }
    }
}
