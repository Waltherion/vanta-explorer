use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, TryRecvError};
use std::sync::Arc;

/// Recursive size of one or more directories, computed off the UI thread.
///
/// Takes a SET of roots, not a single path: the selection summary needs the total for
/// everything selected at once, and running one walk per folder would race and thrash. A new
/// set cancels the walk in flight, so changing selection does not leave workers running.
pub struct DirSize {
    /// The directories being measured. Empty means idle.
    roots: Vec<PathBuf>,
    rx: Option<Receiver<(u64, u64)>>,
    cancel: Option<Arc<AtomicBool>>,
    pub bytes: u64,
    pub files: u64,
    pub done: bool,
}

impl DirSize {
    pub fn new() -> Self {
        Self {
            roots: Vec::new(),
            rx: None,
            cancel: None,
            bytes: 0,
            files: 0,
            done: true,
        }
    }

    /// True when the result on hand is for exactly these roots.
    pub fn matches(&self, roots: &[PathBuf]) -> bool {
        self.roots == roots
    }

    pub fn start(&mut self, roots: Vec<PathBuf>) {
        if self.roots == roots {
            return;
        }
        if let Some(c) = &self.cancel {
            c.store(true, Ordering::Relaxed);
        }
        let seed = roots.clone();
        self.roots = roots;
        self.bytes = 0;
        self.files = 0;
        self.done = false;

        let (tx, rx) = channel();
        let cancel = Arc::new(AtomicBool::new(false));
        self.rx = Some(rx);
        self.cancel = Some(cancel.clone());

        std::thread::Builder::new()
            .name("vx-dirsize".into())
            .spawn(move || {
                use std::os::unix::fs::MetadataExt;
                // Each root is counted on its own filesystem only: a home folder must not walk on
                // into every drive and network mount under it (a cloud drive can take minutes).
                let mut dirs: Vec<(PathBuf, u64)> = seed.into_iter().filter_map(|p| Some((std::fs::metadata(&p).ok()?.dev(), p))).map(|(d, p)| (p, d)).collect();
                let mut total_bytes = 0;
                let mut total_files = 0;
                // Progress at most ten times a second; the UI polls it at about that rate.
                let mut sent = std::time::Instant::now();

                while let Some((d, dev)) = dirs.pop() {
                    if cancel.load(Ordering::Relaxed) { return; }
                    if let Ok(rd) = std::fs::read_dir(&d) {
                        for entry in rd.flatten() {
                            if cancel.load(Ordering::Relaxed) { return; }
                            // DirEntry::metadata does not follow symlinks, so a link is counted
                            // as itself and a link to a parent cannot loop.
                            if let Ok(meta) = entry.metadata() {
                                if meta.is_dir() {
                                    if meta.dev() == dev {
                                        dirs.push((entry.path(), dev));
                                    }
                                } else {
                                    total_bytes += meta.len();
                                    total_files += 1;
                                }
                            }
                            if sent.elapsed() >= std::time::Duration::from_millis(100) {
                                sent = std::time::Instant::now();
                                if tx.send((total_bytes, total_files)).is_err() {
                                    return;
                                }
                            }
                        }
                    }
                }
                let _ = tx.send((total_bytes, total_files));
            })
            .expect("spawn dirsize thread");
    }

    pub fn poll(&mut self) -> bool {
        if self.done || self.rx.is_none() {
            return false;
        }
        let rx = self.rx.as_ref().unwrap();
        let mut updated = false;
        while let Ok((b, f)) = rx.try_recv() {
            self.bytes = b;
            self.files = f;
            updated = true;
        }
        
        let err = rx.try_recv().err();
        if err == Some(TryRecvError::Disconnected) {
            self.done = true;
            updated = true;
        }
        updated
    }
}
