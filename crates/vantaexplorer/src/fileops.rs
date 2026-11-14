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
        reply: Sender<ConflictAnswer>,
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
    if dest.exists() {
        return Err("a file with that name already exists".into());
    }
    std::fs::rename(path, &dest).map_err(|e| e.to_string())?;
    Ok(dest)
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
                let meta = std::fs::symlink_metadata(p).map_err(|e| e.to_string())?;
                if meta.is_dir() {
                    std::fs::remove_dir_all(p).map_err(|e| e.to_string())
                } else {
                    std::fs::remove_file(p).map_err(|e| e.to_string())
                }
            })
        }
        OpKind::Copy | OpKind::Move => transfer(id, &req, cancel, ev, &mut undo),
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
                std::fs::rename(from, to).map_err(|e| e.to_string())?;
                back.push((to.clone(), from.clone()));
                let _ = ev.send(Event::Progress {
                    id,
                    bytes_done: 0,
                    items_done: n as u64 + 1,
                    current: to.file_name().unwrap_or_default().to_string_lossy().into_owned(),
                });
                Ok(())
            });
            if r.is_ok() && !back.is_empty() {
                undo = Some(UndoRecord { action: UndoAction::Moved(back), displaced: Vec::new() });
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
            trash::os_limited::list()
                .and_then(trash::os_limited::purge_all)
                .map_err(|e| format!("{e:?}"))
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
    // Cheap shape check before listing the trash: <trash>/files/<name> next to <trash>/info.
    let files_dir = path.parent()?;
    if files_dir.file_name()? != "files" || !files_dir.parent()?.join("info").is_dir() {
        return None;
    }
    trash::os_limited::list()
        .ok()?
        .into_iter()
        .find(|i| trash_item_file(i).is_some_and(|f| f == path))
}

/// Trash entries for the given payload paths (selection in the trash view), in order.
pub fn trash_items_at(paths: &[PathBuf]) -> Vec<trash::TrashItem> {
    let Ok(all) = trash::os_limited::list() else { return Vec::new() };
    let by_file: std::collections::HashMap<PathBuf, trash::TrashItem> = all
        .into_iter()
        .filter_map(|i| trash_item_file(&i).map(|f| (f, i)))
        .collect();
    paths.iter().filter_map(|p| by_file.get(p).cloned()).collect()
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
) -> Result<(), String> {
    let dest_dir = req.dest.as_ref().ok_or("no destination")?;
    let mut moved: Vec<(PathBuf, PathBuf)> = Vec::new();
    let mut created: Vec<PathBuf> = Vec::new();
    let mut displaced: Vec<trash::TrashItem> = Vec::new();

    // Same-device Move degenerates to rename: instant, no byte progress needed.
    if req.kind == OpKind::Move {
        if let (Ok(a), Ok(b)) = (
            std::fs::metadata(dest_dir),
            req.sources
                .first()
                .and_then(|s| std::fs::symlink_metadata(s).ok())
                .ok_or("missing source"),
        ) {
            use std::os::unix::fs::MetadataExt;
            if a.dev() == b.dev() {
                let _ = ev.send(Event::Started {
                    id,
                    kind: req.kind,
                    total_bytes: 0,
                    total_items: req.sources.len() as u64,
                });
                let mut policy = ConflictPolicy::default();
                for (n, src) in req.sources.iter().enumerate() {
                    let name = src.file_name().ok_or("nameless source")?;
                    let mut target = dest_dir.join(name);
                    if target.exists() && target != *src {
                        match resolve_conflict(id, dest_dir, name, &mut policy, ev)? {
                            Do::Skip => continue,
                            Do::Overwrite => {
                                if let Some(item) = trash_and_record(&target)? {
                                    displaced.push(item);
                                }
                            }
                            Do::Rename(new_name) => target = dest_dir.join(new_name),
                        }
                    }
                    std::fs::rename(src, &target).map_err(|e| e.to_string())?;
                    moved.push((target.clone(), src.clone()));
                    let _ = ev.send(Event::Progress {
                        id,
                        bytes_done: 0,
                        items_done: n as u64 + 1,
                        current: name.to_string_lossy().into_owned(),
                    });
                }
                if !moved.is_empty() || !displaced.is_empty() {
                    *undo = Some(UndoRecord { action: UndoAction::Moved(moved), displaced });
                }
                return Ok(());
            }
        }
    }

    // Pre-scan: totals for honest progress, done before touching anything.
    let mut files: Vec<(PathBuf, PathBuf, u64)> = Vec::new(); // (src, rel, bytes)
    let mut dirs: Vec<PathBuf> = Vec::new(); // relative
    let mut total_bytes = 0u64;
    for src in &req.sources {
        let base = src.parent().unwrap_or(Path::new("/"));
        scan(src, base, &mut files, &mut dirs, &mut total_bytes)?;
    }
    let _ = ev.send(Event::Started {
        id,
        kind: req.kind,
        total_bytes,
        total_items: files.len() as u64,
    });

    // Top-level conflicts are decided up front; nested paths follow their root's fate.
    let mut policy = ConflictPolicy::default();
    let mut skipped_roots: Vec<PathBuf> = Vec::new();
    // (source root name, name it is written under) for roots the user chose to keep both of.
    let mut renamed_roots: Vec<(PathBuf, PathBuf)> = Vec::new();
    for src in &req.sources {
        let name = src.file_name().ok_or("nameless source")?;
        let target = dest_dir.join(name);
        if target.exists() {
            match resolve_conflict(id, dest_dir, name, &mut policy, ev)? {
                Do::Skip => skipped_roots.push(PathBuf::from(name)),
                Do::Overwrite => {
                    if let Some(item) = trash_and_record(&target)? {
                        displaced.push(item);
                    }
                }
                Do::Rename(new_name) => renamed_roots.push((PathBuf::from(name), PathBuf::from(new_name))),
            }
        }
    }
    // Where a relative path lands: under its root's new name if that root was renamed.
    let dest_of = |rel: &Path| -> PathBuf {
        for (from, to) in &renamed_roots {
            if let Ok(tail) = rel.strip_prefix(from) {
                // The root itself strips to "", and join("") appends a trailing slash that
                // makes the kernel treat a file path as a directory.
                return if tail.as_os_str().is_empty() {
                    dest_dir.join(to)
                } else {
                    dest_dir.join(to).join(tail)
                };
            }
        }
        dest_dir.join(rel)
    };

    for rel in &dirs {
        if skipped_roots.iter().any(|r| rel.starts_with(r)) {
            continue;
        }
        std::fs::create_dir_all(dest_of(rel)).map_err(|e| e.to_string())?;
    }

    let mut bytes_done = 0u64;
    for (n, (src, rel, size)) in files.iter().enumerate() {
        if skipped_roots.iter().any(|r| rel.starts_with(r)) {
            continue;
        }
        if cancel.load(Ordering::Relaxed) {
            return Err("cancelled".into());
        }
        let target = dest_of(rel);
        copy_file(src, &target, cancel, &mut |done| {
            let _ = ev.send(Event::Progress {
                id,
                bytes_done: bytes_done + done,
                items_done: n as u64,
                current: rel.to_string_lossy().into_owned(),
            });
        })?;
        bytes_done += size;
    }

    // Record what actually landed: every source root that was not skipped, under the name
    // it was really written as.
    for src in &req.sources {
        let name = src.file_name().ok_or("nameless source")?;
        if skipped_roots.iter().any(|r| Path::new(name) == r) {
            continue;
        }
        let target = dest_of(Path::new(name));
        if req.kind == OpKind::Move {
            moved.push((target, src.clone()));
        } else {
            created.push(target);
        }
    }

    if req.kind == OpKind::Move {
        for src in &req.sources {
            let name = src.file_name().ok_or("nameless source")?;
            if skipped_roots.iter().any(|r| Path::new(name) == r) {
                continue;
            }
            remove_any(src)?;
        }
    }

    let action = if req.kind == OpKind::Move {
        UndoAction::Moved(moved)
    } else {
        UndoAction::Copied(created)
    };
    let empty = match &action {
        UndoAction::Moved(v) => v.is_empty(),
        UndoAction::Copied(v) => v.is_empty(),
        _ => true,
    };
    if !empty || !displaced.is_empty() {
        *undo = Some(UndoRecord { action, displaced });
    }
    Ok(())
}

enum Do {
    Overwrite,
    Skip,
    /// Write under this name instead of the source's.
    Rename(std::ffi::OsString),
}

/// Sticky "... all" answers for one operation.
#[derive(Default)]
struct ConflictPolicy {
    overwrite_all: bool,
    skip_all: bool,
    rename_all: bool,
}

/// First free "name (n).ext" sibling in `dir`, the name Dolphin and Nautilus would pick.
/// Directories and dot-files have no extension to preserve, so they get "name (n)".
fn free_sibling_name(dir: &Path, name: &std::ffi::OsStr) -> std::ffi::OsString {
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
        if !dir.join(&candidate).exists() {
            return candidate.into();
        }
        n += 1;
    }
}

fn resolve_conflict(
    id: u64,
    dest_dir: &Path,
    name: &std::ffi::OsStr,
    policy: &mut ConflictPolicy,
    ev: &Sender<Event>,
) -> Result<Do, String> {
    if policy.overwrite_all {
        return Ok(Do::Overwrite);
    }
    if policy.skip_all {
        return Ok(Do::Skip);
    }
    if policy.rename_all {
        return Ok(Do::Rename(free_sibling_name(dest_dir, name)));
    }
    let (tx, rx) = channel();
    let _ = ev.send(Event::Conflict {
        id,
        name: name.to_string_lossy().into_owned(),
        reply: tx,
    });
    match rx.recv().map_err(|_| "ui gone")? {
        ConflictAnswer::Overwrite => Ok(Do::Overwrite),
        ConflictAnswer::Skip => Ok(Do::Skip),
        ConflictAnswer::Rename => Ok(Do::Rename(free_sibling_name(dest_dir, name))),
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
            Ok(Do::Rename(free_sibling_name(dest_dir, name)))
        }
        ConflictAnswer::Cancel => Err("cancelled".into()),
    }
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
