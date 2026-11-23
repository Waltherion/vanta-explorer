//! F1: the keyboard shortcuts, and which build this is and when it was made. No button for it:
//! the toolbar stays as it is, and F1 is where help is everywhere else.

use std::time::{Duration, UNIX_EPOCH};

const COMMIT: &str = env!("VANTA_EXPLORER_COMMIT");
const COMMIT_TIME: &str = env!("VANTA_EXPLORER_COMMIT_TIME");
const BUILT: &str = env!("VANTA_EXPLORER_BUILT");

/// Kept by hand next to the code that handles each key; check it here when one changes.
const GROUPS: &[(&str, &[(&str, &str)])] = &[
    (
        "Moving around",
        &[
            ("Arrows, Page Up/Down, Home/End", "Move the selection (with Shift: extend it)"),
            ("Enter", "Open"),
            ("Backspace, Alt+Up", "Parent folder"),
            ("Alt+Left, Alt+Right", "Back, forward"),
            ("Alt+Home", "Home folder"),
            ("Ctrl+L", "Type a path"),
            ("F5", "Reload"),
            ("Typing", "Filter this folder by name"),
            ("Ctrl+F", "Search the subfolders too"),
            ("Escape", "Clear the filter"),
        ],
    ),
    (
        "Files",
        &[
            ("Ctrl+C, Ctrl+X, Ctrl+V", "Copy, cut, paste (also between windows)"),
            ("Ctrl+Alt+C, Ctrl+Shift+P", "Copy the path as text"),
            ("Ctrl+A", "Select all"),
            ("F2", "Rename"),
            ("Ctrl+Shift+N", "New folder"),
            ("Delete", "Move to the trash"),
            ("Shift+Delete", "Delete for good (asks first)"),
            ("Ctrl+Z, Ctrl+Y", "Undo, redo"),
            ("Alt+Enter", "Properties (of the folder, when nothing is marked)"),
            ("Ctrl+E", "Edit a text file in the preview; Ctrl+S saves, Escape stops"),
            ("Drag", "Move; with Ctrl: copy; with the right button: ask"),
        ],
    ),
    (
        "View",
        &[
            ("Space", "Show or hide the preview"),
            ("Ctrl+H", "Show hidden files"),
            ("Ctrl+1, Ctrl+2", "List, grid"),
            ("Ctrl+Plus, Ctrl+Minus, Ctrl+scroll", "Bigger, smaller"),
            ("Ctrl+0", "Default size"),
            ("F9", "Show or hide the sidebar"),
        ],
    ),
    (
        "Tabs and panes",
        &[
            ("Ctrl+T, Ctrl+W", "New tab, close tab"),
            ("Ctrl+Shift+T", "Reopen the last closed tab"),
            ("Ctrl+Page Down, Ctrl+Page Up", "Next, previous tab"),
            ("Ctrl+Tab, Ctrl+Shift+Tab, F6", "Next, previous pane"),
            ("F3, Shift+F3", "Split side by side, one above the other; again: close the pane"),
        ],
    ),
    ("Other", &[("F4", "Terminal in this folder"), ("F1", "This window")]),
];

const OPEN: &str = "help-open";

/// F1 opens and closes it.
pub fn handle_key(ctx: &egui::Context) {
    if ctx.input(|i| i.key_pressed(egui::Key::F1)) {
        let open = is_open(ctx);
        ctx.data_mut(|d| d.insert_temp(egui::Id::new(OPEN), !open));
    }
}

pub fn is_open(ctx: &egui::Context) -> bool {
    ctx.data(|d| d.get_temp::<bool>(egui::Id::new(OPEN)).unwrap_or(false))
}

fn when(secs: &str, date_format: &str) -> Option<String> {
    let secs: u64 = secs.parse().ok().filter(|s| *s > 0)?;
    Some(crate::model::format_mtime(Some(UNIX_EPOCH + Duration::from_secs(secs)), date_format))
}

pub fn show(ctx: &egui::Context, theme: &crate::browser::Theme, date_format: &str) {
    if !is_open(ctx) {
        return;
    }
    let resp = egui::Modal::new(egui::Id::new("help")).show(ctx, |ui| {
        ui.set_max_width(560.0);
        // The window is drawn in egui's own (dark) style, like every menu, whatever the theme;
        // the theme's text color would be dark on it with a light theme.
        let text = ui.visuals().strong_text_color();
        ui.heading(egui::RichText::new("Vanta Explorer").color(theme.accent));
        let mut version = format!("{} (commit {COMMIT}", env!("CARGO_PKG_VERSION"));
        if let Some(t) = when(COMMIT_TIME, date_format) {
            version.push_str(&format!(", from {t}"));
        }
        version.push(')');
        ui.label(egui::RichText::new(version).color(text.gamma_multiply(0.7)));
        if let Some(t) = when(BUILT, date_format) {
            ui.label(egui::RichText::new(format!("Built and installed {t}")).color(text.gamma_multiply(0.7)));
        }
        ui.add_space(8.0);
        egui::ScrollArea::vertical().max_height(ctx.content_rect().height() * 0.7).show(ui, |ui| {
            for (title, keys) in GROUPS {
                ui.add_space(6.0);
                ui.label(egui::RichText::new(*title).color(theme.accent).strong());
                egui::Grid::new(title).num_columns(2).spacing([24.0, 4.0]).show(ui, |ui| {
                    for (key, what) in *keys {
                        ui.label(egui::RichText::new(*key).color(text));
                        ui.label(egui::RichText::new(*what).color(text.gamma_multiply(0.75)));
                        ui.end_row();
                    }
                });
            }
        });
        ui.add_space(8.0);
        ui.label(egui::RichText::new("F1 or Escape closes this.").color(text.gamma_multiply(0.5)));
    });
    if resp.should_close() {
        ctx.data_mut(|d| d.insert_temp(egui::Id::new(OPEN), false));
    }
}
