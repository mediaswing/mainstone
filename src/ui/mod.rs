//! The panes, and the widget helpers they share. The helpers come from
//! watchspend, so the two apps look and behave alike.

pub mod connection;
pub mod devices;
pub mod export;
pub mod groups;
pub mod licensing;
pub mod logs;
pub mod settings;
pub mod update;
pub mod users;

use egui::{Atom, Color32, Id, Response, RichText, Ui, Widget as _};

/// Height of the buttons that span a whole pane.
pub const WIDE_BUTTON_HEIGHT: f32 = 40.0;

/// Label for a button, padded either side so the text sits in the middle
/// rather than against the left edge. A button that spans the pane is much
/// wider than its words, and left-aligned words in a wide button read as the
/// start of a list rather than as the button's name.
pub fn centred<'a>(text: impl Into<egui::WidgetText>) -> (Atom<'a>, Atom<'a>, Atom<'a>) {
    (Atom::grow(), text.into().into(), Atom::grow())
}

/// A button that fills the width of whatever it is placed in — which is what
/// the design asks for in several places, and what makes the primary action of
/// a pane unmissable.
pub fn wide_button(ui: &mut Ui, text: &str) -> Response {
    egui::Button::new(centred(RichText::new(text).size(16.0)))
        .corner_radius(6.0)
        .min_size(egui::vec2(ui.available_width(), WIDE_BUTTON_HEIGHT))
        .ui(ui)
}

/// A single-line text field that fills the width of the pane, under its label.
///
/// The label is tied to the field rather than merely drawn above it. A caption
/// and a box that only *look* related are related to a sighted reader and to
/// nobody else: without the link, every field reaches a screen reader as an
/// unnamed edit box. See also [`named`], for the controls whose face
/// is a symbol.
pub fn labelled_field(ui: &mut Ui, label: &str, value: &mut String, hint: &str) -> Response {
    let caption = ui.label(RichText::new(label).size(13.0));
    let response = egui::TextEdit::singleline(value)
        .hint_text(hint)
        .desired_width(f32::INFINITY)
        .margin(egui::vec2(8.0, 6.0))
        .ui(ui)
        .labelled_by(caption.id);
    ui.add_space(6.0);
    response
}

/// The same, for text that runs to several lines.
pub fn labelled_multiline(ui: &mut Ui, label: &str, value: &mut String, hint: &str) -> Response {
    let caption = ui.label(RichText::new(label).size(13.0));
    let response = egui::TextEdit::multiline(value)
        .hint_text(hint)
        .desired_width(f32::INFINITY)
        .desired_rows(4)
        .margin(egui::vec2(8.0, 6.0))
        .ui(ui)
        .labelled_by(caption.id);
    ui.add_space(6.0);
    response
}

/// The same, for text that should not be shown as it is typed.
pub fn labelled_password(ui: &mut Ui, label: &str, value: &mut String) -> Response {
    let caption = ui.label(RichText::new(label).size(13.0));
    let response = egui::TextEdit::singleline(value)
        .password(true)
        .desired_width(f32::INFINITY)
        .margin(egui::vec2(8.0, 6.0))
        .ui(ui)
        .labelled_by(caption.id);
    ui.add_space(6.0);
    response
}

/// Give a control an accessible name of its own, without changing what is on
/// screen.
///
/// A button whose face is `◀` or `✕` announces itself as the name of that
/// character — "black left-pointing triangle" — which is not what it does. The
/// tooltip beside it is the sentence a pointer gets; this is the same sentence
/// for everyone else. For anything with a caption drawn next to it, prefer
/// `Response::labelled_by`: the words are then on screen as well.
pub fn named(response: Response, name: &str) -> Response {
    response.widget_info(|| {
        egui::WidgetInfo::labeled(egui::WidgetType::Button, response.enabled(), name)
    });
    response
}

/// Green for something that worked, dark enough to read on a light background
/// and light enough to read on a dark one.
///
/// A single colour cannot serve both themes: the dark greens and reds that
/// look right on white fall to around 2.5:1 against a dark background, which is
/// below any reasonable contrast floor and unreadable for some people outright.
/// Both live in [`crate::theme`] now, beside the surfaces they were measured
/// against.
pub fn good_colour(ui: &Ui) -> Color32 {
    crate::theme::palette(ui.visuals()).ok
}

/// Red for something that did not, chosen the same way.
pub fn bad_colour(ui: &Ui) -> Color32 {
    crate::theme::palette(ui.visuals()).bad
}

pub fn error_text(ui: &mut Ui, message: &str) {
    let colour = bad_colour(ui);
    ui.label(RichText::new(message).color(colour));
}

/// Move keyboard focus to `response` on the first pass a modal is drawn.
///
/// Opening a modal leaves focus on the button that opened it, behind the
/// dimmed background: a keyboard user has to Tab blindly to reach the dialog,
/// and a screen reader says nothing about it at all. Focusing a control
/// inside it takes both of them there. The modal remembers the last pass it
/// was drawn on, so this needs no clearing up when it closes.
pub fn focus_on_open(ui: &Ui, modal: Id, response: &Response) {
    let key = modal.with("last-pass");
    let pass = ui.ctx().cumulative_pass_nr();
    let last: Option<u64> = ui.ctx().data_mut(|d| {
        let last = d.get_temp(key);
        d.insert_temp(key, pass);
        last
    });
    let still_open = last.is_some_and(|last| last + 1 >= pass);
    if !still_open {
        response.request_focus();
    }
}

/// How a confirmation stands on the frame it was drawn.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Confirmation {
    /// Still on screen, still unanswered.
    Waiting,
    /// Closed without going ahead — Cancel, Escape, or a click outside.
    Dismissed,
    /// The destructive button was pressed.
    Confirmed,
}

/// Ask before something that cannot be undone.
///
/// Called only while the caller has something to confirm, so it draws the box
/// every time and reports back which of the three things happened. `Dismissed`
/// and `Confirmed` both mean the box has gone: the caller clears whatever it
/// was holding on to either way, and acts on it only for `Confirmed`.
pub fn confirm_modal(
    ctx: &egui::Context,
    id: Id,
    title: &str,
    body: &str,
    confirm_label: &str,
) -> Confirmation {
    let mut confirmed = false;
    let mut cancel = false;

    let modal = egui::Modal::new(id).show(ctx, |ui| {
        ui.set_width(340.0);
        ui.heading(title);
        ui.add_space(4.0);
        ui.label(RichText::new(body).size(13.0));
        ui.add_space(14.0);
        ui.horizontal(|ui| {
            let width = (ui.available_width() - ui.spacing().item_spacing.x) / 2.0;
            let cancel_button = ui.add_sized(
                [width, 36.0],
                egui::Button::new(centred("Cancel")),
            );
            // The safe choice has focus, so Enter or Space pressed out of
            // habit never deletes or wipes anything.
            focus_on_open(ui, id, &cancel_button);
            if cancel_button.clicked() {
                cancel = true;
            }
            if ui
                .add_sized([width, 36.0], egui::Button::new(centred(confirm_label)))
                .clicked()
            {
                confirmed = true;
            }
        });
    });

    // Confirming wins over closing, so a click on the destructive button is
    // never read as the box merely going away.
    if confirmed {
        Confirmation::Confirmed
    } else if cancel || modal.should_close() {
        Confirmation::Dismissed
    } else {
        Confirmation::Waiting
    }
}

/// A pane heading with a quieter line of context under it.
pub fn pane_header(ui: &mut Ui, title: &str, subtitle: &str) {
    ui.add_space(10.0);
    ui.heading(title);
    if !subtitle.is_empty() {
        ui.label(RichText::new(subtitle).size(13.0).weak());
    }
    ui.add_space(10.0);
}

/// Amber for something worth a second look.
pub fn warn_colour(ui: &Ui) -> Color32 {
    crate::theme::palette(ui.visuals()).warn
}

/// A search box that fills the width it is given.
pub fn search_box(ui: &mut Ui, query: &mut String, hint: &str) -> Response {
    let response = egui::TextEdit::singleline(query)
        .hint_text(hint)
        .desired_width(ui.available_width())
        .margin(egui::vec2(8.0, 6.0))
        .ui(ui);
    named(response, "Search")
}

/// A toolbar button: the ordinary size, but enabled only when it can work.
pub fn tool_button(ui: &mut Ui, enabled: bool, text: &str) -> Response {
    ui.add_enabled(enabled, egui::Button::new(text).corner_radius(6.0))
}

/// One property in a details panel: the name small and weak, the value under
/// it, selectable so it can be copied.
pub fn property(ui: &mut Ui, name: &str, value: &str) {
    ui.label(RichText::new(name).size(12.0).weak());
    let shown = if value.is_empty() { "—" } else { value };
    ui.add(egui::Label::new(RichText::new(shown).size(14.0)).selectable(true).wrap());
    ui.add_space(4.0);
}

/// A line saying something is happening, with a spinner.
pub fn busy(ui: &mut Ui, text: &str) {
    ui.horizontal(|ui| {
        ui.spinner();
        ui.label(RichText::new(text).size(13.0).weak());
    });
}

/// What a pane shows when there is no connection to ask.
pub fn not_connected(ui: &mut Ui) -> bool {
    ui.add_space(20.0);
    ui.label("Not signed in to a tenant yet.");
    ui.add_space(8.0);
    ui.button("Go to Connection").clicked()
}

/// A selectable table: one row per item, clicking a row selects it. Returns
/// the index of the row clicked this frame, if any.
pub fn select_table(
    ui: &mut Ui,
    id_salt: &str,
    columns: &[(&str, egui_extras::Column)],
    rows: usize,
    selected: Option<usize>,
    mut cell: impl FnMut(usize, usize, &mut Ui),
) -> Option<usize> {
    use egui_extras::TableBuilder;

    let mut clicked = None;
    let mut table = TableBuilder::new(ui)
        .id_salt(id_salt)
        .striped(true)
        .resizable(true)
        .sense(egui::Sense::click())
        .auto_shrink([false, false])
        .cell_layout(egui::Layout::left_to_right(egui::Align::Center));
    for (_, column) in columns {
        table = table.column(column.clip(true));
    }
    table
        .header(26.0, |mut header| {
            for (name, _) in columns {
                header.col(|ui| {
                    ui.label(RichText::new(*name).size(13.0).weak());
                });
            }
        })
        .body(|body| {
            body.rows(28.0, rows, |mut row| {
                let index = row.index();
                row.set_selected(selected == Some(index));
                for column in 0..columns.len() {
                    row.col(|ui| cell(index, column, ui));
                }
                if row.response().clicked() {
                    clicked = Some(index);
                }
            });
        });
    clicked
}

/// A table cell's text, cut short rather than wrapped.
pub fn cell_text(ui: &mut Ui, text: &str) {
    ui.add(egui::Label::new(text).truncate().selectable(false));
}

/// Two buttons at the foot of a form, Cancel and the action. Returns which
/// was pressed: `Some(true)` for the action.
pub fn form_buttons(ui: &mut Ui, action: &str, enabled: bool) -> Option<bool> {
    let mut answer = None;
    ui.add_space(10.0);
    ui.horizontal(|ui| {
        let width = (ui.available_width() - ui.spacing().item_spacing.x) / 2.0;
        if ui
            .add_sized([width, 36.0], egui::Button::new(centred("Cancel")))
            .clicked()
        {
            answer = Some(false);
        }
        if ui
            .add_enabled(enabled, egui::Button::new(centred(action)).min_size(egui::vec2(width, 36.0)))
            .clicked()
        {
            answer = Some(true);
        }
    });
    answer
}

/// Whether an item matches every whitespace-separated word of a search,
/// ignoring case, anywhere in the given fields.
pub fn matches_search(terms: &[String], fields: &[&str]) -> bool {
    if terms.is_empty() {
        return true;
    }
    let haystack = fields.join(" ").to_lowercase();
    terms.iter().all(|t| haystack.contains(t.as_str()))
}

pub fn search_terms(query: &str) -> Vec<String> {
    query.split_whitespace().map(str::to_lowercase).collect()
}
