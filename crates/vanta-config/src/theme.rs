//! Themes: a folder of the same files as `~/.config/vanta/`, laid under the user's own.
//!
//! A theme is any folder holding one or more of the Vanta files (`colors.jsonc`, `bar.jsonc`,
//! `lock.jsonc` and so on, or the same names with a `vanta` in front: `vanta-colors.jsonc`,
//! `vantabar.jsonc`), a `wallpapers` folder, or only pictures, which makes it a theme that changes
//! the wallpaper and nothing else. It needs none of them in particular: what it leaves out comes
//! from the built-in defaults.
//!
//! Which theme is in use is one line in `$XDG_STATE_HOME/vanta/theme`: the folder's full path.
//! Every app reads it, and watches it, so writing a new line there switches them all.
//! `$VANTA_THEME` puts another theme in its place for one program, which is how a theme can be
//! tried without switching to it.

use std::path::{Path, PathBuf};

use crate::paths;

/// Pictures and videos a wallpaper folder may hold, as Vanta Paper plays them.
pub const IMAGE_EXTENSIONS: &[&str] = &["png", "jpg", "jpeg", "avif", "jxl", "heic", "heif", "webp", "tiff", "tif", "bmp"];
pub const VIDEO_EXTENSIONS: &[&str] = &["mp4", "mkv", "webm", "mov", "m4v"];

/// The file that says which theme is in use.
pub fn state_file() -> PathBuf {
    paths::state_home().join("vanta/theme")
}

/// The theme in use: `$VANTA_THEME` when set, otherwise the folder named in the state file.
/// Nothing when neither names a folder that exists, which is the same as having no theme.
pub fn active() -> Option<PathBuf> {
    let named = match std::env::var_os("VANTA_THEME") {
        Some(v) if !v.is_empty() => PathBuf::from(v),
        _ => PathBuf::from(std::fs::read_to_string(state_file()).ok()?.trim()),
    };
    (!named.as_os_str().is_empty() && named.is_dir()).then_some(named)
}

/// Makes `dir` the theme in use. Written whole or not at all, so an app never reads half a path.
pub fn set(dir: &Path) -> std::io::Result<()> {
    let file = state_file();
    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = file.with_extension("tmp");
    std::fs::write(&tmp, format!("{}\n", dir.display()))?;
    std::fs::rename(tmp, file)
}

/// The names a theme may give an app's file: the short one used in `~/.config/vanta`, then the
/// one with `vanta` in front, which is what a theme folder shared with other programs tends to use.
pub fn names(app: &str) -> [String; 2] {
    match app {
        "colors" => ["colors.jsonc".into(), "vanta-colors.jsonc".into()],
        _ => [format!("{app}.jsonc"), format!("vanta{app}.jsonc")],
    }
}

/// A theme's file for `app` (`colors` for the palette), if it has one.
pub fn file(dir: &Path, app: &str) -> Option<PathBuf> {
    names(app).into_iter().map(|n| dir.join(n)).find(|p| p.exists())
}

/// Every file a theme could hold for `app`, found or not: what to watch, so a file that appears
/// in the theme is noticed too.
pub fn candidates(dir: &Path, app: &str) -> Vec<PathBuf> {
    names(app).into_iter().map(|n| dir.join(n)).collect()
}

/// Whether the file is a picture or a video Vanta Paper can show.
pub fn is_wallpaper(p: &Path) -> bool {
    let Some(ext) = p.extension().and_then(|e| e.to_str()) else { return false };
    let ext = ext.to_ascii_lowercase();
    IMAGE_EXTENSIONS.contains(&ext.as_str()) || VIDEO_EXTENSIONS.contains(&ext.as_str())
}

fn holds_wallpapers(dir: &Path) -> bool {
    std::fs::read_dir(dir).is_ok_and(|rd| rd.flatten().any(|e| e.path().is_file() && is_wallpaper(&e.path())))
}

/// A theme's wallpapers: its `wallpapers` folder, or the theme folder itself when the pictures lie
/// there directly.
pub fn wallpapers(dir: &Path) -> Option<PathBuf> {
    let sub = dir.join("wallpapers");
    if sub.is_dir() {
        return Some(sub);
    }
    holds_wallpapers(dir).then(|| dir.to_path_buf())
}

/// Whether a folder counts as a theme: it holds a Vanta file, a `meta.toml`, wallpapers, or both.
pub fn is_theme(dir: &Path) -> bool {
    const APPS: [&str; 7] = ["colors", "bar", "lock", "paper", "organizer", "explorer", "viewer"];
    dir.is_dir() && (dir.join("meta.toml").exists() || APPS.iter().any(|a| file(dir, a).is_some()) || wallpapers(dir).is_some())
}

/// The folder Vanta Paper plays from, given its layered settings (`path`) and the theme they came
/// with: the `path` it is told, else the theme's wallpapers, else the wallpapers a package or the
/// user installed. The bar's picker asks the same question for a theme it shows, so both agree on
/// the paths they pass each other.
pub fn paper_dir(settings: &serde_json::Value, theme: Option<&Path>) -> String {
    match settings.get("path").and_then(|v| v.as_str()).filter(|p| !p.is_empty()) {
        Some(p) => paths::expand(p),
        None => theme.and_then(wallpapers).map(|d| d.display().to_string()).unwrap_or_else(paths::default_wallpapers),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("vanta-theme-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn both_names_are_found_and_the_short_one_first() {
        let d = scratch("names");
        std::fs::write(d.join("vantabar.jsonc"), "{}").unwrap();
        assert_eq!(file(&d, "bar"), Some(d.join("vantabar.jsonc")));
        std::fs::write(d.join("bar.jsonc"), "{}").unwrap();
        assert_eq!(file(&d, "bar"), Some(d.join("bar.jsonc")));
        std::fs::write(d.join("vanta-colors.jsonc"), "{}").unwrap();
        assert_eq!(file(&d, "colors"), Some(d.join("vanta-colors.jsonc")));
        assert_eq!(file(&d, "lock"), None);
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn wallpapers_are_a_subfolder_or_the_folder_itself() {
        let d = scratch("walls");
        assert_eq!(wallpapers(&d), None, "an empty folder has none");
        assert!(!is_theme(&d));
        std::fs::write(d.join("notes.txt"), "").unwrap();
        assert_eq!(wallpapers(&d), None, "nor does one without pictures");
        std::fs::write(d.join("sea.AVIF"), "").unwrap();
        assert_eq!(wallpapers(&d), Some(d.clone()), "pictures lying in the theme itself");
        assert!(is_theme(&d), "a folder of pictures is a theme");
        std::fs::create_dir(d.join("wallpapers")).unwrap();
        assert_eq!(wallpapers(&d), Some(d.join("wallpapers")), "a wallpapers folder wins");
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn the_paper_folder_is_the_setting_then_the_theme_then_the_default() {
        let d = scratch("paper");
        std::fs::create_dir(d.join("wallpapers")).unwrap();
        let none = serde_json::json!({});
        assert_eq!(paper_dir(&none, Some(&d)), d.join("wallpapers").display().to_string());
        assert_eq!(paper_dir(&none, None), paths::default_wallpapers());
        let set = serde_json::json!({ "path": "/srv/walls" });
        assert_eq!(paper_dir(&set, Some(&d)), "/srv/walls", "a path the user or theme gives wins");
        let _ = std::fs::remove_dir_all(d);
    }
}
