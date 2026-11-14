//! The files a vanta app reads its settings from, and how they are laid over each other.
//!
//! Every app keeps its files in one folder, `~/.config/vanta/`, and reads them in the same order,
//! each later file winning key by key over the ones before it:
//!
//! 1. the app's built-in defaults (in its own code);
//! 2. the active theme's palette and its file for the app (see `crate::theme`), when a theme is
//!    in use: the theme is where everything starts from, and anything of the user's own wins;
//! 3. `colors.jsonc`, the palette every app shares: only its shared sections (`color`, `font`,
//!    `rainbow`, `shape`) are taken, so colours are written once and not once per app (the same
//!    goes for a theme's palette);
//! 4. `<app>.jsonc`, the app's own look and behaviour;
//! 5. `<app>.local.jsonc`, what belongs to the machine rather than the look: commands, screens,
//!    folders.
//!
//! Next to them, `display.jsonc` holds what every app shares about the screens (where SDR white
//! sits on an HDR screen).

use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::{paths, theme};

/// The sections of `colors.jsonc` an app takes. Anything else in that file is ignored, so a
/// palette cannot change an app's behaviour.
pub const SHARED: [&str; 4] = ["color", "font", "rainbow", "shape"];

/// `~/.config/vanta`, or `$VANTA_CONFIG_DIR` (a build under test with a folder of its own).
pub fn dir() -> PathBuf {
    match std::env::var_os("VANTA_CONFIG_DIR") {
        Some(d) if !d.is_empty() => PathBuf::from(d),
        _ => paths::config_home().join("vanta"),
    }
}

/// The files of one app, `app` being its short name (`bar`, `lock`, `paper`, `organizer`,
/// `explorer`, `viewer`).
pub struct Files {
    pub colors: PathBuf,
    /// `$VANTA<APP>_CONFIG` replaces this one (`VANTABAR_CONFIG` and so on).
    pub app: PathBuf,
    pub local: PathBuf,
    pub display: PathBuf,
    /// The theme under them, when one is in use.
    pub theme: Option<ThemeFiles>,
}

/// What a theme holds for one app.
pub struct ThemeFiles {
    pub dir: PathBuf,
    pub colors: Option<PathBuf>,
    pub app: Option<PathBuf>,
    /// Every name either file could have, so one that appears is noticed.
    pub watch: Vec<PathBuf>,
}

impl ThemeFiles {
    pub fn of(dir: &Path, app: &str) -> ThemeFiles {
        let mut watch = theme::candidates(dir, "colors");
        watch.extend(theme::candidates(dir, app));
        ThemeFiles { dir: dir.to_path_buf(), colors: theme::file(dir, "colors"), app: theme::file(dir, app), watch }
    }
}

/// An app's files with the theme in use under them.
pub fn files(app: &str) -> Files {
    files_for(app, theme::active().as_deref())
}

/// An app's files with `theme` under them instead of the one in use: how a theme looks with the
/// user's own settings on top, before switching to it.
pub fn files_for(app: &str, theme: Option<&Path>) -> Files {
    let d = dir();
    let app_file = match std::env::var_os(override_var(app)) {
        Some(p) if !p.is_empty() => PathBuf::from(p),
        _ => d.join(format!("{app}.jsonc")),
    };
    Files {
        colors: d.join("colors.jsonc"),
        app: app_file,
        local: d.join(format!("{app}.local.jsonc")),
        display: d.join("display.jsonc"),
        theme: theme.map(|t| ThemeFiles::of(t, app)),
    }
}

/// A watch on an app's files that also follows the theme: switching theme changes which files
/// are looked at, and that counts as a change.
pub fn stamp(app: &str) -> crate::watch::Stamp {
    let app = app.to_string();
    crate::watch::Stamp::following(move || files(&app).watched())
}

fn override_var(app: &str) -> String {
    format!("VANTA{}_CONFIG", app.to_uppercase())
}

impl Files {
    /// Every file worth watching for a change, the ones that do not exist yet included.
    pub fn watched(&self) -> Vec<PathBuf> {
        let mut w = vec![theme::state_file(), self.colors.clone(), self.app.clone(), self.local.clone(), self.display.clone()];
        if let Some(t) = &self.theme {
            w.extend(t.watch.iter().cloned());
        }
        w
    }
}

/// The layered settings, and what went wrong reading them: a file that does not parse is left
/// out, and said so, rather than taking the others down with it.
pub struct Loaded {
    pub value: Value,
    pub errors: Vec<String>,
}

pub fn load(app: &str) -> Loaded {
    load_files(&files(app))
}

pub fn load_files(f: &Files) -> Loaded {
    let mut value = Value::Object(Default::default());
    let mut errors = Vec::new();
    let mut read = |path: &PathBuf| -> Option<Value> {
        let text = std::fs::read_to_string(path).ok()?;
        match crate::jsonc::parse(&text) {
            Ok(v) if v.is_object() => Some(v),
            Ok(_) => {
                errors.push(format!("{}: not an object, ignored", path.display()));
                None
            }
            Err(e) => {
                errors.push(format!("{}: {e:#}, ignored", path.display()));
                None
            }
        }
    };
    let palette = |value: &mut Value, v: Option<Value>| {
        if let Some(Value::Object(colors)) = v {
            let shared: serde_json::Map<String, Value> = colors.into_iter().filter(|(k, _)| SHARED.contains(&k.as_str())).collect();
            merge(value, Value::Object(shared));
        }
    };
    if let Some(t) = &f.theme {
        palette(&mut value, t.colors.as_ref().and_then(&mut read));
        if let Some(v) = t.app.as_ref().and_then(&mut read) {
            merge(&mut value, v);
        }
    }
    palette(&mut value, read(&f.colors));
    for path in [&f.app, &f.local] {
        if let Some(v) = read(path) {
            merge(&mut value, v);
        }
    }
    Loaded { value, errors }
}

/// Lays `over` onto `base`: objects are merged key by key, anything else is replaced whole (a
/// list of colour stops is one setting, not a list to append to).
pub fn merge(base: &mut Value, over: Value) {
    match (base, over) {
        (Value::Object(b), Value::Object(o)) => {
            for (k, v) in o {
                match b.get_mut(&k) {
                    Some(existing) if existing.is_object() && v.is_object() => merge(existing, v),
                    _ => {
                        b.insert(k, v);
                    }
                }
            }
        }
        (b, o) => *b = o,
    }
}

/// Writes `text` as `name` in the folder if there is nothing there yet, not even a dangling
/// link: the commented file a new user finds on first start. Never touches an existing file.
pub fn seed(name: &str, text: &str) {
    let path = dir().join(name);
    if std::fs::symlink_metadata(&path).is_ok() {
        return;
    }
    let _ = std::fs::create_dir_all(dir());
    let _ = std::fs::write(&path, text);
}

/// Seeds an app's own file and its local file, and the shared `display.jsonc` and
/// `colors.jsonc`, unless the app file is overridden for a test.
pub fn seed_app(app: &str, app_text: &str, local_text: &str) {
    if std::env::var_os(override_var(app)).is_some_and(|v| !v.is_empty()) {
        return;
    }
    seed(&format!("{app}.jsonc"), app_text);
    seed(&format!("{app}.local.jsonc"), local_text);
    seed("display.jsonc", DISPLAY_DEFAULT);
    seed("colors.jsonc", crate::palette::DEFAULT_TEXT);
}

/// Where SDR white sits on an HDR screen for this app: its own `sdrWhite` if its files set one,
/// otherwise the shared one in `display.jsonc`.
pub fn sdr_white_for(root: &Value) -> crate::hypr::SdrWhite {
    match root.get("sdrWhite") {
        Some(v) => crate::hypr::SdrWhite::parse(Some(v)),
        None => sdr_white(),
    }
}

/// Where SDR white sits on an HDR screen, from `display.jsonc` (`"sdrWhite": "desktop"` or a
/// number of nits). The desktop's own level when the file or the key is missing.
pub fn sdr_white() -> crate::hypr::SdrWhite {
    let v = std::fs::read_to_string(files("display").display).ok().and_then(|t| crate::jsonc::parse(&t).ok());
    crate::hypr::SdrWhite::parse(v.as_ref().and_then(|v| v.get("sdrWhite")))
}

/// The seeded `display.jsonc`.
pub const DISPLAY_DEFAULT: &str = r##"// Shared by every Vanta app: how they meet the screens.
//
// Lines starting with // are options left at their default: remove the // to use one.
{
  // Where SDR content (text, panels, SDR pictures) sits on an HDR screen: "desktop" puts it
  // where the compositor puts every other SDR program, a number puts SDR white at that many
  // nits. HDR pictures are always shown as mastered.
  // "sdrWhite": "desktop",
}
"##;

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn files_in(dir: &std::path::Path) -> Files {
        Files { colors: dir.join("colors.jsonc"), app: dir.join("bar.jsonc"), local: dir.join("bar.local.jsonc"), display: dir.join("display.jsonc"), theme: None }
    }

    #[test]
    fn the_theme_lies_under_the_users_own_files() {
        let dir = std::env::temp_dir().join(format!("vanta-layers-theme-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let theme_dir = dir.join("themes/gold");
        std::fs::create_dir_all(&theme_dir).unwrap();
        std::fs::write(theme_dir.join("vanta-colors.jsonc"), r##"{ "color": { "text": "#ffd700", "accent": "#ffd700" }, "commands": { "lock": "evil" } }"##).unwrap();
        std::fs::write(theme_dir.join("vantabar.jsonc"), r##"{ "bloom": 2, "hub": { "buttonShape": "squircle" } }"##).unwrap();
        std::fs::write(dir.join("colors.jsonc"), r##"{ "color": { "accent": "#ffffff" } }"##).unwrap();
        std::fs::write(dir.join("bar.jsonc"), r##"{ "bloom": 0 }"##).unwrap();
        let mut f = files_in(&dir);
        f.theme = Some(ThemeFiles::of(&theme_dir, "bar"));
        let l = load_files(&f);
        assert!(l.errors.is_empty(), "{:?}", l.errors);
        assert_eq!(l.value["color"]["text"], "#ffd700", "the theme's colour where the user sets none");
        assert_eq!(l.value["color"]["accent"], "#ffffff", "the user's own colour wins");
        assert_eq!(l.value["bloom"], 0, "and the user's own setting");
        assert_eq!(l.value["hub"]["buttonShape"], "squircle");
        assert!(l.value.get("commands").is_none(), "a theme's palette cannot set behaviour either");
        let watched = f.watched();
        assert!(watched.contains(&theme_dir.join("bar.jsonc")), "a file the theme could add is watched");
        assert!(watched.contains(&theme::state_file()), "and the theme in use");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn later_files_win_key_by_key_and_the_palette_gives_only_its_sections() {
        let dir = std::env::temp_dir().join(format!("vanta-layers-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("colors.jsonc"), r##"{ "color": { "text": "#fff", "accent": "#f00" }, "font": { "family": "Poppins" }, "commands": { "lock": "evil" } }"##).unwrap();
        std::fs::write(dir.join("bar.jsonc"), "// the theme's\n{ \"color\": { \"accent\": \"#0f0\" }, \"bloom\": 1, }").unwrap();
        std::fs::write(dir.join("bar.local.jsonc"), r#"{ "commands": { "lock": "loginctl lock-session" } }"#).unwrap();
        let l = load_files(&files_in(&dir));
        assert!(l.errors.is_empty(), "{:?}", l.errors);
        assert_eq!(l.value["color"], json!({ "text": "#fff", "accent": "#0f0" }), "the app overrides one colour, keeps the other");
        assert_eq!(l.value["font"]["family"], "Poppins");
        assert_eq!(l.value["bloom"], 1);
        assert_eq!(l.value["commands"]["lock"], "loginctl lock-session", "a palette cannot set behaviour");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_broken_file_is_left_out_and_reported() {
        let dir = std::env::temp_dir().join(format!("vanta-layers-broken-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("bar.jsonc"), "{ \"bloom\": ").unwrap();
        std::fs::write(dir.join("bar.local.jsonc"), r#"{ "sleepLabel": "Hibernate" }"#).unwrap();
        let l = load_files(&files_in(&dir));
        assert_eq!(l.errors.len(), 1);
        assert_eq!(l.value["sleepLabel"], "Hibernate");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn lists_replace_rather_than_append() {
        let mut v = json!({ "rainbow": { "stops": ["a", "b"], "speed": 1 } });
        merge(&mut v, json!({ "rainbow": { "stops": ["c"] } }));
        assert_eq!(v, json!({ "rainbow": { "stops": ["c"], "speed": 1 } }));
    }

    /// `docs/config.md` shows the files this writes on first run word for word; a change here
    /// has to be made there too.
    #[test]
    fn the_reference_shows_what_is_written_on_first_run() {
        let doc = include_str!("../../../docs/config.md");
        assert!(doc.contains(DISPLAY_DEFAULT.trim_end()), "docs/config.md no longer shows DISPLAY_DEFAULT as the code writes it");
    }
}
