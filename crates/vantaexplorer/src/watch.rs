//! Live folder refresh: reload a tab when something ELSE changes the folder it shows.
//!
//! Vanta Explorer's own operations already reload after themselves; this is for the download that
//! finishes, the file Krita saves, the script that writes. One non-recursive inotify watch per
//! visible folder. Events are debounced (a `cp` of 500 files must cost one reload, not 500)
//! and drained from `about_to_wait`, which already wakes at least every 100 ms, so no
//! event-loop proxy is needed (the Wayland on-demand-render trap is handled by the caller
//! marking the frame dirty).

use notify::{RecursiveMode, Watcher as _};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver};
use std::time::{Duration, Instant};

/// Reload once the folder has been quiet this long ...
const QUIET: Duration = Duration::from_millis(200);
/// ... or at least this often while it keeps changing (a long download never goes quiet).
const MAX_WAIT: Duration = Duration::from_secs(1);

pub struct Watch {
    inner: Option<notify::RecommendedWatcher>,
    rx: Receiver<PathBuf>,
    watched: HashSet<PathBuf>,
    /// Folders that could not be watched (virtual folders, gone folders); not retried every
    /// tick, only after they leave and re-enter the wanted set.
    failed: HashSet<PathBuf>,
    /// folder -> (first event, last event) since its last reload.
    pending: HashMap<PathBuf, (Instant, Instant)>,
}

impl Watch {
    pub fn new() -> Self {
        let (tx, rx) = channel();
        let inner = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            if let Ok(ev) = res {
                // inotify reports opens and reads too. Our OWN listing scan opens the folder,
                // so passing Access events through turns every reload into the next reload:
                // a self-sustaining loop at the debounce interval (measured, 17 reloads for
                // 6 real changes, continuing after the last one).
                if matches!(ev.kind, notify::EventKind::Access(_)) {
                    return;
                }
                log::trace!("fs event {:?} {:?}", ev.kind, ev.paths);
                for p in ev.paths {
                    let _ = tx.send(p);
                }
            }
        });
        let inner = match inner {
            Ok(w) => Some(w),
            Err(e) => {
                log::warn!("folder watching unavailable: {e}");
                None
            }
        };
        Self {
            inner,
            rx,
            watched: HashSet::new(),
            failed: HashSet::new(),
            pending: HashMap::new(),
        }
    }

    /// Make the watched set equal to `want`: the folders currently on screen.
    pub fn sync<'a>(&mut self, want: impl Iterator<Item = &'a Path>) {
        let Some(w) = self.inner.as_mut() else { return };
        let want: HashSet<PathBuf> = want.map(Path::to_path_buf).collect();
        for gone in self.watched.difference(&want).cloned().collect::<Vec<_>>() {
            let _ = w.unwatch(&gone);
            self.watched.remove(&gone);
            self.pending.remove(&gone);
        }
        self.failed.retain(|p| want.contains(p));
        for new in want.difference(&self.watched).cloned().collect::<Vec<_>>() {
            if self.failed.contains(&new) {
                continue;
            }
            match w.watch(&new, RecursiveMode::NonRecursive) {
                Ok(()) => {
                    self.watched.insert(new);
                }
                Err(e) => {
                    log::debug!("cannot watch {}: {e}", new.display());
                    self.failed.insert(new);
                }
            }
        }
    }

    /// Folders whose changes have settled enough to reload now.
    pub fn poll(&mut self) -> Vec<PathBuf> {
        let now = Instant::now();
        while let Ok(p) = self.rx.try_recv() {
            // Non-recursive watches deliver direct children (and the folder itself).
            let dir = if self.watched.contains(&p) {
                Some(p)
            } else {
                p.parent().filter(|d| self.watched.contains(*d)).map(Path::to_path_buf)
            };
            if let Some(dir) = dir {
                self.pending
                    .entry(dir)
                    .and_modify(|(_, last)| *last = now)
                    .or_insert((now, now));
            }
        }
        let due: Vec<PathBuf> = self
            .pending
            .iter()
            .filter(|(_, (first, last))| {
                now.duration_since(*last) >= QUIET || now.duration_since(*first) >= MAX_WAIT
            })
            .map(|(d, _)| d.clone())
            .collect();
        for d in &due {
            self.pending.remove(d);
        }
        due
    }

    /// The caller could not reload this folder right now (a listing is still streaming);
    /// try again on a later tick.
    pub fn defer(&mut self, dir: PathBuf) {
        let now = Instant::now();
        self.pending.entry(dir).or_insert((now, now));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn external_write_is_reported_once_after_quiet() {
        let dir = std::env::temp_dir().join("vantaexplorer-watch-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut w = Watch::new();
        if w.inner.is_none() {
            return; // no inotify here; nothing to test
        }
        w.sync([dir.as_path()].into_iter());
        // A burst of writes must collapse into one reload.
        for i in 0..20 {
            std::fs::write(dir.join(format!("f{i}")), b"x").unwrap();
        }
        // Wait for the first report, then keep listening a while to catch any second one.
        let deadline = Instant::now() + Duration::from_secs(3);
        let mut reports = 0;
        let mut settle: Option<Instant> = None;
        while Instant::now() < deadline {
            reports += w.poll().iter().filter(|d| **d == dir).count();
            if reports > 0 && settle.is_none() {
                settle = Some(Instant::now() + QUIET * 3);
            }
            if settle.is_some_and(|s| Instant::now() >= s) {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(reports, 1, "a burst of writes must cost exactly one reload");
    }
}
