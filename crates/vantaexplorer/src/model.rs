//! Directory listing model. Listing runs on a background thread and streams entries to the
//! UI in chunks, so the first rows appear immediately even in huge directories.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::time::SystemTime;

const CHUNK: usize = 2048;

#[derive(Clone)]
pub struct Entry {
    pub name: String,
    pub path: PathBuf,
    pub is_dir: bool,
    pub is_symlink: bool,
    pub size: u64,
    pub modified: Option<SystemTime>,
    pub hidden: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SortKey {
    Name,
    Size,
    Modified,
}

#[derive(Clone, Copy)]
pub struct SortSpec {
    pub key: SortKey,
    pub ascending: bool,
}

impl Default for SortSpec {
    fn default() -> Self {
        Self { key: SortKey::Name, ascending: true }
    }
}

pub enum LoadMsg {
    Chunk(u64, Vec<Entry>),
    Done(u64),
    Error(u64, String),
}

#[derive(PartialEq, Eq, Clone, Copy)]
pub enum ListingState {
    Loading,
    Ready,
    Error,
}

pub struct Listing {
    pub path: PathBuf,
    pub entries: Vec<Entry>,
    pub state: ListingState,
    /// Entries are the PREVIOUS listing, kept on screen while a reload streams in, so a
    /// refresh does not blink the view empty. Cleared when the first fresh chunk lands.
    pub stale: bool,
    pub error: Option<String>,
    #[allow(dead_code)] // bookkeeping; read via Tab::generation matching
    pub generation: u64,
    pub sort: SortSpec,
}

impl Listing {
    pub fn new(path: PathBuf, generation: u64) -> Self {
        Self {
            path,
            entries: Vec::new(),
            state: ListingState::Loading,
            stale: false,
            error: None,
            generation,
            sort: SortSpec::default(),
        }
    }

    /// Directories always sort before files (Dolphin-style), then by the active key.
    pub fn apply_sort(&mut self) {
        let spec = self.sort;
        self.entries.sort_unstable_by(|a, b| {
            b.is_dir
                .cmp(&a.is_dir)
                .then_with(|| {
                    let ord = match spec.key {
                        SortKey::Name => natural_cmp(&a.name, &b.name),
                        SortKey::Size => a.size.cmp(&b.size).then_with(|| natural_cmp(&a.name, &b.name)),
                        SortKey::Modified => a.modified.cmp(&b.modified).then_with(|| natural_cmp(&a.name, &b.name)),
                    };
                    if spec.ascending { ord } else { ord.reverse() }
                })
        });
    }
}

/// Case-insensitive compare with numeric runs compared as numbers, so "file2" < "file10".
fn natural_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    let mut ai = a.chars().flat_map(|c| c.to_lowercase()).peekable();
    let mut bi = b.chars().flat_map(|c| c.to_lowercase()).peekable();
    loop {
        match (ai.peek().copied(), bi.peek().copied()) {
            (None, None) => return std::cmp::Ordering::Equal,
            (None, Some(_)) => return std::cmp::Ordering::Less,
            (Some(_), None) => return std::cmp::Ordering::Greater,
            (Some(x), Some(y)) if x.is_ascii_digit() && y.is_ascii_digit() => {
                let mut xn: u64 = 0;
                let mut yn: u64 = 0;
                while let Some(d) = ai.peek().and_then(|c| c.to_digit(10)) {
                    xn = xn.saturating_mul(10).saturating_add(d as u64);
                    ai.next();
                }
                while let Some(d) = bi.peek().and_then(|c| c.to_digit(10)) {
                    yn = yn.saturating_mul(10).saturating_add(d as u64);
                    bi.next();
                }
                match xn.cmp(&yn) {
                    std::cmp::Ordering::Equal => {}
                    ord => return ord,
                }
            }
            (Some(x), Some(y)) => {
                match x.cmp(&y) {
                    std::cmp::Ordering::Equal => {
                        ai.next();
                        bi.next();
                    }
                    ord => return ord,
                }
            }
        }
    }
}

/// Spawn a background read of `path`. Chunks arrive on the receiver; poll it from
/// `about_to_wait` and render DIRECTLY on new data (the Wayland on-demand-render trap).
/// Walk `root` depth-first, emitting entries whose file name contains `needle`.
///
/// `Entry::name` carries the path RELATIVE to the root, not the bare file name. That is
/// load-bearing: selection is name-keyed (`Tab::selection: HashSet<String>`), so two
/// `report.txt` in different subfolders would otherwise collide into one selectable item.
/// It doubles as the only indication of where a hit actually lives.
///
/// Symlinked directories are not descended into, a single loop would hang the walk.
fn walk_search(root: &Path, needle: &str, generation: u64, tx: &Sender<LoadMsg>) {
    let needle = needle.to_lowercase();
    let mut batch = Vec::with_capacity(CHUNK);
    let mut stack = vec![root.to_path_buf()];

    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        for de in rd.flatten() {
            let entry_path = de.path();
            let file_name = de.file_name().to_string_lossy().into_owned();
            let ft = de.file_type().ok();
            let is_symlink = ft.map(|t| t.is_symlink()).unwrap_or(false);
            let meta = de.metadata().ok();
            let is_dir = meta.as_ref().map(|m| m.is_dir()).unwrap_or(false);

            if is_dir && !is_symlink {
                stack.push(entry_path.clone());
            }
            if !file_name.to_lowercase().contains(&needle) {
                continue;
            }
            let rel = entry_path
                .strip_prefix(root)
                .unwrap_or(&entry_path)
                .to_string_lossy()
                .into_owned();
            batch.push(Entry {
                path: entry_path,
                is_dir,
                is_symlink,
                size: meta.as_ref().map(|m| m.len()).unwrap_or(0),
                modified: meta.and_then(|m| m.modified().ok()),
                // Judged on the file's own name: a hit inside a visible folder is not hidden
                // just because its relative path has no leading dot.
                hidden: file_name.starts_with('.'),
                name: rel,
            });
            if batch.len() >= CHUNK {
                // A failed send means the tab moved on; stop walking rather than finish a
                // search nobody is waiting for.
                if tx.send(LoadMsg::Chunk(generation, std::mem::take(&mut batch))).is_err() {
                    return;
                }
                batch.reserve(CHUNK);
            }
        }
    }
    if !batch.is_empty() {
        let _ = tx.send(LoadMsg::Chunk(generation, batch));
    }
}

pub fn spawn_listing(path: PathBuf, generation: u64, search: Option<String>) -> Receiver<LoadMsg> {
    let (tx, rx): (Sender<LoadMsg>, Receiver<LoadMsg>) = channel();
    std::thread::Builder::new()
        .name("vx-list".into())
        .spawn(move || {
            let mut batch = Vec::with_capacity(CHUNK);
            if let Some(needle) = search.filter(|n| !n.is_empty()) {
                walk_search(&path, &needle, generation, &tx);
                let _ = tx.send(LoadMsg::Done(generation));
                return;
            }
            if path.to_string_lossy() == "vantaexplorer://recent" {
                if let Some(dir) = state_dir() {
                    if let Ok(text) = std::fs::read_to_string(dir.join("recent.jsonl")) {
                        let mut seen = std::collections::HashSet::new();
                        for line in text.lines().rev() {
                            let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else { continue; };
                            let Some(p) = v["path"].as_str() else { continue; };
                            let real_path = PathBuf::from(p);
                            if !seen.insert(real_path.clone()) || !real_path.exists() {
                                continue;
                            }
                            let name = real_path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                            let hidden = name.starts_with('.');
                            let meta = real_path.metadata().or_else(|_| real_path.symlink_metadata()).ok();
                            let is_dir = meta.as_ref().map(|m| m.is_dir()).unwrap_or(false);
                            let is_symlink = real_path.symlink_metadata().map(|m| m.file_type().is_symlink()).unwrap_or(false);
                            batch.push(Entry {
                                path: real_path,
                                is_dir,
                                is_symlink,
                                size: meta.as_ref().map(|m| m.len()).unwrap_or(0),
                                modified: meta.and_then(|m| m.modified().ok()),
                                hidden,
                                name,
                            });
                            if batch.len() >= CHUNK {
                                if tx.send(LoadMsg::Chunk(generation, std::mem::take(&mut batch))).is_err() {
                                    return;
                                }
                                batch.reserve(CHUNK);
                            }
                        }
                    }
                }
            } else {
                let rd = match std::fs::read_dir(&path) {
                    Ok(rd) => rd,
                    Err(e) => {
                        let _ = tx.send(LoadMsg::Error(generation, e.to_string()));
                        return;
                    }
                };
                // The trash view: the home trash, and after it the trash of every other drive.
                let others: Vec<std::fs::ReadDir> = if crate::fileops::in_trash(&path) {
                    crate::fileops::trash_folders().iter().skip(1).filter_map(|t| std::fs::read_dir(t.join("files")).ok()).collect()
                } else {
                    Vec::new()
                };
                for dent in rd.flatten().chain(others.into_iter().flat_map(|r| r.flatten())) {
                let name = dent.file_name().to_string_lossy().into_owned();
                let hidden = name.starts_with('.');
                let ft = dent.file_type().ok();
                let is_symlink = ft.map(|t| t.is_symlink()).unwrap_or(false);
                // metadata() follows symlinks, so a link to a directory browses like one.
                let meta = dent.path().metadata().or_else(|_| dent.metadata()).ok();
                let is_dir = meta.as_ref().map(|m| m.is_dir()).unwrap_or(false);
                batch.push(Entry {
                    path: dent.path(),
                    is_dir,
                    is_symlink,
                    size: meta.as_ref().map(|m| m.len()).unwrap_or(0),
                    modified: meta.and_then(|m| m.modified().ok()),
                    hidden,
                    name,
                });
                if batch.len() >= CHUNK {
                    if tx.send(LoadMsg::Chunk(generation, std::mem::take(&mut batch))).is_err() {
                        return; // listing superseded; stop reading
                    }
                    batch.reserve(CHUNK);
                }
            }
            }
            if !batch.is_empty() {
                let _ = tx.send(LoadMsg::Chunk(generation, batch));
            }
            let _ = tx.send(LoadMsg::Done(generation));
        })
        .expect("spawn listing thread");
    rx
}

/// Files, folders, and the byte total of the FILES ONLY.
///
/// A directory's `size` is its inode, 4 KiB or so on most filesystems, and has nothing to
/// do with what is inside it. Summing it with real file sizes produced a total that was
/// neither the files nor the contents, just quietly wrong by a few KiB per folder. Callers
/// report the folder count separately rather than pretend to a number they do not have;
/// getting the real one means walking every tree, which `dir_size` does for ONE target at a
/// time and cannot do for a live selection.
pub fn selection_totals<'a>(entries: impl Iterator<Item = &'a Entry>) -> (usize, usize, u64) {
    let mut files = 0usize;
    let mut dirs = 0usize;
    let mut bytes = 0u64;
    for e in entries {
        if e.is_dir {
            dirs += 1;
        } else {
            files += 1;
            bytes += e.size;
        }
    }
    (files, dirs, bytes)
}

pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 6] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"];
    let mut v = bytes as f64;
    let mut unit = 0;
    while v >= 1024.0 && unit < UNITS.len() - 1 {
        v /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{v:.1} {}", UNITS[unit])
    }
}

/// Absolute local date/time via the configured strftime pattern. Deliberately never
/// relative ("3 d ago"): the Modified column shows real dates, and chrono handles
/// the local timezone and DST that hand-rolled epoch arithmetic would get wrong.
pub fn format_mtime(t: Option<SystemTime>, fmt: &str) -> String {
    let Some(t) = t else { return String::new() };
    let dt: chrono::DateTime<chrono::Local> = t.into();
    dt.format(fmt).to_string()
}

/// Append to the recent-files log; feeds the sidebar's Recent section.
pub fn log_recent(path: &Path) {
    let Some(state) = state_dir() else { return };
    let _ = std::fs::create_dir_all(&state);
    let ts = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let line = format!("{{\"ts\":{ts},\"path\":{}}}\n", serde_json::to_string(&path.to_string_lossy()).unwrap_or_default());
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(state.join("recent.jsonl")) {
        let _ = f.write_all(line.as_bytes());
    }
}

pub fn state_dir() -> Option<PathBuf> {
    std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/state")))
        .map(|p| p.join("vantaexplorer"))
}

#[cfg(test)]
mod search_tests {
    use super::*;

    fn collect(root: &Path, needle: &str) -> Vec<String> {
        let (tx, rx) = channel();
        walk_search(root, needle, 1, &tx);
        drop(tx);
        let mut names = Vec::new();
        while let Ok(LoadMsg::Chunk(_, batch)) = rx.recv() {
            names.extend(batch.into_iter().map(|e| e.name));
        }
        names.sort();
        names
    }

    /// The reason results are keyed by relative path: selection is name-keyed, so two files
    /// called the same thing in different subfolders must stay distinguishable.
    #[test]
    fn same_name_in_two_branches_stays_distinct() {
        let tmp = std::env::temp_dir().join(format!("vantaexplorer-search-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join("a/deep")).unwrap();
        std::fs::create_dir_all(tmp.join("b")).unwrap();
        std::fs::write(tmp.join("a/deep/rapport.txt"), "x").unwrap();
        std::fs::write(tmp.join("b/rapport.txt"), "x").unwrap();
        std::fs::write(tmp.join("andet.md"), "x").unwrap();

        let hits = collect(&tmp, "rapport");
        assert_eq!(hits, vec!["a/deep/rapport.txt", "b/rapport.txt"]);

        // Case-insensitive, and it reaches the top level too.
        assert_eq!(collect(&tmp, "ANDET"), vec!["andet.md"]);
        assert!(collect(&tmp, "findes-ikke").is_empty());

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// A symlinked directory pointing at its own ancestor must not send the walk into a loop.
    #[test]
    #[cfg(unix)]
    fn symlink_loop_terminates() {
        let tmp = std::env::temp_dir().join(format!("vantaexplorer-loop-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join("sub")).unwrap();
        std::fs::write(tmp.join("sub/mål.txt"), "x").unwrap();
        std::os::unix::fs::symlink(&tmp, tmp.join("sub/loop")).unwrap();

        assert_eq!(collect(&tmp, "mål"), vec!["sub/mål.txt"]);
        let _ = std::fs::remove_dir_all(&tmp);
    }
}

#[cfg(test)]
mod selection_tests {
    use super::*;

    fn entry(name: &str, is_dir: bool, size: u64) -> Entry {
        Entry {
            path: PathBuf::from(name),
            name: name.into(),
            is_dir,
            is_symlink: false,
            size,
            modified: None,
            hidden: false,
        }
    }

    /// The bug this guards: a directory's `size` is its inode, so summing it with file sizes
    /// added a few KiB of nonsense per folder to every mixed selection.
    #[test]
    fn folder_inodes_are_not_counted_as_content() {
        let items = [
            entry("a.txt", false, 1000),
            entry("photos", true, 4096),
            entry("b.txt", false, 500),
            entry("docs", true, 4096),
        ];
        let (files, dirs, bytes) = selection_totals(items.iter());
        assert_eq!((files, dirs), (2, 2));
        assert_eq!(bytes, 1500, "folder inodes must not be in the byte total");
    }

    #[test]
    fn files_only_and_folders_only() {
        let files = [entry("a", false, 7), entry("b", false, 3)];
        assert_eq!(selection_totals(files.iter()), (2, 0, 10));

        let dirs = [entry("x", true, 4096)];
        assert_eq!(selection_totals(dirs.iter()), (0, 1, 0));

        assert_eq!(selection_totals([].iter()), (0, 0, 0));
    }
}
