//! Properties (Alt+Enter): what a file or folder is, how big, whose, who may do what with it,
//! when it changed, and what opens it. Read-only; the folder sizes are counted in the
//! background while the window is open.

use std::path::{Path, PathBuf};

use crate::dir_size::DirSize;

/// One open Properties window.
pub struct Properties {
    paths: Vec<PathBuf>,
    /// Facts read once when the window opened (the folder sizes are counted as it stays).
    rows: Vec<(String, String)>,
    /// The folders among `paths`, measured together.
    folders: DirSize,
    /// The files' own bytes and count, read when the window opened.
    file_bytes: u64,
    file_count: u64,
    has_folders: bool,
    /// For a single file: its type, for "Change..." (the Open with window, set as default).
    mime: Option<String>,
}

/// What the window asks for when it closes or a button is pressed.
pub enum Wants {
    Nothing,
    /// "Change..." next to the application that opens it: the Open with window for the file.
    OpenWith(PathBuf),
}

impl Properties {
    pub fn new(paths: Vec<PathBuf>, date_format: &str) -> Self {
        let mut folders: Vec<PathBuf> = Vec::new();
        let (mut file_bytes, mut file_count) = (0u64, 0u64);
        for p in &paths {
            match std::fs::symlink_metadata(p) {
                Ok(m) if m.is_dir() => folders.push(p.clone()),
                Ok(m) => {
                    file_bytes += m.len();
                    file_count += 1;
                }
                Err(_) => {}
            }
        }
        let has_folders = !folders.is_empty();
        let mut size = DirSize::new();
        if has_folders {
            size.start(folders);
        }
        let single = paths.len() == 1;
        let mime = single.then(|| crate::apps::mime_for(&paths[0])).flatten().filter(|_| file_count == 1);
        let rows = if single { facts(&paths[0], date_format) } else { several(&paths) };
        Self { paths, rows, folders: size, file_bytes, file_count, has_folders, mime }
    }

    /// Draws the window; false once it is closed.
    pub fn show(&mut self, ctx: &egui::Context, accent: egui::Color32) -> (bool, Wants) {
        let mut open = true;
        let mut close = false;
        let mut wants = Wants::Nothing;
        if self.has_folders && self.folders.poll() {
            ctx.request_repaint();
        }
        if self.has_folders && !self.folders.done {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
        let title = match self.paths.as_slice() {
            [one] => format!("{} properties", display_name(one)),
            many => format!("{} items", many.len()),
        };
        egui::Window::new(title)
            .id(egui::Id::new("properties"))
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .open(&mut open)
            .show(ctx, |ui| {
                let text = ui.visuals().strong_text_color();
                let label = ui.visuals().weak_text_color();
                egui::Grid::new("properties-grid").num_columns(2).spacing([16.0, 6.0]).show(ui, |ui| {
                    let row = |ui: &mut egui::Ui, k: &str, v: &str| {
                        ui.label(egui::RichText::new(k).color(label));
                        ui.add(egui::Label::new(egui::RichText::new(v).color(text)).wrap().selectable(true));
                        ui.end_row();
                    };
                    for (k, v) in &self.rows {
                        row(ui, k, v);
                    }
                    // The size, which for folders keeps counting while the window is open.
                    let (bytes, files) = (self.file_bytes + self.folders.bytes, self.file_count + self.folders.files);
                    let counting = self.has_folders && !self.folders.done;
                    let mut size = format!("{} ({} bytes)", crate::model::human_size(bytes), group(bytes));
                    if self.has_folders {
                        size.push_str(&format!(", {} file{}", group(files), if files == 1 { "" } else { "s" }));
                    }
                    if counting {
                        size.push_str(", counting...");
                    }
                    row(ui, "Size", &size);
                    if let Some(mime) = &self.mime {
                        ui.label(egui::RichText::new("Opens with").color(label));
                        ui.horizontal(|ui| {
                            let app = default_app(mime).unwrap_or_else(|| "nothing set".into());
                            ui.label(egui::RichText::new(app).color(text));
                            if ui.small_button("Change...").on_hover_text("Choose the application, for every file of this type").clicked() {
                                wants = Wants::OpenWith(self.paths[0].clone());
                            }
                        });
                        ui.end_row();
                    }
                });
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    // Alt+Enter opened it; Enter alone, or Escape, closes it.
                    let keys = ui.input(|i| i.key_pressed(egui::Key::Escape) || (i.key_pressed(egui::Key::Enter) && !i.modifiers.alt));
                    if ui.button(egui::RichText::new("Close").color(accent)).clicked() || keys {
                        close = true;
                    }
                });
            });
        if matches!(wants, Wants::OpenWith(_)) {
            close = true;
        }
        (open && !close, wants)
    }
}

fn display_name(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| p.display().to_string())
}

/// 1234567 as "1 234 567": readable digits for the exact byte count.
fn group(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(' ');
        }
        out.push(c);
    }
    out
}

/// Everything known about one item, in the order shown.
fn facts(path: &Path, date_format: &str) -> Vec<(String, String)> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let mut rows: Vec<(String, String)> = Vec::new();
    let mut put = |k: &str, v: String| rows.push((k.to_string(), v));
    put("Name", display_name(path));
    let Ok(link) = std::fs::symlink_metadata(path) else {
        put("Error", "it can no longer be read".into());
        return rows;
    };
    let meta = std::fs::metadata(path).unwrap_or_else(|_| link.clone());
    let kind = if link.file_type().is_symlink() {
        "Link".to_string()
    } else if meta.is_dir() {
        "Folder".to_string()
    } else {
        let mime = crate::apps::mime_for(path).unwrap_or_default();
        match describe(path) {
            Some(d) if !mime.is_empty() => format!("{d} ({mime})"),
            Some(d) => d,
            None => mime,
        }
    };
    put("Type", kind);
    if link.file_type().is_symlink() {
        let target = std::fs::read_link(path).map(|t| t.display().to_string()).unwrap_or_default();
        let broken = std::fs::metadata(path).is_err();
        put("Points to", if broken { format!("{target} (missing)") } else { target });
    }
    if let Some(parent) = path.parent() {
        put("Location", parent.display().to_string());
    }
    if meta.is_dir() {
        if let Ok(rd) = std::fs::read_dir(path) {
            let n = rd.count();
            put("Contains", format!("{n} item{}", if n == 1 { "" } else { "s" }));
        }
    }
    let when = |t: std::io::Result<std::time::SystemTime>| t.ok().map(|t| crate::model::format_mtime(Some(t), date_format));
    if let Some(t) = when(meta.modified()) {
        put("Modified", t);
    }
    if let Some(t) = when(meta.created()) {
        put("Created", t);
    }
    if let Some(t) = when(meta.accessed()) {
        put("Opened", t);
    }
    put("Owner", format!("{}, group {}", user_name(meta.uid()), group_name(meta.gid())));
    let mode = meta.permissions().mode();
    put("Permissions", format!("{} ({:o})", rwx(mode), mode & 0o7777));
    put("Access", access(&meta));
    if let Some((mount, fs)) = mount_of(path) {
        put("On", format!("{mount} ({fs})"));
    }
    rows
}

/// A selection of several: how many of what, and where when they share a folder.
fn several(paths: &[PathBuf]) -> Vec<(String, String)> {
    let dirs = paths.iter().filter(|p| std::fs::symlink_metadata(p).is_ok_and(|m| m.is_dir())).count();
    let files = paths.len() - dirs;
    let plural = |n: usize, w: &str| format!("{n} {w}{}", if n == 1 { "" } else { "s" });
    let mut rows = vec![("Items".to_string(), format!("{}, {}", plural(files, "file"), plural(dirs, "folder")))];
    let first = paths[0].parent();
    if paths.iter().all(|p| p.parent() == first) {
        if let Some(parent) = first {
            rows.push(("Location".into(), parent.display().to_string()));
        }
    }
    rows
}

/// What `file(1)` says the contents are, short.
fn describe(path: &Path) -> Option<String> {
    let out = std::process::Command::new("file").arg("-b").arg(path).output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    // The first clause is the kind; the rest is detail the preview already shows.
    let short = text.split(',').next().unwrap_or(&text).trim().to_string();
    (!short.is_empty()).then_some(short)
}

fn rwx(mode: u32) -> String {
    let kind = match mode & 0o170000 {
        0o040000 => 'd',
        0o120000 => 'l',
        _ => '-',
    };
    let mut s = String::from(kind);
    for shift in [6, 3, 0] {
        let bits = (mode >> shift) & 7;
        s.push(if bits & 4 != 0 { 'r' } else { '-' });
        s.push(if bits & 2 != 0 { 'w' } else { '-' });
        s.push(if bits & 1 != 0 { 'x' } else { '-' });
    }
    s
}

/// What you may do with it, in words, for the user running this.
fn access(meta: &std::fs::Metadata) -> String {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let mode = meta.permissions().mode();
    let uid = unsafe { libc::getuid() };
    let in_group = meta.gid() == unsafe { libc::getgid() };
    let bits = if uid == 0 {
        7
    } else if meta.uid() == uid {
        (mode >> 6) & 7
    } else if in_group {
        (mode >> 3) & 7
    } else {
        mode & 7
    };
    let mut can = Vec::new();
    if bits & 4 != 0 {
        can.push(if meta.is_dir() { "list" } else { "read" });
    }
    if bits & 2 != 0 {
        can.push(if meta.is_dir() { "add and remove" } else { "change" });
    }
    if bits & 1 != 0 {
        can.push(if meta.is_dir() { "open" } else { "run" });
    }
    if can.is_empty() {
        "You may not use it".into()
    } else {
        format!("You may {}", can.join(", "))
    }
}

fn user_name(uid: u32) -> String {
    let pw = unsafe { libc::getpwuid(uid) };
    if pw.is_null() {
        return uid.to_string();
    }
    unsafe { std::ffi::CStr::from_ptr((*pw).pw_name) }.to_string_lossy().into_owned()
}

fn group_name(gid: u32) -> String {
    let gr = unsafe { libc::getgrgid(gid) };
    if gr.is_null() {
        return gid.to_string();
    }
    unsafe { std::ffi::CStr::from_ptr((*gr).gr_name) }.to_string_lossy().into_owned()
}

/// The mount `path` is on and its filesystem.
fn mount_of(path: &Path) -> Option<(String, String)> {
    let mounts = std::fs::read_to_string("/proc/self/mounts").ok()?;
    let resolved = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    mounts
        .lines()
        .filter_map(|l| {
            let mut f = l.split(' ');
            let (_, mnt, fs) = (f.next()?, f.next()?, f.next()?);
            let mnt = mnt.replace("\\040", " ");
            resolved.starts_with(&mnt).then(|| (mnt, fs.to_string()))
        })
        .max_by_key(|(m, _)| m.len())
}

/// The name of the application set to open this type.
fn default_app(mime: &str) -> Option<String> {
    crate::apps::default_app_name(mime)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permissions_and_numbers_read_as_people_write_them() {
        assert_eq!(rwx(0o100644), "-rw-r--r--");
        assert_eq!(rwx(0o040755), "drwxr-xr-x");
        assert_eq!(group(1234567), "1 234 567");
        assert_eq!(group(12), "12");
    }

    #[test]
    fn a_file_has_its_facts() {
        let dir = std::env::temp_dir().join("vantaexplorer-props");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.txt"), b"hello").unwrap();
        let rows = facts(&dir.join("a.txt"), "%Y");
        let get = |k: &str| rows.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());
        assert_eq!(get("Name").as_deref(), Some("a.txt"));
        assert!(get("Permissions").is_some_and(|p| p.starts_with("-rw")));
        assert!(get("Access").is_some_and(|a| a.contains("read")));
        assert_eq!(get("Location"), Some(dir.display().to_string()));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
