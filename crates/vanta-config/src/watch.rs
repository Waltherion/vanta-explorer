//! Noticing that a file has changed, by looking at it now and then.
//!
//! Polling rather than a kernel watch, on purpose. The files that matter here are reached through
//! a symlink that a theme switcher repoints, editors save through a temporary file and a rename,
//! and a file can sit on a network drive. A kernel watch has to be told about
//! each of those separately and still misses some; a look at each file every half second sees them
//! all the same way and costs nothing.

use std::path::PathBuf;
use std::time::SystemTime;

/// What a file was last time: which file it really is, once links are followed, and its time
/// and size. Nothing when it is not there.
type Seen = Option<(PathBuf, SystemTime, u64)>;

fn look(p: &PathBuf) -> Seen {
    let real = std::fs::canonicalize(p).ok()?;
    let m = std::fs::metadata(&real).ok()?;
    Some((real, m.modified().ok()?, m.len()))
}

type Source = Box<dyn Fn() -> Vec<PathBuf> + Send>;

pub struct Stamp {
    paths: Vec<PathBuf>,
    /// Where the list comes from when it can change between looks: the active theme decides
    /// which of its files are watched, so a theme switch changes the list itself.
    source: Option<Source>,
    seen: Vec<Seen>,
}

impl Stamp {
    /// Remembers the files as they are now.
    pub fn new(paths: Vec<PathBuf>) -> Stamp {
        let seen = paths.iter().map(look).collect();
        Stamp { paths, source: None, seen }
    }

    /// Like `new`, but asks `source` for the files at every look, so a file that joins or leaves
    /// the list counts as a change too.
    pub fn following(source: impl Fn() -> Vec<PathBuf> + Send + 'static) -> Stamp {
        let paths = source();
        let seen = paths.iter().map(look).collect();
        Stamp { paths, source: Some(Box::new(source)), seen }
    }

    /// Whether any of the files differs from the last look: edited, replaced, created, removed,
    /// or, through a link, a different file altogether.
    pub fn changed(&mut self) -> bool {
        if let Some(source) = &self.source {
            let paths = source();
            if paths != self.paths {
                self.seen = paths.iter().map(look).collect();
                self.paths = paths;
                return true;
            }
        }
        let now: Vec<Seen> = self.paths.iter().map(look).collect();
        let changed = now != self.seen;
        self.seen = now;
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_edit_a_new_file_and_a_repointed_link_all_count() {
        let dir = std::env::temp_dir().join(format!("vanta-watch-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let (a, b, link) = (dir.join("a.jsonc"), dir.join("b.jsonc"), dir.join("current.jsonc"));
        std::fs::write(&a, "{}").unwrap();
        std::fs::write(&b, "{ }").unwrap();
        std::os::unix::fs::symlink(&a, &link).unwrap();
        let missing = dir.join("later.jsonc");

        let mut s = Stamp::new(vec![link.clone(), missing.clone()]);
        assert!(!s.changed(), "nothing happened yet");
        std::fs::write(&a, "{ \"x\": 1 }").unwrap();
        assert!(s.changed(), "an edit");
        assert!(!s.changed(), "and only once");
        std::fs::remove_file(&link).unwrap();
        std::os::unix::fs::symlink(&b, &link).unwrap();
        assert!(s.changed(), "the link now points at another file, as on a theme switch");
        std::fs::write(&missing, "{}").unwrap();
        assert!(s.changed(), "a file that appears");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_list_that_changes_counts_once() {
        let dir = std::env::temp_dir().join(format!("vanta-watch-follow-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let pick = dir.join("pick");
        std::fs::write(&pick, "a").unwrap();
        let p = pick.clone();
        let d = dir.clone();
        let mut s = Stamp::following(move || {
            let name = std::fs::read_to_string(&p).unwrap_or_default();
            vec![p.clone(), d.join(format!("{}.jsonc", name.trim()))]
        });
        assert!(!s.changed());
        std::fs::write(&pick, "b").unwrap();
        assert!(s.changed(), "another file is watched now");
        assert!(!s.changed(), "and only once");
        std::fs::write(dir.join("b.jsonc"), "{}").unwrap();
        assert!(s.changed(), "the new file is the one looked at");
        let _ = std::fs::remove_dir_all(dir);
    }
}
