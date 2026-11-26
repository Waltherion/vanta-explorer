//! The sidebar: mounted disks, remote/cloud, XDG special folders, recent files, and the
//! user's own Places sections.
//!
//! Config lives in ~/.config/vanta/explorer-sidebar.jsonc, a real file that no theme links to.
//! Section content is user data and must survive theme switches; only colours come from the
//! theme. Order, collapsed state and hidden built-ins are all persisted there too, because
//! reordering is part of the feature rather than a preference hidden in a dialog.

use crate::drives::Drives;
use crate::glyph::{self, Glyph};
use crate::icons::IconCache;
use crate::model::human_size;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

const ROW_H: f32 = 22.0;
const ICON_SZ: f32 = 16.0;
const INDENT: f32 = 12.0;

pub const DEFAULT_SIDEBAR: &str = r##"// Vanta Explorer, sidebar contents.
// A real file, NOT a theme symlink: this is your data and survives theme switches.
//
// Built-in sections are referenced by "id" and can be reordered, collapsed or hidden, but
// not deleted. Add your own by giving a section a "name" and a list of "paths".
// "New section", and right-clicking a heading, an entry or a folder, edit this file for you.
{
  "sections": [
    { "id": "mounted" },
    { "id": "remote" },
    { "id": "special" },
    { "id": "recent" }

    // Example of your own section:
    // { "name": "Dotfiles", "paths": ["~/.config", "~/.config/hypr"] }
  ]
}
"##;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Section {
    /// Built-in identifier: "mounted" | "remote" | "special" | "recent". None for custom.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// Custom section heading. Ignored for built-ins (they have fixed titles).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub paths: Vec<String>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub collapsed: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub hidden: bool,
    /// Special folders taken out of the list. Those come from xdg-user-dirs, which would put
    /// them back on the next read, so they are remembered here until "Add to sidebar" brings
    /// one back.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub removed: Vec<String>,
}

impl Section {
    pub fn title(&self) -> String {
        match self.id.as_deref() {
            Some("mounted") => "Mounted Disks".into(),
            Some("remote") => "Remote".into(),
            Some("special") => "Special Folders".into(),
            Some("recent") => "Recent Files".into(),
            _ => self.name.clone().unwrap_or_else(|| "Places".into()),
        }
    }

    pub fn is_builtin(&self) -> bool {
        self.id.is_some()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SidebarConfig {
    pub sections: Vec<Section>,
}

impl Default for SidebarConfig {
    fn default() -> Self {
        Self {
            sections: ["mounted", "remote", "special", "recent"]
                .iter()
                .map(|id| Section { id: Some((*id).into()), ..Default::default() })
                .collect(),
        }
    }
}

impl SidebarConfig {
    pub fn path() -> PathBuf {
        vanta_config::layers::dir().join("explorer-sidebar.jsonc")
    }

    pub fn load() -> Self {
        let path = Self::path();
        if !path.exists() {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).ok();
            }
            std::fs::write(&path, DEFAULT_SIDEBAR).ok();
        }
        std::fs::read_to_string(&path)
            .ok()
            .and_then(|t| json5::from_str::<Self>(&t).ok())
            .unwrap_or_default()
    }

    /// Rewrites the file after an in-UI edit. The header comment is re-emitted so the file
    /// stays self-documenting even though serde_json cannot round-trip comments.
    pub fn save(&self) {
        let path = Self::path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        let Ok(body) = serde_json::to_string_pretty(self) else { return };
        let text = format!(
            "// Vanta Explorer, sidebar contents. Rewritten by the app when you add, move, rename or\n\
             // remove something in the sidebar. A real file, NOT a theme symlink: it survives theme\n\
             // switches.\n{body}\n"
        );
        let _ = std::fs::write(path, text);
    }
}

/// One clickable entry under a section heading.
#[derive(Clone)]
struct Row {
    label: String,
    path: PathBuf,
    icons: Vec<String>,
    /// Fill fraction plus the "187 GiB / 971 GiB - 19%" line under the bar.
    usage: Option<(f32, String)>,
    indent: f32,
    /// Block device to eject, when this row is a removable drive. None = no button.
    eject: Option<String>,
}

pub struct Sidebar {
    pub config: SidebarConfig,
    /// Set by an eject click during show(); main.rs takes it after, because show() only
    /// has a shared borrow of Drives and the eject needs a mutable one.
    pending_eject: Option<String>,
    /// "Empty Trash" was chosen on the Trash entry this frame.
    pending_empty_trash: bool,
    /// Index of the custom section whose title is being renamed, plus the edit buffer.
    renaming: Option<(usize, String)>,
    /// The remote and special-folder rows, read from /proc/self/mountinfo, user-dirs.dirs and
    /// a stat per folder: every few seconds, not on every frame the sidebar is drawn.
    rows_cache: Option<(Instant, Vec<Row>, Vec<Row>)>,
    /// mtime as of our own last read/write, so editing the file by hand reloads but our own
    /// saves do not bounce back at us.
    file_mtime: Option<SystemTime>,
    last_check: Instant,
    /// Whether edits made here are written to the file. Not in the chooser, which borrows the
    /// file manager's places and must not rearrange them behind its back.
    pub persist: bool,
    /// F6 or Ctrl+Left: the keys move through the sidebar rather than the pane.
    pub focused: bool,
    /// The entry the keys are on, counted down the sidebar (headings too). None: not yet
    /// placed; the next frame puts it on the folder shown, or the top.
    cursor: Option<usize>,
    /// How many entries the keys could reach last frame.
    reachable: usize,
    /// The keys' ring glides between entries (`Settings::animations`), set before each frame.
    pub animations: bool,
}

/// What the keys do in the sidebar this frame.
#[derive(Default)]
struct Keys {
    on: bool,
    moved: bool,
    enter: bool,
    menu: bool,
}

impl Sidebar {
    /// F6 and Ctrl+arrows take the keys in and out.
    pub fn focus(&mut self, on: bool) {
        if on && !self.focused {
            self.cursor = None;
        }
        self.focused = on;
    }

    pub fn new() -> Self {
        Self {
            pending_eject: None,
            pending_empty_trash: false,
            config: SidebarConfig::load(),
            renaming: None,
            rows_cache: None,
            file_mtime: file_mtime(),
            last_check: Instant::now(),
            persist: true,
            focused: false,
            cursor: None,
            reachable: 0,
            animations: true,
        }
    }

    /// Picks up hand edits to explorer-sidebar.jsonc. Returns true when something changed.
    pub fn poll(&mut self) -> bool {
        if self.last_check.elapsed() < Duration::from_millis(500) {
            return false;
        }
        self.last_check = Instant::now();
        let now = file_mtime();
        if now == self.file_mtime {
            return false;
        }
        self.file_mtime = now;
        self.config = SidebarConfig::load();
        log::info!("sidebar reloaded");
        true
    }

    /// Returns a path when the user clicked an entry.
    /// `current_dir` is the active pane's folder, "Add current folder" uses it, which keeps
    /// Places editable without needing a file dialog.
    /// The device an eject button was clicked for this frame, if any.
    pub fn take_eject_request(&mut self) -> Option<String> {
        self.pending_eject.take()
    }

    /// Whether "Empty Trash" was chosen on the Trash entry this frame.
    pub fn take_empty_trash_request(&mut self) -> bool {
        std::mem::take(&mut self.pending_empty_trash)
    }

    /// The sections a folder can be added to from the file view's menu: Special Folders and the
    /// user's own, with what each holds now so the menu can grey out where it already is.
    pub fn targets(&self) -> Vec<Target> {
        self.config
            .sections
            .iter()
            .enumerate()
            .filter(|(_, s)| !s.hidden && (s.id.as_deref() == Some("special") || !s.is_builtin()))
            .map(|(index, s)| Target {
                index,
                title: s.title(),
                paths: if s.is_builtin() {
                    self.special_list(s).into_iter().map(|r| r.path).collect()
                } else {
                    s.paths.iter().map(|p| expand(p)).collect()
                },
            })
            .collect()
    }

    /// Puts `path` in section `index`, at the end, or back where it was if it is a special
    /// folder that was removed.
    pub fn add(&mut self, index: usize, path: &Path) {
        let Some(sec) = self.config.sections.get_mut(index) else { return };
        let p = contract(path);
        let was_removed = sec.removed.len();
        sec.removed.retain(|r| expand(r) != path);
        let changed = sec.removed.len() != was_removed || !sec.paths.iter().any(|q| expand(q) == path);
        if !sec.paths.iter().any(|q| expand(q) == path) {
            sec.paths.push(p);
        }
        if changed {
            self.commit();
        }
    }

    fn commit(&mut self) {
        if self.persist {
            self.config.save();
            // Adopt our own write so poll() does not treat it as an external edit.
            self.file_mtime = file_mtime();
        }
    }

    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        drives: &Drives,
        icons: &mut IconCache,
        theme: &crate::browser::Theme,
        current_dir: &Path,
        keys: bool,
    ) -> Option<PathBuf> {
        let animations = self.animations;
        // The keys: up and down through the entries, Home and End, Enter to open (staying
        // here, to look through several places), F10 or the Menu key for the entry's menu,
        // Escape back to the pane.
        let mut k = Keys::default();
        if keys {
            let menu_key = ui.ctx().data_mut(|d| d.remove_temp::<bool>(egui::Id::new(crate::browser::MENU_KEY))).unwrap_or(false);
            let (up, down, home, end, enter, f10, escape) = ui.input(|i| {
                let plain = i.modifiers.is_none();
                (
                    plain && i.key_pressed(egui::Key::ArrowUp),
                    plain && i.key_pressed(egui::Key::ArrowDown),
                    plain && i.key_pressed(egui::Key::Home),
                    plain && i.key_pressed(egui::Key::End),
                    plain && i.key_pressed(egui::Key::Enter),
                    i.key_pressed(egui::Key::F10),
                    i.key_pressed(egui::Key::Escape),
                )
            });
            if escape {
                self.focused = false;
            } else {
                let last = self.reachable.saturating_sub(1);
                if let Some(c) = self.cursor {
                    let to = if up {
                        c.saturating_sub(1)
                    } else if down {
                        (c + 1).min(last)
                    } else if home {
                        0
                    } else if end {
                        last
                    } else {
                        c
                    };
                    k.moved = to != c;
                    self.cursor = Some(to.min(last));
                }
                k = Keys { on: true, enter, menu: f10 || menu_key, ..k };
            }
        }
        let cursor = self.cursor.filter(|_| k.on);
        // Counted as they are drawn; the first entry showing the folder in view, for a cursor
        // not yet placed.
        let mut seen = 0usize;
        let mut shown_here: Option<usize> = None;
        // Marks the entry the keys are on. Returns whether it is that one.
        // Where the keys' ring belongs, found as the entries are drawn and painted after them,
        // gliding there from where it was.
        let ring_at = std::cell::Cell::new(None);
        let reach = |ui: &egui::Ui, rect: egui::Rect, n: &mut usize| -> bool {
            let here = cursor == Some(*n);
            *n += 1;
            if here {
                ring_at.set(Some(rect.expand(1.0)));
                if k.moved {
                    ui.scroll_to_rect(rect, None);
                }
            }
            here
        };
        let ring_id = egui::Id::new("sidebar-ring");
        if self.rows_cache.as_ref().is_none_or(|(t, _, _)| t.elapsed() >= Duration::from_secs(5)) {
            self.rows_cache = Some((Instant::now(), remote_rows(), special_rows()));
        }

        let mut clicked = None;
        let mut dirty = false;

        // Initialize special paths if empty, so the user can reorder them
        for section in &mut self.config.sections {
            if section.id.as_deref() == Some("special") && section.paths.is_empty() {
                section.paths = special_rows().into_iter().map(|r| contract(&r.path)).collect();
                dirty = true;
            }
        }
        // Edits are collected and applied after the loop so we never mutate the vector we
        // are iterating.
        let mut move_up: Option<usize> = None;
        let mut move_down: Option<usize> = None;
        let mut toggle: Option<usize> = None;
        let mut remove: Option<usize> = None;
        let mut add_section = false;
        let mut add_path: Option<usize> = None;
        // (section, the entries as shown, which one, -1 up / +1 down)
        let mut path_move: Option<(usize, Vec<PathBuf>, usize, isize)> = None;
        let mut path_remove: Option<(usize, PathBuf)> = None;
        let mut commit_rename = false;
        let mut cancel_rename = false;

        let visible: Vec<usize> = (0..self.config.sections.len())
            .filter(|&i| !self.config.sections[i].hidden)
            .collect();
        let ppp = ui.ctx().pixels_per_point();

        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                // What the entries scroll with: the ring is placed relative to it.
                let origin = ui.min_rect().min.to_vec2();
                for (pos, &i) in visible.iter().enumerate() {
                    let section = self.config.sections[i].clone();

                    // --- heading ---
                    let is_recent = section.id.as_deref() == Some("recent");
                    if is_recent {
                        let rect = egui::Rect::from_min_size(ui.cursor().min, egui::vec2(ui.available_width(), 22.0));
                        let bg_resp = ui.interact(rect, ui.id().with(i).with("recent_bg"), egui::Sense::click());
                        if bg_resp.hovered() {
                            ui.painter().rect_filled(rect, 2.0, theme.accent.gamma_multiply(0.18));
                        }
                        let entered = reach(ui, rect, &mut seen) && k.enter;
                        if entered {
                            self.focused = false;
                        }
                        if entered || bg_resp.clicked() {
                            clicked = Some(PathBuf::from("vantaexplorer://recent"));
                        }
                    }
                    // A heading the keys are on: Enter folds it, F10 opens its menu.
                    let heading_here = !is_recent && cursor == Some(seen);

                    ui.horizontal(|ui| {
                        let renaming_this =
                            matches!(&self.renaming, Some((idx, _)) if *idx == i);
                        if renaming_this {
                            if let Some((_, buf)) = &mut self.renaming {
                                let resp = ui.add(
                                    egui::TextEdit::singleline(buf).desired_width(140.0),
                                );
                                if ui.input(|inp| inp.key_pressed(egui::Key::Escape)) {
                                    cancel_rename = true;
                                } else if resp.lost_focus()
                                    && ui.input(|inp| inp.key_pressed(egui::Key::Enter))
                                {
                                    commit_rename = true;
                                } else {
                                    resp.request_focus();
                                }
                            }
                        } else {
                            if !is_recent {
                                let chevron = if section.collapsed {
                                    Glyph::ChevronRight
                                } else {
                                    Glyph::ChevronDown
                                };
                                if glyph::button(
                                    ui,
                                    chevron,
                                    true,
                                    "",
                                    theme.text.gamma_multiply(0.75),
                                    theme.accent_text,
                                ) {
                                    toggle = Some(i);
                                }
                            } else {
                                // Match the width of the chevron button (18.0) so the text aligns with other headings.
                                ui.allocate_space(egui::vec2(18.0, 18.0));
                            }
                            let resp = ui.add(
                                egui::Button::new(
                                    egui::RichText::new(section.title())
                                        .color(theme.accent_text)
                                        .strong(),
                                )
                                .frame(false),
                            );
                            if resp.clicked() || (heading_here && k.enter) {
                                if is_recent {
                                    clicked = Some(PathBuf::from("vantaexplorer://recent"));
                                } else {
                                    toggle = Some(i);
                                }
                            }
                            // Custom sections rename on double-click; built-in titles are fixed.
                            if !section.is_builtin() && resp.double_clicked() {
                                self.renaming = Some((i, section.title()));
                            }
                            // Moving, renaming and removing a section live in its right-click
                            // menu rather than as buttons on every heading.
                            let key_at = (heading_here && k.menu).then(|| resp.rect.left_bottom());
                            crate::browser::context_popup(&resp, crate::browser::right_click_opens(&resp), key_at, |ui| {
                                if ui.add_enabled(pos > 0, egui::Button::new("Move up")).clicked() {
                                    move_up = Some(i);
                                    ui.close();
                                }
                                if ui.add_enabled(pos + 1 < visible.len(), egui::Button::new("Move down")).clicked() {
                                    move_down = Some(i);
                                    ui.close();
                                }
                                if !section.is_builtin() {
                                    ui.separator();
                                    if ui.button("Rename").clicked() {
                                        self.renaming = Some((i, section.title()));
                                        ui.close();
                                    }
                                    if ui.button("Remove section").clicked() {
                                        remove = Some(i);
                                        ui.close();
                                    }
                                }
                            });
                        }
                    });
                    if !is_recent {
                        let line = egui::Rect::from_min_size(egui::pos2(ui.min_rect().left(), ui.min_rect().bottom() - 20.0), egui::vec2(ui.available_width(), 20.0));
                        reach(ui, line, &mut seen);
                    }

                    if section.collapsed || section.id.as_deref() == Some("recent") {
                        continue;
                    }

                    let rows = self.rows_for(&section, drives);
                    let n = rows.len();
                    let custom = !section.is_builtin();
                    let editable = custom || section.id.as_deref() == Some("special");
                    for (ri, row) in rows.iter().enumerate() {
                        let (action, resp) = draw_row(ui, row, icons, theme, ppp);
                        match action {
                            RowAction::Open => clicked = Some(row.path.clone()),
                            RowAction::Eject => self.pending_eject = row.eject.clone(),
                            RowAction::None => {}
                        }
                        // Plain labels (Today, Yesterday) are not entries.
                        let entry = !row.icons.is_empty() && !row.path.as_os_str().is_empty();
                        let here = entry && {
                            if shown_here.is_none() && row.path == current_dir {
                                shown_here = Some(seen);
                            }
                            reach(ui, resp.rect, &mut seen)
                        };
                        if here && k.enter {
                            // Opened, and back to the pane to look at it.
                            clicked = Some(row.path.clone());
                            self.focused = false;
                        }
                        let key_at = (here && k.menu).then(|| resp.rect.left_bottom() + egui::vec2(24.0, 0.0));
                        if !editable {
                            // A removable drive's menu: its eject button, for the keys.
                            if row.eject.is_some() {
                                let mut eject = false;
                                crate::browser::context_popup(&resp, crate::browser::right_click_opens(&resp), key_at, |ui| {
                                    if ui.button("Safely remove").clicked() {
                                        eject = true;
                                        ui.close();
                                    }
                                });
                                if eject {
                                    self.pending_eject = row.eject.clone();
                                }
                            }
                            continue;
                        }
                        let shown = || rows.iter().map(|r| r.path.clone()).collect::<Vec<_>>();
                        let trash = self.persist && crate::fileops::in_trash(&row.path);
                        let mut empty_trash = false;
                        crate::browser::context_popup(&resp, crate::browser::right_click_opens(&resp), key_at, |ui| {
                            // What the Trash entry is for, above the arranging every entry has.
                            if trash {
                                if ui.button(egui::RichText::new("Empty Trash").color(theme.error)).clicked() {
                                    empty_trash = true;
                                    ui.close();
                                }
                                ui.separator();
                            }
                            if ui.add_enabled(ri > 0, egui::Button::new("Move up")).clicked() {
                                path_move = Some((i, shown(), ri, -1));
                                ui.close();
                            }
                            if ui.add_enabled(ri + 1 < n, egui::Button::new("Move down")).clicked() {
                                path_move = Some((i, shown(), ri, 1));
                                ui.close();
                            }
                            ui.separator();
                            if ui
                                .button("Remove")
                                .on_hover_text("Right-click a folder and choose \"Add to sidebar\" to bring it back")
                                .clicked()
                            {
                                path_remove = Some((i, row.path.clone()));
                                ui.close();
                            }
                        });
                        self.pending_empty_trash |= empty_trash;
                    }
                    if custom {
                        ui.horizontal(|ui| {
                            ui.add_space(INDENT);
                            if glyph::button(ui, Glyph::Plus, true, "", theme.text.gamma_multiply(0.75), theme.accent_text) {
                                add_path = Some(i);
                            }
                            if ui
                                .add(
                                    egui::Button::new(
                                        egui::RichText::new("Add current folder").size(11.0).weak(),
                                    )
                                    .frame(false),
                                )
                                .on_hover_text(current_dir.to_string_lossy().to_string())
                                .clicked()
                            {
                                add_path = Some(i);
                            }
                        });
                    }
                    ui.add_space(6.0);
                }

                ui.separator();
                ui.horizontal(|ui| {
                    if glyph::button(ui, Glyph::Plus, true, "", theme.text.gamma_multiply(0.75), theme.accent_text) {
                        add_section = true;
                    }
                    if ui
                        .add(egui::Button::new(egui::RichText::new("New section")).frame(false))
                        .clicked()
                    {
                        add_section = true;
                    }
                });
                // The keys' ring, over the entries, gliding from the one it was on.
                match ring_at.get() {
                    Some(r) => {
                        let at = crate::browser::glide_rect(ui.ctx(), ring_id, r.translate(-origin), animations).translate(origin);
                        ui.painter().rect_stroke(at, 3.0, egui::Stroke::new(1.5, theme.accent), egui::StrokeKind::Inside);
                    }
                    None => crate::browser::glide_forget(ui.ctx(), ring_id),
                }
            });

        self.reachable = seen;
        if self.focused && self.cursor.is_none() && seen > 0 {
            self.cursor = Some(shown_here.unwrap_or(0));
        }
        if let Some(i) = toggle {
            self.config.sections[i].collapsed = !self.config.sections[i].collapsed;
            dirty = true;
        }
        if let Some(i) = move_up {
            if i > 0 {
                self.config.sections.swap(i, i - 1);
                dirty = true;
            }
        }
        if let Some(i) = move_down {
            if i + 1 < self.config.sections.len() {
                self.config.sections.swap(i, i + 1);
                dirty = true;
            }
        }
        if let Some(i) = remove {
            self.config.sections.remove(i);
            dirty = true;
        }
        if cancel_rename {
            self.renaming = None;
        }
        if commit_rename {
            if let Some((i, buf)) = self.renaming.take() {
                let name = buf.trim();
                if !name.is_empty() {
                    if let Some(sec) = self.config.sections.get_mut(i) {
                        sec.name = Some(name.to_string());
                        dirty = true;
                    }
                }
            }
        }
        if let Some(i) = add_path {
            if let Some(sec) = self.config.sections.get_mut(i) {
                let p = contract(current_dir);
                if !sec.paths.contains(&p) {
                    sec.paths.push(p);
                    dirty = true;
                }
            }
        }
        if let Some((i, shown, ri, delta)) = path_move {
            if let Some(sec) = self.config.sections.get_mut(i) {
                dirty |= move_entry(sec, shown, ri, delta);
            }
        }
        if let Some((i, path)) = path_remove {
            let defaults = self.rows_cache.as_ref().map(|c| c.2.clone()).unwrap_or_default();
            if let Some(sec) = self.config.sections.get_mut(i) {
                dirty |= remove_entry(sec, &path, defaults.iter().any(|r| r.path == path));
            }
        }
        if add_section {
            let n = self.config.sections.iter().filter(|s| !s.is_builtin()).count() + 1;
            self.config.sections.push(Section {
                name: Some(format!("Places {n}")),
                ..Default::default()
            });
            dirty = true;
        }
        if dirty {
            self.commit();
        }
        clicked
    }

    fn rows_for(&self, section: &Section, drives: &Drives) -> Vec<Row> {
        match section.id.as_deref() {
            Some("mounted") => drives
                .mounts()
                .iter()
                .map(|m| Row {
                    label: m.display_name(),
                    path: m.mountpoint.clone(),
                    icons: m.kind.icon_names().iter().map(|s| (*s).to_string()).collect(),
                    usage: Some((
                        m.percent_used() / 100.0,
                        format!(
                            "{} / {}  ·  {:.0}%",
                            human_size(m.used),
                            human_size(m.total),
                            m.percent_used()
                        ),
                    )),
                    indent: INDENT,
                    eject: m.ejectable.then(|| m.device.clone()),
                })
                .collect(),
            Some("remote") => self.rows_cache.as_ref().map(|c| c.1.clone()).unwrap_or_default(),
            Some("special") => self.special_list(section),
            Some("recent") => self.recent_rows(),
            _ => section.paths.iter().map(|p| folder_row(expand(p), INDENT)).collect(),
        }
    }

    /// Special Folders as shown: in the order the file keeps, the folders xdg-user-dirs names
    /// that the file does not mention yet at the end, folders the user added among them, and
    /// none that were removed.
    fn special_list(&self, section: &Section) -> Vec<Row> {
        let defaults = self.rows_cache.as_ref().map(|c| c.2.clone()).unwrap_or_default();
        let removed: Vec<PathBuf> = section.removed.iter().map(|p| expand(p)).collect();
        let mut out: Vec<Row> = Vec::new();
        for p in &section.paths {
            let path = expand(p);
            if removed.contains(&path) || out.iter().any(|r| r.path == path) {
                continue;
            }
            match defaults.iter().find(|r| r.path == path) {
                Some(row) => out.push(row.clone()),
                None => out.push(folder_row(path, INDENT * 2.0)),
            }
        }
        for row in defaults {
            if !removed.contains(&row.path) && !out.iter().any(|r| r.path == row.path) {
                out.push(row);
            }
        }
        out
    }

    fn recent_rows(&self) -> Vec<Row> {
        Vec::new() // Rendered purely as a clickable heading now.
    }

}

/// A section the file view's "Add to sidebar" menu offers, as `Sidebar::targets` lists them.
pub struct Target {
    pub index: usize,
    pub title: String,
    /// What the section shows now, to grey out the sections a folder is already in.
    pub paths: Vec<PathBuf>,
}

/// A folder the user put in a section: its name, and the plain folder icon.
fn folder_row(path: PathBuf, indent: f32) -> Row {
    Row {
        label: path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.to_string_lossy().into_owned()),
        icons: vec!["inode-directory".into()],
        path,
        usage: None,
        indent,
        eject: None,
    }
}

/// Moves entry `ri` of `shown` (the section's entries as drawn) one place up or down, and
/// keeps that order. Removed special folders stay remembered after the shown ones, so one
/// that is added back can find its place again.
fn move_entry(sec: &mut Section, mut shown: Vec<PathBuf>, ri: usize, delta: isize) -> bool {
    let Some(to) = ri.checked_add_signed(delta).filter(|&t| t < shown.len()) else { return false };
    shown.swap(ri, to);
    let mut paths: Vec<String> = shown.iter().map(|p| contract(p)).collect();
    for p in &sec.paths {
        if sec.removed.iter().any(|r| expand(r) == expand(p)) && !paths.iter().any(|q| expand(q) == expand(p)) {
            paths.push(p.clone());
        }
    }
    sec.paths = paths;
    true
}

/// Takes `path` out of the section. A special folder from xdg-user-dirs (`from_desktop`) would
/// come straight back, so it is remembered as removed instead; anything the user added is
/// simply dropped.
fn remove_entry(sec: &mut Section, path: &Path, from_desktop: bool) -> bool {
    if sec.id.as_deref() == Some("special") && from_desktop {
        if sec.removed.iter().any(|r| expand(r) == path) {
            return false;
        }
        sec.removed.push(contract(path));
        return true;
    }
    let before = sec.paths.len();
    sec.paths.retain(|p| expand(p) != path);
    sec.paths.len() != before
}

/// What the user did on an entry row with the left button; the right button's menu is the
/// caller's, on the response this hands back.
#[derive(PartialEq)]
enum RowAction {
    None,
    Open,
    Eject,
}

fn draw_row(
    ui: &mut egui::Ui,
    row: &Row,
    icons: &mut IconCache,
    theme: &crate::browser::Theme,
    ppp: f32,
) -> (RowAction, egui::Response) {
    // Headings inside a section (Today/Yesterday) render as plain labels.
    if row.icons.is_empty() {
        let resp = ui
            .horizontal(|ui| {
                ui.add_space(row.indent);
                ui.label(egui::RichText::new(&row.label).weak().size(11.0));
            })
            .response;
        return (RowAction::None, resp);
    }

    let usage_h = if row.usage.is_some() { 16.0 } else { 0.0 };
    let height = ROW_H + usage_h;
    let (rect, resp) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), height), egui::Sense::click());
    if resp.hovered() {
        ui.painter().rect_filled(rect, 2.0, theme.accent.gamma_multiply(0.18));
    }

    let icon_rect = egui::Rect::from_min_size(
        egui::pos2(rect.min.x + row.indent, rect.min.y + 3.0),
        egui::vec2(ICON_SZ, ICON_SZ),
    );
    // Walk the fallback chain so a future custom drive-harddisk-nvme.svg wins automatically.
    let tex = row
        .icons
        .iter()
        .find_map(|n| icons.get(ui.ctx(), n, ICON_SZ, ppp, theme.icon_tint));
    if let Some(tex) = tex {
        ui.painter().image(
            tex,
            icon_rect,
            egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
            egui::Color32::WHITE,
        );
    }

    // Inline controls, right-aligned on the same row, stacking them underneath tripled the
    // vertical space each entry took.
    let mut action = RowAction::None;
    let mut text_right = rect.max.x - 16.0;

    // Eject, for removable drives. Same right-aligned slot the reorder controls use, and it
    // shrinks text_right the same way, forgetting that is how the label paints over it.
    if row.eject.is_some() {
        let btn = egui::Rect::from_min_size(
            egui::pos2(text_right - 16.0, rect.min.y + 3.0),
            egui::vec2(16.0, 16.0),
        );
        let r = ui.interact(btn, ui.id().with((&row.label, "eject")), egui::Sense::click());
        let col = if r.hovered() { theme.accent_text } else { theme.text.gamma_multiply(0.7) };
        if r.hovered() {
            ui.painter().rect_filled(btn, 2.0, glyph::hover_wash(ui));
        }
        glyph::paint(ui.painter(), btn, Glyph::Eject, col);
        let _ = r.clone().on_hover_text("Safely remove");
        if r.clicked() {
            action = RowAction::Eject;
        }
        text_right -= 18.0;
    }

    // (1) Elide instead of painting past the sidebar edge.
    let text_left = icon_rect.max.x + 6.0;
    let label = crate::text::elide_end(ui, &row.label, (text_right - text_left).max(16.0));
    ui.painter().text(
        egui::pos2(text_left, icon_rect.center().y),
        egui::Align2::LEFT_CENTER,
        label,
        egui::TextStyle::Body.resolve(ui.style()),
        theme.text,
    );

    // Capacity bar plus used/total/percent, as in the Dolphin reference.
    if let Some((frac, text)) = &row.usage {
        let bar = egui::Rect::from_min_max(
            egui::pos2(text_left, rect.min.y + ROW_H - 2.0),
            egui::pos2(rect.max.x - 6.0, rect.min.y + ROW_H + 2.0),
        );
        ui.painter().rect_filled(bar, 2.0, theme.text.gamma_multiply(0.15));
        let filled = egui::Rect::from_min_max(
            bar.min,
            egui::pos2(bar.min.x + bar.width() * frac.clamp(0.0, 1.0), bar.max.y),
        );
        // Nearly-full drives turn red; that is worth noticing at a glance.
        let colour = if *frac > 0.9 {
            egui::Color32::from_rgb(0xe0, 0x50, 0x50)
        } else {
            theme.accent
        };
        ui.painter().rect_filled(filled, 2.0, colour);

        let small = egui::FontId::proportional(10.0);
        let fitted = crate::text::elide_start(ui, text, bar.width());
        ui.painter().text(
            egui::pos2(bar.min.x, bar.max.y + 6.0),
            egui::Align2::LEFT_CENTER,
            fitted,
            small,
            theme.text.gamma_multiply(0.6),
        );
    }

    if action != RowAction::None {
        return (action, resp);
    }
    if resp.clicked() && !row.path.as_os_str().is_empty() {
        (RowAction::Open, resp)
    } else {
        (RowAction::None, resp)
    }
}

/// Network and FUSE mounts a user would open: cloud drives, sshfs, rclone, NFS and SMB shares,
/// wherever such things are mounted (home, /media, /mnt, /run/media). The FUSE mounts that are
/// plumbing rather than places (the document portal, gvfs's container, fusectl) are left out.
fn remote_rows() -> Vec<Row> {
    let mut out = Vec::new();
    let home = crate::panes::home_dir();
    let places = [home.clone(), PathBuf::from("/media"), PathBuf::from("/mnt"), PathBuf::from("/run/media")];
    if let Ok(info) = std::fs::read_to_string("/proc/self/mountinfo") {
        for line in info.lines() {
            let Some((left, right)) = line.split_once(" - ") else { continue };
            let Some(mp) = left.split(' ').nth(4) else { continue };
            let fstype = right.split(' ').next().unwrap_or("");
            let network = ["nfs", "nfs4", "cifs", "smb3", "sshfs", "fuse.sshfs", "9p"].contains(&fstype);
            let fuse = fstype.starts_with("fuse") && !["fusectl", "fuse.portal", "fuse.gvfsd-fuse"].contains(&fstype);
            if !network && !fuse {
                continue;
            }
            let path = PathBuf::from(mp.replace("\\040", " "));
            if !places.iter().any(|p| path.starts_with(p)) {
                continue;
            }
            out.push(Row {
                label: path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| path.to_string_lossy().into_owned()),
                icons: vec!["folder-cloud".into(), "inode-directory".into()],
                path,
                usage: None,
                indent: INDENT,
                    eject: None,
            });
        }
    }
    out.sort_by(|a, b| a.label.cmp(&b.label));
    out
}

/// Home plus the XDG user dirs, read from xdg-user-dirs rather than hardcoded.
fn special_rows() -> Vec<Row> {
    let home = crate::panes::home_dir();
    let mut out = vec![Row {
        label: "Home".into(),
        path: home.clone(),
        icons: vec!["user-home".into()],
        usage: None,
        indent: INDENT,
                    eject: None,
    }];

    let dirs = [
        ("DESKTOP", "Desktop", "user-desktop"),
        ("DOCUMENTS", "Documents", "folder-documents"),
        ("DOWNLOAD", "Downloads", "folder-download"),
        ("MUSIC", "Music", "folder-music"),
        ("PICTURES", "Pictures", "folder-pictures"),
        ("PUBLICSHARE", "Public", "folder-publicshare"),
        ("TEMPLATES", "Templates", "folder-templates"),
        ("VIDEOS", "Videos", "folder-videos"),
    ];
    let config_home = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).filter(|p| p.is_absolute()).unwrap_or_else(|| home.join(".config"));
    let conf = std::fs::read_to_string(config_home.join("user-dirs.dirs")).unwrap_or_default();
    for (key, fallback_name, icon) in dirs {
        let path = conf
            .lines()
            .find_map(|l| {
                let l = l.trim();
                let rest = l.strip_prefix(&format!("XDG_{key}_DIR="))?;
                let val = rest.trim_matches('"');
                Some(PathBuf::from(val.replace("$HOME", &home.to_string_lossy())))
            })
            .unwrap_or_else(|| home.join(fallback_name));
        // xdg-user-dirs points a folder that does not exist at $HOME itself (Omarchy, for one,
        // has no Desktop, Public or Templates), and two keys may share a folder: neither is a
        // row of its own, or Home shows up several times.
        if !path.is_dir() || out.iter().any(|r| r.path == path) {
            continue;
        }
        out.push(Row {
            label: path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| fallback_name.into()),
            path,
            icons: vec![icon.into(), "inode-directory".into()],
            usage: None,
            indent: INDENT * 2.0,
                    eject: None,
        });
    }

    // The trash view is the home trash's files/ folder; browser.rs recognises it and swaps
    // in Restore / Empty Trash for the usual menu.
    let trash = crate::fileops::trash_files_dir();
    if trash.is_dir() {
        out.push(Row {
            label: "Trash".into(),
            path: trash,
            icons: vec!["user-trash".into()],
            usage: None,
            indent: INDENT * 2.0,
                    eject: None,
        });
    }
    out
}

/// Store paths under $HOME as "~/..." so the file stays portable and readable.
fn contract(p: &Path) -> String {
    let home = crate::panes::home_dir();
    match p.strip_prefix(&home) {
        Ok(rest) if !rest.as_os_str().is_empty() => format!("~/{}", rest.to_string_lossy()),
        _ => p.to_string_lossy().into_owned(),
    }
}

fn file_mtime() -> Option<SystemTime> {
    std::fs::metadata(SidebarConfig::path()).ok()?.modified().ok()
}

fn expand(s: &str) -> PathBuf {
    if let Some(rest) = s.strip_prefix("~/") {
        return crate::panes::home_dir().join(rest);
    }
    PathBuf::from(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn special(paths: &[&str]) -> Section {
        Section { id: Some("special".into()), paths: paths.iter().map(|p| p.to_string()).collect(), ..Default::default() }
    }

    #[test]
    fn a_removed_special_folder_is_remembered_and_keeps_its_place_through_a_move() {
        let mut sec = special(&["/h", "/h/Documents", "/h/Music", "/h/Videos"]);
        assert!(remove_entry(&mut sec, Path::new("/h/Music"), true));
        assert_eq!(sec.removed, ["/h/Music"], "xdg-user-dirs would bring it back, so it is remembered");
        assert!(!remove_entry(&mut sec, Path::new("/h/Music"), true), "removing it twice changes nothing");

        // As drawn now: Music is gone. Moving Videos up past Documents keeps Music on file.
        let shown = ["/h", "/h/Documents", "/h/Videos"].map(PathBuf::from).to_vec();
        assert!(move_entry(&mut sec, shown, 2, -1));
        assert_eq!(sec.paths, ["/h", "/h/Videos", "/h/Documents", "/h/Music"]);
        assert!(sec.removed.contains(&"/h/Music".to_string()));
    }

    #[test]
    fn a_folder_the_user_added_is_dropped_when_removed() {
        let mut sec = special(&["/h", "/srv/projects"]);
        assert!(remove_entry(&mut sec, Path::new("/srv/projects"), false));
        assert_eq!(sec.paths, ["/h"]);
        assert!(sec.removed.is_empty());

        let mut own = Section { name: Some("Work".into()), paths: vec!["/a".into(), "/b".into()], ..Default::default() };
        assert!(remove_entry(&mut own, Path::new("/a"), false));
        assert_eq!(own.paths, ["/b"]);
    }

    #[test]
    fn moving_past_either_end_does_nothing() {
        let mut sec = special(&["/a", "/b"]);
        let shown = vec![PathBuf::from("/a"), PathBuf::from("/b")];
        assert!(!move_entry(&mut sec, shown.clone(), 0, -1));
        assert!(!move_entry(&mut sec, shown, 1, 1));
        assert_eq!(sec.paths, ["/a", "/b"]);
    }

    #[test]
    fn adding_a_removed_special_folder_back_puts_it_where_it_was() {
        let mut sb = Sidebar {
            config: SidebarConfig { sections: vec![special(&["/h", "/h/Music", "/h/Videos"])] },
            pending_eject: None,
            pending_empty_trash: false,
            renaming: None,
            rows_cache: None,
            file_mtime: None,
            last_check: Instant::now(),
            persist: false,
            focused: false,
            cursor: None,
            reachable: 0,
            animations: true,
        };
        sb.config.sections[0].removed.push("/h/Music".into());
        sb.add(0, Path::new("/h/Music"));
        assert!(sb.config.sections[0].removed.is_empty());
        assert_eq!(sb.config.sections[0].paths, ["/h", "/h/Music", "/h/Videos"], "back in its old place, not at the end");
        sb.add(0, Path::new("/srv/new"));
        assert_eq!(sb.config.sections[0].paths.last().map(String::as_str), Some("/srv/new"));
    }
}
