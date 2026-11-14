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
// The +, rename and arrow controls in the UI edit this file for you.
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
            "// vantaexplorer, sidebar contents. Rewritten by the app when you use the +, rename or\n\
             // arrow controls. A real file, NOT a theme symlink: it survives theme switches.\n{body}\n"
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
}

impl Sidebar {
    pub fn new() -> Self {
        Self {
            pending_eject: None,
            config: SidebarConfig::load(),
            renaming: None,
            rows_cache: None,
            file_mtime: file_mtime(),
            last_check: Instant::now(),
            persist: true,
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

    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        drives: &Drives,
        icons: &mut IconCache,
        theme: &crate::browser::Theme,
        current_dir: &Path,
    ) -> Option<PathBuf> {
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
        let mut path_up: Option<(usize, usize)> = None;
        let mut path_down: Option<(usize, usize)> = None;
        let mut path_remove: Option<(usize, usize)> = None;
        let mut commit_rename = false;
        let mut cancel_rename = false;

        let visible: Vec<usize> = (0..self.config.sections.len())
            .filter(|&i| !self.config.sections[i].hidden)
            .collect();
        let ppp = ui.ctx().pixels_per_point();

        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
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
                        if bg_resp.clicked() {
                            clicked = Some(PathBuf::from("vantaexplorer://recent"));
                        }
                    }

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
                                    theme.accent,
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
                                        .color(theme.accent)
                                        .strong(),
                                )
                                .frame(false),
                            );
                            if resp.clicked() {
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
                        }
                        ui.with_layout(
                            egui::Layout::right_to_left(egui::Align::Center),
                            |ui| {
                                if !section.is_builtin()
                                    && glyph::button(ui, Glyph::Cross, true, "Remove section", theme.text.gamma_multiply(0.75), theme.accent)
                                {
                                    remove = Some(i);
                                }
                                if glyph::button(
                                    ui,
                                    Glyph::Down,
                                    pos + 1 < visible.len(),
                                    "Move down",
                                    theme.text.gamma_multiply(0.75),
                                    theme.accent,
                                ) {
                                    move_down = Some(i);
                                }
                                if glyph::button(ui, Glyph::Up, pos > 0, "Move up", theme.text.gamma_multiply(0.75), theme.accent) {
                                    move_up = Some(i);
                                }
                            },
                        );
                    });

                    if section.collapsed || section.id.as_deref() == Some("recent") {
                        continue;
                    }

                    let rows = self.rows_for(&section, drives);
                    let n = rows.len();
                    let custom = !section.is_builtin();
                    for (ri, row) in rows.iter().enumerate() {
                        let is_special = section.id.as_deref() == Some("special");
                        let controls = if custom || is_special {
                            Some((ri > 0, ri + 1 < n, custom)) // can_remove only if custom
                        } else {
                            None
                        };
                        match draw_row(ui, row, icons, theme, ppp, controls) {
                            RowAction::Open => clicked = Some(row.path.clone()),
                            RowAction::Eject => self.pending_eject = row.eject.clone(),
                            RowAction::MoveUp => path_up = Some((i, ri)),
                            RowAction::MoveDown => path_down = Some((i, ri)),
                            RowAction::Remove => path_remove = Some((i, ri)),
                            RowAction::None => {}
                        }
                    }
                    if custom {
                        ui.horizontal(|ui| {
                            ui.add_space(INDENT);
                            if glyph::button(ui, Glyph::Plus, true, "", theme.text.gamma_multiply(0.75), theme.accent) {
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
                    if glyph::button(ui, Glyph::Plus, true, "", theme.text.gamma_multiply(0.75), theme.accent) {
                        add_section = true;
                    }
                    if ui
                        .add(egui::Button::new(egui::RichText::new("New section")).frame(false))
                        .clicked()
                    {
                        add_section = true;
                    }
                });
            });

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
        if let Some((i, ri)) = path_up {
            if ri > 0 {
                self.config.sections[i].paths.swap(ri, ri - 1);
                dirty = true;
            }
        }
        if let Some((i, ri)) = path_down {
            if ri + 1 < self.config.sections[i].paths.len() {
                self.config.sections[i].paths.swap(ri, ri + 1);
                dirty = true;
            }
        }
        if let Some((i, ri)) = path_remove {
            if ri < self.config.sections[i].paths.len() {
                self.config.sections[i].paths.remove(ri);
                dirty = true;
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
        if dirty && self.persist {
            self.config.save();
            // Adopt our own write so poll() does not treat it as an external edit.
            self.file_mtime = file_mtime();
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
            Some("special") => {
                let default_rows = self.rows_cache.as_ref().map(|c| c.2.clone()).unwrap_or_default();
                if section.paths.is_empty() {
                    default_rows
                } else {
                    let mut out = Vec::new();
                    for p in &section.paths {
                        let path = expand(p);
                        if let Some(idx) = default_rows.iter().position(|r| r.path == path) {
                            out.push(default_rows[idx].clone());
                        }
                    }
                    for row in default_rows {
                        if !out.iter().any(|r| r.path == row.path) {
                            out.push(row);
                        }
                    }
                    out
                }
            }
            Some("recent") => self.recent_rows(),
            _ => section
                .paths
                .iter()
                .map(|p| {
                    let path = expand(p);
                    Row {
                        label: path
                            .file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_else(|| path.to_string_lossy().into_owned()),
                        icons: vec!["inode-directory".into()],
                        path,
                        usage: None,
                        indent: INDENT,
                    eject: None,
                    }
                })
                .collect(),
        }
    }

    fn recent_rows(&self) -> Vec<Row> {
        Vec::new() // Rendered purely as a clickable heading now.
    }

}

/// What the user did on an entry row.
#[derive(PartialEq)]
enum RowAction {
    None,
    Open,
    Eject,
    MoveUp,
    MoveDown,
    Remove,
}

fn draw_row(
    ui: &mut egui::Ui,
    row: &Row,
    icons: &mut IconCache,
    theme: &crate::browser::Theme,
    ppp: f32,
    // Some((can_up, can_down, can_remove)) draws reorder/remove controls inline on the row.
    controls: Option<(bool, bool, bool)>,
) -> RowAction {
    // Headings inside a section (Today/Yesterday) render as plain labels.
    if row.icons.is_empty() {
        ui.horizontal(|ui| {
            ui.add_space(row.indent);
            ui.label(egui::RichText::new(&row.label).weak().size(11.0));
        });
        return RowAction::None;
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
        let col = if r.hovered() { theme.accent } else { theme.text.gamma_multiply(0.7) };
        if r.hovered() {
            ui.painter().rect_filled(btn, 2.0, egui::Color32::from_white_alpha(20));
        }
        glyph::paint(ui.painter(), btn, Glyph::Eject, col);
        let _ = r.clone().on_hover_text("Safely remove");
        if r.clicked() {
            action = RowAction::Eject;
        }
        text_right -= 18.0;
    }

    if let Some((can_up, can_down, can_remove)) = controls {
        let mut x = rect.max.x - 16.0;
        let mut btns = Vec::new();
        if can_remove {
            btns.push((Glyph::Cross, true, "Remove"));
        }
        btns.push((Glyph::Down, can_down, "Move down"));
        btns.push((Glyph::Up, can_up, "Move up"));

        for (glyph, enabled, tip) in btns {
            let btn = egui::Rect::from_min_size(
                egui::pos2(x - 16.0, rect.min.y + 3.0),
                egui::vec2(16.0, 16.0),
            );
            let r = ui.interact(btn, ui.id().with((&row.label, tip)), egui::Sense::click());
            let col = if !enabled {
                theme.text.gamma_multiply(0.3)
            } else if r.hovered() {
                theme.accent
            } else {
                theme.text.gamma_multiply(0.7)
            };
            if enabled && r.hovered() {
                ui.painter().rect_filled(btn, 2.0, egui::Color32::from_white_alpha(20));
            }
            glyph::paint(ui.painter(), btn, glyph, col);
            let _ = r.clone().on_hover_text(tip);
            if enabled && r.clicked() {
                action = match glyph {
                    Glyph::Cross => RowAction::Remove,
                    Glyph::Down => RowAction::MoveDown,
                    _ => RowAction::MoveUp,
                };
            }
            x -= 18.0;
        }
        text_right = x;
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
        return action;
    }
    if resp.clicked() && !row.path.as_os_str().is_empty() {
        RowAction::Open
    } else {
        RowAction::None
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
        if !path.is_dir() {
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
