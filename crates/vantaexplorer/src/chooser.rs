//! Choosing a file for another program.
//!
//! `vantaexplorer --choose` is the window a program gets when it asks the desktop for a file, a
//! folder or a place to save: the same sidebar, theme, thumbnails and preview as the file manager,
//! cut down to one pane with a bar along the bottom that says exactly what will be chosen. The
//! request arrives as JSON on standard input and the answer leaves as JSON on standard output
//! (exit 0 chosen, 1 cancelled), which is what `vantaportal` speaks on the program's behalf.
//!
//! The chooser keeps its own few preferences (window size, preview, the folder each program last
//! used) and never writes the file manager's session, recents or sidebar.

use serde::{Deserialize, Serialize};
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// What is being asked for.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Mode {
    /// One file.
    #[default]
    Open,
    /// One file or several.
    OpenMany,
    /// A folder.
    Folder,
    /// A folder and a name to save under.
    Save,
    /// A folder to save several named files in.
    SaveMany,
}

/// One entry in the "which kinds of file" list.
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct Filter {
    pub name: String,
    /// Shell patterns, such as `*.png`.
    pub globs: Vec<String>,
    /// Types, such as `image/png` or `image/*`.
    pub mimes: Vec<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct Request {
    pub mode: Mode,
    pub title: String,
    /// The program's own word for the confirm button; empty leaves it to the chooser.
    pub accept_label: String,
    /// Where the program wants to start; nothing leaves it to what the chooser remembers.
    pub folder: Option<PathBuf>,
    /// The name a save starts with.
    pub name: String,
    pub filters: Vec<Filter>,
    /// The filter to start on.
    pub filter: Option<usize>,
    /// The names to save, for `SaveMany`.
    pub files: Vec<String>,
    /// The asking window, as the portal names it (`wayland:<handle>`).
    pub parent: String,
    /// The asking program, when the portal knows it.
    pub app: String,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Outcome {
    Chosen(Vec<PathBuf>),
    Cancelled,
}

/// What the chooser remembers between uses, in `chooser.json` beside the file manager's state.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Prefs {
    pub width: f32,
    pub height: f32,
    pub preview_open: bool,
    pub preview_w: f32,
    pub sidebar_width: f32,
    pub zoom: f32,
    pub show_hidden: bool,
    /// The folder each program last chose from.
    pub folders: HashMap<String, PathBuf>,
    /// The folder last chosen from, whoever asked.
    pub last: Option<PathBuf>,
}

impl Default for Prefs {
    fn default() -> Self {
        Self {
            width: 1100.0,
            height: 720.0,
            preview_open: false,
            preview_w: crate::panes::DEFAULT_PREVIEW_W,
            sidebar_width: crate::panes::DEFAULT_SIDEBAR_W,
            zoom: crate::panes::DEFAULT_ZOOM,
            show_hidden: false,
            folders: HashMap::new(),
            last: None,
        }
    }
}

impl Prefs {
    fn path() -> Option<PathBuf> {
        crate::model::state_dir().map(|d| d.join("chooser.json"))
    }

    pub fn load() -> Self {
        Self::path()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) {
        let Some(path) = Self::path() else { return };
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(text) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(path, text);
        }
    }

    /// Where a request starts: where the program asks to, then where that program last chose
    /// from, then where anyone last did, then home. A folder that is gone is passed over.
    pub fn start_folder(&self, request: &Request, home: PathBuf) -> PathBuf {
        let remembered = if request.app.is_empty() { None } else { self.folders.get(&request.app) };
        [request.folder.as_ref(), remembered, self.last.as_ref()]
            .into_iter()
            .flatten()
            .find(|p| p.is_dir())
            .cloned()
            .unwrap_or(home)
    }

    /// Remembers where a choice was made, for the program that asked and for everyone.
    pub fn remember(&mut self, app: &str, folder: &Path) {
        if !app.is_empty() {
            self.folders.insert(app.to_string(), folder.to_path_buf());
        }
        self.last = Some(folder.to_path_buf());
    }
}

pub struct State {
    pub request: Request,
    /// The name field of a save.
    pub name: String,
    /// The filter in use.
    pub filter: Option<usize>,
    /// A save onto a file that exists waits here for an answer; the flag is whether the keyboard
    /// is on Replace (it starts on Cancel).
    pub replace: Option<(PathBuf, bool)>,
    pub outcome: Option<Outcome>,
    /// The name field has been given the keyboard once, when the window opened.
    name_focused: bool,
    /// The file last marked, so marking another one puts its name in the field once rather than
    /// fighting what is typed there on every frame.
    last_marked: Option<String>,
    /// Whether a name passes the mime part of the filter in use, looked up once per name.
    mime_hits: RefCell<HashMap<String, bool>>,
}

impl State {
    pub fn new(request: Request) -> Self {
        let filter = if request.filters.is_empty() { None } else { Some(request.filter.unwrap_or(0).min(request.filters.len() - 1)) };
        Self {
            name: request.name.clone(),
            filter,
            request,
            replace: None,
            outcome: None,
            name_focused: false,
            last_marked: None,
            mime_hits: RefCell::new(HashMap::new()),
        }
    }

    /// Whether an entry is shown under the filter in use. Folders always are: they are how one
    /// gets anywhere.
    pub fn passes(&self, name: &str, is_dir: bool) -> bool {
        let Some(filter) = self.filter.and_then(|i| self.request.filters.get(i)) else { return true };
        if is_dir || (filter.globs.is_empty() && filter.mimes.is_empty()) {
            return true;
        }
        if filter.globs.iter().any(|g| glob_match(g, name)) {
            return true;
        }
        if filter.mimes.is_empty() {
            return false;
        }
        if let Some(hit) = self.mime_hits.borrow().get(name) {
            return *hit;
        }
        // By the name alone: reading every file in a folder to type it would cost far more than
        // a filter is worth.
        let hit = crate::apps::mime_by_name(name).is_some_and(|m| filter.mimes.iter().any(|want| mime_match(want, &m)));
        self.mime_hits.borrow_mut().insert(name.to_string(), hit);
        hit
    }

    fn set_filter(&mut self, i: usize) {
        if self.filter != Some(i) {
            self.filter = Some(i);
            self.mime_hits.borrow_mut().clear();
            if self.request.mode == Mode::Save {
                if let Some(ext) = self.request.filters.get(i).and_then(Filter::extension) {
                    self.name = with_extension(&self.name, &ext);
                }
            }
        }
    }

    /// What the confirm button would choose right now, or nothing when it cannot choose.
    pub fn choice(&self, here: &Path, marked: &[(PathBuf, bool)]) -> Option<Vec<PathBuf>> {
        choice(self.request.mode, here, marked, &self.name, &self.request.files)
    }

    /// Takes a choice: straight away, or after asking when a save would replace a file.
    pub fn accept(&mut self, paths: Vec<PathBuf>) {
        if paths.is_empty() {
            return;
        }
        if self.request.mode == Mode::Save && paths[0].exists() {
            self.replace = Some((paths[0].clone(), false));
        } else {
            self.outcome = Some(Outcome::Chosen(paths));
        }
    }

    /// A file was opened in the list (double-click, Enter): in the chooser that is the choice.
    pub fn activated(&mut self, paths: Vec<PathBuf>) {
        match self.request.mode {
            Mode::Open => self.accept(paths.into_iter().take(1).collect()),
            Mode::OpenMany => self.accept(paths),
            // Saving onto the file that was opened, which asks first.
            Mode::Save => {
                if let Some(p) = paths.into_iter().next() {
                    self.name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    self.accept(vec![p]);
                }
            }
            // A file is not a folder: nothing to choose.
            Mode::Folder | Mode::SaveMany => {}
        }
    }

    pub fn cancel(&mut self) {
        self.outcome = Some(Outcome::Cancelled);
    }

    /// The button's words: the program's own when it gave any, and the folder's name when it is
    /// a folder that is being chosen, so there is no doubt which.
    fn accept_text(&self, target: Option<&[PathBuf]>, here: &Path) -> String {
        let folder_name = |p: &Path| p.file_name().map_or_else(|| p.to_string_lossy().into_owned(), |n| n.to_string_lossy().into_owned());
        match self.request.mode {
            Mode::Folder => format!("Select \"{}\"", folder_name(target.and_then(|t| t.first()).map_or(here, |p| p.as_path()))),
            Mode::SaveMany => {
                let dir = target.and_then(|t| t.first()).and_then(|p| p.parent()).unwrap_or(here);
                format!("Save in \"{}\"", folder_name(dir))
            }
            _ if !self.request.accept_label.is_empty() => self.request.accept_label.replace('_', ""),
            Mode::Save => "Save".into(),
            _ => "Open".into(),
        }
    }

    /// The line that says what the button would choose.
    fn target_text(&self, target: Option<&[PathBuf]>, here: &Path) -> String {
        match (self.request.mode, target) {
            (Mode::Open, None) => "Mark a file".into(),
            (Mode::OpenMany, None) => "Mark one file or more".into(),
            (Mode::Save, None) => "Type a name".into(),
            (Mode::SaveMany, Some(t)) => {
                let dir = t.first().and_then(|p| p.parent()).unwrap_or(here);
                format!("{} file(s) into {}", t.len(), dir.display())
            }
            (_, Some([one])) => one.display().to_string(),
            (_, Some(many)) => format!("{} files in {}", many.len(), here.display()),
            (_, None) => here.display().to_string(),
        }
    }

    /// How tall the bar is: a row for the button, and one above it for the name when saving.
    pub fn bar_height(&self) -> f32 {
        if self.request.mode == Mode::Save { 68.0 } else { 36.0 }
    }

    /// The bar along the bottom: the name when saving, the kinds of file, what will be chosen,
    /// and the two buttons. `marked` is the active pane's selection as (path, is a folder).
    pub fn show_bar(&mut self, ui: &mut egui::Ui, theme: &crate::browser::Theme, here: &Path, marked: &[(PathBuf, bool)], modal_open: bool) {
        let rect = ui.max_rect();
        ui.painter().line_segment([rect.left_top(), rect.right_top()], egui::Stroke::new(1.0, theme.text.gamma_multiply(0.15)));
        let dim = theme.text.gamma_multiply(0.6);

        // Marking a file while saving takes its name, once per file marked.
        if self.request.mode == Mode::Save {
            let now = match marked {
                [(p, false)] => p.file_name().map(|n| n.to_string_lossy().into_owned()),
                _ => None,
            };
            if now != self.last_marked {
                if let Some(n) = &now {
                    self.name = n.clone();
                }
                self.last_marked = now;
            }
        }

        let mut enter_in_name = false;
        ui.add_space(6.0);
        if self.request.mode == Mode::Save {
            ui.horizontal(|ui| {
                ui.add_space(8.0);
                ui.label(egui::RichText::new("Name:").color(dim));
                let id = ui.id().with("chooser-name");
                let resp = ui.add(egui::TextEdit::singleline(&mut self.name).id(id).desired_width((ui.available_width() - 12.0).max(80.0)));
                if !self.name_focused {
                    // Once, as the window opens, with the name up to its extension selected: the
                    // part one usually wants to type over.
                    self.name_focused = true;
                    resp.request_focus();
                    let stem = self.name.rfind('.').filter(|&i| i > 0).map_or(self.name.chars().count(), |i| self.name[..i].chars().count());
                    let mut state = egui::TextEdit::load_state(ui.ctx(), id).unwrap_or_default();
                    state.cursor.set_char_range(Some(egui::text::CCursorRange::two(egui::text::CCursor::new(0), egui::text::CCursor::new(stem))));
                    state.store(ui.ctx(), id);
                }
                enter_in_name = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            });
            ui.add_space(4.0);
        }

        let target = self.choice(here, marked);
        let mut accept = false;
        let mut cancel = false;
        ui.horizontal(|ui| {
            ui.add_space(8.0);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add_space(8.0);
                let text = self.accept_text(target.as_deref(), here);
                let button = egui::Button::new(egui::RichText::new(text).color(theme.text).strong())
                    .fill(theme.accent.gamma_multiply(0.35))
                    .stroke(egui::Stroke::new(1.0, theme.accent));
                accept = ui.add_enabled(target.is_some(), button).on_hover_text("Ctrl+Enter").clicked();
                cancel = ui.button("Cancel").on_hover_text("Esc").clicked();
                if let Some(current) = self.filter {
                    ui.separator();
                    let mut picked = current;
                    egui::ComboBox::from_id_salt("chooser-filter")
                        .selected_text(self.request.filters[current].name.clone())
                        .show_ui(ui, |ui| {
                            for (i, f) in self.request.filters.iter().enumerate() {
                                ui.selectable_value(&mut picked, i, &f.name);
                            }
                        });
                    self.set_filter(picked);
                }
                ui.separator();
                // What is left of the row says what the button would choose, cut short at the
                // front of a long path rather than the end, since the end is the part that differs.
                ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                    let line = self.target_text(target.as_deref(), here);
                    ui.add(egui::Label::new(egui::RichText::new(line).color(if target.is_some() { theme.text } else { dim })).truncate());
                });
            });
        });

        let ctrl_enter = !modal_open && ui.input(|i| i.modifiers.ctrl && i.key_pressed(egui::Key::Enter));
        if cancel {
            self.cancel();
        } else if accept || enter_in_name || ctrl_enter {
            if let Some(paths) = target {
                self.accept(paths);
            }
        }
    }

    /// "Replace the file?", asked before a save lands on something that is there. Starts on
    /// Cancel, like every question here whose yes cannot be taken back.
    pub fn show_replace(&mut self, ctx: &egui::Context, theme: &crate::browser::Theme) {
        let Some((path, mut on_replace)) = self.replace.take() else { return };
        let (toggle, activate, escape) = ctx.input(|i| {
            (
                i.key_pressed(egui::Key::ArrowLeft) || i.key_pressed(egui::Key::ArrowRight) || i.key_pressed(egui::Key::Tab),
                i.key_pressed(egui::Key::Enter) || i.key_pressed(egui::Key::Space),
                i.key_pressed(egui::Key::Escape),
            )
        });
        if toggle {
            on_replace = !on_replace;
        }
        let mut decision = if escape { Some(false) } else if activate { Some(on_replace) } else { None };
        egui::Modal::new(egui::Id::new("chooser-replace")).show(ctx, |ui| {
            let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            ui.heading("Replace the file?");
            ui.add_space(4.0);
            ui.label(format!("'{name}' is already in this folder."));
            ui.label(egui::RichText::new("Saving replaces what is in it.").color(egui::Color32::LIGHT_RED));
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                let button = |ui: &mut egui::Ui, text: egui::RichText, selected: bool| {
                    let btn = egui::Button::new(text);
                    let btn = if selected {
                        btn.fill(theme.accent.gamma_multiply(0.35)).stroke(egui::Stroke::new(2.0, theme.accent))
                    } else {
                        btn.stroke(egui::Stroke::new(1.0, theme.text.gamma_multiply(0.3)))
                    };
                    ui.add(btn).clicked()
                };
                if button(ui, egui::RichText::new("Replace").color(egui::Color32::LIGHT_RED), on_replace) {
                    decision = Some(true);
                }
                if button(ui, egui::RichText::new("Cancel"), !on_replace) {
                    decision = Some(false);
                }
                ui.add_space(8.0);
                ui.label(egui::RichText::new("←/→ choose · Enter confirm · Esc cancel").small().color(theme.text.gamma_multiply(0.5)));
            });
        });
        match decision {
            Some(true) => self.outcome = Some(Outcome::Chosen(vec![path])),
            Some(false) => {}
            None => self.replace = Some((path, on_replace)),
        }
    }
}

/// Makes this window a child of the window that asked, which the portal names with a handle
/// the compositor gave that window (`wayland:<handle>`, xdg-foreign). The chooser then stays over
/// the program it belongs to, and follows it between workspaces.
///
/// `display` and `surface` are winit's own: the request has to be made on the connection the
/// surface lives on. Nothing is kept: the relation is the compositor's once asked for. An error
/// only means the window is an ordinary one, which is what it would be without this.
pub fn adopt(display: *mut std::ffi::c_void, surface: *mut std::ffi::c_void, parent: &str) -> anyhow::Result<()> {
    use anyhow::{anyhow, Context};
    use wayland_client::backend::{Backend, ObjectId};
    use wayland_client::protocol::{wl_registry, wl_surface::WlSurface};
    use wayland_client::{Connection, Dispatch, Proxy, QueueHandle};
    use wayland_protocols::xdg::foreign::zv2::client::{zxdg_imported_v2::ZxdgImportedV2, zxdg_importer_v2::ZxdgImporterV2};

    #[derive(Default)]
    struct Found(Option<ZxdgImporterV2>);
    impl Dispatch<wl_registry::WlRegistry, ()> for Found {
        fn event(found: &mut Self, registry: &wl_registry::WlRegistry, event: wl_registry::Event, _: &(), _: &Connection, qh: &QueueHandle<Self>) {
            if let wl_registry::Event::Global { name, interface, .. } = event {
                if interface == "zxdg_importer_v2" {
                    found.0 = Some(registry.bind(name, 1, qh, ()));
                }
            }
        }
    }
    wayland_client::delegate_noop!(Found: ignore ZxdgImporterV2);
    wayland_client::delegate_noop!(Found: ignore ZxdgImportedV2);

    let handle = parent.strip_prefix("wayland:").filter(|h| !h.is_empty()).ok_or_else(|| anyhow!("no Wayland window named"))?;
    let conn = Connection::from_backend(unsafe { Backend::from_foreign_display(display.cast()) });
    let mut queue = conn.new_event_queue();
    let qh = queue.handle();
    let mut found = Found::default();
    conn.display().get_registry(&qh, ());
    queue.roundtrip(&mut found).context("list the compositor's globals")?;
    let importer = found.0.take().ok_or_else(|| anyhow!("the compositor has no zxdg_importer_v2"))?;
    let id = unsafe { ObjectId::from_ptr(WlSurface::interface(), surface.cast())? };
    let surface = WlSurface::from_id(&conn, id)?;
    importer.import_toplevel(handle.to_string(), &qh, ()).set_parent_of(&surface);
    conn.flush().context("send the request")?;
    Ok(())
}

/// What a mode chooses from where one stands (`here`), what is marked there (path, is a folder),
/// the name typed, and the names to save.
pub fn choice(mode: Mode, here: &Path, marked: &[(PathBuf, bool)], name: &str, files: &[String]) -> Option<Vec<PathBuf>> {
    // A marked folder is the folder meant; with nothing marked (or something that is not one
    // folder) it is the folder one stands in.
    let folder = || match marked {
        [(p, true)] => p.clone(),
        _ => here.to_path_buf(),
    };
    match mode {
        Mode::Open => match marked {
            [(p, false)] => Some(vec![p.clone()]),
            _ => None,
        },
        Mode::OpenMany => {
            let all_files = !marked.is_empty() && marked.iter().all(|(_, dir)| !dir);
            all_files.then(|| marked.iter().map(|(p, _)| p.clone()).collect())
        }
        Mode::Folder => Some(vec![folder()]),
        Mode::Save => {
            let name = name.trim();
            (!name.is_empty() && !name.contains('/') && name != "." && name != "..").then(|| vec![here.join(name)])
        }
        Mode::SaveMany => {
            let dir = folder();
            Some(files.iter().map(|f| dir.join(Path::new(f).file_name().unwrap_or_default())).collect())
        }
    }
}

/// A shell pattern against a name, without regard to case: `*`, `?` and `[...]` sets.
pub fn glob_match(pattern: &str, name: &str) -> bool {
    fn go(p: &[char], n: &[char]) -> bool {
        match p.split_first() {
            None => n.is_empty(),
            Some(('*', rest)) => (0..=n.len()).any(|k| go(rest, &n[k..])),
            Some(('?', rest)) => !n.is_empty() && go(rest, &n[1..]),
            Some(('[', rest)) => {
                let Some(end) = rest.iter().skip(1).position(|c| *c == ']').map(|i| i + 1) else {
                    // No closing bracket: a plain character.
                    return n.first() == Some(&'[') && go(rest, &n[1..]);
                };
                let Some((c, tail)) = n.split_first() else { return false };
                let (negate, set) = match rest[..end].split_first() {
                    Some(('!' | '^', s)) => (true, s),
                    _ => (false, &rest[..end]),
                };
                let mut hit = false;
                let mut i = 0;
                while i < set.len() {
                    if i + 2 < set.len() && set[i + 1] == '-' {
                        hit |= set[i] <= *c && *c <= set[i + 2];
                        i += 3;
                    } else {
                        hit |= set[i] == *c;
                        i += 1;
                    }
                }
                hit != negate && go(&rest[end + 1..], tail)
            }
            Some((c, rest)) => n.first() == Some(c) && go(rest, &n[1..]),
        }
    }
    let lower = |s: &str| s.to_lowercase().chars().collect::<Vec<_>>();
    go(&lower(pattern), &lower(name))
}

/// A wanted type against a file's: exact, or a whole family (`image/*`).
fn mime_match(want: &str, have: &str) -> bool {
    match want.strip_suffix("/*") {
        Some(family) => have.split('/').next() == Some(family),
        None => want == "*/*" || want.eq_ignore_ascii_case(have),
    }
}

/// A path as the `file://` address the portal answers with.
pub fn uri(path: &Path) -> String {
    use std::os::unix::ffi::OsStrExt;
    let mut out = String::from("file://");
    for &b in path.as_os_str().as_bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'/' | b'-' | b'.' | b'_' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// The answer written to standard output.
impl Filter {
    /// The one extension a filter stands for, when it stands for one: `*.png` alone. Picking such a
    /// filter while saving is picking the format, so the name follows it.
    fn extension(&self) -> Option<String> {
        match self.globs.as_slice() {
            [g] => g.strip_prefix("*.").filter(|e| !e.is_empty() && !e.contains(['*', '?', '['])).map(str::to_string),
            _ => None,
        }
    }
}

/// `name` with its extension swapped for `ext`, or given one if it had none.
fn with_extension(name: &str, ext: &str) -> String {
    let stem = match name.rfind('.') {
        Some(i) if i > 0 => &name[..i],
        _ => name,
    };
    format!("{stem}.{ext}")
}

pub fn answer(paths: &[PathBuf], filter: Option<usize>) -> String {
    serde_json::json!({ "uris": paths.iter().map(|p| uri(p)).collect::<Vec<_>>(), "filter": filter }).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(p: &str) -> (PathBuf, bool) {
        (PathBuf::from(p), false)
    }
    fn dir(p: &str) -> (PathBuf, bool) {
        (PathBuf::from(p), true)
    }
    const HERE: &str = "/home/u/Downloads";

    #[test]
    fn a_marked_folder_is_chosen_and_otherwise_the_one_stood_in() {
        let here = Path::new(HERE);
        assert_eq!(choice(Mode::Folder, here, &[], "", &[]), Some(vec![PathBuf::from(HERE)]));
        assert_eq!(choice(Mode::Folder, here, &[dir("/home/u/Downloads/new")], "", &[]), Some(vec![PathBuf::from("/home/u/Downloads/new")]));
        // A marked file, or two folders, is not "a marked folder".
        assert_eq!(choice(Mode::Folder, here, &[file("/home/u/Downloads/a.txt")], "", &[]), Some(vec![PathBuf::from(HERE)]));
        assert_eq!(choice(Mode::Folder, here, &[dir("/a"), dir("/b")], "", &[]), Some(vec![PathBuf::from(HERE)]));
    }

    #[test]
    fn opening_wants_files_and_as_many_as_asked_for() {
        let here = Path::new(HERE);
        assert_eq!(choice(Mode::Open, here, &[], "", &[]), None);
        assert_eq!(choice(Mode::Open, here, &[dir("/d")], "", &[]), None);
        assert_eq!(choice(Mode::Open, here, &[file("/a"), file("/b")], "", &[]), None, "one file was asked for");
        assert_eq!(choice(Mode::Open, here, &[file("/a")], "", &[]), Some(vec![PathBuf::from("/a")]));
        assert_eq!(choice(Mode::OpenMany, here, &[file("/a"), file("/b")], "", &[]).map(|v| v.len()), Some(2));
        assert_eq!(choice(Mode::OpenMany, here, &[file("/a"), dir("/d")], "", &[]), None);
    }

    #[test]
    fn saving_is_the_folder_stood_in_and_the_name_typed() {
        let here = Path::new(HERE);
        assert_eq!(choice(Mode::Save, here, &[dir("/elsewhere")], "  notes.txt ", &[]), Some(vec![PathBuf::from("/home/u/Downloads/notes.txt")]));
        assert_eq!(choice(Mode::Save, here, &[], "", &[]), None);
        assert_eq!(choice(Mode::Save, here, &[], "a/b", &[]), None, "a name, not a path");
        let many = choice(Mode::SaveMany, here, &[dir("/out")], "", &["a.png".into(), "sub/b.png".into()]);
        assert_eq!(many, Some(vec![PathBuf::from("/out/a.png"), PathBuf::from("/out/b.png")]));
    }

    #[test]
    fn patterns_match_as_a_shell_would_whatever_the_case() {
        assert!(glob_match("*.png", "Photo.PNG"));
        assert!(glob_match("*.[jJ][pP][gG]", "a.jpg"));
        assert!(glob_match("*.tar.?z", "x.tar.gz"));
        assert!(glob_match("[a-c]*", "beta"));
        assert!(!glob_match("[!a-c]*", "beta"));
        assert!(!glob_match("*.png", "png"));
        assert!(glob_match("*", ""));
    }

    #[test]
    fn picking_a_format_while_saving_changes_the_extension() {
        let r: Request = serde_json::from_str(
            r#"{"mode":"save","title":"Save as","name":"shot.avif","filters":[{"name":"AVIF","globs":["*.avif"]},{"name":"JPG","globs":["*.jpg"]},{"name":"Any","globs":["*.png","*.jpg"]}],"app":"x"}"#,
        )
        .unwrap();
        let mut s = State::new(r);
        s.set_filter(1);
        assert_eq!(s.name, "shot.jpg");
        s.set_filter(2);
        assert_eq!(s.name, "shot.jpg", "a filter of several formats leaves the name alone");
        assert_eq!(with_extension("noext", "png"), "noext.png");
        assert_eq!(with_extension(".hidden", "png"), ".hidden.png");
    }

    #[test]
    fn a_filter_lets_folders_through_and_matches_by_pattern_or_type() {
        let mut s = State::new(Request {
            filters: vec![
                Filter { name: "Images".into(), globs: vec!["*.png".into()], mimes: vec![] },
                Filter { name: "All".into(), globs: vec![], mimes: vec![] },
            ],
            ..Default::default()
        });
        assert!(s.passes("a.png", false));
        assert!(!s.passes("a.txt", false));
        assert!(s.passes("a.txt", true), "a folder is how one gets anywhere");
        s.set_filter(1);
        assert!(s.passes("a.txt", false), "a filter with nothing in it shows everything");
        assert!(mime_match("image/*", "image/avif") && !mime_match("image/*", "text/plain") && mime_match("text/plain", "text/plain"));
    }

    #[test]
    fn an_address_is_escaped_byte_by_byte() {
        assert_eq!(uri(Path::new("/home/u/My Files/æ.txt")), "file:///home/u/My%20Files/%C3%A6.txt");
        let back: serde_json::Value = serde_json::from_str(&answer(&[PathBuf::from("/a b")], Some(1))).unwrap();
        assert_eq!(back, serde_json::json!({ "uris": ["file:///a%20b"], "filter": 1 }));
    }

    #[test]
    fn a_request_starts_where_asked_then_where_remembered() {
        let tmp = std::env::temp_dir();
        let home = PathBuf::from("/");
        let mut prefs = Prefs::default();
        let ask = |folder: Option<PathBuf>, app: &str| Request { folder, app: app.into(), ..Default::default() };
        assert_eq!(prefs.start_folder(&ask(None, "x"), home.clone()), home);
        prefs.remember("browser", &tmp);
        assert_eq!(prefs.start_folder(&ask(None, "browser"), home.clone()), tmp);
        assert_eq!(prefs.start_folder(&ask(None, "other"), home.clone()), tmp, "anyone's last folder, for a program not seen before");
        assert_eq!(prefs.start_folder(&ask(Some(PathBuf::from("/usr")), "browser"), home.clone()), PathBuf::from("/usr"), "the program's own wish first");
        assert_eq!(prefs.start_folder(&ask(Some(PathBuf::from("/no/such/folder")), "browser"), home), tmp, "a folder that is gone is passed over");
    }

    #[test]
    fn a_request_is_read_from_json() {
        let r: Request = serde_json::from_str(r#"{"mode":"save","title":"Save as","name":"a.txt","filters":[{"name":"Text","globs":["*.txt"]}],"app":"x"}"#).unwrap();
        assert_eq!((r.mode, r.name.as_str(), r.filters.len()), (Mode::Save, "a.txt", 1));
        let r: Request = serde_json::from_str(r#"{"mode":"open-many"}"#).unwrap();
        assert_eq!(r.mode, Mode::OpenMany);
    }
}
