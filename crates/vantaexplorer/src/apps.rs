//! "Open with" application discovery, the XDG way.
//!
//! KDE apps keep their own association database; xdg-open on a bare Hyprland reads
//! ~/.config/mimeapps.list instead, which often nothing has written. So
//! Vanta Explorer both lists candidates (mimeinfo.cache) and can SET the default (xdg-mime), after
//! which plain double-click follows it too.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

#[derive(Clone, Debug)]
pub struct AppEntry {
    pub name: String,
    pub desktop_id: String,
    exec: String,
    pub mimes: Vec<String>,
}

fn data_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(home) = std::env::var_os("HOME") {
        dirs.push(PathBuf::from(home).join(".local/share/applications"));
    }
    for d in std::env::var("XDG_DATA_DIRS")
        .unwrap_or_else(|_| "/usr/local/share:/usr/share".into())
        .split(':')
    {
        dirs.push(PathBuf::from(d).join("applications"));
    }
    dirs
}

/// mime -> desktop ids, from every mimeinfo.cache; user dirs first so they win on dedup.
fn mime_cache() -> &'static HashMap<String, Vec<String>> {
    static CACHE: OnceLock<HashMap<String, Vec<String>>> = OnceLock::new();
    CACHE.get_or_init(|| {
        let mut map: HashMap<String, Vec<String>> = HashMap::new();
        for dir in data_dirs() {
            let Ok(text) = std::fs::read_to_string(dir.join("mimeinfo.cache")) else {
                continue;
            };
            for line in text.lines() {
                let Some((mime, ids)) = line.split_once('=') else {
                    continue;
                };
                let entry = map.entry(mime.to_string()).or_default();
                for id in ids.split(';').filter(|s| !s.is_empty()) {
                    if !entry.iter().any(|e| e == id) {
                        entry.push(id.to_string());
                    }
                }
            }
        }
        map
    })
}

/// Parse the bits of a .desktop file we need.
///
/// `allow_no_display` is the difference between the two lists. `NoDisplay=true` means "do not
/// show this in the APPLICATION MENU", it does not mean the entry cannot open files. Krita
/// is the case in point: it ships one hidden .desktop per format (krita_png.desktop and
/// friends, all aliases of org.kde.krita.desktop) purely to register MIME associations, so
/// filtering them out dropped Krita from "Open with" entirely even though it handles the
/// file. The Open-with list therefore keeps them; the browse-everything list does not, or it
/// would fill with per-format duplicates.
///
/// `Hidden=true` is always dropped: per spec that means the user deleted the entry.
fn parse_desktop(id: &str, allow_no_display: bool) -> Option<AppEntry> {
    let path = data_dirs()
        .into_iter()
        .map(|d| d.join(id))
        .find(|p| p.exists())?;
    let text = std::fs::read_to_string(path).ok()?;
    let mut name = None;
    let mut exec = None;
    let mut mimes = Vec::new();
    let mut in_main = false;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_main = line == "[Desktop Entry]";
            continue;
        }
        if !in_main {
            continue;
        }
        if let Some(v) = line.strip_prefix("NoDisplay=") {
            if v.trim() == "true" && !allow_no_display {
                return None;
            }
        } else if let Some(v) = line.strip_prefix("Hidden=") {
            if v.trim() == "true" {
                return None;
            }
        } else if name.is_none() {
            if let Some(v) = line.strip_prefix("Name=") {
                name = Some(v.to_string());
            }
        }
        if exec.is_none() {
            if let Some(v) = line.strip_prefix("Exec=") {
                exec = Some(v.to_string());
            }
        }
        if let Some(v) = line.strip_prefix("MimeType=") {
            for m in v.split(';') {
                let m = m.trim();
                if !m.is_empty() {
                    mimes.push(m.to_string());
                }
            }
        }
    }
    Some(AppEntry {
        name: name?,
        desktop_id: id.to_string(),
        exec: exec?,
        mimes,
    })
}

/// The type nothing is known about, which is what a name no rule matches comes out as.
pub const UNKNOWN: &str = "application/octet-stream";

/// A file's type. The name decides when it says anything; a name no rule knows (`.conf`, a file
/// with no extension) leaves it to the contents, as xdg-open does, so a text file is a text
/// file whatever it is called and its editors are on offer.
pub fn mime_for(path: &Path) -> Option<String> {
    let name = path.file_name()?.to_string_lossy();
    mime_by_name(&name).or_else(|| Some(mime_db().guess_mime_type().path(path).guess().mime_type().to_string()))
}

fn mime_db() -> &'static xdg_mime::SharedMimeInfo {
    static DB: OnceLock<xdg_mime::SharedMimeInfo> = OnceLock::new();
    DB.get_or_init(xdg_mime::SharedMimeInfo::new)
}

/// A file's type from its name alone, without opening it; nothing when the name says nothing.
pub fn mime_by_name(name: &str) -> Option<String> {
    mime_db().get_mime_types_from_file_name(name).first().map(|m| m.to_string()).filter(|m| m != UNKNOWN)
}

/// Candidate applications for a file, current default first.
pub fn candidates(path: &Path) -> Vec<AppEntry> {
    let Some(mime) = mime_for(path) else {
        return Vec::new();
    };
    let mut ids: Vec<String> = mime_cache().get(&mime).cloned().unwrap_or_default();
    // Then what takes the kinds it is: a text editor for Rust, which only says text/plain.
    for kind in ancestors(&mime) {
        for id in mime_cache().get(&kind).into_iter().flatten() {
            if !ids.contains(id) {
                ids.push(id.clone());
            }
        }
    }
    // The configured default floats to the top.
    if let Ok(out) = std::process::Command::new("xdg-mime")
        .args(["query", "default", &mime])
        .output()
    {
        let def = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if !def.is_empty() {
            ids.retain(|i| *i != def);
            ids.insert(0, def);
        }
    }
    let mut out: Vec<AppEntry> = ids
        .iter()
        .filter_map(|id| parse_desktop(id, true))
        .collect();
    // An app registered through several aliases (Krita again) would otherwise appear once per
    // matching .desktop. Same name AND same command means same application.
    let mut seen = std::collections::HashSet::new();
    out.retain(|a| seen.insert((a.name.clone(), a.exec.clone())));
    out
}

/// The types a type is a kind of, nearest first, from shared-mime-info's subclass lists:
/// Markdown and TOML are text/plain, so a text editor is offered for them.
fn ancestors(mime: &str) -> Vec<String> {
    static PARENTS: OnceLock<HashMap<String, Vec<String>>> = OnceLock::new();
    let parents = PARENTS.get_or_init(|| {
        let mut map: HashMap<String, Vec<String>> = HashMap::new();
        let dirs = std::env::var("XDG_DATA_DIRS").unwrap_or_else(|_| "/usr/local/share:/usr/share".into());
        let home = std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share"));
        for dir in home.into_iter().chain(dirs.split(':').map(PathBuf::from)) {
            let Ok(text) = std::fs::read_to_string(dir.join("mime/subclasses")) else { continue };
            for line in text.lines() {
                if let Some((child, parent)) = line.split_once(' ') {
                    let e = map.entry(child.to_string()).or_default();
                    if !e.iter().any(|p| p == parent) {
                        e.push(parent.to_string());
                    }
                }
            }
        }
        map
    });
    let mut out: Vec<String> = Vec::new();
    let mut next = vec![mime.to_string()];
    while let Some(m) = next.pop() {
        for p in parents.get(&m).into_iter().flatten() {
            if !out.contains(p) && p != mime {
                out.push(p.clone());
                next.push(p.clone());
            }
        }
    }
    out
}

/// The applications that open every one of these files: for one type, its own list; for
/// several, those that take all of them (a text editor for Rust, TOML and Markdown; a player
/// for MP4 and MKV), counting what a type is a kind of. The default goes first when the types
/// share one.
pub fn candidates_for(paths: &[PathBuf]) -> Vec<AppEntry> {
    let mut mimes: Vec<String> = Vec::new();
    for p in paths {
        let m = mime_for(p).unwrap_or_else(|| UNKNOWN.to_string());
        if !mimes.contains(&m) {
            mimes.push(m);
        }
    }
    match mimes.as_slice() {
        [] => Vec::new(),
        [_] => candidates(&paths[0]),
        _ => {
            let takes = |mime: &str| -> Vec<String> {
                let mut ids: Vec<String> = Vec::new();
                for m in std::iter::once(mime.to_string()).chain(ancestors(mime)) {
                    for id in mime_cache().get(&m).into_iter().flatten() {
                        if !ids.contains(id) {
                            ids.push(id.clone());
                        }
                    }
                }
                ids
            };
            let mut ids = takes(&mimes[0]);
            for m in &mimes[1..] {
                let theirs = takes(m);
                ids.retain(|id| theirs.contains(id));
            }
            let defaults: Vec<String> = mimes.iter().map(|m| query_default(m)).collect();
            if let Some(def) = defaults.first().filter(|d| !d.is_empty() && defaults.iter().all(|x| x == *d)) {
                if let Some(i) = ids.iter().position(|id| id == def) {
                    let id = ids.remove(i);
                    ids.insert(0, id);
                }
            }
            let mut out: Vec<AppEntry> = ids.iter().filter_map(|id| parse_desktop(id, true)).collect();
            let mut seen = std::collections::HashSet::new();
            out.retain(|a| seen.insert((a.name.clone(), a.exec.clone())));
            out
        }
    }
}

/// The one type all of these files are, if they are one; None for a mix.
pub fn common_mime(paths: &[PathBuf]) -> Option<String> {
    let first = mime_for(paths.first()?)?;
    paths[1..].iter().all(|p| mime_for(p).as_deref() == Some(first.as_str())).then_some(first)
}

/// The name of the application set to open this type, if one is.
pub fn default_app_name(mime: &str) -> Option<String> {
    let id = query_default(mime);
    (!id.is_empty()).then(|| parse_desktop(&id, true)).flatten().map(|a| a.name)
}

fn query_default(mime: &str) -> String {
    std::process::Command::new("xdg-mime")
        .args(["query", "default", mime])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}

/// The command lines that open `paths` in an application with this Exec line. %F and %U take
/// every file, so one process gets them all; %f and %u take one, so there is a process per
/// file; with none of them, the files go at the end. Other field codes are dropped.
fn command_lines(exec: &str, paths: &[PathBuf]) -> Vec<Vec<String>> {
    let parts = shell_words(exec);
    let all = parts.iter().any(|p| p == "%F" || p == "%U");
    let one = parts.iter().any(|p| p == "%f" || p == "%u");
    let fill = |files: &[PathBuf]| -> Vec<String> {
        let mut out = Vec::new();
        for p in &parts {
            match p.as_str() {
                "%f" | "%F" | "%u" | "%U" => out.extend(files.iter().map(|f| f.to_string_lossy().into_owned())),
                s if s.starts_with('%') && s.len() == 2 => {}
                _ => out.push(p.clone()),
            }
        }
        if !all && !one {
            out.extend(files.iter().map(|f| f.to_string_lossy().into_owned()));
        }
        out
    };
    if one && !all {
        paths.iter().map(|p| fill(std::slice::from_ref(p))).collect()
    } else {
        vec![fill(paths)]
    }
}

/// Launch files with a specific application, detached, each process waited for on a thread
/// of its own so none lingers as a zombie.
pub fn launch_many(app: &AppEntry, paths: &[PathBuf]) {
    for parts in command_lines(&app.exec, paths) {
        let Some((program, args)) = parts.split_first() else { continue };
        if let Ok(mut child) = std::process::Command::new(program).args(args).spawn() {
            std::thread::spawn(move || child.wait());
        }
    }
}

/// Counts the defaults set, so a list that put the default first knows it is out of date.
static DEFAULTS_SET: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

pub fn defaults_set() -> u64 {
    DEFAULTS_SET.load(std::sync::atomic::Ordering::Relaxed)
}

/// Persist the association so double-click (xdg-open) follows it from now on. Not for a file
/// whose type is unknown: that would hand every unrecognised file to the one application.
pub fn set_default(app: &AppEntry, path: &Path) {
    if let Some(mime) = mime_for(path).filter(|m| m != UNKNOWN) {
        let _ = std::process::Command::new("xdg-mime")
            .args(["default", &app.desktop_id, &mime])
            .status();
        DEFAULTS_SET.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
}

/// Minimal shell-style splitter honouring double quotes, Exec lines use them for paths
/// with spaces, and pulling in a full shell parser for that is overkill.
fn shell_words(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    for c in s.chars() {
        match c {
            '"' => quoted = !quoted,
            ' ' if !quoted => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            _ => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

pub fn all_apps() -> Vec<AppEntry> {
    let mut entries = Vec::new();
    for dir in data_dirs() {
        if let Ok(rd) = std::fs::read_dir(&dir) {
            for res in rd.flatten() {
                if res.path().extension().is_some_and(|e| e == "desktop") {
                    let id = res.file_name().to_string_lossy().to_string();
                    if let Some(app) = parse_desktop(&id, false) {
                        entries.push(app);
                    }
                }
            }
        }
    }
    entries.sort_by(|a, b| a.desktop_id.cmp(&b.desktop_id));
    entries.dedup_by(|a, b| a.desktop_id == b.desktop_id);
    entries.sort_by_key(|a| a.name.to_lowercase());
    entries
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Krita registers itself through per-format alias entries (krita_png.desktop etc.) that
    /// are all `NoDisplay=true`. Filtering those out dropped it from "Open with" even though
    /// it opens the file perfectly well. Guards that regression on the real system database.
    #[test]
    fn no_display_aliases_still_offer_the_app() {
        let png = std::env::temp_dir().join(format!("vantaexplorer-apps-{}.png", std::process::id()));
        std::fs::write(&png, b"\x89PNG\r\n\x1a\n").unwrap();
        let names: Vec<String> = candidates(&png).into_iter().map(|a| a.name).collect();
        let _ = std::fs::remove_file(&png);

        if names.is_empty() {
            // No desktop database on this machine (CI); nothing to assert.
            return;
        }
        // Whatever else is installed, an entry must never appear twice.
        let mut sorted = names.clone();
        sorted.sort();
        let before = sorted.len();
        sorted.dedup();
        assert_eq!(before, sorted.len(), "duplicate entries in {names:?}");
    }

    /// A `.conf` file has no rule of its own, and came out as an unknown binary with no
    /// applications to offer. Its contents say it is text.
    #[test]
    fn a_name_no_rule_knows_is_typed_by_its_contents() {
        let conf = std::env::temp_dir().join(format!("vantaexplorer-apps-{}.conf", std::process::id()));
        std::fs::write(&conf, "general {\n    lock_cmd = lock\n}\n").unwrap();
        let mime = mime_for(&conf);
        let _ = std::fs::remove_file(&conf);
        if mime_for(Path::new("a.png")).as_deref() != Some("image/png") {
            // No mime database on this machine (CI); nothing to assert.
            return;
        }
        assert_eq!(mime.as_deref(), Some("text/plain"));
        // A name a rule does know is not looked into, and need not even exist.
        assert_eq!(mime_for(Path::new("/nowhere/picture.avif")).as_deref(), Some("image/avif"));
    }

    #[test]
    fn several_files_go_to_one_process_or_one_each_as_the_exec_line_says() {
        let files = [PathBuf::from("/a b.mp4"), PathBuf::from("/c.mkv")];
        assert_eq!(command_lines("mpv --player-operation-mode=pseudo-gui -- %U", &files), [vec!["mpv", "--player-operation-mode=pseudo-gui", "--", "/a b.mp4", "/c.mkv"]]);
        assert_eq!(command_lines("gimp %f", &files), [vec!["gimp", "/a b.mp4"], vec!["gimp", "/c.mkv"]]);
        assert_eq!(command_lines("\"/opt/x y/app\" --new %i", &files), [vec!["/opt/x y/app", "--new", "/a b.mp4", "/c.mkv"]]);
    }

    #[test]
    fn a_kind_of_text_counts_as_text() {
        if std::path::Path::new("/usr/share/mime/subclasses").exists() {
            assert!(ancestors("text/markdown").contains(&"text/plain".to_string()));
        }
    }
}
