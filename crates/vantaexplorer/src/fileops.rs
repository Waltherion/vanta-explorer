//! File operations on a worker thread, with progress, cancellation and conflict prompts.
//!
//! One worker processes operations sequentially: parallel ops into the same directory create
//! more races than they save time, and sequential progress is honest progress.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;

const CHUNK: usize = 1 << 20; // 1 MiB copy chunks: fine progress, cheap cancellation checks

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum OpKind {
    #[default]
    Copy,
    Move,
    Trash,
    Delete,
    /// Undo of a Move: rename each `pairs` entry back, exact target paths. A normal Move
    /// only knows a destination *directory*, which cannot express "put each file back where
    /// it individually came from".
    MoveExact,
    /// Put `trash_items` back where they came from (undo of a Trash, or Restore in the
    /// trash view).
    RestoreTrash,
    /// Purge everything in every trash folder. Never undoable.
    EmptyTrash,
    /// Unpack `sources[0]` into `dest`.
    Extract,
    /// Pack `sources` into one archive in `dest`, format from `Request::format`.
    Compress,
}

impl OpKind {
    pub fn label(self) -> &'static str {
        match self {
            OpKind::Copy => "Copying",
            OpKind::Move => "Moving",
            OpKind::Trash => "Moving to trash",
            OpKind::Delete => "Deleting",
            OpKind::MoveExact => "Moving",
            OpKind::RestoreTrash => "Restoring from trash",
            OpKind::EmptyTrash => "Emptying trash",
            OpKind::Extract => "Extracting",
            OpKind::Compress => "Compressing",
        }
    }

    /// Permanent deletion is the one thing no undo can reverse, so it never reaches the stack.
    pub fn undoable(self) -> bool {
        !matches!(self, OpKind::Delete | OpKind::EmptyTrash)
    }
}

#[derive(Default)]
pub struct Request {
    pub kind: OpKind,
    pub sources: Vec<PathBuf>,
    /// Destination directory for Copy/Move; ignored for Trash/Delete.
    pub dest: Option<PathBuf>,
    /// MoveExact only: (from, to) pairs.
    pub pairs: Vec<(PathBuf, PathBuf)>,
    /// RestoreTrash only.
    pub trash_items: Vec<trash::TrashItem>,
    /// Compress only: the archive suffix, e.g. "tar.zst". bsdtar picks the format from it.
    pub format: Option<String>,
}

impl Request {
    pub fn new(kind: OpKind, sources: Vec<PathBuf>, dest: Option<PathBuf>) -> Self {
        Self { kind, sources, dest, ..Default::default() }
    }
}

/// What an operation actually did, in enough detail to reverse it.
///
/// This has to be *observed*, not derived from the Request: a conflict may have skipped
/// sources, and deriving `dest.join(name)` for every source would then "undo" files that were
/// never touched.
#[derive(Debug, Clone)]
pub enum UndoAction {
    /// (from, to) for every item that actually moved.
    Moved(Vec<(PathBuf, PathBuf)>),
    /// Top-level paths a copy created.
    Copied(Vec<PathBuf>),
    /// Items now sitting in the trash.
    Trashed(Vec<trash::TrashItem>),
    /// Items that were put back from the trash.
    Restored(Vec<PathBuf>),
    Renamed { from: PathBuf, to: PathBuf },
}

#[derive(Debug, Clone)]
pub struct UndoRecord {
    pub action: UndoAction,
    /// Files an overwrite displaced. They go to the trash rather than being deleted, so undo
    /// can put them back, otherwise "overwrite" would be a silently unrecoverable action.
    /// Restored AFTER the main action is reversed, since their paths are occupied until then.
    pub displaced: Vec<trash::TrashItem>,
    /// Folders the action made that undo removes again, once undo has left them empty: where
    /// a move that left some files behind put the rest. Deepest first.
    pub emptied: Vec<PathBuf>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ConflictAnswer {
    Overwrite,
    Skip,
    /// Keep both: the incoming item gets a free numbered name, the existing one is untouched.
    Rename,
    OverwriteAll,
    SkipAll,
    RenameAll,
    /// Both are folders: put the contents of the one into the other.
    Merge,
    MergeAll,
    Cancel,
}

pub enum Event {
    Started {
        id: u64,
        kind: OpKind,
        total_bytes: u64,
        total_items: u64,
    },
    Progress {
        id: u64,
        bytes_done: u64,
        items_done: u64,
        current: String,
    },
    /// The worker is blocked until the reply channel receives an answer.
    Conflict {
        id: u64,
        name: String,
        /// Both are folders, so they can be merged.
        folders: bool,
        reply: Sender<ConflictAnswer>,
    },
    /// One item failed; the worker is blocked until the reply says what to do about it.
    Error {
        id: u64,
        name: String,
        error: String,
        reply: Sender<ErrorAnswer>,
    },
    Done {
        id: u64,
        affected: Vec<PathBuf>,
        /// How to reverse what just happened; None when there is nothing to undo.
        undo: Option<UndoRecord>,
        /// Something worth telling the user although the operation as a whole went through
        /// (a restore that put back all but one item).
        warning: Option<String>,
    },
    Failed {
        id: u64,
        error: String,
        affected: Vec<PathBuf>,
    },
}

pub struct FileOps {
    tx: Sender<(u64, Request, Arc<AtomicBool>)>,
    pub rx: Receiver<Event>,
    next_id: u64,
    /// Cancellation flags for in-flight ops, by id.
    cancels: std::collections::HashMap<u64, Arc<AtomicBool>>,
}

impl FileOps {
    pub fn new() -> Self {
        let (req_tx, req_rx) = channel::<(u64, Request, Arc<AtomicBool>)>();
        let (ev_tx, ev_rx) = channel::<Event>();
        std::thread::Builder::new()
            .name("vx-fileops".into())
            .spawn(move || {
                while let Ok((id, req, cancel)) = req_rx.recv() {
                    run(id, req, &cancel, &ev_tx);
                }
            })
            .expect("spawn fileops worker");
        Self {
            tx: req_tx,
            rx: ev_rx,
            next_id: 1,
            cancels: Default::default(),
        }
    }

    pub fn submit(&mut self, req: Request) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        let cancel = Arc::new(AtomicBool::new(false));
        self.cancels.insert(id, Arc::clone(&cancel));
        let _ = self.tx.send((id, req, cancel));
        id
    }

    pub fn cancel(&mut self, id: u64) {
        if let Some(c) = self.cancels.get(&id) {
            c.store(true, Ordering::Relaxed);
        }
    }

    pub fn forget(&mut self, id: u64) {
        self.cancels.remove(&id);
    }
}

/// Rename within a directory; synchronous because it is instant and inline UI wants the
/// result immediately.
pub fn rename(path: &Path, new_name: &str) -> Result<PathBuf, String> {
    if new_name.is_empty() || new_name.contains('/') {
        return Err("invalid name".into());
    }
    let dest = path.with_file_name(new_name);
    rename_no_replace(path, &dest).map_err(|e| match e.kind() {
        std::io::ErrorKind::AlreadyExists => "a file with that name already exists".to_string(),
        _ => e.to_string(),
    })?;
    Ok(dest)
}

/// Rename that never replaces anything at `to`, a dead link included: the kernel checks and
/// renames in one step (RENAME_NOREPLACE), so nothing created between a check and the rename
/// can be overwritten either. Where a filesystem cannot do that, it is checked first.
///
/// The one exception is `to` being `from` itself under another spelling, which a filesystem
/// that ignores case (exFAT, NTFS) says of "Notes" and "notes": that rename is let through.
pub fn rename_no_replace(from: &Path, to: &Path) -> std::io::Result<()> {
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::MetadataExt;
    let same_file = |a: &Path, b: &Path| match (std::fs::symlink_metadata(a), std::fs::symlink_metadata(b)) {
        (Ok(a), Ok(b)) => a.dev() == b.dev() && a.ino() == b.ino(),
        _ => false,
    };
    let c = |p: &Path| std::ffi::CString::new(p.as_os_str().as_bytes()).map_err(|_| std::io::Error::from(std::io::ErrorKind::InvalidInput));
    let (cf, ct) = (c(from)?, c(to)?);
    let r = unsafe { libc::renameat2(libc::AT_FDCWD, cf.as_ptr(), libc::AT_FDCWD, ct.as_ptr(), libc::RENAME_NOREPLACE) };
    if r == 0 {
        return Ok(());
    }
    let e = std::io::Error::last_os_error();
    log::debug!("rename {} -> {}: {e} (same file: {})", from.display(), to.display(), same_file(from, to));
    match e.raw_os_error() {
        // exFAT takes a rename that only changes case as done and leaves the old spelling, so
        // it goes by way of a name of its own.
        Some(libc::EEXIST) if from != to && same_file(from, to) => {
            let step = from.with_file_name(format!(".{}.vx-rename", std::process::id()));
            std::fs::rename(from, &step)?;
            std::fs::rename(&step, to).or_else(|e| std::fs::rename(&step, from).and(Err(e)))
        }
        // The filesystem (or kernel) has no RENAME_NOREPLACE: check, then rename.
        Some(libc::EINVAL | libc::ENOSYS | libc::EOPNOTSUPP) => {
            if to.symlink_metadata().is_ok() && !same_file(from, to) {
                return Err(std::io::Error::from(std::io::ErrorKind::AlreadyExists));
            }
            std::fs::rename(from, to)
        }
        _ => Err(e),
    }
}

/// Create a uniquely named folder and return it, so the UI can start an inline rename on it.
/// Create an empty "New File" (then "New File 1", ...). `create_new` makes the existence
/// check and the creation one atomic step, so two panes racing cannot both claim a name.
pub fn new_file(parent: &Path) -> Result<PathBuf, String> {
    for i in 0..100 {
        let name = if i == 0 { "New File".to_string() } else { format!("New File ({i})") };
        let path = parent.join(&name);
        match std::fs::OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(_) => return Ok(path),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e.to_string()),
        }
    }
    Err("ran out of names".into())
}

pub fn new_folder(parent: &Path) -> Result<PathBuf, String> {
    for i in 0..100 {
        let name = if i == 0 { "New Folder".to_string() } else { format!("New Folder ({i})") };
        let dir = parent.join(&name);
        match std::fs::create_dir(&dir) {
            Ok(()) => return Ok(dir),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e.to_string()),
        }
    }
    Err("ran out of names".into())
}

fn run(id: u64, req: Request, cancel: &AtomicBool, ev: &Sender<Event>) {
    let mut affected: Vec<PathBuf> = req
        .sources
        .iter()
        .filter_map(|s| s.parent().map(Into::into))
        .collect();
    if let Some(d) = &req.dest {
        affected.push(d.clone());
    }
    // The undo kinds carry their paths in `pairs`/`trash_items`, not `sources`; without this
    // an undo would complete without any pane refreshing.
    for (from, to) in &req.pairs {
        affected.extend(from.parent().map(Into::into));
        affected.extend(to.parent().map(Into::into));
    }
    for item in &req.trash_items {
        affected.push(item.original_parent.clone());
    }
    if matches!(req.kind, OpKind::Trash | OpKind::RestoreTrash | OpKind::EmptyTrash) {
        // The trash view is a pane like any other and must refresh too.
        affected.push(trash_files_dir());
    }
    affected.sort();
    affected.dedup();

    // Filled in by whichever branch runs; None means "nothing to undo".
    let mut undo: Option<UndoRecord> = None;
    let mut warning: Option<String> = None;

    let result = match req.kind {
        OpKind::Trash => {
            let _ = ev.send(Event::Started {
                id,
                kind: req.kind,
                total_bytes: 0,
                total_items: req.sources.len() as u64,
            });
            // Record the resulting trash entries immediately, while nothing else can have
            // trashed a file with the same original path.
            trash::delete_all(&req.sources)
                .map_err(|e| format!("{e:?}"))
                .map(|()| {
                    let items: Vec<_> = req
                        .sources
                        .iter()
                        .filter_map(|s| newest_trash_entry(s))
                        .collect();
                    if !items.is_empty() {
                        undo = Some(UndoRecord {
                            action: UndoAction::Trashed(items),
                            displaced: Vec::new(),
                            emptied: Vec::new(),
                        });
                    }
                })
        }
        OpKind::Delete => {
            let _ = ev.send(Event::Started {
                id,
                kind: req.kind,
                total_bytes: 0,
                total_items: req.sources.len() as u64,
            });
            req.sources.iter().try_for_each(|p| {
                // Inside the trash, a delete is a purge: the .trashinfo must go with the
                // file, or the trash listing keeps a ghost entry forever.
                if let Some(item) = trash_item_at(p) {
                    return trash::os_limited::purge_all([item]).map_err(|e| format!("{e:?}"));
                }
                // An entry whose info cannot be read: the file goes, and the info with it.
                if let Some(info) = info_file_of(p) {
                    let _ = std::fs::remove_file(info);
                }
                let meta = std::fs::symlink_metadata(p).map_err(|e| e.to_string())?;
                if meta.is_dir() {
                    std::fs::remove_dir_all(p).map_err(|e| e.to_string())
                } else {
                    std::fs::remove_file(p).map_err(|e| e.to_string())
                }
            })
        }
        OpKind::Copy | OpKind::Move => transfer(id, &req, cancel, ev, &mut undo, &mut warning),
        OpKind::Extract => extract(id, &req, cancel, ev, &mut undo),
        OpKind::Compress => compress(id, &req, cancel, ev, &mut undo),

        // --- undo operations ---
        OpKind::MoveExact => {
            let _ = ev.send(Event::Started {
                id,
                kind: req.kind,
                total_bytes: 0,
                total_items: req.pairs.len() as u64,
            });
            // Reverse order: a move may have emptied a directory that a later pair recreates.
            let mut back: Vec<(PathBuf, PathBuf)> = Vec::new();
            let r = req.pairs.iter().rev().enumerate().try_for_each(|(n, (from, to))| {
                if let Some(parent) = to.parent() {
                    std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
                }
                move_path(from, to, cancel)?;
                back.push((to.clone(), from.clone()));
                let _ = ev.send(Event::Progress {
                    id,
                    bytes_done: 0,
                    items_done: n as u64 + 1,
                    current: to.file_name().unwrap_or_default().to_string_lossy().into_owned(),
                });
                Ok(())
            });
            // Folders the move made, gone again now that undo has emptied them; a folder that still
            // holds something stays (remove_dir only takes empty ones).
            if r.is_ok() {
                for dir in &req.sources {
                    let _ = std::fs::remove_dir(dir);
                }
            }
            if r.is_ok() && !back.is_empty() {
                undo = Some(UndoRecord { action: UndoAction::Moved(back), displaced: Vec::new(), emptied: Vec::new() });
            }
            r
        }
        OpKind::RestoreTrash => {
            let _ = ev.send(Event::Started {
                id,
                kind: req.kind,
                total_bytes: 0,
                total_items: req.trash_items.len() as u64,
            });
            // restore_all is all-or-nothing and has no force option: one occupied original
            // path and it restores NOTHING. So restore one item at a time, keep going past
            // failures, and report them together at the end. Undo covers what came back.
            let mut restored: Vec<PathBuf> = Vec::new();
            let mut failures: Vec<String> = Vec::new();
            for (n, item) in req.trash_items.iter().enumerate() {
                match trash::os_limited::restore_all([item.clone()]) {
                    Ok(()) => restored.push(item.original_path()),
                    Err(trash::Error::RestoreCollision { path, .. }) => {
                        failures.push(format!("'{}' already exists", path.display()))
                    }
                    Err(trash::Error::RestoreTwins { path, .. }) => failures.push(format!(
                        "two trashed items share the path '{}'",
                        path.display()
                    )),
                    Err(other) => failures.push(format!("{other:?}")),
                }
                let _ = ev.send(Event::Progress {
                    id,
                    bytes_done: 0,
                    items_done: n as u64 + 1,
                    current: item.name.to_string_lossy().into_owned(),
                });
            }
            if !restored.is_empty() {
                undo = Some(UndoRecord {
                    action: UndoAction::Restored(restored),
                    displaced: Vec::new(),
                    emptied: Vec::new(),
                });
            }
            match (failures.is_empty(), undo.is_some()) {
                (true, _) => Ok(()),
                (false, true) => {
                    warning = Some(format!("Not restored: {}", failures.join("; ")));
                    Ok(())
                }
                (false, false) => Err(format!("cannot restore: {}", failures.join("; "))),
            }
        }
        OpKind::EmptyTrash => {
            let _ = ev.send(Event::Started {
                id,
                kind: req.kind,
                total_bytes: 0,
                total_items: 0,
            });
            let r = trash::os_limited::list()
                .and_then(trash::os_limited::purge_all)
                .map_err(|e| format!("{e:?}"));
            // What the listing could not read stays behind otherwise: an entry whose info file
            // is damaged (all zeros after a drive was pulled, say) is shown, but never purged.
            let folders = trash_folders();
            let left = empty_trash_folders(&folders);
            remove_orphaned_info(&folders);
            match (r, left.is_empty()) {
                (Err(e), _) => Err(e),
                (Ok(()), true) => Ok(()),
                (Ok(()), false) => Err(format!("could not delete: {}", left.join("; "))),
            }
        }
    };

    if !req.kind.undoable() {
        undo = None;
    }

    match result {
        Ok(()) => {
            let _ = ev.send(Event::Done {
                id,
                affected,
                undo,
                warning,
            });
        }
        Err(e) => {
            let _ = ev.send(Event::Failed {
                id,
                error: e,
                affected,
            });
        }
    }
}

/// The home trash's `files` directory: what the sidebar's Trash entry opens.
pub fn trash_files_dir() -> PathBuf {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/share")
        })
        .join("Trash/files")
}

/// Is this folder the trash view?
pub fn in_trash(dir: &Path) -> bool {
    dir == trash_files_dir()
}

/// Where a trashed item's file lives. On freedesktop the id is the .trashinfo path
/// (`<trash>/info/<name>.trashinfo`) and the payload sits at `<trash>/files/<name>`, which
/// is also what the trash view lists, so this is the bridge from a listed Entry back to
/// the TrashItem that can restore or purge it.
pub fn trash_item_file(item: &trash::TrashItem) -> Option<PathBuf> {
    let trash_root = Path::new(&item.id).parent()?.parent()?;
    Some(trash_root.join("files").join(&item.name))
}

/// The trash entry whose payload is at `path`, if `path` is inside a trash folder.
pub fn trash_item_at(path: &Path) -> Option<trash::TrashItem> {
    // Cheap shape check: <trash>/files/<name> next to <trash>/info.
    let files_dir = path.parent()?;
    if files_dir.file_name()? != "files" || !files_dir.parent()?.join("info").is_dir() {
        return None;
    }
    item_from_info(files_dir.parent()?, path.file_name()?).or_else(|| {
        trash::os_limited::list().ok()?.into_iter().find(|i| trash_item_file(i).is_some_and(|f| f == path))
    })
}

/// Trash entries for the given payload paths (selection in the trash view), in order.
pub fn trash_items_at(paths: &[PathBuf]) -> Vec<trash::TrashItem> {
    let quick: Vec<Option<trash::TrashItem>> = paths
        .iter()
        .map(|p| item_from_info(p.parent()?.parent()?, p.file_name()?))
        .collect();
    if quick.iter().all(Option::is_some) {
        return quick.into_iter().flatten().collect();
    }
    let Ok(all) = trash::os_limited::list() else { return Vec::new() };
    let by_file: std::collections::HashMap<PathBuf, trash::TrashItem> = all
        .into_iter()
        .filter_map(|i| trash_item_file(&i).map(|f| (f, i)))
        .collect();
    paths.iter().filter_map(|p| by_file.get(p).cloned()).collect()
}

/// The entry for `<root>/files/<name>`, from its own `<root>/info/<name>.trashinfo`.
fn item_from_info(root: &Path, name: &std::ffi::OsStr) -> Option<trash::TrashItem> {
    use std::os::unix::ffi::OsStrExt;
    let mut info_name = name.to_os_string();
    info_name.push(".trashinfo");
    let info = root.join("info").join(info_name);
    let text = std::fs::read_to_string(&info).ok()?;
    let recorded = text.lines().find_map(|l| l.strip_prefix("Path="))?;
    let mut original = PathBuf::from(std::ffi::OsStr::from_bytes(&percent_decode(recorded.trim())));
    if original.is_relative() {
        original = mount_top(root)?.join(original);
    }
    let when = std::fs::metadata(&info).and_then(|m| m.modified()).ok()?;
    Some(trash::TrashItem {
        id: info.into_os_string(),
        name: original.file_name()?.to_os_string(),
        original_parent: original.parent()?.to_path_buf(),
        time_deleted: when.duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0),
    })
}

/// Every trash folder there is for this user: the home trash first, then one per mounted
/// drive that has one (`.Trash/<uid>` or `.Trash-<uid>` at its top), which is where a file
/// trashed on that drive goes. The trash view shows them together.
pub fn trash_folders() -> Vec<PathBuf> {
    let home = trash_files_dir().parent().map(Path::to_path_buf).unwrap_or_default();
    let mut out = vec![home.clone()];
    let uid = unsafe { libc::getuid() };
    let mounts = std::fs::read_to_string("/proc/self/mounts").unwrap_or_default();
    for line in mounts.lines() {
        let mut f = line.split(' ');
        let (Some(_), Some(mnt), Some(fstype)) = (f.next(), f.next(), f.next()) else { continue };
        // Kernel filesystems hold no trash, and looking in a FUSE mount can hang on the network.
        if matches!(fstype, "proc" | "sysfs" | "devtmpfs" | "devpts" | "cgroup" | "cgroup2" | "securityfs" | "debugfs" | "tracefs" | "configfs" | "pstore" | "bpf" | "mqueue" | "hugetlbfs" | "autofs" | "efivarfs" | "binfmt_misc" | "fusectl" | "nsfs" | "ramfs" | "rpc_pipefs" | "overlay" | "squashfs") || fstype.starts_with("fuse") {
            continue;
        }
        let mnt = PathBuf::from(mnt.replace("\\040", " ").replace("\\011", "\t").replace("\\134", "\\"));
        for t in [mnt.join(".Trash").join(uid.to_string()), mnt.join(format!(".Trash-{uid}"))] {
            if t != home && !out.contains(&t) && t.join("files").is_dir() {
                out.push(t);
            }
        }
    }
    out
}

/// Entries whose file is gone (deleted from the trash by hand, or by a program that left the
/// info behind): never shown, never restorable, so they go when the trash is emptied.
/// `<trash>/info/<name>.trashinfo` for a payload `<trash>/files/<name>`, when `path` is one.
fn info_file_of(path: &Path) -> Option<PathBuf> {
    let files = path.parent()?;
    if files.file_name()? != "files" {
        return None;
    }
    let mut name = path.file_name()?.to_os_string();
    name.push(".trashinfo");
    let info = files.parent()?.join("info").join(name);
    info.is_file().then_some(info)
}

/// Whatever is still in the trash folders' files/ after the purge, deleted outright, with its
/// info. Returns what could not be deleted.
fn empty_trash_folders(folders: &[PathBuf]) -> Vec<String> {
    let mut left = Vec::new();
    for root in folders {
        let Ok(rd) = std::fs::read_dir(root.join("files")) else { continue };
        for entry in rd.flatten() {
            let path = entry.path();
            if let Err(e) = remove_any(&path) {
                left.push(format!("{} ({e})", path.display()));
                continue;
            }
            if let Some(info) = info_file_of(&path) {
                let _ = std::fs::remove_file(info);
            }
        }
    }
    left
}

fn remove_orphaned_info(folders: &[PathBuf]) {
    use std::os::unix::ffi::OsStrExt;
    for root in folders {
        let Ok(rd) = std::fs::read_dir(root.join("info")) else { continue };
        for entry in rd.flatten() {
            let name = entry.file_name();
            let Some(payload) = name.as_bytes().strip_suffix(b".trashinfo") else { continue };
            if root.join("files").join(std::ffi::OsStr::from_bytes(payload)).symlink_metadata().is_err() {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
}

/// Send a path to the trash and return the resulting entry, so it can be restored later.
///
/// `trash::delete_all` reports nothing about what it created, so the entry is recovered by
/// listing the trash and matching on the original path, newest deletion winning.
fn trash_and_record(path: &Path) -> Result<Option<trash::TrashItem>, String> {
    trash::delete_all([path]).map_err(|e| format!("{e:?}"))?;
    Ok(newest_trash_entry(path))
}

fn newest_trash_entry(original: &Path) -> Option<trash::TrashItem> {
    find_trash_entry(original).or_else(|| list_trash_entry(original))
}

/// The entry just made for `original`, found by its name in the trash folders it can be in:
/// a directory listing and a few small files, where listing the trash reads every entry it
/// holds. That was the cost of each overwritten or trashed file, so 500 of them with a full
/// trash took minutes.
fn find_trash_entry(original: &Path) -> Option<trash::TrashItem> {
    use std::os::unix::ffi::OsStrExt;
    let name = original.file_name()?;
    // The trash crate records the path with its folders resolved.
    let resolved = original.parent().and_then(|p| std::fs::canonicalize(p).ok()).map(|p| p.join(name));
    let top = mount_top(resolved.as_deref().unwrap_or(original));
    let uid = unsafe { libc::getuid() };
    let mut folders = vec![trash_files_dir().parent()?.to_path_buf()];
    if let Some(top) = &top {
        folders.push(top.join(".Trash").join(uid.to_string()));
        folders.push(top.join(format!(".Trash-{uid}")));
    }
    let mut best: Option<(std::time::SystemTime, trash::TrashItem)> = None;
    for folder in folders {
        let info = folder.join("info");
        let Ok(rd) = std::fs::read_dir(&info) else { continue };
        for entry in rd.flatten() {
            let file = entry.file_name();
            // "<name>.trashinfo", or "<name>.<n>.trashinfo" when the name was taken.
            let b = file.as_bytes();
            let Some(stem) = b.strip_suffix(b".trashinfo") else { continue };
            let Some(rest) = stem.strip_prefix(name.as_bytes()) else { continue };
            if !(rest.is_empty() || rest.strip_prefix(b".").is_some_and(|n| !n.is_empty() && n.iter().all(u8::is_ascii_digit))) {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(entry.path()) else { continue };
            let Some(recorded) = text.lines().find_map(|l| l.strip_prefix("Path=")) else { continue };
            let mut path = PathBuf::from(std::ffi::OsString::from(std::ffi::OsStr::from_bytes(&percent_decode(recorded.trim()))));
            if path.is_relative() {
                path = top.as_ref()?.join(path);
            }
            if path != original && Some(&path) != resolved.as_ref() {
                continue;
            }
            let Ok(when) = entry.metadata().and_then(|m| m.modified()) else { continue };
            if best.as_ref().is_some_and(|(t, _)| *t >= when) {
                continue;
            }
            let item = trash::TrashItem {
                id: entry.path().into_os_string(),
                name: name.to_os_string(),
                original_parent: path.parent()?.to_path_buf(),
                time_deleted: when.duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0),
            };
            best = Some((when, item));
        }
    }
    best.map(|(_, item)| item)
}

/// The mount point `path` is on: the longest one that is a prefix of it.
fn mount_top(path: &Path) -> Option<PathBuf> {
    let mounts = std::fs::read_to_string("/proc/self/mounts").ok()?;
    mounts
        .lines()
        .filter_map(|l| l.split(' ').nth(1))
        // Spaces and the like are octal escapes there ("\040").
        .map(|m| PathBuf::from(m.replace("\\040", " ").replace("\\011", "\t").replace("\\134", "\\")))
        .filter(|m| path.starts_with(m))
        .max_by_key(|m| m.as_os_str().len())
}

fn percent_decode(s: &str) -> Vec<u8> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let hex = |c: u8| (c as char).to_digit(16);
        if b[i] == b'%' && i + 2 < b.len() {
            if let (Some(h), Some(l)) = (hex(b[i + 1]), hex(b[i + 2])) {
                out.push((h * 16 + l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    out
}

/// The slow way, by listing everything in the trash, if the entry was not where expected.
fn list_trash_entry(original: &Path) -> Option<trash::TrashItem> {
    trash::os_limited::list()
        .ok()?
        .into_iter()
        .filter(|i| i.original_path() == original)
        .max_by_key(|i| i.time_deleted)
}

/// Copy/Move with a pre-scan for totals and per-chunk progress.
/// Extensions we offer to unpack. bsdtar is libarchive, so one tool covers all of these,
/// which is why this shells out rather than pulling in a crate per format.
pub fn is_archive(path: &Path) -> bool {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    const DOUBLE: &[&str] = &[".tar.gz", ".tar.bz2", ".tar.xz", ".tar.zst", ".tar.lz4", ".tar"];
    const SINGLE: &[&str] = &[
        ".zip", ".7z", ".rar", ".tgz", ".tbz2", ".txz", ".tzst", ".iso", ".cab", ".jar",
        ".xpi", ".whl", ".cbz", ".cbr", ".deb", ".rpm",
    ];
    DOUBLE.iter().chain(SINGLE).any(|e| name.ends_with(e))
}

/// Where "extract to folder" should put things: the archive name with its suffixes removed.
pub fn extract_dir_name(path: &Path) -> String {
    let mut name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "archive".into());
    // Strip once, then again, so "x.tar.gz" becomes "x" and not "x.tar".
    for _ in 0..2 {
        if let Some(dot) = name.rfind('.') {
            if dot > 0 {
                name.truncate(dot);
            }
        }
    }
    name
}

/// A name not yet taken in `dir`: "notes.zip", then "notes-1.zip", and so on.
///
/// Compressing must never overwrite: the archive you would clobber may be the only copy of
/// something, and unlike a file operation there is no conflict prompt to fall back on.
fn free_archive_path(dir: &Path, stem: &str, suffix: &str) -> PathBuf {
    let mut candidate = dir.join(format!("{stem}.{suffix}"));
    let mut n = 1;
    while candidate.exists() {
        candidate = dir.join(format!("{stem}-{n}.{suffix}"));
        n += 1;
    }
    candidate
}

/// Pack `sources` into one archive.
///
/// bsdtar's `-a` picks the format from the file name, so one code path covers tar.zst,
/// tar.gz, zip and 7z. Sources are passed as names relative to their parent with `-C`, so the
/// archive contains "photos/a.png" and not "home/you/pictures/photos/a.png".
fn compress(
    id: u64,
    req: &Request,
    cancel: &AtomicBool,
    ev: &Sender<Event>,
    undo: &mut Option<UndoRecord>,
) -> Result<(), String> {
    use std::io::{BufRead, BufReader};
    use std::process::{Command, Stdio};

    if req.sources.is_empty() {
        return Err("nothing selected".into());
    }
    let suffix = req.format.clone().ok_or("no archive format given")?;
    let parent = req
        .dest
        .clone()
        .or_else(|| req.sources[0].parent().map(Into::into))
        .ok_or("no destination given")?;

    // One item names itself; several take the folder's name, which is what you meant when you
    // selected a handful of things inside it.
    let stem = if req.sources.len() == 1 {
        req.sources[0]
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "archive".into())
    } else {
        parent
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "archive".into())
    };
    let archive = free_archive_path(&parent, &stem, &suffix);

    // Count the files so the bar means something. Directories are walked, which is the same
    // work bsdtar is about to do, but on metadata only, so it is cheap next to compressing.
    let mut total_items = 0u64;
    for src in &req.sources {
        let (mut files, mut dirs, mut bytes) = (Vec::new(), Vec::new(), 0u64);
        let base = src.parent().unwrap_or(&parent);
        if scan(src, base, &mut files, &mut dirs, &mut bytes).is_ok() {
            total_items += (files.len() + dirs.len()) as u64;
        }
    }
    let _ = ev.send(Event::Started {
        id,
        kind: req.kind,
        total_bytes: 0,
        total_items,
    });

    let mut cmd = Command::new("bsdtar");
    cmd.arg("-a").arg("-c").arg("-v").arg("-f").arg(&archive);
    cmd.arg("-C").arg(&parent);
    for src in &req.sources {
        // Relative to -C, so the archive has clean paths inside it.
        match src.strip_prefix(&parent) {
            Ok(rel) => cmd.arg(rel),
            Err(_) => cmd.arg(src),
        };
    }
    let mut child = cmd
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("bsdtar: {e}"))?;

    let mut items_done = 0u64;
    let mut tail: Vec<String> = Vec::new();
    if let Some(err) = child.stderr.take() {
        for line in BufReader::new(err).lines().map_while(Result::ok) {
            if cancel.load(Ordering::Relaxed) {
                let _ = child.kill();
                break;
            }
            items_done += 1;
            tail.push(line.trim().to_string());
            if tail.len() > 4 {
                tail.remove(0);
            }
            let _ = ev.send(Event::Progress {
                id,
                bytes_done: 0,
                items_done,
                current: tail.last().cloned().unwrap_or_default(),
            });
        }
    }
    let status = child.wait().map_err(|e| e.to_string())?;

    let cancelled = cancel.load(Ordering::Relaxed);
    if cancelled || !status.success() {
        // A half-written archive is worse than none: it looks usable and is not.
        let _ = std::fs::remove_file(&archive);
        return if cancelled {
            Ok(())
        } else {
            Err(if tail.is_empty() {
                format!("bsdtar exited with {status}")
            } else {
                tail.join("; ")
            })
        };
    }

    // Undoing a compression trashes the archive it created, same shape as a copy.
    *undo = Some(UndoRecord {
        action: UndoAction::Copied(vec![archive]),
        displaced: Vec::new(),
        emptied: Vec::new(),
    });
    Ok(())
}

/// Unpack one archive into `dest`.
///
/// Runs through the worker like every other operation, so progress and cancel come for free.
/// `-k` is deliberate: bsdtar overwrites by default, and an extraction that silently eats
/// existing files would be both destructive and impossible to undo honestly.
fn extract(
    id: u64,
    req: &Request,
    cancel: &AtomicBool,
    ev: &Sender<Event>,
    undo: &mut Option<UndoRecord>,
) -> Result<(), String> {
    use std::io::{BufRead, BufReader};
    use std::process::{Command, Stdio};

    let archive = req.sources.first().ok_or("no archive given")?;
    let dest = req.dest.clone().ok_or("no destination given")?;
    std::fs::create_dir_all(&dest).map_err(|e| e.to_string())?;

    // Snapshot first, so undo can trash exactly what the archive added, and nothing that
    // happened to be there already.
    let before: std::collections::HashSet<PathBuf> = std::fs::read_dir(&dest)
        .map(|rd| rd.flatten().map(|e| e.path()).collect())
        .unwrap_or_default();

    // A counting pass, so the progress bar shows a real fraction instead of sitting at zero.
    // It costs a second read of the archive; worth it for a bar that means something.
    let total_items = Command::new("bsdtar")
        .arg("-tf")
        .arg(archive)
        .stderr(Stdio::null())
        .output()
        .map(|o| o.stdout.iter().filter(|b| **b == b'\n').count() as u64)
        .unwrap_or(0);

    let _ = ev.send(Event::Started {
        id,
        kind: req.kind,
        total_bytes: 0,
        total_items,
    });

    let mut child = Command::new("bsdtar")
        .args(["-x", "-v", "-k", "-f"])
        .arg(archive)
        .arg("-C")
        .arg(&dest)
        .stdout(Stdio::null())
        // -v writes one line per entry to STDERR; it is our only progress signal.
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("bsdtar: {e}"))?;

    let mut items_done = 0u64;
    let mut tail: Vec<String> = Vec::new();
    if let Some(err) = child.stderr.take() {
        for line in BufReader::new(err).lines().map_while(Result::ok) {
            if cancel.load(Ordering::Relaxed) {
                let _ = child.kill();
                break;
            }
            items_done += 1;
            // Keep the last few lines: if bsdtar fails, they are the actual reason.
            tail.push(line.trim().to_string());
            if tail.len() > 4 {
                tail.remove(0);
            }
            let _ = ev.send(Event::Progress {
                id,
                bytes_done: 0,
                items_done,
                current: tail.last().cloned().unwrap_or_default(),
            });
        }
    }
    let status = child.wait().map_err(|e| e.to_string())?;

    // Record what appeared regardless of how it ended: a cancelled or partial extraction still
    // left files behind, and undo should be able to clear them.
    let created: Vec<PathBuf> = std::fs::read_dir(&dest)
        .map(|rd| {
            rd.flatten()
                .map(|e| e.path())
                .filter(|p| !before.contains(p))
                .collect()
        })
        .unwrap_or_default();
    if !created.is_empty() {
        // Same shape as a copy: undoing it puts the new entries in the trash.
        *undo = Some(UndoRecord {
            action: UndoAction::Copied(created),
            displaced: Vec::new(),
            emptied: Vec::new(),
        });
    }

    if cancel.load(Ordering::Relaxed) || status.success() {
        Ok(())
    } else {
        Err(if tail.is_empty() {
            format!("bsdtar exited with {status}")
        } else {
            tail.join("; ")
        })
    }
}

fn transfer(
    id: u64,
    req: &Request,
    cancel: &AtomicBool,
    ev: &Sender<Event>,
    undo: &mut Option<UndoRecord>,
    warning: &mut Option<String>,
) -> Result<(), String> {
    let mut done = Transferred::default();
    match transfer_into(id, req, cancel, ev, &mut done) {
        Ok(()) => {}
        // Stopped part-way (cancelled, or an answer of Cancel, or an error that ends it): what
        // already happened is still recorded, so undo can reverse it.
        Err(e) if done.anything() => done.failed.insert(0, if e == "cancelled" { "the rest (cancelled)".into() } else { format!("the rest ({e})") }),
        Err(e) => return Err(e),
    }
    finish(req, done, undo, warning);
    Ok(())
}

/// What a transfer has done so far, kept outside the work so a stop part-way loses none of it.
#[derive(Default)]
struct Transferred {
    moved: Vec<(PathBuf, PathBuf)>,
    created: Vec<PathBuf>,
    displaced: Vec<trash::TrashItem>,
    /// What could not be done, by name, for the summary at the end.
    failed: Vec<String>,
    /// See UndoRecord::emptied.
    emptied: Vec<PathBuf>,
}

impl Transferred {
    fn anything(&self) -> bool {
        !self.moved.is_empty() || !self.created.is_empty() || !self.displaced.is_empty()
    }
}

fn transfer_into(id: u64, req: &Request, cancel: &AtomicBool, ev: &Sender<Event>, done: &mut Transferred) -> Result<(), String> {
    use std::collections::HashSet;
    use std::ffi::OsString;
    use std::os::unix::fs::MetadataExt;

    let dest_dir = req.dest.as_ref().ok_or("no destination")?;
    let mut conflicts = ConflictPolicy::default();
    let mut errors = ErrorPolicy::default();
    // Names this operation has already written into the destination. Two sources can share a
    // name (search results from different folders): the second must not land on the first.
    let mut claimed: HashSet<OsString> = HashSet::new();

    // A move within one filesystem is a rename: instant, no bytes to copy. Decided per source,
    // since a selection (search results, say) can span filesystems; the others are copied
    // and then removed below.
    let dest_dev = std::fs::metadata(dest_dir).map_err(|e| e.to_string())?.dev();
    let mut to_copy: Vec<&PathBuf> = Vec::new();
    if req.kind == OpKind::Move {
        let same: Vec<&PathBuf> = req
            .sources
            .iter()
            .filter(|s| std::fs::symlink_metadata(s).is_ok_and(|m| m.dev() == dest_dev))
            .collect();
        if !same.is_empty() {
            let _ = ev.send(Event::Started { id, kind: req.kind, total_bytes: 0, total_items: same.len() as u64 });
        }
        let mut renamed = 0u64;
        for src in &req.sources {
            if !same.contains(&src) {
                to_copy.push(src);
                continue;
            }
            if cancel.load(Ordering::Relaxed) {
                return Err("cancelled".into());
            }
            let Some(target) = decide(id, dest_dir, src, &mut claimed, &mut conflicts, &mut done.displaced, ev)? else { continue };
            if target.path == *src {
                continue;
            }
            if target.merge {
                let mut rn = Renames { id, cancel, ev, conflicts: &mut conflicts, errors: &mut errors, done: &mut *done };
                rn.merge(src, &target.path, Path::new(src.file_name().unwrap_or_default()))?;
            } else {
                Renames { id, cancel, ev, conflicts: &mut conflicts, errors: &mut errors, done: &mut *done }.rename(src, &target.path)?;
            }
            renamed += 1;
            let _ = ev.send(Event::Progress { id, bytes_done: 0, items_done: renamed, current: display_name(src) });
        }
    } else {
        to_copy = req.sources.iter().collect();
    }
    if to_copy.is_empty() {
        return Ok(());
    }

    // Copying, and moving between filesystems. Pre-scan first: totals for honest progress,
    // done before touching anything.
    let mut files: Vec<(PathBuf, PathBuf, u64)> = Vec::new(); // (src, rel to the root's parent, bytes)
    let mut dirs: Vec<PathBuf> = Vec::new(); // rel to the root's parent
    let mut total_bytes = 0u64;
    let mut roots: Vec<(&Path, std::ops::Range<usize>, std::ops::Range<usize>)> = Vec::new();
    for src in &to_copy {
        if cancel.load(Ordering::Relaxed) {
            return Err("cancelled".into());
        }
        let base = src.parent().unwrap_or(Path::new("/"));
        let (f0, d0) = (files.len(), dirs.len());
        scan(src, base, &mut files, &mut dirs, &mut total_bytes)?;
        roots.push((src.as_path(), f0..files.len(), d0..dirs.len()));
    }
    let _ = ev.send(Event::Started { id, kind: req.kind, total_bytes, total_items: files.len() as u64 });

    // Every root's destination is settled before the first byte is written.
    let mut targets: Vec<Option<Target>> = Vec::new();
    for (src, ..) in &roots {
        targets.push(decide(id, dest_dir, src, &mut claimed, &mut conflicts, &mut done.displaced, ev)?);
    }

    let mut progress = (0u64, 0u64); // (bytes, items)
    for ((src_root, file_range, dir_range), target) in roots.into_iter().zip(targets) {
        let Some(Target { path: target_root, merge }) = target else { continue };
        if req.kind == OpKind::Copy && !merge {
            // Recorded before it is written, so undo also clears a copy that stopped half-way.
            done.created.push(target_root.clone());
        }
        let root_name = src_root.file_name().unwrap_or_default();
        // Below the root, a path keeps its place; the root itself may have a new name.
        let dest_of = |rel: &Path| -> PathBuf {
            match rel.strip_prefix(root_name) {
                // The root strips to "", and join("") would append a slash that makes the
                // kernel treat a file path as a directory.
                Ok(tail) if !tail.as_os_str().is_empty() => target_root.join(tail),
                _ => target_root.clone(),
            }
        };
        let mut job = CopyRoot {
            id,
            cancel,
            ev,
            errors: &mut errors,
            conflicts: &mut conflicts,
            done: &mut *done,
            merge,
            // A merged copy is undone item by item: what it added, never the folder it went into.
            record_created: req.kind == OpKind::Copy && merge,
            progress: &mut progress,
        };
        let result = job.run(&files[file_range.clone()], &dirs[dir_range.clone()], &dest_of);
        let (arrived, new_dirs) = match result {
            Ok(r) => r,
            Err(e) => {
                if req.kind == OpKind::Move && !merge {
                    // The originals are all still in place; the half copy is only ours.
                    let _ = remove_any(&target_root);
                }
                return Err(e);
            }
        };
        if req.kind != OpKind::Move {
            continue;
        }
        let complete = arrived.iter().all(Option::is_some) && done.failed.iter().all(|f| !Path::new(f).starts_with(root_name));
        if complete && !merge {
            // Everything arrived: the source goes, and undo moves the whole root back.
            match remove_any(src_root) {
                Ok(()) => done.moved.push((target_root.clone(), src_root.to_path_buf())),
                Err(e) => done.failed.push(format!("{} (copied, but the original could not be removed: {e})", display_name(src_root))),
            }
        } else {
            // Item by item: only the files that arrived leave the source, the rest stay where
            // they were with the folders that still hold them. Undo moves each one back, and
            // takes away again the folders this move made (never one that was there).
            for ((src, _, _), to) in files[file_range].iter().zip(&arrived) {
                if let Some(to) = to {
                    if std::fs::remove_file(src).is_ok() {
                        done.moved.push((to.clone(), src.clone()));
                    }
                }
            }
            done.emptied.extend(new_dirs.into_iter().rev());
            let base = src_root.parent().unwrap_or(Path::new("/"));
            for rel in dirs[dir_range].iter().rev() {
                let _ = std::fs::remove_dir(base.join(rel)); // fails, as it should, unless empty
            }
        }
    }
    Ok(())
}

/// Where one source goes.
struct Target {
    path: PathBuf,
    /// Into a folder already there, rather than a new one.
    merge: bool,
}

/// Moves on one filesystem, by rename.
struct Renames<'a> {
    id: u64,
    cancel: &'a AtomicBool,
    ev: &'a Sender<Event>,
    conflicts: &'a mut ConflictPolicy,
    errors: &'a mut ErrorPolicy,
    done: &'a mut Transferred,
}

impl Renames<'_> {
    fn rename(&mut self, src: &Path, target: &Path) -> Result<(), String> {
        loop {
            match rename_no_replace(src, target) {
                Ok(()) => {
                    self.done.moved.push((target.to_path_buf(), src.to_path_buf()));
                    return Ok(());
                }
                Err(e) => match ask_error(self.id, src, &e.to_string(), self.errors, self.ev)? {
                    OnError::Retry => continue,
                    OnError::Skip => {
                        self.done.failed.push(display_name(src));
                        return Ok(());
                    }
                },
            }
        }
    }

    /// The contents of `src` into the folder `dst`, each by rename. A folder on both sides is
    /// merged in turn; a file on both sides is asked about. `rel` names it in questions.
    fn merge(&mut self, src: &Path, dst: &Path, rel: &Path) -> Result<(), String> {
        let entries = std::fs::read_dir(src).map_err(|e| e.to_string())?;
        for entry in entries.flatten() {
            if self.cancel.load(Ordering::Relaxed) {
                return Err("cancelled".into());
            }
            let (from, name) = (entry.path(), entry.file_name());
            let mut to = dst.join(&name);
            let rel = rel.join(&name);
            if let Ok(there) = to.symlink_metadata() {
                let folders = there.is_dir() && entry.file_type().is_ok_and(|t| t.is_dir());
                if folders {
                    self.merge(&from, &to, &rel)?;
                    continue;
                }
                match resolve_conflict(self.id, rel.as_os_str(), false, self.conflicts, self.ev)? {
                    Do::Skip | Do::Merge => continue,
                    Do::Overwrite => {
                        if let Some(item) = trash_and_record(&to)? {
                            self.done.displaced.push(item);
                        }
                    }
                    Do::Rename => to = dst.join(free_sibling_name_skipping(dst, &name, &[])),
                }
            }
            self.rename(&from, &to)?;
        }
        // Empty now unless something was skipped; then it stays, holding what is left.
        let _ = std::fs::remove_dir(src);
        Ok(())
    }
}

/// Copies one source root's folders and files to where `dest_of` puts them.
struct CopyRoot<'a> {
    id: u64,
    cancel: &'a AtomicBool,
    ev: &'a Sender<Event>,
    errors: &'a mut ErrorPolicy,
    conflicts: &'a mut ConflictPolicy,
    done: &'a mut Transferred,
    /// Into a folder that is there: what is already in it is asked about, never overwritten.
    merge: bool,
    /// Put what this adds in `done.created` (the topmost new folder or the file), for undo.
    record_created: bool,
    progress: &'a mut (u64, u64),
}

impl CopyRoot<'_> {
    /// Per file, where it arrived (None: it did not); and the folders it made, outermost first.
    #[allow(clippy::type_complexity)]
    fn run(&mut self, files: &[(PathBuf, PathBuf, u64)], dirs: &[PathBuf], dest_of: &dyn Fn(&Path) -> PathBuf) -> Result<(Vec<Option<PathBuf>>, Vec<PathBuf>), String> {
        let mut failed_dirs: Vec<&Path> = Vec::new();
        let mut new_dirs: Vec<PathBuf> = Vec::new();
        for rel in dirs {
            if failed_dirs.iter().any(|f| rel.starts_with(f)) {
                continue;
            }
            let to = dest_of(rel);
            if to.is_dir() {
                continue;
            }
            loop {
                match std::fs::create_dir(&to).or_else(|e| if to.is_dir() { Ok(()) } else { Err(e) }) {
                    Ok(()) => {
                        if self.record_created && !new_dirs.iter().any(|d| to.starts_with(d)) {
                            self.done.created.push(to.clone());
                        }
                        new_dirs.push(to.clone());
                        break;
                    }
                    Err(e) => match ask_error(self.id, rel, &e.to_string(), self.errors, self.ev)? {
                        OnError::Retry => continue,
                        OnError::Skip => {
                            self.done.failed.push(rel.to_string_lossy().into_owned());
                            failed_dirs.push(rel);
                            break;
                        }
                    },
                }
            }
        }
        let mut arrived = vec![None; files.len()];
        for (i, (src, rel, size)) in files.iter().enumerate() {
            if self.cancel.load(Ordering::Relaxed) {
                return Err("cancelled".into());
            }
            if failed_dirs.iter().any(|f| rel.starts_with(f)) {
                continue;
            }
            let mut target = dest_of(rel);
            // Only in a merge can something be there already (otherwise the folder is new).
            if self.merge && target.symlink_metadata().is_ok() {
                match resolve_conflict(self.id, rel.as_os_str(), false, self.conflicts, self.ev)? {
                    Do::Skip | Do::Merge => {
                        self.progress.0 += size;
                        self.progress.1 += 1;
                        continue;
                    }
                    Do::Overwrite => {
                        if let Some(item) = trash_and_record(&target)? {
                            self.done.displaced.push(item);
                        }
                    }
                    Do::Rename => {
                        let dir = target.parent().unwrap_or(Path::new("/")).to_path_buf();
                        target = dir.join(free_sibling_name_skipping(&dir, target.file_name().unwrap_or_default(), &[]));
                    }
                }
            }
            loop {
                let (bytes, items) = *self.progress;
                let (id, ev) = (self.id, self.ev);
                let result = copy_file(src, &target, self.cancel, &mut |n| {
                    let _ = ev.send(Event::Progress { id, bytes_done: bytes + n, items_done: items, current: rel.to_string_lossy().into_owned() });
                });
                match result {
                    Ok(()) => {
                        if self.record_created && !new_dirs.iter().any(|d| target.starts_with(d)) {
                            self.done.created.push(target.clone());
                        }
                        arrived[i] = Some(target.clone());
                        break;
                    }
                    Err(e) if self.cancel.load(Ordering::Relaxed) => return Err(e),
                    Err(e) => {
                        // What a failed copy left behind is part of a file, never something the
                        // user had: the target did not exist (conflicts are settled above).
                        let _ = std::fs::remove_file(&target);
                        match ask_error(self.id, rel, &e, self.errors, self.ev)? {
                            OnError::Retry => continue,
                            OnError::Skip => {
                                self.done.failed.push(rel.to_string_lossy().into_owned());
                                break;
                            }
                        }
                    }
                }
            }
            self.progress.0 += size;
            self.progress.1 += 1;
        }
        Ok((arrived, new_dirs))
    }
}

/// Where one source goes: its own name in the destination, unless that is taken. None: skipped.
fn decide(
    id: u64,
    dest_dir: &Path,
    src: &Path,
    claimed: &mut std::collections::HashSet<std::ffi::OsString>,
    conflicts: &mut ConflictPolicy,
    displaced: &mut Vec<trash::TrashItem>,
    ev: &Sender<Event>,
) -> Result<Option<Target>, String> {
    let name = src.file_name().ok_or("nameless source")?.to_os_string();
    let mut target = dest_dir.join(&name);
    let mut merge = false;
    if claimed.contains(&name) {
        // Taken by another source of this same operation, not by anything the user had: both
        // are kept, without asking.
        target = dest_dir.join(free_name(dest_dir, &name, claimed));
    } else if let Ok(there) = target.symlink_metadata().map(|m| m.is_dir()) {
        if target != src {
            let folders = there && std::fs::symlink_metadata(src).is_ok_and(|m| m.is_dir());
            match resolve_conflict(id, &name, folders, conflicts, ev)? {
                Do::Skip => return Ok(None),
                Do::Overwrite => {
                    if let Some(item) = trash_and_record(&target)? {
                        displaced.push(item);
                    }
                }
                Do::Rename => target = dest_dir.join(free_name(dest_dir, &name, claimed)),
                Do::Merge => merge = true,
            }
        }
    }
    claimed.insert(target.file_name().unwrap_or_default().to_os_string());
    Ok(Some(Target { path: target, merge }))
}

/// The undo record and the summary of what was left out, at the end of a transfer.
fn finish(req: &Request, done: Transferred, undo: &mut Option<UndoRecord>, warning: &mut Option<String>) {
    let Transferred { moved, created, displaced, failed, emptied } = done;
    let action = if req.kind == OpKind::Move { UndoAction::Moved(moved) } else { UndoAction::Copied(created) };
    let empty = match &action {
        UndoAction::Moved(v) => v.is_empty(),
        UndoAction::Copied(v) => v.is_empty(),
        _ => true,
    };
    if !empty || !displaced.is_empty() {
        *undo = Some(UndoRecord { action, displaced, emptied });
    }
    if !failed.is_empty() {
        const SHOWN: usize = 5;
        let mut list = failed.iter().take(SHOWN).cloned().collect::<Vec<_>>().join(", ");
        if failed.len() > SHOWN {
            list.push_str(&format!(" and {} more", failed.len() - SHOWN));
        }
        let verb = if req.kind == OpKind::Move { "moved" } else { "copied" };
        *warning = Some(format!("Not {verb}: {list}"));
    }
}

fn display_name(p: &Path) -> String {
    p.file_name().unwrap_or(p.as_os_str()).to_string_lossy().into_owned()
}

/// First free "name (n).ext" in `dir` that this operation has not already taken either.
fn free_name(dir: &Path, name: &std::ffi::OsStr, claimed: &std::collections::HashSet<std::ffi::OsString>) -> std::ffi::OsString {
    if !claimed.contains(name) && dir.join(name).symlink_metadata().is_err() {
        return name.to_os_string();
    }
    let mut taken: Vec<std::ffi::OsString> = Vec::new();
    loop {
        let candidate = free_sibling_name_skipping(dir, name, &taken);
        if !claimed.contains(&candidate) {
            return candidate;
        }
        taken.push(candidate);
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ErrorAnswer {
    Retry,
    Skip,
    SkipAll,
    Cancel,
}

enum OnError {
    Retry,
    Skip,
}

#[derive(Default)]
struct ErrorPolicy {
    skip_all: bool,
}

/// One item of a copy or move failed: ask whether to try it again, leave it out, or stop.
fn ask_error(id: u64, what: &Path, error: &str, policy: &mut ErrorPolicy, ev: &Sender<Event>) -> Result<OnError, String> {
    if policy.skip_all {
        return Ok(OnError::Skip);
    }
    let (tx, rx) = channel();
    let _ = ev.send(Event::Error {
        id,
        name: what.to_string_lossy().into_owned(),
        error: error.to_string(),
        reply: tx,
    });
    match rx.recv().map_err(|_| "ui gone")? {
        ErrorAnswer::Retry => Ok(OnError::Retry),
        ErrorAnswer::Skip => Ok(OnError::Skip),
        ErrorAnswer::SkipAll => {
            policy.skip_all = true;
            Ok(OnError::Skip)
        }
        ErrorAnswer::Cancel => Err("cancelled".into()),
    }
}

enum Do {
    Overwrite,
    Skip,
    /// Keep both: written under the first free "name (n)".
    Rename,
    /// Into the folder that is there.
    Merge,
}

/// Sticky "... all" answers for one operation.
#[derive(Default)]
struct ConflictPolicy {
    overwrite_all: bool,
    skip_all: bool,
    rename_all: bool,
    merge_all: bool,
}

/// First free "name (n).ext" sibling in `dir`, the name Dolphin and Nautilus would pick.
/// Directories and dot-files have no extension to preserve, so they get "name (n)".
#[cfg(test)]
fn free_sibling_name(dir: &Path, name: &std::ffi::OsStr) -> std::ffi::OsString {
    free_sibling_name_skipping(dir, name, &[])
}

/// The same, passing over names in `skip` as if they were taken.
fn free_sibling_name_skipping(dir: &Path, name: &std::ffi::OsStr, skip: &[std::ffi::OsString]) -> std::ffi::OsString {
    let p = Path::new(name);
    let is_dir = dir.join(name).is_dir();
    let (stem, ext) = match (p.file_stem(), p.extension()) {
        (Some(stem), Some(ext)) if !is_dir => (stem.to_string_lossy(), Some(ext.to_string_lossy())),
        _ => (name.to_string_lossy(), None),
    };
    let mut n = 1;
    loop {
        let candidate = match &ext {
            Some(ext) => format!("{stem} ({n}).{ext}"),
            None => format!("{stem} ({n})"),
        };
        if dir.join(&candidate).symlink_metadata().is_err() && !skip.iter().any(|t| *t == *candidate) {
            return candidate.into();
        }
        n += 1;
    }
}

fn resolve_conflict(
    id: u64,
    name: &std::ffi::OsStr,
    folders: bool,
    policy: &mut ConflictPolicy,
    ev: &Sender<Event>,
) -> Result<Do, String> {
    if folders && policy.merge_all {
        return Ok(Do::Merge);
    }
    if policy.overwrite_all {
        return Ok(Do::Overwrite);
    }
    if policy.skip_all {
        return Ok(Do::Skip);
    }
    if policy.rename_all {
        return Ok(Do::Rename);
    }
    let (tx, rx) = channel();
    let _ = ev.send(Event::Conflict {
        id,
        name: name.to_string_lossy().into_owned(),
        folders,
        reply: tx,
    });
    match rx.recv().map_err(|_| "ui gone")? {
        ConflictAnswer::Overwrite => Ok(Do::Overwrite),
        ConflictAnswer::Skip => Ok(Do::Skip),
        ConflictAnswer::Rename => Ok(Do::Rename),
        ConflictAnswer::OverwriteAll => {
            policy.overwrite_all = true;
            Ok(Do::Overwrite)
        }
        ConflictAnswer::SkipAll => {
            policy.skip_all = true;
            Ok(Do::Skip)
        }
        ConflictAnswer::RenameAll => {
            policy.rename_all = true;
            Ok(Do::Rename)
        }
        ConflictAnswer::Merge if folders => Ok(Do::Merge),
        ConflictAnswer::MergeAll if folders => {
            policy.merge_all = true;
            Ok(Do::Merge)
        }
        // Only offered for two folders.
        ConflictAnswer::Merge | ConflictAnswer::MergeAll => Ok(Do::Skip),
        ConflictAnswer::Cancel => Err("cancelled".into()),
    }
}

/// Rename, or where the two are on different filesystems (undoing a move from one drive to
/// another), copy and then remove the original once all of it has arrived.
fn move_path(from: &Path, to: &Path, cancel: &AtomicBool) -> Result<(), String> {
    // Undo puts things back where they were; whatever has appeared there since is not replaced.
    match rename_no_replace(from, to) {
        Err(e) if e.raw_os_error() == Some(libc::EXDEV) => {}
        other => return other.map_err(|e| e.to_string()),
    }
    if to.symlink_metadata().is_ok() {
        return Err(format!("{} already exists", to.display()));
    }
    let base = from.parent().unwrap_or(Path::new("/"));
    let (mut files, mut dirs, mut total) = (Vec::new(), Vec::new(), 0u64);
    scan(from, base, &mut files, &mut dirs, &mut total)?;
    let root = from.file_name().unwrap_or_default();
    let dest_of = |rel: &Path| match rel.strip_prefix(root) {
        Ok(tail) if !tail.as_os_str().is_empty() => to.join(tail),
        _ => to.to_path_buf(),
    };
    let copied = (|| -> Result<(), String> {
        for rel in &dirs {
            std::fs::create_dir_all(dest_of(rel)).map_err(|e| e.to_string())?;
        }
        for (src, rel, _) in &files {
            copy_file(src, &dest_of(rel), cancel, &mut |_| {})?;
        }
        Ok(())
    })();
    if let Err(e) = copied {
        let _ = remove_any(to); // only the half copy; the original is untouched
        return Err(e);
    }
    remove_any(from)
}

fn remove_any(path: &Path) -> Result<(), String> {
    let meta = std::fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    if meta.is_dir() {
        std::fs::remove_dir_all(path).map_err(|e| e.to_string())
    } else {
        std::fs::remove_file(path).map_err(|e| e.to_string())
    }
}

fn scan(
    path: &Path,
    base: &Path,
    files: &mut Vec<(PathBuf, PathBuf, u64)>,
    dirs: &mut Vec<PathBuf>,
    total: &mut u64,
) -> Result<(), String> {
    let meta = std::fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    let rel = path
        .strip_prefix(base)
        .map_err(|e| e.to_string())?
        .to_path_buf();
    if meta.is_dir() {
        dirs.push(rel);
        for entry in std::fs::read_dir(path)
            .map_err(|e| e.to_string())?
            .flatten()
        {
            scan(&entry.path(), base, files, dirs, total)?;
        }
    } else {
        *total += meta.len();
        files.push((path.to_path_buf(), rel, meta.len()));
    }
    Ok(())
}

/// Chunked copy: byte-accurate progress and prompt cancellation. Symlinks are recreated as
/// links rather than followed, copying a symlink to a 2 TB target should not copy 2 TB.
fn copy_file(
    src: &Path,
    dst: &Path,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(u64),
) -> Result<(), String> {
    use std::io::{Read, Write};

    let meta = std::fs::symlink_metadata(src).map_err(|e| e.to_string())?;
    if meta.file_type().is_symlink() {
        let link = std::fs::read_link(src).map_err(|e| e.to_string())?;
        std::os::unix::fs::symlink(&link, dst).map_err(|e| e.to_string())?;
        return Ok(());
    }

    if let Some(what) = special_kind(&meta.file_type()) {
        let name = src.file_name().unwrap_or(src.as_os_str()).to_string_lossy();
        return Err(format!("{name} is {what}, not a file with contents, so it cannot be copied"));
    }

    let mut r = std::fs::File::open(src).map_err(|e| e.to_string())?;
    let mut w = std::fs::File::create(dst).map_err(|e| e.to_string())?;
    let mut done = 0u64;
    match copy_in_kernel(&r, &w, meta.len(), cancel, progress) {
        InKernel::Done => {
            let _ = std::fs::set_permissions(dst, meta.permissions());
            return Ok(());
        }
        InKernel::Cancelled => {
            drop(w);
            let _ = std::fs::remove_file(dst);
            return Err("cancelled".into());
        }
        InKernel::Failed(e) => return Err(e),
        // Nothing written yet; the ordinary copy below takes it from the start.
        InKernel::Unsupported => {}
    }
    let mut buf = vec![0u8; CHUNK];
    loop {
        if cancel.load(Ordering::Relaxed) {
            drop(w);
            let _ = std::fs::remove_file(dst); // no half files on cancel
            return Err("cancelled".into());
        }
        let n = r.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        w.write_all(&buf[..n]).map_err(|e| e.to_string())?;
        done += n as u64;
        progress(done);
    }
    // Permissions carry over; timestamps deliberately do not (a copy is a new file).
    let _ = std::fs::set_permissions(dst, meta.permissions());
    Ok(())
}

/// What a directory entry is when it holds no data: a socket or pipe a running program talks
/// through, or a device. Opening one fails (a socket), waits forever for a writer (a pipe), or
/// reads the device itself, so none of them is copied.
fn special_kind(t: &std::fs::FileType) -> Option<&'static str> {
    use std::os::unix::fs::FileTypeExt;
    if t.is_socket() {
        Some("a socket, where a running program takes connections")
    } else if t.is_fifo() {
        Some("a pipe, where running programs pass data")
    } else if t.is_char_device() || t.is_block_device() {
        Some("a device")
    } else {
        None
    }
}

enum InKernel {
    Done,
    Cancelled,
    Failed(String),
    /// Neither way works between these two files; nothing has been written.
    Unsupported,
}

/// Copies without the data passing through here. On a filesystem that shares blocks (btrfs,
/// XFS) the copy is a reflink: instant whatever the size, and taking no space until one side
/// is changed. Otherwise `copy_file_range`, where the kernel moves the bytes itself, in chunks
/// so progress and cancelling still work.
fn copy_in_kernel(r: &std::fs::File, w: &std::fs::File, len: u64, cancel: &AtomicBool, progress: &mut dyn FnMut(u64)) -> InKernel {
    use std::os::fd::AsRawFd;
    if unsafe { libc::ioctl(w.as_raw_fd(), libc::FICLONE, r.as_raw_fd()) } == 0 {
        progress(len);
        return InKernel::Done;
    }
    let mut done = 0u64;
    loop {
        if cancel.load(Ordering::Relaxed) {
            return InKernel::Cancelled;
        }
        let n = unsafe { libc::copy_file_range(r.as_raw_fd(), std::ptr::null_mut(), w.as_raw_fd(), std::ptr::null_mut(), IN_KERNEL_CHUNK, 0) };
        if n == 0 {
            return InKernel::Done;
        }
        if n < 0 {
            let e = std::io::Error::last_os_error();
            // Between filesystems that cannot, or on a kernel without it: the plain copy
            // does it, as long as nothing has been written yet.
            if done == 0 && matches!(e.raw_os_error(), Some(libc::EXDEV | libc::ENOSYS | libc::EINVAL | libc::EOPNOTSUPP | libc::EBADF)) {
                return InKernel::Unsupported;
            }
            return InKernel::Failed(e.to_string());
        }
        done += n as u64;
        progress(done);
    }
}

/// Bytes per `copy_file_range` call: large enough that the kernel is not asked too often,
/// small enough that progress moves and a cancel lands within a fraction of a second.
const IN_KERNEL_CHUNK: usize = 16 << 20;

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;

    /// On the home directory's filesystem: a reflink where it is btrfs, so the time says which
    /// way was taken.
    #[test]
    #[ignore = "writes a 1 GB file under the home directory"]
    fn a_copy_on_btrfs_is_a_reflink() {
        let dir = vanta_config::paths::home().join(".cache/vantaexplorer-reflink-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let data = vec![7u8; 1 << 30];
        std::fs::write(dir.join("a"), &data).unwrap();
        let at = std::time::Instant::now();
        copy_file(&dir.join("a"), &dir.join("b"), &AtomicBool::new(false), &mut |_| {}).unwrap();
        eprintln!("1 GB copied in {:?}", at.elapsed());
        assert_eq!(std::fs::metadata(dir.join("b")).unwrap().len(), 1 << 30);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_large_file_copies_whole_with_progress() {
        let (src, dst) = setup("large");
        let data: Vec<u8> = (0..(40u32 << 20) / 4).flat_map(|i| i.to_le_bytes()).collect();
        std::fs::write(src.join("big.bin"), &data).unwrap();
        let cancel = AtomicBool::new(false);
        let mut last = 0;
        copy_file(&src.join("big.bin"), &dst.join("big.bin"), &cancel, &mut |d| last = d).unwrap();
        assert_eq!(std::fs::read(dst.join("big.bin")).unwrap(), data);
        assert_eq!(last, data.len() as u64);
    }

    fn setup(name: &str) -> (PathBuf, PathBuf) {
        let base = std::env::temp_dir().join(format!("vantaexplorer-ops-{name}"));
        let _ = std::fs::remove_dir_all(&base);
        let src = base.join("src");
        let dst = base.join("dst");
        std::fs::create_dir_all(src.join("sub")).unwrap();
        std::fs::create_dir_all(&dst).unwrap();
        std::fs::write(src.join("a.txt"), b"hello").unwrap();
        std::fs::write(src.join("sub/n.txt"), b"nested").unwrap();
        (src, dst)
    }

    /// Run the operation on ITS OWN thread and answer events from this one, calling run()
    /// inline deadlocks on the first conflict, because resolve_conflict blocks awaiting the
    /// reply this loop is supposed to send. That mirrors production, where the worker thread
    /// blocks and the UI thread answers.
    fn run_and_drain(req: Request) {
        let (tx, rx) = channel();
        let handle = std::thread::spawn(move || {
            run(1, req, &AtomicBool::new(false), &tx);
        });
        loop {
            match rx.recv().expect("worker alive") {
                Event::Done { .. } => break,
                Event::Failed { error, .. } => panic!("op failed: {error}"),
                Event::Conflict { reply, .. } => {
                    reply.send(ConflictAnswer::Overwrite).unwrap();
                }
                _ => {}
            }
        }
        handle.join().unwrap();
    }

    #[test]
    fn copy_recursive_preserves_structure() {
        let (src, dst) = setup("copy");
        run_and_drain(Request::new(OpKind::Copy, vec![src.clone()], Some(dst.clone())));
        assert_eq!(std::fs::read(dst.join("src/a.txt")).unwrap(), b"hello");
        assert_eq!(std::fs::read(dst.join("src/sub/n.txt")).unwrap(), b"nested");
        assert!(src.exists(), "copy must not remove the source");
    }

    #[test]
    fn move_cross_layout_and_conflict_overwrite() {
        let (src, dst) = setup("move");
        // Pre-place a conflicting root so the conflict path runs.
        std::fs::create_dir_all(dst.join("src")).unwrap();
        std::fs::write(dst.join("src/stale"), b"old").unwrap();
        run_and_drain(Request::new(OpKind::Move, vec![src.clone()], Some(dst.clone())));
        assert_eq!(std::fs::read(dst.join("src/a.txt")).unwrap(), b"hello");
        assert!(!src.exists(), "move must remove the source");
        assert!(
            !dst.join("src/stale").exists(),
            "overwrite replaced the old tree"
        );
        // The replaced tree went to the trash; it is not left there for the trash view to show.
        if let Some(item) = find_trash_entry(&dst.join("src")) {
            let _ = trash::os_limited::purge_all([item]);
        }
    }

    /// Runs an operation, answering every failed item with `on_error`; hands back the undo
    /// record, the warning, and how many items were asked about.
    fn run_answering(req: Request, on_error: ErrorAnswer) -> (Option<UndoRecord>, Option<String>, usize) {
        let (tx, rx) = channel();
        let handle = std::thread::spawn(move || run(1, req, &AtomicBool::new(false), &tx));
        let mut asked = 0;
        let out = loop {
            match rx.recv().expect("worker alive") {
                Event::Done { undo, warning, .. } => break (undo, warning),
                Event::Failed { error, .. } => panic!("op failed: {error}"),
                Event::Conflict { reply, .. } => reply.send(ConflictAnswer::Overwrite).unwrap(),
                Event::Error { reply, .. } => {
                    asked += 1;
                    reply.send(on_error).unwrap();
                }
                _ => {}
            }
        };
        handle.join().unwrap();
        (out.0, out.1, asked)
    }

    /// An unreadable file in the middle: the others still arrive, the user is told which one
    /// did not, and a move leaves exactly that one (and the folder holding it) in place.
    #[test]
    fn a_failing_file_is_skipped_and_reported() {
        use std::os::unix::fs::PermissionsExt;
        for kind in [OpKind::Copy, OpKind::Move] {
            let (src, _) = setup(&format!("skip-{kind:?}"));
            std::fs::write(src.join("sub/locked.txt"), b"x").unwrap();
            std::fs::set_permissions(src.join("sub/locked.txt"), std::fs::Permissions::from_mode(0o000)).unwrap();
            // Another filesystem, so the move copies too.
            let dst = PathBuf::from(format!("/dev/shm/vantaexplorer-skip-{kind:?}"));
            let _ = std::fs::remove_dir_all(&dst);
            std::fs::create_dir_all(&dst).unwrap();
            let (undo, warning, asked) = run_answering(Request::new(kind, vec![src.clone()], Some(dst.clone())), ErrorAnswer::SkipAll);
            assert_eq!(asked, 1);
            assert!(warning.is_some_and(|w| w.contains("locked.txt")));
            assert_eq!(std::fs::read(dst.join("src/a.txt")).unwrap(), b"hello");
            assert_eq!(std::fs::read(dst.join("src/sub/n.txt")).unwrap(), b"nested");
            assert!(!dst.join("src/sub/locked.txt").exists(), "no half file left behind");
            if kind == OpKind::Move {
                assert!(!src.join("a.txt").exists() && !src.join("sub/n.txt").exists(), "what arrived left the source");
                assert!(src.join("sub/locked.txt").symlink_metadata().is_ok(), "what did not, stayed");
                let Some(UndoRecord { action: UndoAction::Moved(pairs), emptied, .. }) = undo else { panic!("no undo") };
                assert_eq!(pairs.len(), 2, "undo moves back the two files that moved");
                let back = Request { kind: OpKind::MoveExact, pairs, sources: emptied, ..Default::default() };
                run_answering(back, ErrorAnswer::Cancel);
                assert_eq!(std::fs::read(src.join("sub/n.txt")).unwrap(), b"nested");
                assert!(!dst.join("src").exists(), "the folders the move made are gone again");
            }
            std::fs::set_permissions(src.join("sub/locked.txt"), std::fs::Permissions::from_mode(0o644)).unwrap();
            let _ = std::fs::remove_dir_all(&dst);
        }
    }

    /// Two sources with the same name (search results from different folders): both arrive,
    /// the second under a numbered name, and nothing is asked.
    #[test]
    fn sources_with_the_same_name_are_both_kept() {
        for kind in [OpKind::Copy, OpKind::Move] {
            let (src, dst) = setup(&format!("same-name-{kind:?}"));
            std::fs::write(src.join("sub/a.txt"), b"second").unwrap();
            let (_, warning, asked) = run_answering(
                Request::new(kind, vec![src.join("a.txt"), src.join("sub/a.txt")], Some(dst.clone())),
                ErrorAnswer::Cancel,
            );
            assert_eq!((asked, warning), (0, None));
            assert_eq!(std::fs::read(dst.join("a.txt")).unwrap(), b"hello");
            assert_eq!(std::fs::read(dst.join("a (1).txt")).unwrap(), b"second");
        }
    }

    /// A move between filesystems is undone by copying back, not by a rename that cannot work.
    #[test]
    fn a_move_to_another_filesystem_can_be_undone() {
        let (src, _) = setup("undo-cross");
        let dst = PathBuf::from("/dev/shm/vantaexplorer-undo-cross");
        let _ = std::fs::remove_dir_all(&dst);
        std::fs::create_dir_all(&dst).unwrap();
        let (undo, ..) = run_answering(Request::new(OpKind::Move, vec![src.join("sub")], Some(dst.clone())), ErrorAnswer::Cancel);
        assert!(!src.join("sub").exists());
        let Some(UndoRecord { action: UndoAction::Moved(pairs), .. }) = undo else { panic!("no undo") };
        let back = Request { kind: OpKind::MoveExact, pairs: pairs.clone(), ..Default::default() };
        run_answering(back, ErrorAnswer::Cancel);
        assert_eq!(std::fs::read(src.join("sub/n.txt")).unwrap(), b"nested");
        assert!(!dst.join("sub").exists());
        let _ = std::fs::remove_dir_all(&dst);
    }

    /// The quick lookup finds the same entry the full listing does, the newest of two.
    #[test]
    fn a_trashed_file_is_found_without_listing_the_trash() {
        let (src, _) = setup("trash-lookup");
        let f = src.join("twice trashed.txt");
        let purge = || {
            let all = trash::os_limited::list().unwrap_or_default();
            let _ = trash::os_limited::purge_all(all.into_iter().filter(|i| i.original_path() == f));
        };
        purge();
        for content in ["first", "second"] {
            std::fs::write(&f, content).unwrap();
            trash::delete_all([&f]).unwrap();
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        // The listing knows deletion times to the second only, so it cannot tell these two
        // apart; the lookup goes by the info file's own time.
        let quick = find_trash_entry(&f).expect("found by name");
        assert_eq!(quick.original_path(), f);
        trash::os_limited::restore_all([quick]).unwrap();
        assert_eq!(std::fs::read(&f).unwrap(), b"second", "the newest one");
        purge();
    }

    /// Runs an operation, giving `answers` to its conflict questions in turn (folders first, as
    /// asked); hands back the undo record and the questions' names.
    fn run_conflicts(req: Request, answers: &[ConflictAnswer]) -> (Option<UndoRecord>, Vec<(String, bool)>) {
        let (tx, rx) = channel();
        let handle = std::thread::spawn(move || run(1, req, &AtomicBool::new(false), &tx));
        let mut asked = Vec::new();
        let undo = loop {
            match rx.recv().expect("worker alive") {
                Event::Done { undo, .. } => break undo,
                Event::Failed { error, .. } => panic!("op failed: {error}"),
                Event::Conflict { name, folders, reply, .. } => {
                    reply.send(answers[asked.len()]).unwrap();
                    asked.push((name, folders));
                }
                Event::Error { error, .. } => panic!("item failed: {error}"),
                _ => {}
            }
        };
        handle.join().unwrap();
        (undo, asked)
    }

    /// A folder copied onto one of the same name: merged, the file in both kept twice, what was
    /// there untouched; and undo takes away only what the copy added.
    #[test]
    fn copying_a_folder_into_one_that_is_there_merges() {
        let (src, dst) = setup("merge-copy");
        std::fs::create_dir_all(dst.join("src")).unwrap();
        std::fs::write(dst.join("src/keep.txt"), b"mine").unwrap();
        std::fs::write(dst.join("src/a.txt"), b"old").unwrap();
        let (undo, asked) = run_conflicts(
            Request::new(OpKind::Copy, vec![src.clone()], Some(dst.clone())),
            &[ConflictAnswer::Merge, ConflictAnswer::Rename],
        );
        assert_eq!(asked, [("src".to_string(), true), ("src/a.txt".to_string(), false)]);
        assert_eq!(std::fs::read(dst.join("src/keep.txt")).unwrap(), b"mine");
        assert_eq!(std::fs::read(dst.join("src/a.txt")).unwrap(), b"old");
        assert_eq!(std::fs::read(dst.join("src/a (1).txt")).unwrap(), b"hello");
        assert_eq!(std::fs::read(dst.join("src/sub/n.txt")).unwrap(), b"nested");
        let Some(UndoRecord { action: UndoAction::Copied(mut created), .. }) = undo else { panic!("no undo") };
        created.sort();
        assert_eq!(created, [dst.join("src/a (1).txt"), dst.join("src/sub")], "never the folder that was there");
    }

    /// The same as a move, on one filesystem and onto another: what was there stays, the file in
    /// both is replaced (into the trash), and undo puts everything back where it came from.
    #[test]
    fn moving_a_folder_into_one_that_is_there_merges_and_undoes() {
        for other_fs in [false, true] {
            let (src, near) = setup(&format!("merge-move-{other_fs}"));
            let dst = if other_fs { PathBuf::from("/dev/shm/vantaexplorer-merge-move") } else { near };
            let _ = std::fs::remove_dir_all(&dst);
            std::fs::create_dir_all(dst.join("src")).unwrap();
            std::fs::write(dst.join("src/keep.txt"), b"mine").unwrap();
            std::fs::write(dst.join("src/a.txt"), b"old").unwrap();
            let (undo, asked) = run_conflicts(
                Request::new(OpKind::Move, vec![src.clone()], Some(dst.clone())),
                &[ConflictAnswer::Merge, ConflictAnswer::Overwrite],
            );
            assert_eq!(asked.len(), 2, "{asked:?}");
            assert!(!src.exists(), "everything moved, so the source folder is gone");
            assert_eq!(std::fs::read(dst.join("src/keep.txt")).unwrap(), b"mine");
            assert_eq!(std::fs::read(dst.join("src/a.txt")).unwrap(), b"hello");
            assert_eq!(std::fs::read(dst.join("src/sub/n.txt")).unwrap(), b"nested");
            let rec = undo.expect("undo");
            assert_eq!(rec.displaced.len(), 1, "the replaced a.txt is in the trash");
            let UndoAction::Moved(pairs) = rec.action else { panic!("not a move") };
            let back = Request { kind: OpKind::MoveExact, pairs, sources: rec.emptied, ..Default::default() };
            run_conflicts(back, &[]);
            assert_eq!(std::fs::read(src.join("a.txt")).unwrap(), b"hello");
            assert_eq!(std::fs::read(src.join("sub/n.txt")).unwrap(), b"nested");
            assert_eq!(std::fs::read(dst.join("src/keep.txt")).unwrap(), b"mine");
            assert!(!dst.join("src/sub").exists(), "the folder the move made is gone again");
            let _ = trash::os_limited::restore_all(rec.displaced);
            assert_eq!(std::fs::read(dst.join("src/a.txt")).unwrap(), b"old");
            let _ = std::fs::remove_dir_all(&dst);
        }
    }

    /// A file trashed on another drive (here /tmp, a filesystem of its own) is in a trash
    /// folder the trash view looks in, and is found from its file there.
    #[test]
    fn the_trash_of_another_drive_is_seen() {
        let (src, _) = setup("other-trash");
        let f = src.join("on another drive.txt");
        std::fs::write(&f, b"x").unwrap();
        trash::delete_all([&f]).unwrap();
        let item = find_trash_entry(&f).expect("trashed");
        let payload = trash_item_file(&item).unwrap();
        let root = payload.parent().unwrap().parent().unwrap().to_path_buf();
        if root != trash_files_dir().parent().unwrap() {
            assert!(trash_folders().contains(&root), "{root:?} not among {:?}", trash_folders());
        }
        let found = trash_item_at(&payload).expect("found from its file");
        assert_eq!(found.original_path(), f);
        // An entry whose file is gone is cleared away.
        std::fs::remove_file(&payload).unwrap();
        remove_orphaned_info(&[root]);
        assert!(!Path::new(&found.id).exists());
    }

    /// An entry with a damaged info file (all zeros) cannot be listed, so it is not purged by
    /// the trash crate; emptying the trash still takes it.
    #[test]
    fn emptying_takes_entries_with_damaged_info_too() {
        let root = std::env::temp_dir().join("vantaexplorer-damaged-trash");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("files/$R1515WX/sub")).unwrap();
        std::fs::write(root.join("files/$R1515WX/sub/a.mp4"), b"x").unwrap();
        std::fs::create_dir_all(root.join("info")).unwrap();
        std::fs::write(root.join("info/$R1515WX.trashinfo"), [0u8; 86]).unwrap();
        assert!(item_from_info(&root, "$R1515WX".as_ref()).is_none(), "unreadable, as on the drive");
        assert!(empty_trash_folders(std::slice::from_ref(&root)).is_empty());
        assert_eq!(std::fs::read_dir(root.join("files")).unwrap().count(), 0);
        assert_eq!(std::fs::read_dir(root.join("info")).unwrap().count(), 0);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Renaming onto a dead link, or onto anything, is refused; a change of case only is not.
    #[test]
    fn rename_never_replaces_a_dead_link() {
        let (src, _) = setup("rename-dead-link");
        std::os::unix::fs::symlink(src.join("gone"), src.join("noter.txt")).unwrap();
        assert!(rename(&src.join("a.txt"), "noter.txt").is_err());
        assert!(src.join("a.txt").exists(), "nothing moved");
        assert!(src.join("noter.txt").symlink_metadata().unwrap().file_type().is_symlink(), "the link is still there");
        assert!(rename(&src.join("a.txt"), "A.txt").is_ok(), "a new spelling is a new name here");
    }

    /// On a drive that ignores case (exFAT, NTFS), set in VANTAEXPLORER_CASE_DIR.
    #[test]
    #[ignore = "needs a case-insensitive drive in VANTAEXPLORER_CASE_DIR"]
    fn a_change_of_case_renames_on_a_drive_that_ignores_case() {
        let dir = PathBuf::from(std::env::var("VANTAEXPLORER_CASE_DIR").unwrap()).join("vx-case-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("test.txt"), b"x").unwrap();
        let r = rename(&dir.join("test.txt"), "Test.txt");
        let names: Vec<_> = std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.file_name()).collect();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(r.is_ok(), "{r:?}");
        assert_eq!(names, ["Test.txt"]);
    }

    /// Same harness as run_and_drain but hands back the undo record.
    fn run_capture(req: Request) -> Option<UndoRecord> {
        let (tx, rx) = channel();
        let handle = std::thread::spawn(move || {
            run(1, req, &AtomicBool::new(false), &tx);
        });
        let out;
        loop {
            match rx.recv().expect("worker alive") {
                Event::Done { undo, .. } => {
                    out = undo;
                    break;
                }
                Event::Failed { error, .. } => panic!("op failed: {error}"),
                Event::Conflict { reply, .. } => {
                    reply.send(ConflictAnswer::Overwrite).unwrap();
                }
                _ => {}
            }
        }
        handle.join().unwrap();
        out
    }

    #[test]
    fn move_records_reversible_pairs() {
        let (src, dst) = setup("undo-move");
        let rec = run_capture(Request::new(
            OpKind::Move,
            vec![src.join("a.txt")],
            Some(dst.clone()),
        ))
        .expect("move must produce an undo record");
        match rec.action {
            UndoAction::Moved(pairs) => {
                assert_eq!(pairs.len(), 1);
                // (from, to) must point back at the ORIGINAL location, not the destination.
                assert_eq!(pairs[0].0, dst.join("a.txt"));
                assert_eq!(pairs[0].1, src.join("a.txt"));
            }
            other => panic!("wrong action: {other:?}"),
        }
        // And the reversal actually restores it.
        run_capture(Request {
            kind: OpKind::MoveExact,
            pairs: vec![(dst.join("a.txt"), src.join("a.txt"))],
            ..Default::default()
        });
        assert_eq!(std::fs::read(src.join("a.txt")).unwrap(), b"hello");
        assert!(!dst.join("a.txt").exists());
    }

    #[test]
    fn copy_records_created_roots() {
        let (src, dst) = setup("undo-copy");
        let rec = run_capture(Request::new(
            OpKind::Copy,
            vec![src.join("a.txt")],
            Some(dst.clone()),
        ))
        .expect("copy must produce an undo record");
        match rec.action {
            UndoAction::Copied(created) => assert_eq!(created, vec![dst.join("a.txt")]),
            other => panic!("wrong action: {other:?}"),
        }
    }

    #[test]
    fn skipped_source_is_not_recorded() {
        // The whole reason the record is observed rather than derived: a skipped conflict
        // must not end up in the undo record, or undo would "restore" an untouched file.
        let (src, dst) = setup("undo-skip");
        std::fs::write(dst.join("a.txt"), b"existing").unwrap();

        let (tx, rx) = channel();
        let req = Request::new(OpKind::Copy, vec![src.join("a.txt")], Some(dst.clone()));
        let handle = std::thread::spawn(move || run(1, req, &AtomicBool::new(false), &tx));
        let rec;
        loop {
            match rx.recv().unwrap() {
                Event::Conflict { reply, .. } => reply.send(ConflictAnswer::Skip).unwrap(),
                Event::Done { undo, .. } => {
                    rec = undo;
                    break;
                }
                Event::Failed { error, .. } => panic!("failed: {error}"),
                _ => {}
            }
        }
        handle.join().unwrap();
        // Nothing was copied, so there must be nothing to undo.
        assert!(rec.is_none(), "a fully skipped copy must record nothing");
        assert_eq!(std::fs::read(dst.join("a.txt")).unwrap(), b"existing");
    }

    #[test]
    fn conflict_rename_keeps_both_and_records_new_name() {
        let (src, dst) = setup("undo-rename");
        std::fs::write(dst.join("a.txt"), b"existing").unwrap();
        // A directory root too: nested paths must follow the renamed root.
        std::fs::create_dir_all(dst.join("sub")).unwrap();

        let (tx, rx) = channel();
        let req = Request::new(
            OpKind::Copy,
            vec![src.join("a.txt"), src.join("sub")],
            Some(dst.clone()),
        );
        let handle = std::thread::spawn(move || run(1, req, &AtomicBool::new(false), &tx));
        let rec;
        loop {
            match rx.recv().unwrap() {
                Event::Conflict { reply, .. } => reply.send(ConflictAnswer::RenameAll).unwrap(),
                Event::Done { undo, .. } => {
                    rec = undo;
                    break;
                }
                Event::Failed { error, .. } => panic!("failed: {error}"),
                _ => {}
            }
        }
        handle.join().unwrap();
        assert_eq!(std::fs::read(dst.join("a.txt")).unwrap(), b"existing", "original untouched");
        assert_eq!(std::fs::read(dst.join("a (1).txt")).unwrap(), b"hello");
        assert_eq!(std::fs::read(dst.join("sub (1)/n.txt")).unwrap(), b"nested");
        match rec.map(|r| r.action) {
            Some(UndoAction::Copied(paths)) => {
                assert_eq!(paths, vec![dst.join("a (1).txt"), dst.join("sub (1)")]);
            }
            other => panic!("undo must point at the names actually written, got {other:?}"),
        }
    }

    #[test]
    fn free_sibling_name_numbers_past_taken_ones() {
        let (src, _) = setup("sibling");
        std::fs::write(src.join("a (1).txt"), b"").unwrap();
        assert_eq!(free_sibling_name(&src, "a.txt".as_ref()), "a (2).txt");
        assert_eq!(free_sibling_name(&src, "sub".as_ref()), "sub (1)");
        assert_eq!(free_sibling_name(&src, ".bashrc".as_ref()), ".bashrc (1)");
    }

    #[test]
    fn trash_item_file_maps_info_to_payload() {
        let item = trash::TrashItem {
            id: "/home/x/.local/share/Trash/info/a.txt.trashinfo".into(),
            name: "a.txt".into(),
            original_parent: PathBuf::from("/home/x/docs"),
            time_deleted: 0,
        };
        assert_eq!(
            trash_item_file(&item).unwrap(),
            PathBuf::from("/home/x/.local/share/Trash/files/a.txt")
        );
    }

    /// Uses the REAL trash, from a folder under $HOME, because the trash has to be on the same
    /// filesystem as the files: a temp dir on tmpfs would land somewhere else or fail outright.
    /// It cleans up after itself, but it does put items in the trash of whoever runs it, so it is
    /// off by default. Run it with `cargo test -p vantaexplorer -- --ignored`.
    ///
    /// Covers: trash -> restore per item with one collision, the partial-success warning, undo
    /// pointing at what actually came back, and purge of the leftover via Delete inside the trash
    /// view.
    #[test]
    #[ignore = "puts real items in the trash"]
    fn restore_is_per_item_and_delete_in_trash_purges() {
        let Some(home) = std::env::var_os("HOME") else { return };
        let dir = PathBuf::from(home).join(".cache/vantaexplorer-trash-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("keep.txt"), b"k").unwrap();
        std::fs::write(dir.join("clash.txt"), b"c").unwrap();

        let rec = run_capture(Request::new(
            OpKind::Trash,
            vec![dir.join("keep.txt"), dir.join("clash.txt")],
            None,
        ));
        let Some(UndoAction::Trashed(items)) = rec.map(|r| r.action) else { panic!("trash record") };
        assert_eq!(items.len(), 2);
        for i in &items {
            assert!(trash_item_file(i).unwrap().exists(), "payload listed in files/");
        }
        // Occupy one original path, then restore both.
        std::fs::write(dir.join("clash.txt"), b"new").unwrap();
        let (tx, rx) = channel();
        let req = Request { kind: OpKind::RestoreTrash, trash_items: items.clone(), ..Default::default() };
        let handle = std::thread::spawn(move || run(1, req, &AtomicBool::new(false), &tx));
        let (rec, warning) = loop {
            match rx.recv().unwrap() {
                Event::Done { undo, warning, .. } => break (undo, warning),
                Event::Failed { error, .. } => panic!("partial restore must not fail outright: {error}"),
                _ => {}
            }
        };
        handle.join().unwrap();
        assert_eq!(std::fs::read(dir.join("keep.txt")).unwrap(), b"k", "restored");
        assert_eq!(std::fs::read(dir.join("clash.txt")).unwrap(), b"new", "untouched");
        assert!(warning.is_some_and(|w| w.contains("clash.txt")), "user told about the one left");
        match rec.map(|r| r.action) {
            Some(UndoAction::Restored(p)) => assert_eq!(p, vec![dir.join("keep.txt")]),
            other => panic!("undo must list only what came back, got {other:?}"),
        }
        // The clash is still in the trash; deleting its payload path must purge it.
        let leftover = trash_item_file(&items[1]).unwrap();
        assert!(trash_item_at(&leftover).is_some());
        run_capture(Request::new(OpKind::Delete, vec![leftover.clone()], None));
        assert!(!leftover.exists());
        assert!(trash_item_at(&leftover).is_none(), "no ghost .trashinfo left behind");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn delete_is_never_undoable() {
        let (src, _) = setup("undo-delete");
        let rec = run_capture(Request::new(OpKind::Delete, vec![src.join("a.txt")], None));
        assert!(rec.is_none(), "permanent delete must never reach the undo stack");
    }

    #[test]
    fn rename_rejects_existing() {
        let (src, _) = setup("rename");
        assert!(rename(&src.join("a.txt"), "b.txt").is_ok());
        std::fs::write(src.join("a.txt"), b"again").unwrap();
        assert!(rename(&src.join("a.txt"), "b.txt").is_err());
    }
}

#[cfg(test)]
mod archive_tests {
    use super::*;

    #[test]
    fn recognises_double_and_single_suffixes() {
        for ok in ["a.zip", "a.tar.gz", "a.tar.zst", "a.7z", "a.TGZ", "a.tar", "a.cbz"] {
            assert!(is_archive(Path::new(ok)), "{ok} should be an archive");
        }
        for no in ["a.txt", "a.png", "a.gzip", "archive", "a.tar.gz.txt"] {
            assert!(!is_archive(Path::new(no)), "{no} should not be");
        }
    }

    /// "x.tar.gz" must become "x", not "x.tar", otherwise every tarball extracts into a
    /// folder with a misleading name.
    #[test]
    fn strips_both_suffixes() {
        assert_eq!(extract_dir_name(Path::new("/t/x.tar.gz")), "x");
        assert_eq!(extract_dir_name(Path::new("/t/x.zip")), "x");
        assert_eq!(extract_dir_name(Path::new("/t/my.data.tar.zst")), "my.data");
    }

    fn run_extract(archive: &Path, dest: &Path) -> (Result<(), String>, Option<UndoRecord>) {
        let (tx, rx) = channel();
        let req = Request::new(OpKind::Extract, vec![archive.to_path_buf()], Some(dest.into()));
        let mut undo = None;
        let r = extract(1, &req, &AtomicBool::new(false), &tx, &mut undo);
        drop(rx);
        (r, undo)
    }

    #[test]
    fn extracts_and_records_only_what_it_created() {
        let tmp = std::env::temp_dir().join(format!("vantaexplorer-arc-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let src = tmp.join("src");
        std::fs::create_dir_all(src.join("inner")).unwrap();
        std::fs::write(src.join("inner/a.txt"), b"hello").unwrap();

        let archive = tmp.join("bundle.tar.gz");
        let ok = std::process::Command::new("bsdtar")
            .args(["-czf"])
            .arg(&archive)
            .arg("-C")
            .arg(&src)
            .arg("inner")
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !ok {
            return; // no bsdtar here; nothing to assert
        }

        let dest = tmp.join("out");
        std::fs::create_dir_all(&dest).unwrap();
        // A file that was already there must NOT end up in the undo record.
        std::fs::write(dest.join("pre-existing.txt"), b"keep").unwrap();

        let (res, undo) = run_extract(&archive, &dest);
        assert!(res.is_ok(), "extract failed: {res:?}");
        assert_eq!(std::fs::read(dest.join("inner/a.txt")).unwrap(), b"hello");

        let Some(UndoRecord { action: UndoAction::Copied(created), .. }) = undo else {
            panic!("expected a Copied undo record, got {undo:?}");
        };
        assert_eq!(created.len(), 1, "only the new top-level entry: {created:?}");
        assert!(created[0].ends_with("inner"));

        let _ = std::fs::remove_dir_all(&tmp);
    }
}

#[cfg(test)]
mod compress_tests {
    use super::*;

    fn run(req: Request) -> (Result<(), String>, Option<UndoRecord>) {
        let (tx, rx) = channel();
        let mut undo = None;
        let r = compress(1, &req, &AtomicBool::new(false), &tx, &mut undo);
        drop(rx);
        (r, undo)
    }

    fn tmpdir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("vantaexplorer-cmp-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// Never overwrite an existing archive: the one you would clobber may be the only copy.
    #[test]
    fn picks_a_free_name() {
        let d = tmpdir("free");
        assert_eq!(free_archive_path(&d, "x", "zip"), d.join("x.zip"));
        std::fs::write(d.join("x.zip"), b"taken").unwrap();
        assert_eq!(free_archive_path(&d, "x", "zip"), d.join("x-1.zip"));
        std::fs::write(d.join("x-1.zip"), b"taken").unwrap();
        assert_eq!(free_archive_path(&d, "x", "zip"), d.join("x-2.zip"));
        let _ = std::fs::remove_dir_all(&d);
    }

    /// The pair has to work together, so this compresses and then extracts the result and
    /// compares. It also pins the archive's INTERNAL paths: entries must be relative, or the
    /// archive carries the whole absolute path of this machine.
    #[test]
    fn round_trips_through_extract() {
        let d = tmpdir("round");
        std::fs::create_dir_all(d.join("data/sub")).unwrap();
        std::fs::write(d.join("data/top.txt"), b"one").unwrap();
        std::fs::write(d.join("data/sub/deep.txt"), b"two").unwrap();

        let mut req = Request::new(OpKind::Compress, vec![d.join("data")], Some(d.clone()));
        req.format = Some("tar.zst".into());
        let (res, undo) = run(req);
        if res.is_err() && !d.join("data.tar.zst").exists() {
            let _ = std::fs::remove_dir_all(&d);
            return; // no bsdtar here
        }
        assert!(res.is_ok(), "compress failed: {res:?}");
        let archive = d.join("data.tar.zst");
        assert!(archive.exists(), "archive not created");

        // Undo must trash exactly the archive.
        let Some(UndoRecord { action: UndoAction::Copied(created), .. }) = undo else {
            panic!("expected a Copied undo record");
        };
        assert_eq!(created, vec![archive.clone()]);

        // And it must be a real archive our own extractor can read back.
        let out = d.join("out");
        let (er, _) = {
            let (tx, rx) = channel();
            let mut u = None;
            let r = extract(
                2,
                &Request::new(OpKind::Extract, vec![archive], Some(out.clone())),
                &AtomicBool::new(false),
                &tx,
                &mut u,
            );
            drop(rx);
            (r, u)
        };
        assert!(er.is_ok(), "extract failed: {er:?}");
        assert_eq!(std::fs::read(out.join("data/top.txt")).unwrap(), b"one");
        assert_eq!(std::fs::read(out.join("data/sub/deep.txt")).unwrap(), b"two");

        let _ = std::fs::remove_dir_all(&d);
    }
}

#[cfg(test)]
mod new_file_tests {
    use super::*;

    /// New File must never clobber an existing file, and must fall through the numbered
    /// names the same way New Folder does.
    #[test]
    fn picks_free_names_and_creates_empty_files() {
        let d = std::env::temp_dir().join(format!("vantaexplorer-newfile-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("New File"), b"keep me").unwrap();

        let a = new_file(&d).unwrap();
        assert_eq!(a.file_name().unwrap(), "New File (1)");
        assert_eq!(std::fs::metadata(&a).unwrap().len(), 0, "must be empty");
        assert_eq!(std::fs::read(d.join("New File")).unwrap(), b"keep me", "existing untouched");

        let b = new_file(&d).unwrap();
        assert_eq!(b.file_name().unwrap(), "New File (2)");
        let _ = std::fs::remove_dir_all(&d);
    }
}
