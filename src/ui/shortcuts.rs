//! Keyboard shortcuts, the same as speechout's where the two apps do the same
//! thing: Command on a Mac and Ctrl elsewhere, as egui's `COMMAND` means.
//!
//! They are read at the start of each frame, before any pane is drawn, so
//! the Settings pane's list and what the keys do come from the one table
//! below and cannot drift apart. Nothing behind a dialog or an open menu
//! reacts, and the keys a text box uses for itself (Delete, Escape) are left
//! to it while one is being typed in.

use egui::{Key, KeyboardShortcut, Modifiers};

use crate::app::{App, Tab};
use crate::ui;

/// What a shortcut asks the pane that is showing to do. Each pane decides
/// whether it means anything there.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    /// Put the cursor in the search box.
    Find,
    /// Read the list again.
    Refresh,
    /// Start a new user or group.
    New,
    /// Delete what is selected, after asking.
    Delete,
    /// Clear the selection, closing the details panel.
    Deselect,
}

const fn shortcut(modifiers: Modifiers, key: Key) -> KeyboardShortcut {
    KeyboardShortcut::new(modifiers, key)
}

const CTRL_SHIFT: Modifiers = Modifiers {
    ctrl: true,
    shift: true,
    ..Modifiers::NONE
};

pub const FIND: KeyboardShortcut = shortcut(Modifiers::COMMAND, Key::F);
pub const REFRESH: KeyboardShortcut = shortcut(Modifiers::COMMAND, Key::R);
pub const REFRESH_F5: KeyboardShortcut = shortcut(Modifiers::NONE, Key::F5);
pub const NEW: KeyboardShortcut = shortcut(Modifiers::COMMAND, Key::N);
pub const DELETE: KeyboardShortcut = shortcut(Modifiers::NONE, Key::Delete);
/// What a Mac keyboard, which has no forward Delete key, uses instead.
pub const DELETE_MAC: KeyboardShortcut = shortcut(Modifiers::COMMAND, Key::Backspace);
pub const DESELECT: KeyboardShortcut = shortcut(Modifiers::NONE, Key::Escape);
pub const SETTINGS: KeyboardShortcut = shortcut(Modifiers::COMMAND, Key::Comma);
pub const NEXT_TAB: KeyboardShortcut = shortcut(Modifiers::CTRL, Key::Tab);
pub const PREVIOUS_TAB: KeyboardShortcut = shortcut(CTRL_SHIFT, Key::Tab);

/// Command-1 to Command-9, one for each tab in the order they are listed.
/// There are more tabs than digits; the last, Settings, has Command-comma.
const TAB_KEYS: [Key; 9] = [
    Key::Num1,
    Key::Num2,
    Key::Num3,
    Key::Num4,
    Key::Num5,
    Key::Num6,
    Key::Num7,
    Key::Num8,
    Key::Num9,
];

/// The shortcuts as the Settings pane lists them: the keys, in this
/// platform's spelling, and what they do.
pub fn list(ctx: &egui::Context) -> Vec<(String, &'static str)> {
    let f = |s: &KeyboardShortcut| ctx.format_shortcut(s);
    let first = shortcut(Modifiers::COMMAND, TAB_KEYS[0]);
    let last = shortcut(Modifiers::COMMAND, TAB_KEYS[Tab::ALL.len().min(TAB_KEYS.len()) - 1]);
    vec![
        (format!("{} to {}", f(&first), f(&last)), "Go to one of the first nine tabs, in the order they are listed"),
        (format!("{} / {}", f(&NEXT_TAB), f(&PREVIOUS_TAB)), "Next or previous tab"),
        (f(&SETTINGS), "Settings"),
        (f(&FIND), "Search the list"),
        (format!("{} or {}", f(&REFRESH), f(&REFRESH_F5)), "Refresh the list, load the logs, or take the snapshot again"),
        (f(&NEW), "New user or group"),
        (format!("{} or {}", f(&DELETE), f(&DELETE_MAC)), "Delete the selected user or group, after asking"),
        (f(&DESELECT), "Clear the selection, closing the details panel"),
        ("Enter".to_owned(), "In a form's text box: the form's main button"),
    ]
}

pub fn handle(app: &mut App, ctx: &egui::Context) {
    // Both from the frame before, which is what is on screen now.
    let modal = ctx.memory(|m| m.top_modal_layer().is_some());
    if modal || egui::Popup::is_any_open(ctx) {
        return;
    }
    let pressed = |s: KeyboardShortcut| ctx.input_mut(|i| i.consume_shortcut(&s));

    for (key, tab) in TAB_KEYS.into_iter().zip(Tab::ALL) {
        if pressed(shortcut(Modifiers::COMMAND, key)) {
            go_to(app, tab);
        }
    }
    // Shift first: Ctrl-Tab would also match Ctrl-Shift-Tab.
    let i = Tab::ALL.iter().position(|t| *t == app.tab).unwrap_or(0);
    let count = Tab::ALL.len();
    if pressed(PREVIOUS_TAB) {
        go_to(app, Tab::ALL[(i + count - 1) % count]);
    } else if pressed(NEXT_TAB) {
        go_to(app, Tab::ALL[(i + 1) % count]);
    }
    if pressed(SETTINGS) {
        go_to(app, Tab::Settings);
    }

    if pressed(FIND) {
        command(app, ctx, Command::Find);
    }
    if pressed(REFRESH) || pressed(REFRESH_F5) {
        command(app, ctx, Command::Refresh);
    }
    if pressed(NEW) {
        command(app, ctx, Command::New);
    }

    // Delete and Escape belong to a text box while one is being typed in.
    if ctx.text_edit_focused() {
        return;
    }
    if pressed(DELETE) || pressed(DELETE_MAC) {
        command(app, ctx, Command::Delete);
    }
    // Escape is only taken when there is a selection to clear, so it is
    // otherwise left for whatever else might want it.
    let escape = ctx.input(|i| i.key_pressed(Key::Escape) && i.modifiers.is_none());
    if escape && command(app, ctx, Command::Deselect) {
        pressed(DESELECT);
    }
}

fn go_to(app: &mut App, tab: Tab) {
    if app.tab != tab {
        log::debug!("tab: {} (keyboard)", tab.title());
        app.tab = tab;
    }
}

/// Hand a command to the pane that is showing. Returns whether it meant
/// anything there.
fn command(app: &mut App, ctx: &egui::Context, command: Command) -> bool {
    match app.tab {
        Tab::Users => ui::users::command(app, ctx, command),
        Tab::Groups => ui::groups::command(app, ctx, command),
        Tab::Devices => ui::devices::command(app, ctx, command),
        Tab::Apps => ui::apps::command(app, ctx, command),
        Tab::Licensing => ui::licensing::command(app, ctx, command),
        Tab::Logs => ui::logs::command(app, ctx, command),
        Tab::Servers => ui::servers::command(app, ctx, command),
        Tab::Connection | Tab::Export | Tab::Settings => false,
    }
}
