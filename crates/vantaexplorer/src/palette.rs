//! Ctrl+Shift+P: every command by name. A command chosen here is run by pressing its key on
//! the next frame, so it does exactly what the key does, with nothing to keep in step.

use egui::{Key, Modifiers};

/// What the palette presses to run a command.
#[derive(Clone)]
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
    cmd("Menu", "F10", key(Key::F10, NONE), When::Always),
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
    cmd("Next pane", "Ctrl+Tab", key(Key::Tab, CTRL), When::Panes),
    cmd("Previous pane", "Ctrl+Shift+Tab", key(Key::Tab, CTRL_SHIFT), When::Panes),
    cmd("Go to the sidebar", "F6", key(Key::F6, NONE), When::Always),
    cmd("Split side by side", "F3", Fire::Split(crate::panes::SplitDir::Horizontal), When::Always),
    cmd("Split one above the other", "Shift+F3", Fire::Split(crate::panes::SplitDir::Vertical), When::Always),
    cmd("Close this pane", "Ctrl+Shift+W", key(Key::W, CTRL_SHIFT), When::Panes),
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
    /// Folders to go to, best known first: the open tabs', the sidebar's, the ones visited.
    pub places: Vec<(std::path::PathBuf, &'static str)>,
}

/// What a row of the palette does when it is picked.
#[derive(Clone)]
pub enum Pick {
    Fire(Fire),
    /// Go to this folder in the pane you are in.
    Go(std::path::PathBuf),
}

/// A row of the palette.
struct Row {
    label: String,
    /// The key, why it cannot run, or what kind of place it is.
    right: String,
    ok: bool,
    pick: Pick,
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
    /// A folder's subfolders, read once while its path is being typed.
    children: Option<(std::path::PathBuf, Vec<std::path::PathBuf>)>,
}

/// `~` for the home folder, as a person writes it.
fn contract(p: &std::path::Path) -> String {
    let home = crate::panes::home_dir();
    match p.strip_prefix(&home) {
        Ok(rest) if rest.as_os_str().is_empty() => "~".into(),
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => p.display().to_string(),
    }
}

fn expand(q: &str) -> std::path::PathBuf {
    match q.strip_prefix('~') {
        Some(rest) => crate::panes::home_dir().join(rest.trim_start_matches('/')),
        None => std::path::PathBuf::from(q),
    }
}

/// A query starting with / or ~ is a folder to go to.
fn is_folder_query(q: &str) -> bool {
    q.starts_with('/') || q.starts_with('~')
}

/// The folders for a query starting with / or ~: the one typed, if it is there; the
/// subfolders of the one typed up to its last /, starting with what follows; then the known
/// places matching it, as the commands match.
fn folder_rows(query: &str, state: &State, children: &mut Option<(std::path::PathBuf, Vec<std::path::PathBuf>)>) -> Vec<Row> {
    let q = query.trim();
    let mut rows: Vec<Row> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut push = |rows: &mut Vec<Row>, path: std::path::PathBuf, kind: &str| {
        if seen.insert(path.clone()) {
            rows.push(Row { label: contract(&path), right: kind.to_string(), ok: true, pick: Pick::Go(path) });
        }
    };
    let typed = expand(q);
    if typed.is_dir() {
        push(&mut rows, typed.clone(), "folder");
    }
    if let Some(cut) = q.rfind('/') {
        let parent = expand(&q[..=cut]);
        let start = q[cut + 1..].to_lowercase();
        if children.as_ref().is_none_or(|(p, _)| *p != parent) {
            let mut found: Vec<std::path::PathBuf> = std::fs::read_dir(&parent)
                .map(|rd| rd.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect())
                .unwrap_or_default();
            found.sort_by_key(|p| p.file_name().map(|n| n.to_string_lossy().to_lowercase()));
            *children = Some((parent.clone(), found));
        }
        if let Some((_, found)) = children.as_ref() {
            for p in found {
                let name = p.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
                // Hidden ones only once a dot is typed.
                if name.starts_with(&start) && (start.starts_with('.') || !name.starts_with('.')) {
                    push(&mut rows, p.clone(), "folder");
                }
                if rows.len() >= 60 {
                    break;
                }
            }
        }
    }
    let needle = q.trim_start_matches(['/', '~']);
    let mut known: Vec<(i32, &(std::path::PathBuf, &'static str))> = state
        .places
        .iter()
        .filter_map(|place| {
            let s = if needle.is_empty() { Some(0) } else { score(needle, &contract(&place.0)) }?;
            Some((s, place))
        })
        .collect();
    known.sort_by_key(|(s, _)| -s);
    for (_, (path, kind)) in known {
        push(&mut rows, path.clone(), kind);
    }
    rows
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

/// Draws the palette. Returns what was picked, and whether it is to close.
pub fn show(ctx: &egui::Context, palette: &mut Palette, state: &State) -> (Option<Pick>, bool) {
    let id = egui::Id::new("palette");
    let area = egui::Modal::default_area(id).anchor(egui::Align2::CENTER_TOP, egui::vec2(0.0, 72.0));
    let mut pick = None;
    let mut close = false;
    let resp = egui::Modal::new(id).area(area).show(ctx, |ui| {
        ui.set_width(560.0);
        let (down, up, enter, escape, tab) = ui.input_mut(|i| {
            (
                i.consume_key(NONE, Key::ArrowDown),
                i.consume_key(NONE, Key::ArrowUp),
                i.consume_key(NONE, Key::Enter),
                i.consume_key(NONE, Key::Escape),
                i.consume_key(NONE, Key::Tab),
            )
        });
        let edit_id = egui::Id::new("palette-query");
        let mut edit = ui.add(
            egui::TextEdit::singleline(&mut palette.query)
                .id(edit_id)
                .hint_text("Type a command, or / or ~ for a folder")
                .desired_width(f32::INFINITY),
        );
        edit.request_focus();
        // New contents, not the keys, moved the marking: it is put in place, not glided.
        let mut snap = edit.changed();
        if edit.changed() {
            palette.cursor = 0;
        }
        let folders = is_folder_query(palette.query.trim());
        let rows: Vec<Row> = if folders {
            folder_rows(&palette.query, state, &mut palette.children)
        } else {
            matches(&palette.query, state)
                .into_iter()
                .map(|(c, ok)| Row {
                    label: c.label.to_string(),
                    right: if ok { c.keys } else { c.when.why_not() }.to_string(),
                    ok,
                    pick: Pick::Fire(c.fire.clone()),
                })
                .collect()
        };
        let runnable = rows.iter().filter(|r| r.ok).count();
        let moved = down || up;
        if runnable > 0 {
            if down {
                palette.cursor = (palette.cursor + 1) % runnable;
            } else if up {
                palette.cursor = (palette.cursor + runnable - 1) % runnable;
            }
            palette.cursor = palette.cursor.min(runnable - 1);
        }
        // Tab fills in the folder the cursor is on, to go on typing below it.
        if tab && folders {
            if let Some(Row { pick: Pick::Go(p), .. }) = rows.get(palette.cursor) {
                palette.query = format!("{}/", contract(p).trim_end_matches('/'));
                palette.cursor = 0;
                snap = true;
                let end = egui::text::CCursor::new(palette.query.chars().count());
                if let Some(mut st) = egui::TextEdit::load_state(ui.ctx(), edit_id) {
                    st.cursor.set_char_range(Some(egui::text::CCursorRange::one(end)));
                    st.store(ui.ctx(), edit_id);
                }
                edit.mark_changed();
                ui.ctx().request_repaint();
            }
        }
        ui.add_space(4.0);
        let text = ui.visuals().strong_text_color();
        let font = egui::TextStyle::Body.resolve(ui.style());
        egui::ScrollArea::vertical().max_height(360.0).auto_shrink([false, true]).show(ui, |ui| {
            let mut glider = crate::browser::Glider::begin(ui);
            if rows.is_empty() {
                let none = if folders { "No folder by that name" } else { "No command by that name" };
                ui.label(egui::RichText::new(none).color(text.gamma_multiply(0.5)));
            }
            for (i, row) in rows.iter().enumerate() {
                let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 26.0), if row.ok { egui::Sense::click() } else { egui::Sense::hover() });
                if row.ok && resp.hovered() && ui.input(|i| i.pointer.delta() != egui::Vec2::ZERO) {
                    // The mouse moves it: straight there, as the pointer is already.
                    snap |= palette.cursor != i;
                    palette.cursor = i;
                }
                let current = row.ok && i == palette.cursor;
                if current {
                    glider.chosen(rect);
                    if moved {
                        ui.scroll_to_rect(rect, None);
                    }
                }
                let ink = if current { ui.visuals().selection.stroke.color } else if row.ok { text } else { text.gamma_multiply(0.4) };
                let right_w = ui.painter().layout_no_wrap(row.right.clone(), font.clone(), ink).size().x;
                let label = crate::text::elide_start(ui, &row.label, (rect.width() - right_w - 32.0).max(40.0));
                ui.painter().text(rect.left_center() + egui::vec2(8.0, 0.0), egui::Align2::LEFT_CENTER, label, font.clone(), ink);
                ui.painter().text(rect.right_center() - egui::vec2(8.0, 0.0), egui::Align2::RIGHT_CENTER, &row.right, font.clone(), ink.gamma_multiply(if row.ok { 0.6 } else { 1.0 }));
                if resp.clicked() {
                    pick = Some(row.pick.clone());
                }
            }
            let fill = ui.visuals().selection.bg_fill;
            glider.finish(ui, egui::Id::new("palette-glide"), crate::browser::animations(ui.ctx()), snap, 4.0, fill);
        });
        if enter {
            pick = rows.get(palette.cursor).filter(|r| r.ok).map(|r| r.pick.clone());
        }
        if escape || pick.is_some() {
            edit.surrender_focus();
            close = true;
        }
    });
    if resp.backdrop_response.clicked() {
        close = true;
    }
    (pick, close)
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
        State { marked: 0, clipboard: false, panes: 1, undo: false, redo: false, places: Vec::new() }
    }

    fn first(query: &str) -> &'static str {
        matches(query, &state()).first().map(|(c, _)| c.label).unwrap_or("")
    }

    #[test]
    fn finds_by_initials_and_words() {
        assert_eq!(first("spl"), "Split side by side");
        assert_eq!(first("new tab"), "New tab");
        assert_eq!(first("hide side"), "Show or hide the sidebar");
        assert_eq!(first("go side"), "Go to the sidebar");
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

    #[test]
    fn a_slash_or_tilde_goes_to_folders() {
        let dir = std::env::temp_dir().join(format!("vx-palette-{}", std::process::id()));
        for d in ["Alpha", "alpine", "Beta", ".hidden"] {
            std::fs::create_dir_all(dir.join(d)).unwrap();
        }
        std::fs::write(dir.join("afile"), "").unwrap();
        let state = State { places: vec![(std::path::PathBuf::from("/usr/share"), "sidebar")], ..state() };
        let mut cache = None;
        let names = |rows: Vec<Row>| rows.into_iter().map(|r| r.label).collect::<Vec<_>>();
        // Typed up to a slash: the subfolders starting with what follows, files and hidden
        // ones left out; then the known places.
        let q = format!("{}/al", dir.display());
        let got = names(folder_rows(&q, &state, &mut cache));
        assert_eq!(&got[..2], [contract(&dir.join("Alpha")), contract(&dir.join("alpine"))]);
        assert!(!got.iter().any(|g| g.ends_with("afile") || g.ends_with(".hidden")));
        // A folder that is there comes first.
        let q = format!("{}/Beta", dir.display());
        assert_eq!(names(folder_rows(&q, &state, &mut None))[0], contract(&dir.join("Beta")));
        // "/sha" matches the known place by its letters.
        assert!(names(folder_rows("/sha", &state, &mut None)).contains(&"/usr/share".to_string()));
        assert!(is_folder_query("~/x") && is_folder_query("/x") && !is_folder_query("x"));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
