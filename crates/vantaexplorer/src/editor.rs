//! Quick edits in the preview pane: a text file or a small config, opened with Edit (Ctrl+E),
//! saved with Ctrl+S. It is not an editor to write code in, only a way to change a line without
//! opening one.
//!
//! Saving writes through a symlink to the file it points at (a theme's config is usually one),
//! by way of a temporary file beside it that is renamed over it, so a crash halfway never leaves
//! half a file. The file's permissions are kept, and so are its line endings.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// Highlighting is redone on every keystroke; past this size the text is shown plain so typing
/// stays instant.
const HIGHLIGHT_LIMIT: usize = 96 * 1024;

pub struct Editor {
    /// The file as the listing shows it, which may be a symlink.
    pub path: PathBuf,
    pub text: String,
    original: String,
    /// When the file was last written, as it was when it was opened or saved: a different time
    /// at save means someone else changed it meanwhile.
    mtime: Option<SystemTime>,
    crlf: bool,
    /// Give the text the keyboard on the next frame.
    pub focus: bool,
}

/// Why a file cannot be edited here, or nothing when it can. Cheap: no reading.
pub fn refusal(path: &Path) -> Option<&'static str> {
    if !crate::docs::is_text(path) || path.extension().is_some_and(|e| e.eq_ignore_ascii_case("svg")) {
        return Some("Not a text file");
    }
    let meta = std::fs::metadata(path).ok()?;
    if meta.len() as usize > crate::docs::TEXT_LIMIT_BYTES {
        return Some("Too large to edit here (over 256 KB)");
    }
    if !writable(path) {
        return Some("You may not change this file");
    }
    None
}

fn writable(path: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    let Ok(c) = std::ffi::CString::new(path.as_os_str().as_bytes()) else { return false };
    // SAFETY: a valid NUL-terminated path; access() only reads it.
    unsafe { libc::access(c.as_ptr(), libc::W_OK) == 0 }
}

impl Editor {
    pub fn open(path: &Path) -> Result<Self, String> {
        if let Some(why) = refusal(path) {
            return Err(why.into());
        }
        let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
        if bytes.contains(&0) {
            return Err("Not a text file".into());
        }
        let raw = String::from_utf8(bytes).map_err(|_| "Not UTF-8 text, so it would not come back the same".to_string())?;
        let crlf = raw.contains("\r\n");
        let text = if crlf { raw.replace("\r\n", "\n") } else { raw };
        Ok(Self { path: path.to_path_buf(), original: text.clone(), text, mtime: mtime(path), crlf, focus: true })
    }

    pub fn dirty(&self) -> bool {
        self.text != self.original
    }

    /// Whether the file was changed by something else since it was opened or saved.
    pub fn changed_on_disk(&self) -> bool {
        mtime(&self.path) != self.mtime
    }

    pub fn file_name(&self) -> String {
        self.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
    }

    /// Writes the text to the file (the one a symlink points at).
    pub fn save(&mut self) -> Result<(), String> {
        let target = std::fs::canonicalize(&self.path).map_err(|e| e.to_string())?;
        let content = if self.crlf { self.text.replace('\n', "\r\n") } else { self.text.clone() };
        write_replacing(&target, content.as_bytes())?;
        self.original = self.text.clone();
        self.mtime = mtime(&self.path);
        Ok(())
    }

    /// Takes the file as it is on disk now, dropping the edits.
    pub fn reload(&mut self) -> Result<(), String> {
        let fresh = Editor::open(&self.path)?;
        *self = Self { focus: true, ..fresh };
        Ok(())
    }
}

fn mtime(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

/// `bytes` as the new content of `target`: written beside it and renamed over it, with its
/// permissions. A folder we may not create files in (but a file in it we may write) gets the
/// file rewritten in place instead.
fn write_replacing(target: &Path, bytes: &[u8]) -> Result<(), String> {
    use std::io::Write;
    let dir = target.parent().ok_or("no folder")?;
    let name = target.file_name().ok_or("no file name")?.to_string_lossy();
    let tmp = dir.join(format!(".{name}.vx-{}.tmp", std::process::id()));
    let perms = std::fs::metadata(target).map(|m| m.permissions()).ok();
    let attempt = (|| -> std::io::Result<()> {
        let mut f = std::fs::OpenOptions::new().write(true).create_new(true).open(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        if let Some(p) = &perms {
            std::fs::set_permissions(&tmp, p.clone())?;
        }
        std::fs::rename(&tmp, target)
    })();
    match attempt {
        Ok(()) => Ok(()),
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            if e.kind() == std::io::ErrorKind::PermissionDenied {
                std::fs::write(target, bytes).map_err(|e| e.to_string())
            } else {
                Err(e.to_string())
            }
        }
    }
}

/// What the preview pane hands back about the editor, to be acted on after the frame.
pub enum Event {
    Save,
    Cancel,
    /// Edit could not open the file; why.
    Failed(String),
}

/// What the editor in the pane asks for this frame.
#[derive(Default)]
pub struct Asked {
    pub save: bool,
    pub cancel: bool,
}

/// The editor in the preview pane: a bar with Save and Cancel, and the text below it.
pub fn show(ui: &mut egui::Ui, ed: &mut Editor, theme: &crate::browser::Theme) -> Asked {
    let mut asked = Asked::default();
    ui.horizontal(|ui| {
        if ui.add_enabled(ed.dirty(), egui::Button::new("Save")).on_hover_text("Ctrl+S").clicked() {
            asked.save = true;
        }
        if ui.button("Cancel").on_hover_text("Escape").clicked() {
            asked.cancel = true;
        }
        if ed.dirty() {
            ui.label(egui::RichText::new("• unsaved").color(theme.accent));
        }
    });
    ui.add_space(4.0);

    let path = ed.path.clone();
    let mut layouter = |ui: &egui::Ui, text: &dyn egui::TextBuffer, wrap: f32| {
        let mut job = if text.as_str().len() <= HIGHLIGHT_LIMIT {
            highlighted(&path, text.as_str())
        } else {
            egui::text::LayoutJob::simple(text.as_str().to_string(), egui::FontId::monospace(12.0), theme.text, f32::INFINITY)
        };
        // No wrapping: a line is a line, so the numbers beside it stay level (the area scrolls
        // sideways instead).
        let _ = wrap;
        job.wrap.max_width = f32::INFINITY;
        ui.fonts_mut(|f| f.layout_job(job))
    };
    let frame = egui::Frame::new().stroke(egui::Stroke::new(1.0, theme.text.gamma_multiply(0.2))).inner_margin(4.0).corner_radius(3.0);
    frame.show(ui, |ui| {
        egui::ScrollArea::both().id_salt("preview-editor").auto_shrink([false, false]).show(ui, |ui| {
            ui.horizontal_top(|ui| {
                // Line numbers, in the same font and line height as the text beside them.
                crate::browser::line_numbers(ui, &ed.text, theme);
                let resp = ui.add(
                    egui::TextEdit::multiline(&mut ed.text)
                        .id_salt("preview-editor-text")
                        .code_editor()
                        .frame(egui::Frame::NONE)
                        .desired_width(f32::INFINITY)
                        .desired_rows(20)
                        .lock_focus(true)
                        .margin(egui::Margin::ZERO)
                        .layouter(&mut layouter),
                );
                if std::mem::take(&mut ed.focus) {
                    resp.request_focus();
                }
                // Escape takes the keyboard from a text field before we see it, so it counts when the
                // text had the keyboard until now.
                let escape = ui.input(|i| i.key_pressed(egui::Key::Escape));
                asked.cancel |= escape && (resp.has_focus() || resp.lost_focus());
                let save = ui.input_mut(|i| i.consume_key(egui::Modifiers::CTRL, egui::Key::S));
                asked.save |= save && ed.dirty();
            });
        });
    });
    asked
}

/// The highlighted layout of `text`, remembered for the last text asked about: egui asks for the
/// layout every frame, and the text only changes on a keystroke.
fn highlighted(path: &Path, text: &str) -> egui::text::LayoutJob {
    use std::sync::Mutex;
    static LAST: Mutex<Option<(PathBuf, u64, egui::text::LayoutJob)>> = Mutex::new(None);
    let hash = {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        text.hash(&mut h);
        h.finish()
    };
    let mut last = LAST.lock().unwrap();
    if let Some((p, h, job)) = last.as_ref() {
        if p == path && *h == hash {
            return job.clone();
        }
    }
    let job = crate::docs::highlight(path, text, usize::MAX);
    *last = Some((path.to_path_buf(), hash, job.clone()));
    job
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("vx-editor-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn saving_through_a_symlink_changes_the_file_and_keeps_the_link() {
        use std::os::unix::fs::PermissionsExt;
        let d = dir("link");
        let real = d.join("real.conf");
        std::fs::write(&real, "a = 1\n").unwrap();
        std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o640)).unwrap();
        let link = d.join("link.conf");
        std::os::unix::fs::symlink(&real, &link).unwrap();

        let mut ed = Editor::open(&link).unwrap();
        assert!(!ed.dirty());
        ed.text.push_str("b = 2\n");
        assert!(ed.dirty());
        ed.save().unwrap();
        assert!(!ed.dirty());
        assert!(std::fs::symlink_metadata(&link).unwrap().file_type().is_symlink(), "still a link");
        assert_eq!(std::fs::read_to_string(&real).unwrap(), "a = 1\nb = 2\n");
        assert_eq!(std::fs::metadata(&real).unwrap().permissions().mode() & 0o777, 0o640);
        assert!(std::fs::read_dir(&d).unwrap().all(|e| !e.unwrap().file_name().to_string_lossy().ends_with(".tmp")));
    }

    #[test]
    fn windows_line_endings_come_back_as_they_were() {
        let d = dir("crlf");
        let f = d.join("w.txt");
        std::fs::write(&f, "one\r\ntwo\r\n").unwrap();
        let mut ed = Editor::open(&f).unwrap();
        assert_eq!(ed.text, "one\ntwo\n");
        ed.text = "one\ntwo\nthree".into();
        ed.save().unwrap();
        assert_eq!(std::fs::read(&f).unwrap(), b"one\r\ntwo\r\nthree");
    }

    #[test]
    fn what_cannot_be_edited_says_why() {
        let d = dir("refuse");
        let big = d.join("big.txt");
        std::fs::write(&big, vec![b'x'; 300 * 1024]).unwrap();
        assert!(refusal(&big).is_some_and(|w| w.contains("Too large")));
        let latin1 = d.join("old.txt");
        std::fs::write(&latin1, b"caf\xe9\n").unwrap();
        assert!(Editor::open(&latin1).err().unwrap().contains("UTF-8"));
        assert_eq!(refusal(&d.join("photo.png")), Some("Not a text file"));
        let ok = d.join("fine.jsonc");
        std::fs::write(&ok, "{}\n").unwrap();
        assert_eq!(refusal(&ok), None);
    }

    #[test]
    fn a_change_on_disk_is_noticed() {
        let d = dir("disk");
        let f = d.join("n.txt");
        std::fs::write(&f, "x\n").unwrap();
        let ed = Editor::open(&f).unwrap();
        assert!(!ed.changed_on_disk());
        let later = std::fs::File::options().write(true).open(&f).unwrap();
        later.set_modified(SystemTime::now() + std::time::Duration::from_secs(5)).unwrap();
        assert!(ed.changed_on_disk());
    }
}
