//! Ctrl+Shift+P: every command by name. A command chosen here is run by pressing its key on
//! the next frame, so it does exactly what the key does, with nothing to keep in step.

use egui::{Key, Modifiers};

/// What the palette presses to run a command.
#[derive(Clone, Copy)]
pub enum Fire {
    Key(Key, Modifiers),
    /// Ctrl+C, X, V reach the app as these events, not as keys (see browser::handle_shortcuts).
    Copy,
    Cut,
    Paste,
    /// Ctrl+Alt+C, which main.rs sees before egui does.
    CopyPath,
    /// Split the pane you are in, however many there are, as the buttons do. F3 splits only
    /// a single pane; with more, it closes the one you are in.
    Split(crate::panes::SplitDir),
}

/// When a command makes sense; otherwise it is shown grayed out.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum When {
    Always,
    /// Something is marked.
    Marked,
    /// Exactly one thing is marked.
    One,
    /// There are files on the clipboard.
    Clipboard,
    /// There is more than one pane.
    Panes,
    Undo,
    Redo,
}

pub struct Command {
    pub label: &'static str,
    /// As F1 writes it.
    pub keys: &'static str,
    pub fire: Fire,
    pub when: When,
}

const NONE: Modifiers = Modifiers::NONE;
const CTRL: Modifiers = Modifiers { ctrl: true, command: true, ..Modifiers::NONE };
const CTRL_SHIFT: Modifiers = Modifiers { ctrl: true, command: true, shift: true, ..Modifiers::NONE };
const ALT: Modifiers = Modifiers::ALT;
const SHIFT: Modifiers = Modifiers::SHIFT;

const fn cmd(label: &'static str, keys: &'static str, fire: Fire, when: When) -> Command {
    Command { label, keys, fire, when }
}

const fn key(k: Key, m: Modifiers) -> Fire {
    Fire::Key(k, m)
}

pub const COMMANDS: &[Command] = &[
    cmd("Open", "Enter", key(Key::Enter, NONE), When::Marked),
    cmd("Parent folder", "Alt+Up", key(Key::ArrowUp, ALT), When::Always),
    cmd("Back", "Alt+Left", key(Key::ArrowLeft, ALT), When::Always),
    cmd("Forward", "Alt+Right", key(Key::ArrowRight, ALT), When::Always),
    cmd("Home folder", "Alt+Home", key(Key::Home, ALT), When::Always),
    cmd("Type a path", "Ctrl+L", key(Key::L, CTRL), When::Always),
    cmd("Reload", "F5", key(Key::F5, NONE), When::Always),
    cmd("Search the subfolders too", "Ctrl+F", key(Key::F, CTRL), When::Always),
    cmd("Copy", "Ctrl+C", Fire::Copy, When::Marked),
    cmd("Cut", "Ctrl+X", Fire::Cut, When::Marked),
    cmd("Paste", "Ctrl+V", Fire::Paste, When::Clipboard),
    cmd("Copy the path as text", "Ctrl+Alt+C", Fire::CopyPath, When::Marked),
    cmd("Select all", "Ctrl+A", key(Key::A, CTRL), When::Always),
    cmd("Rename", "F2", key(Key::F2, NONE), When::One),
    cmd("New folder", "Ctrl+Shift+N", key(Key::N, CTRL_SHIFT), When::Always),
    cmd("Move to the trash", "Delete", key(Key::Delete, NONE), When::Marked),
    cmd("Delete for good", "Shift+Delete", key(Key::Delete, SHIFT), When::Marked),
    cmd("Undo", "Ctrl+Z", key(Key::Z, CTRL), When::Undo),
    cmd("Redo", "Ctrl+Y", key(Key::Y, CTRL), When::Redo),
    cmd("Properties", "Alt+Enter", key(Key::Enter, ALT), When::Always),
    cmd("Edit the text file in the preview", "Ctrl+E", key(Key::E, CTRL), When::One),
    cmd("Show or hide the preview", "Space", key(Key::Space, NONE), When::Always),
    cmd("Show hidden files", "Ctrl+H", key(Key::H, CTRL), When::Always),
    cmd("List view", "Ctrl+1", key(Key::Num1, CTRL), When::Always),
    cmd("Grid view", "Ctrl+2", key(Key::Num2, CTRL), When::Always),
    cmd("Bigger", "Ctrl+Plus", key(Key::Plus, CTRL), When::Always),
    cmd("Smaller", "Ctrl+Minus", key(Key::Minus, CTRL), When::Always),
    cmd("Default size", "Ctrl+0", key(Key::Num0, CTRL), When::Always),
    cmd("Show or hide the sidebar", "F9", key(Key::F9, NONE), When::Always),
    cmd("New tab", "Ctrl+T", key(Key::T, CTRL), When::Always),
    cmd("Close tab", "Ctrl+W", key(Key::W, CTRL), When::Always),
    cmd("Reopen the last closed tab", "Ctrl+Shift+T", key(Key::T, CTRL_SHIFT), When::Always),
    cmd("Next tab", "Ctrl+Page Down", key(Key::PageDown, CTRL), When::Always),
    cmd("Previous tab", "Ctrl+Page Up", key(Key::PageUp, CTRL), When::Always),
    cmd("Next pane", "Ctrl+Tab", key(Key::F6, NONE), When::Panes),
    cmd("Previous pane", "Ctrl+Shift+Tab", key(Key::F6, SHIFT), When::Panes),
    cmd("Split side by side", "F3", Fire::Split(crate::panes::SplitDir::Horizontal), When::Always),
    cmd("Split one above the other", "Shift+F3", Fire::Split(crate::panes::SplitDir::Vertical), When::Always),
    cmd("Close this pane", "F3", key(Key::F3, NONE), When::Panes),
    cmd("Terminal in this folder", "F4", key(Key::F4, NONE), When::Always),
    cmd("Keyboard shortcuts", "F1", key(Key::F1, NONE), When::Always),
];

/// What the commands' `When` is judged by.
pub struct State {
    pub marked: usize,
    pub clipboard: bool,
    pub panes: usize,
    pub undo: bool,
    pub redo: bool,
}

impl When {
    /// Said in place of the key when the command cannot run, so the gray is explained.
    fn why_not(self) -> &'static str {
        match self {
            When::Always => "",
            When::Marked => "mark something first",
            When::One => "mark one thing first",
            When::Clipboard => "nothing to paste",
            When::Panes => "there is only one pane",
            When::Undo => "nothing to undo",
            When::Redo => "nothing to redo",
        }
    }
}

impl State {
    fn allows(&self, w: When) -> bool {
        match w {
            When::Always => true,
            When::Marked => self.marked > 0,
            When::One => self.marked == 1,
            When::Clipboard => self.clipboard,
            When::Panes => self.panes > 1,
            When::Undo => self.undo,
            When::Redo => self.redo,
        }
    }
}

#[derive(Default)]
pub struct Palette {
    query: String,
    /// Index into the current matches.
    cursor: usize,
}

/// Ctrl+Shift+P, which also closes it.
pub fn toggle_pressed(ctx: &egui::Context) -> bool {
    ctx.input(|i| i.modifiers.ctrl && i.modifiers.shift && !i.modifiers.alt && i.key_pressed(Key::P))
}

/// How well `query` matches `text`: every letter of the query in order, with points for
/// letters at the start of a word and for letters that follow each other. Spaces in the query
/// are ignored. `None`: no match.
fn score(query: &str, text: &str) -> Option<i32> {
    let text: Vec<char> = text.to_lowercase().chars().collect();
    let mut at = 0;
    let mut last: Option<usize> = None;
    let mut points = 0;
    for q in query.to_lowercase().chars().filter(|c| !c.is_whitespace()) {
        let word_start = |i: usize| i == 0 || !text[i - 1].is_alphanumeric();
        let next = (at..text.len()).find(|&i| text[i] == q)?;
        // Unless it follows the last letter, the same letter at a word start further on is
        // the better match: "hid" is "hidden", not the h in "Show".
        let found = if last.is_some_and(|l| next == l + 1) {
            next
        } else {
            (next..text.len()).find(|&i| text[i] == q && word_start(i)).unwrap_or(next)
        };
        if word_start(found) {
            points += 10;
        }
        if last.is_some_and(|l| found == l + 1) {
            points += 5;
        }
        points -= (found - at) as i32;
        last = Some(found);
        at = found + 1;
    }
    Some(points)
}

/// The commands matching `query`, best first, those that can run before those that cannot.
fn matches(query: &str, state: &State) -> Vec<(&'static Command, bool)> {
    let mut found: Vec<(i32, &'static Command, bool)> = COMMANDS
        .iter()
        .filter_map(|c| {
            let s = if query.trim().is_empty() {
                Some(0)
            } else {
                // The keys count too, below the name: "f3" finds the splits.
                score(query, c.label).or_else(|| score(query, c.keys).map(|s| s - 20))
            }?;
            Some((s, c, state.allows(c.when)))
        })
        .collect();
    // Stable: equal scores keep the list's order.
    found.sort_by_key(|(s, _, ok)| (!ok, -s));
    found.into_iter().map(|(_, c, ok)| (c, ok)).collect()
}

/// Draws the palette. Returns the command to run, and whether it is to close.
pub fn show(ctx: &egui::Context, palette: &mut Palette, state: &State) -> (Option<Fire>, bool) {
    let id = egui::Id::new("palette");
    let area = egui::Modal::default_area(id).anchor(egui::Align2::CENTER_TOP, egui::vec2(0.0, 72.0));
    let mut fire = None;
    let mut close = false;
    let resp = egui::Modal::new(id).area(area).show(ctx, |ui| {
        ui.set_width(520.0);
        let (down, up, enter, escape) = ui.input_mut(|i| {
            (
                i.consume_key(NONE, Key::ArrowDown),
                i.consume_key(NONE, Key::ArrowUp),
                i.consume_key(NONE, Key::Enter),
                i.consume_key(NONE, Key::Escape),
            )
        });
        let edit = ui.add(egui::TextEdit::singleline(&mut palette.query).hint_text("Type a command").desired_width(f32::INFINITY));
        edit.request_focus();
        if edit.changed() {
            palette.cursor = 0;
        }
        let found = matches(&palette.query, state);
        let runnable = found.iter().filter(|(_, ok)| *ok).count();
        let moved = down || up;
        if runnable > 0 {
            if down {
                palette.cursor = (palette.cursor + 1) % runnable;
            } else if up {
                palette.cursor = (palette.cursor + runnable - 1) % runnable;
            }
            palette.cursor = palette.cursor.min(runnable - 1);
        }
        ui.add_space(4.0);
        let text = ui.visuals().strong_text_color();
        let font = egui::TextStyle::Body.resolve(ui.style());
        egui::ScrollArea::vertical().max_height(360.0).auto_shrink([false, true]).show(ui, |ui| {
            if found.is_empty() {
                ui.label(egui::RichText::new("No command by that name").color(text.gamma_multiply(0.5)));
            }
            for (i, (c, ok)) in found.iter().enumerate() {
                let (rect, row) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 26.0), if *ok { egui::Sense::click() } else { egui::Sense::hover() });
                if *ok && row.hovered() && ui.input(|i| i.pointer.delta() != egui::Vec2::ZERO) {
                    palette.cursor = i;
                }
                let current = *ok && i == palette.cursor;
                if current {
                    ui.painter().rect_filled(rect, 4.0, ui.visuals().selection.bg_fill);
                    if moved {
                        ui.scroll_to_rect(rect, None);
                    }
                }
                let ink = if current { ui.visuals().selection.stroke.color } else if *ok { text } else { text.gamma_multiply(0.4) };
                ui.painter().text(rect.left_center() + egui::vec2(8.0, 0.0), egui::Align2::LEFT_CENTER, c.label, font.clone(), ink);
                let right = if *ok { c.keys } else { c.when.why_not() };
                ui.painter().text(rect.right_center() - egui::vec2(8.0, 0.0), egui::Align2::RIGHT_CENTER, right, font.clone(), ink.gamma_multiply(if *ok { 0.6 } else { 1.0 }));
                if row.clicked() {
                    fire = Some(c.fire);
                }
            }
        });
        if enter {
            fire = found.get(palette.cursor).filter(|(_, ok)| *ok).map(|(c, _)| c.fire);
        }
        if escape || fire.is_some() {
            edit.surrender_focus();
            close = true;
        }
    });
    if resp.backdrop_response.clicked() {
        close = true;
    }
    (fire, close)
}

/// A key the palette pressed, to let go of in the next frame.
pub struct Release {
    key: Key,
    /// The modifiers really held when it was pressed.
    held: Modifiers,
}

/// Presses `fire` in this frame's input, before egui sees it. egui keeps the modifiers it is
/// told of and counts a key as held until it is let go, so a pressed key is let go, and the
/// real modifiers told again, in the next frame (`release`).
pub fn press(fire: Fire, raw: &mut egui::RawInput, ctx: &egui::Context) -> Option<Release> {
    let event = match fire {
        Fire::Key(key, modifiers) => {
            let held = ctx.input(|i| i.modifiers);
            raw.events.push(egui::Event::ModifiersChanged(modifiers));
            raw.events.push(egui::Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers });
            return Some(Release { key, held });
        }
        Fire::Copy => egui::Event::Copy,
        Fire::Cut => egui::Event::Cut,
        Fire::Paste => egui::Event::Paste(String::new()),
        Fire::CopyPath => {
            ctx.data_mut(|d| d.insert_temp(egui::Id::new(crate::browser::COPY_PATH_KEY), true));
            return None;
        }
        // Done by main.rs as it is chosen.
        Fire::Split(_) => return None,
    };
    raw.events.push(event);
    None
}

pub fn release(r: Release, raw: &mut egui::RawInput) {
    raw.events.insert(0, egui::Event::Key { key: r.key, physical_key: None, pressed: false, repeat: false, modifiers: r.held });
    raw.events.insert(0, egui::Event::ModifiersChanged(r.held));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> State {
        State { marked: 0, clipboard: false, panes: 1, undo: false, redo: false }
    }

    fn first(query: &str) -> &'static str {
        matches(query, &state()).first().map(|(c, _)| c.label).unwrap_or("")
    }

    #[test]
    fn finds_by_initials_and_words() {
        assert_eq!(first("spl"), "Split side by side");
        assert_eq!(first("new tab"), "New tab");
        assert_eq!(first("sidebar"), "Show or hide the sidebar");
        assert_eq!(first("hid"), "Show hidden files");
        assert_eq!(first("f3"), "Split side by side");
        assert!(score("xyz", "Reload").is_none());
    }

    #[test]
    fn what_cannot_run_comes_last() {
        let found = matches("", &state());
        let first_off = found.iter().position(|(_, ok)| !ok).unwrap();
        assert!(found[first_off..].iter().all(|(_, ok)| !ok));
        // One pane: closing it cannot run, splitting can.
        assert!(found.iter().any(|(c, ok)| c.label == "Close this pane" && !ok));
        assert!(found.iter().any(|(c, ok)| c.label == "Split side by side" && *ok));
        // Several: splitting still can.
        let several = State { panes: 3, ..state() };
        assert!(matches("split", &several).iter().all(|(_, ok)| *ok));
    }
}
