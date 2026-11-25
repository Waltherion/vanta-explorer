//! The view of each folder: list or grid and how large, remembered per folder rather than per
//! pane (`viewPer` in the settings). A folder without its own view takes the nearest parent's
//! that is marked for its subfolders, else the default, else a list.
//!
//! State, not configuration: `~/.local/state/vantaexplorer/views.json`, next to the session.
//! Several windows share the file. Each writes only the folders it changed, merged into what is
//! on disk, and reads the file again when another window has written it.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use serde::{Deserialize, Serialize};

use crate::panes::{Pane, MAX_ZOOM, MIN_ZOOM};

/// A change is written this long after the last one: Ctrl+scroll changes the zoom every frame.
const SAVE_AFTER: Duration = Duration::from_secs(1);

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct FolderView {
    pub zoom: f32,
    /// Subfolders without a view of their own take this one.
    #[serde(default)]
    pub subfolders: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
struct File {
    #[serde(default)]
    default: Option<f32>,
    #[serde(default)]
    folders: BTreeMap<PathBuf, FolderView>,
}

pub struct Views {
    file: File,
    /// Where it is kept; `None`: nowhere (tests, no state directory).
    path: Option<PathBuf>,
    /// The chooser reads the views and never writes them.
    writable: bool,
    /// The zoom of a folder nothing says anything about.
    fallback: f32,
    /// Folders changed here and not yet written; `None` in the set is the default.
    touched: BTreeSet<Option<PathBuf>>,
    changed_at: Option<Instant>,
    /// When the file was last read or written, to notice another window writing it.
    seen: Option<SystemTime>,
    checked_at: Option<Instant>,
    /// Bumped when a menu changes what folders other than the one changed should show, so the
    /// panes look their views up again.
    generation: u64,
}

fn file_path() -> Option<PathBuf> {
    crate::model::state_dir().map(|d| d.join("views.json"))
}

fn modified(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

fn read(path: &Path) -> Option<File> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).map_err(|e| log::warn!("views.json: {e}")).ok()
}

impl Views {
    /// The views on disk. `seed`: folders and zooms to start from when there is no file yet (the
    /// session's panes, so an update changes nothing anyone sees).
    pub fn load(writable: bool, fallback: f32, seed: &[(PathBuf, f32)]) -> Self {
        let path = file_path();
        let mut v = Views::new(path.clone(), writable, fallback);
        match path.as_deref().and_then(|p| read(p).map(|f| (f, modified(p)))) {
            Some((file, seen)) => {
                v.file = file;
                v.seen = seen;
                v.prune();
            }
            None if writable && path.as_deref().is_some_and(|p| !p.exists()) => {
                for (folder, zoom) in seed {
                    v.set(folder, *zoom);
                }
                v.save();
            }
            None => {}
        }
        v
    }

    fn new(path: Option<PathBuf>, writable: bool, fallback: f32) -> Self {
        Views {
            file: File::default(),
            path,
            writable,
            fallback,
            touched: BTreeSet::new(),
            changed_at: None,
            seen: None,
            checked_at: None,
            generation: 0,
        }
    }

    #[cfg(test)]
    fn in_memory(fallback: f32) -> Self {
        Views::new(None, true, fallback)
    }

    /// Forgets folders that are gone: those whose parent is there without them. A folder on a
    /// drive that is not plugged in keeps its view.
    fn prune(&mut self) {
        self.file.folders.retain(|p, _| !p.parent().is_some_and(|parent| parent.is_dir() && !p.exists()));
    }

    /// The view a folder is shown in, if anything says: its own, the nearest parent's that is
    /// for its subfolders too, or the default.
    pub fn lookup(&self, folder: &Path) -> Option<f32> {
        if let Some(v) = self.file.folders.get(folder) {
            return Some(v.zoom);
        }
        folder.ancestors().skip(1).find_map(|a| self.file.folders.get(a).filter(|v| v.subfolders).map(|v| v.zoom)).or(self.file.default)
    }

    /// The view a folder is shown in.
    pub fn effective(&self, folder: &Path) -> f32 {
        self.lookup(folder).unwrap_or(self.fallback)
    }

    pub fn own(&self, folder: &Path) -> Option<FolderView> {
        self.file.folders.get(folder).copied()
    }

    fn touch(&mut self, key: Option<PathBuf>) {
        self.touched.insert(key);
        self.changed_at = Some(Instant::now());
    }

    /// The folder's own view, keeping whether it is for its subfolders too.
    pub fn set(&mut self, folder: &Path, zoom: f32) {
        if !self.writable || folder.to_str().is_none() {
            return;
        }
        let zoom = zoom.clamp(MIN_ZOOM, MAX_ZOOM);
        let subfolders = self.own(folder).is_some_and(|v| v.subfolders);
        self.file.folders.insert(folder.to_path_buf(), FolderView { zoom, subfolders });
        self.touch(Some(folder.to_path_buf()));
    }

    /// Whether subfolders without a view of their own take this folder's.
    pub fn set_subfolders(&mut self, folder: &Path, on: bool) {
        if !self.writable || folder.to_str().is_none() {
            return;
        }
        let zoom = self.effective(folder);
        self.file.folders.insert(folder.to_path_buf(), FolderView { zoom, subfolders: on });
        self.touch(Some(folder.to_path_buf()));
        self.generation += 1;
    }

    /// The view of every folder nothing else says anything about.
    pub fn set_default(&mut self, zoom: f32) {
        if !self.writable {
            return;
        }
        self.file.default = Some(zoom.clamp(MIN_ZOOM, MAX_ZOOM));
        self.touch(None);
        self.generation += 1;
    }

    pub fn forget(&mut self, folder: &Path) {
        if self.file.folders.remove(folder).is_some() {
            self.touch(Some(folder.to_path_buf()));
            self.generation += 1;
        }
    }

    /// Runs once a frame for every pane, before and after the panes are drawn. A folder the pane
    /// has not shown yet gets its view; a zoom changed in a folder it has (Ctrl+scroll, the keys,
    /// the status bar) becomes that folder's own. `record` false: views are applied only.
    pub fn sync(&mut self, pane: &mut Pane, record: bool) {
        let folder = pane.tab_ref().path();
        match &pane.view_for {
            Some((p, zoom, g)) if p == folder && *g == self.generation => {
                if pane.zoom != *zoom {
                    if record {
                        self.set(folder, pane.zoom);
                    }
                    let folder = folder.to_path_buf();
                    pane.view_for = Some((folder, pane.zoom, self.generation));
                }
            }
            _ => {
                let folder = folder.to_path_buf();
                pane.zoom = self.effective(&folder);
                pane.view_for = Some((folder, pane.zoom, self.generation));
            }
        }
    }

    /// Writes what changed once it has stopped changing, and reads the file again when another
    /// window wrote it. Called every turn of the event loop.
    pub fn tick(&mut self) {
        if self.changed_at.is_some_and(|t| t.elapsed() >= SAVE_AFTER) {
            self.save();
            return;
        }
        if self.touched.is_empty() && self.checked_at.is_none_or(|t| t.elapsed() >= SAVE_AFTER) {
            self.checked_at = Some(Instant::now());
            let Some(path) = self.path.clone() else { return };
            let now = modified(&path);
            if now.is_some() && now != self.seen {
                if let Some(file) = read(&path) {
                    self.file = file;
                    self.generation += 1;
                }
                self.seen = now;
            }
        }
    }

    /// Writes the folders changed here into the file as it is now, so another window's changes
    /// to other folders are kept.
    pub fn save(&mut self) {
        self.changed_at = None;
        if self.touched.is_empty() || !self.writable {
            return;
        }
        let Some(path) = self.path.clone() else {
            self.touched.clear();
            return;
        };
        let mut on_disk = read(&path).unwrap_or_default();
        for key in std::mem::take(&mut self.touched) {
            match key {
                None => on_disk.default = self.file.default,
                Some(folder) => match self.file.folders.get(&folder) {
                    Some(v) => {
                        on_disk.folders.insert(folder, *v);
                    }
                    None => {
                        on_disk.folders.remove(&folder);
                    }
                },
            }
        }
        self.file = on_disk;
        let Ok(text) = serde_json::to_string_pretty(&self.file) else { return };
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let tmp = path.with_extension(format!("json.{}", std::process::id()));
        if std::fs::write(&tmp, text).and_then(|_| std::fs::rename(&tmp, &path)).is_err() {
            let _ = std::fs::remove_file(&tmp);
            log::warn!("could not write {}", path.display());
            return;
        }
        self.seen = modified(&path);
    }
}

impl Default for Views {
    /// Views kept nowhere, written nowhere.
    fn default() -> Self {
        Views::new(None, false, crate::panes::DEFAULT_ZOOM)
    }
}

impl Drop for Views {
    fn drop(&mut self) {
        self.save();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::panes::DEFAULT_ZOOM;

    #[test]
    fn a_folder_takes_its_own_then_a_marked_parents_then_the_default() {
        let mut v = Views::in_memory(DEFAULT_ZOOM);
        let pics = Path::new("/home/u/Pictures");
        let trip = Path::new("/home/u/Pictures/2026/Rome");
        assert_eq!(v.effective(trip), DEFAULT_ZOOM, "nothing said: a list");
        v.set(pics, 128.0);
        assert_eq!(v.effective(pics), 128.0);
        assert_eq!(v.effective(trip), DEFAULT_ZOOM, "a parent's own view is its own");
        v.set_subfolders(pics, true);
        assert_eq!(v.effective(trip), 128.0, "unless it is for its subfolders too");
        v.set(trip, 20.0);
        assert_eq!(v.effective(trip), 20.0, "and a folder's own beats it");
        v.set(pics, 140.0);
        assert!(v.own(pics).unwrap().subfolders, "changing the zoom keeps the mark");
        v.set_default(96.0);
        assert_eq!(v.effective(Path::new("/srv")), 96.0);
        v.forget(trip);
        assert_eq!(v.effective(trip), 140.0);
    }

    #[test]
    fn applying_a_view_is_not_taken_for_a_change() {
        let mut v = Views::in_memory(DEFAULT_ZOOM);
        let mut pane = Pane::new(PathBuf::from("/a"));
        v.set(Path::new("/b"), 128.0);
        v.touched.clear();
        v.sync(&mut pane, true);
        assert_eq!(pane.zoom, DEFAULT_ZOOM);
        pane.tab().navigate(PathBuf::from("/b"));
        v.sync(&mut pane, true);
        assert_eq!(pane.zoom, 128.0, "the folder's view is applied");
        assert!(v.touched.is_empty(), "and not recorded as a change");
        pane.zoom = 64.0;
        v.sync(&mut pane, true);
        assert_eq!(v.own(Path::new("/b")).unwrap().zoom, 64.0, "a changed zoom is the folder's own");
        pane.zoom = 80.0;
        let mut read_only = Views::new(None, false, DEFAULT_ZOOM);
        read_only.sync(&mut pane, true);
        assert!(read_only.touched.is_empty(), "the chooser writes nothing");
    }

    #[test]
    fn gone_folders_are_forgotten_but_not_those_on_a_missing_drive() {
        let dir = std::env::temp_dir().join(format!("vx-views-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("here")).unwrap();
        let mut v = Views::in_memory(DEFAULT_ZOOM);
        v.set(&dir.join("here"), 100.0);
        v.set(&dir.join("deleted"), 100.0);
        v.set(Path::new("/run/media/nobody-vx/USB/Photos"), 100.0);
        v.prune();
        assert!(v.own(&dir.join("here")).is_some());
        assert!(v.own(&dir.join("deleted")).is_none());
        assert!(v.own(Path::new("/run/media/nobody-vx/USB/Photos")).is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_save_keeps_what_another_window_wrote() {
        let dir = std::env::temp_dir().join(format!("vx-views-save-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("views.json");
        let mut a = Views::new(Some(path.clone()), true, DEFAULT_ZOOM);
        let mut b = Views::new(Some(path.clone()), true, DEFAULT_ZOOM);
        a.set(Path::new("/a"), 100.0);
        a.save();
        b.set(Path::new("/b"), 50.0);
        b.save();
        let file = read(&path).unwrap();
        assert_eq!(file.folders.len(), 2);
        assert_eq!(file.folders[Path::new("/a")].zoom, 100.0);
        b.forget(Path::new("/a"));
        b.save();
        assert_eq!(read(&path).unwrap().folders.len(), 1, "a forgotten folder is removed from the file");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
