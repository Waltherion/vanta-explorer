//! Icon pipeline: freedesktop lookup -> resvg rasterization at PHYSICAL pixel size (or, for a
//! theme drawn in PNG, the bitmap scaled to it) -> egui texture cache.
//!
//! Two load-bearing rules:
//! - Always rasterize at `logical_size * pixels_per_point` (fractional scales such as 1.25
//!   included; scaling a bitmap afterwards is what makes icons soft). Cache key includes the px size.
//! - Resolution consults an override layer BEFORE the MIME lookup, that is the hook the
//!   per-drive icon picker and any future per-item icons plug into.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

/// Explorer's own icons: the folders, file types and drives of Breeze Dark, installed with it as
/// an icon theme of this name. Its folders are painted in the folder color (`paint_accent`).
pub const OWN_THEME: &str = "vantaexplorer";

/// The entry in the `iconThemes` setting that stands for the desktop's own icon theme.
const DESKTOP: &str = "desktop";

/// The themes to look in, from the `iconThemes` setting, and whether one of them is the
/// desktop's (which can change while Explorer runs). Empty means Explorer's own icons, with the
/// desktop's theme behind them for anything they do not have. Without its own icons installed
/// (run from a build tree), that leaves the desktop's.
fn resolve(setting: &[String]) -> (Vec<String>, bool) {
    let default = [OWN_THEME.to_string(), DESKTOP.to_string()];
    let names = if setting.is_empty() { &default[..] } else { setting };
    let mut out: Vec<String> = Vec::new();
    for name in names {
        let expanded = if name == DESKTOP { desktop_themes() } else { vec![name.clone()] };
        for t in expanded {
            if !out.contains(&t) {
                out.push(t);
            }
        }
    }
    (out, names.iter().any(|n| n == DESKTOP))
}

/// The desktop's own theme, then the common themes that are installed, then hicolor. The common
/// ones are there for an icon the desktop's theme does not have, so a theme with gaps never
/// leaves a file without an icon.
fn desktop_themes() -> Vec<String> {
    let mut out = vec![vanta_config::icon_theme::desktop()];
    for t in vanta_config::icon_theme::fallbacks().into_iter().chain(["hicolor".to_string()]) {
        if !out.contains(&t) {
            out.push(t);
        }
    }
    out
}

/// How often the desktop's icon theme is asked for again, when the settings leave it to the
/// desktop. A desktop may change it a moment after the colours (Omarchy does, once the new
/// theme is in place), so the config reload alone would catch the old one.
const DESKTOP_THEME_CHECK: Duration = Duration::from_secs(2);

pub struct IconCache {
    /// Keyed by (icon name, pixel size, size looked up). The last belongs in the key: the same
    /// name at the same size resolves to DIFFERENT files depending on it.
    textures: HashMap<(String, u32, Option<u16>), Option<egui::TextureHandle>>,
    mime_db: xdg_mime::SharedMimeInfo,
    /// Per-item icon-name overrides, keyed by canonical path. Consulted before MIME.
    overrides: HashMap<PathBuf, String>,
    /// Icon themes to look in, best first, from the settings.
    themes: Vec<String>,
    /// Newest mtime seen across the watched icon files.
    theme_stamp: Option<SystemTime>,
    last_check: Instant,
    /// The settings include the desktop's theme, which is asked for again now and then.
    follows_desktop: bool,
    last_desktop_check: Instant,
    /// The `iconThemes` setting as written, to resolve again when the desktop's theme changes.
    setting: Vec<String>,
    /// What the folders in Explorer's own icons are painted, as "#rrggbb".
    folder_color: String,
    /// Folders in the file view drawn as the small outline folder, scaled up (`OUTLINE_SIZE`).
    outline_folders: bool,
}

impl IconCache {
    pub fn new(setting: Vec<String>, folder_color: [u8; 3]) -> Self {
        let (themes, follows_desktop) = resolve(&setting);
        Self {
            textures: HashMap::new(),
            mime_db: xdg_mime::SharedMimeInfo::new(),
            overrides: HashMap::new(),
            theme_stamp: icon_theme_stamp(&themes),
            themes,
            last_check: Instant::now(),
            follows_desktop,
            last_desktop_check: Instant::now(),
            setting,
            folder_color: hex(folder_color),
            outline_folders: false,
        }
    }

    /// Outline folders in the file view, or the full ones.
    pub fn set_outline_folders(&mut self, on: bool) {
        if on != self.outline_folders {
            self.outline_folders = on;
            self.clear();
        }
    }

    /// A new folder color: what was drawn in the old one is stale.
    pub fn set_folder_color(&mut self, color: [u8; 3]) {
        let color = hex(color);
        if color != self.folder_color {
            self.folder_color = color;
            self.clear();
        }
    }

    /// The settings named other themes: look in those from now on, and forget what was drawn
    /// from the old ones.
    pub fn set_themes(&mut self, setting: Vec<String>) {
        let (themes, follows_desktop) = resolve(&setting);
        self.setting = setting;
        self.follows_desktop = follows_desktop;
        self.last_desktop_check = Instant::now();
        if themes != self.themes {
            self.themes = themes;
            self.theme_stamp = icon_theme_stamp(&self.themes);
            self.clear();
        }
    }

    /// Drop all rasterized icons; next frame re-reads and re-renders the SVGs.
    pub fn clear(&mut self) {
        self.textures.clear();
    }

    /// Detect that the icon theme itself was repainted, and drop the cache when it was.
    ///
    /// This must NOT be driven off the config reload. A desktop that recolours its icons on a
    /// theme switch does it in the background, after the colours themselves have changed.
    /// Invalidating on the config change re-reads the icons before they have been repainted and
    /// caches the outgoing theme's colours, so folder colours end up permanently one theme
    /// behind. Watching the icon files' mtimes removes the race, whenever the repaint finishes.
    ///
    /// Returns true when the cache was dropped, so the caller can force a redraw.
    pub fn poll_theme(&mut self) -> bool {
        if self.last_check.elapsed() < Duration::from_millis(400) {
            return false;
        }
        self.last_check = Instant::now();
        if self.follows_desktop && self.last_desktop_check.elapsed() >= DESKTOP_THEME_CHECK {
            self.last_desktop_check = Instant::now();
            let (themes, _) = resolve(&self.setting);
            if themes != self.themes {
                log::info!("icon themes are now {}; icon cache dropped", themes.join(", "));
                self.themes = themes;
                self.theme_stamp = icon_theme_stamp(&self.themes);
                self.textures.clear();
                return true;
            }
        }
        let now = icon_theme_stamp(&self.themes);
        if now == self.theme_stamp {
            return false;
        }
        self.theme_stamp = now;
        self.textures.clear();
        log::info!("icon theme repainted; icon cache dropped");
        true
    }

    pub fn icon_name_for(&self, path: &Path, is_dir: bool) -> String {
        if let Some(name) = self.overrides.get(path) {
            return name.clone();
        }
        if is_dir {
            // The freedesktop name for a folder, which icon themes (and Dolphin) use.
            return "inode-directory".into();
        }
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy())
            .unwrap_or_default();
        let guess = self.mime_db.get_mime_types_from_file_name(&name);
        let mime = guess.first();
        match mime {
            Some(m) => m.to_string().replace('/', "-"),
            None => "text-x-generic".into(),
        }
    }

    /// Rasterize (or fetch cached) at `logical * ppp` physical pixels.
    /// Sidebar and other fixed-size chrome: resolve at the size actually being drawn, which
    /// is what icon themes are designed for.
    pub fn get(
        &mut self,
        ctx: &egui::Context,
        name: &str,
        logical: f32,
        ppp: f32,
        tint: [u8; 3],
    ) -> Option<egui::TextureId> {
        self.get_sized(ctx, name, logical, ppp, tint, None)
    }

    fn get_sized(
        &mut self,
        ctx: &egui::Context,
        name: &str,
        logical: f32,
        ppp: f32,
        tint: [u8; 3],
        bucket: Option<u16>,
    ) -> Option<egui::TextureId> {
        let px = (logical * ppp).round().max(1.0) as u32;
        let key = (name.to_string(), px, bucket);
        if let Some(cached) = self.textures.get(&key) {
            return cached.as_ref().map(|h| h.id());
        }

        let handle = rasterize(&self.themes, name, px, tint, &self.folder_color, bucket).map(|img| {
            ctx.load_texture(
                format!("icon:{name}:{px}"),
                img,
                egui::TextureOptions::LINEAR,
            )
        });
        let id = handle.as_ref().map(|h| h.id());
        self.textures.insert(key, handle);
        id
    }

    /// Resolve + rasterize with mime fallback chain: exact mime icon, then major-generic,
    /// then text-x-generic.
    pub fn get_for_entry(
        &mut self,
        ctx: &egui::Context,
        path: &Path,
        is_dir: bool,
        logical: f32,
        ppp: f32,
        tint: [u8; 3],
    ) -> Option<egui::TextureId> {
        let name = self.icon_name_for(path, is_dir);
        let bucket = Some(if is_dir && self.outline_folders { OUTLINE_SIZE } else { LOOKUP_SIZE });
        if let Some(id) = self.get_sized(ctx, &name, logical, ppp, tint, bucket) {
            return Some(id);
        }
        // Some themes name a folder only `folder`, not `inode-directory`.
        if name == "inode-directory" {
            if let Some(id) = self.get_sized(ctx, "folder", logical, ppp, tint, bucket) {
                return Some(id);
            }
        }
        if let Some(major) = name.split('-').next() {
            let generic = format!("{major}-x-generic");
            if let Some(id) = self.get_sized(ctx, &generic, logical, ppp, tint, bucket) {
                return Some(id);
            }
        }
        self.get_sized(ctx, "text-x-generic", logical, ppp, tint, bucket)
    }
}

/// Representative files from EVERY icon category we look up.
///
/// A repaint can be staged across categories and land over about a second (folders first, say,
/// and drives a second later). Watching only the first group left drive icons
/// one theme behind even after the folders were right. Because the stamp is the newest time across
/// every category, a staged repaint simply invalidates twice, and each pass re-reads what is
/// current.
fn icon_theme_stamp(themes: &[String]) -> Option<SystemTime> {
    let home = std::env::var_os("HOME").map(PathBuf::from)?;
    let candidates = [
        "mimetypes/22/inode-directory.svg",
        "mimetypes/64/inode-directory.svg",
        "places/22/folder.svg",
        "places/64/folder.svg",
        "devices/22/drive-harddisk.svg",
        "devices/64/drive-harddisk.svg",
        "devices/22/drive-removable-media-usb.svg",
        "actions/22/document-open.svg",
        // rewritten once a sweep over the categories finishes
        "index.theme",
    ];
    // Only the first theme is watched: it is the one a desktop repaints, and the ones behind it
    // are fallbacks that do not change.
    let base = home.join(".local/share/icons").join(themes.first()?);
    candidates
        .iter()
        .filter_map(|rel| std::fs::metadata(base.join(rel)).ok()?.modified().ok())
        .max()
}

fn rasterize(themes: &[String], name: &str, px: u32, tint: [u8; 3], folder: &str, bucket: Option<u16>) -> Option<egui::ColorImage> {
    let path = lookup(themes, name, px, bucket)?;
    if is_png(&path) {
        return bitmap(themes, name, &path, px);
    }
    let svg = std::fs::read_to_string(&path).ok()?;
    let svg = if from_own_theme(&path) { paint_own(&svg, &path, folder, &on_folder(tint, folder)) } else { svg };
    // usvg does not resolve CSS currentColor, and breeze icons rely on it, so the theme's text
    // color is substituted in before the SVG is parsed.
    let svg = svg.replace("currentColor", &hex(tint));

    let opts = resvg::usvg::Options {
        resources_dir: path.parent().map(|p| p.to_path_buf()),
        ..Default::default()
    };
    let tree = resvg::usvg::Tree::from_str(&svg, &opts).ok()?;
    let mut pixmap = resvg::tiny_skia::Pixmap::new(px, px)?;
    let size = tree.size();
    let scale = (px as f32 / size.width()).min(px as f32 / size.height());
    resvg::render(
        &tree,
        resvg::tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );
    Some(egui::ColorImage::from_rgba_premultiplied(
        [px as usize, px as usize],
        pixmap.data(),
    ))
}

/// Icon themes ship genuinely DIFFERENT artwork per size bucket, Breeze's 22px folder is
/// not just a scaled 64px folder. Resolving at the requested pixel size therefore made the
/// grid change folder art as it zoomed: at 46px the lookup landed in a different bucket than
/// at every other zoom step, so folders visibly changed shape mid-zoom.
///
/// Resolve at one canonical size instead, so every zoom level shows the same icon. `px` is
/// still what we rasterize the SVG at (and is part of the cache key), so this costs no
/// sharpness, it only fixes WHICH file is chosen.
///
/// The bucket is 96 deliberately: a breeze icon theme ships 16/22/24/32/48/64/96 and NO
/// scalable, and each bucket is separate artwork (different checksums). 64 is the odd one out
/// visually, and it is what 46 logical pixels resolved to. 96 is the largest and matches the
/// larger zoom steps.
const LOOKUP_SIZE: u16 = 96;

/// The size folders are looked up at with `outlineFolders`: Breeze's small folder, an outline in
/// one color (the folder color, in Explorer's own icons), drawn at whatever size is shown.
const OUTLINE_SIZE: u16 = 24;

/// `bucket` pins the size looked up for the FILE VIEW, where an icon must not change shape as
/// the grid zooms. Chrome drawn at a fixed size (the sidebar) passes None and gets the
/// size-appropriate artwork the theme intends, pinning it too made sidebar folders change.
fn lookup(themes: &[String], name: &str, px: u32, bucket: Option<u16>) -> Option<PathBuf> {
    let want = bucket.unwrap_or(px.min(u16::MAX as u32) as u16);
    for theme in themes {
        let found = freedesktop_icons::lookup(name)
            .with_theme(theme)
            .with_size(want)
            .find();
        if let Some(p) = found {
            if p.extension().is_some_and(|e| e == "svg") || is_png(&p) {
                return Some(p);
            }
        }
    }
    None
}

fn hex(c: [u8; 3]) -> String {
    format!("#{:02x}{:02x}{:02x}", c[0], c[1], c[2])
}

/// Whether an icon file is one of Explorer's own, wherever the theme is installed.
fn from_own_theme(path: &Path) -> bool {
    path.components().collect::<Vec<_>>().windows(2).any(|w| w[0].as_os_str() == "icons" && w[1].as_os_str() == OWN_THEME)
}

/// The white Breeze Dark draws the symbol on a colored folder in (Documents, Music, ...).
const ON_FOLDER: &str = "#fcfcfc";

/// What the symbol on a colored folder is drawn in. Breeze Dark draws it white and Breeze Light
/// in its dark text color, so a dark palette keeps the white, and a light one takes its text
/// color unless the folder is too dark for that to read on it.
fn on_folder(tint: [u8; 3], folder: &str) -> String {
    use vanta_ui::theme::{color32, contrast, is_light};
    let text = egui::Color32::from_rgb(tint[0], tint[1], tint[2]);
    let white = egui::Color32::from_rgb(0xfc, 0xfc, 0xfc);
    match vanta_config::palette::parse_color(folder).map(color32) {
        Some(body) if is_light(text) && contrast(text, body) >= contrast(white, body) => hex(tint),
        _ => ON_FOLDER.into(),
    }
}

/// Explorer's own icons in the folder color. Breeze marks the colored part of an icon (the body
/// of a folder) with the class `ColorScheme-Accent` and fills it with `currentColor`, which KDE
/// replaces with the desktop's accent. The same is done here with `folder`, element by element,
/// and the symbol on such a folder is drawn in `symbol` (see `on_folder`). The small one-color
/// folders (the sidebar's) have no colored part: they are drawn in the folder color as a whole.
/// Anything else keeps `currentColor`, which becomes the text color, as in any other theme.
fn paint_own(svg: &str, path: &Path, folder: &str, symbol: &str) -> String {
    if svg.contains("ColorScheme-Accent") {
        return paint_tags(svg, |tag| {
            if tag.contains("ColorScheme-Accent") {
                Some(folder)
            } else if tag.contains("ColorScheme-Text") {
                Some(symbol)
            } else {
                None
            }
        });
    }
    // The file the name links to: file types such as inode-directory are links into places.
    let place = std::fs::canonicalize(path).is_ok_and(|p| p.parent().and_then(Path::parent).is_some_and(|c| c.ends_with("places")));
    if place { svg.replace("currentColor", folder) } else { svg.to_string() }
}

/// `currentColor` replaced, element by element, with what `pick` gives for the element's tag.
fn paint_tags<'a>(svg: &str, pick: impl Fn(&str) -> Option<&'a str>) -> String {
    let mut out = String::with_capacity(svg.len());
    let mut rest = svg;
    while let Some(start) = rest.find('<') {
        out.push_str(&rest[..start]);
        let end = rest[start..].find('>').map_or(rest.len(), |e| start + e + 1);
        let tag = &rest[start..end];
        match pick(tag) {
            Some(color) => out.push_str(&tag.replace("currentColor", color)),
            None => out.push_str(tag),
        }
        rest = &rest[end..];
    }
    out.push_str(rest);
    out
}

fn is_png(p: &Path) -> bool {
    p.extension().is_some_and(|e| e.eq_ignore_ascii_case("png"))
}

/// A theme drawn in PNG rather than SVG (Yaru, the one Omarchy uses): the pixels as they are,
/// scaled to `px`. A bitmap cannot take the theme's text colour the way an SVG does; a theme
/// like that comes in the colours it was made in, and a desktop that wants other colours picks
/// another variant of it, which `poll_theme` notices.
///
/// Such themes ship a few fixed sizes (Yaru: 16, 24, 32, 48 and 256), and the lookup may land
/// on one smaller than wanted. Scaling a 48 up to a 128-pixel grid tile is what makes icons
/// soft, so a larger one is looked for first and scaled down instead.
fn bitmap(themes: &[String], name: &str, path: &Path, px: u32) -> Option<egui::ColorImage> {
    let mut img = image::open(path).ok()?.to_rgba8();
    if img.width().max(img.height()) < px {
        let larger = lookup(themes, name, 256, None)
            .filter(|p| is_png(p))
            .and_then(|p| image::open(p).ok())
            .map(|i| i.to_rgba8())
            .filter(|i| i.width().max(i.height()) > img.width().max(img.height()));
        if let Some(l) = larger {
            img = l;
        }
    }
    Some(fit(img, px))
}

/// Scaled so the longer side is `px`, kept in proportion and centred on a square of `px`.
fn fit(img: image::RgbaImage, px: u32) -> egui::ColorImage {
    let (w, h) = img.dimensions();
    let s = px as f32 / w.max(h).max(1) as f32;
    let (nw, nh) = (((w as f32 * s).round() as u32).clamp(1, px), ((h as f32 * s).round() as u32).clamp(1, px));
    let img = if (nw, nh) == (w, h) { img } else { image::imageops::resize(&img, nw, nh, image::imageops::FilterType::Lanczos3) };
    let mut square = image::RgbaImage::new(px, px);
    image::imageops::overlay(&mut square, &img, ((px - nw) / 2) as i64, ((px - nh) / 2) as i64);
    egui::ColorImage::from_rgba_unmultiplied([px as usize, px as usize], square.as_raw())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(path: &Path, size: u32, rgba: [u8; 4]) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        image::RgbaImage::from_pixel(size, size, image::Rgba(rgba)).save(path).unwrap();
    }

    /// A theme drawn only in PNG, at a small and a large fixed size, the way Yaru is. The icon
    /// is found at all (it used to be skipped for not being an SVG), comes out at the size asked
    /// for, and a large tile is cut from the large artwork rather than blown up from the small.
    #[test]
    fn a_png_theme_gives_icons_and_the_larger_artwork_for_a_large_tile() {
        let data = std::env::temp_dir().join(format!("vantaexplorer-pngtheme-{}", std::process::id()));
        let theme = data.join("icons/PngOnly");
        std::fs::create_dir_all(&theme).unwrap();
        std::fs::write(
            theme.join("index.theme"),
            "[Icon Theme]\nName=PngOnly\nDirectories=22x22/places,48x48/places,256x256/places\n\n\
             [22x22/places]\nSize=22\nType=Fixed\n\n\
             [48x48/places]\nSize=48\nType=Fixed\n\n[256x256/places]\nSize=256\nType=Fixed\n",
        )
        .unwrap();
        png(&theme.join("22x22/places/folder.png"), 22, [0, 255, 0, 255]);
        png(&theme.join("48x48/places/folder.png"), 48, [255, 0, 0, 255]);
        png(&theme.join("256x256/places/folder.png"), 256, [0, 0, 255, 255]);
        // Read once, by the first lookup in this process; nothing else in these tests looks icons up.
        std::env::set_var("XDG_DATA_HOME", &data);

        let themes = vec!["PngOnly".to_string()];
        let small = rasterize(&themes, "folder", 24, [255, 255, 255], "#ffffff", None).expect("a PNG icon is used");
        assert_eq!(small.size, [24, 24]);
        let large = rasterize(&themes, "folder", 128, [255, 255, 255], "#ffffff", None).expect("a PNG icon is used");
        assert_eq!(large.size, [128, 128]);
        let mid = large.pixels[64 * 128 + 64];
        assert_eq!((mid.r(), mid.b()), (0, 255), "the 256 artwork scaled down, not the 48 scaled up");
        // The file view: the full folder, or with outlineFolders the small one at any size.
        let file = |bucket| lookup(&themes, "folder", 128, Some(bucket)).unwrap();
        assert!(file(LOOKUP_SIZE).ends_with("48x48/places/folder.png"));
        assert!(file(OUTLINE_SIZE).ends_with("22x22/places/folder.png"));
        let _ = std::fs::remove_dir_all(&data);
    }

    #[test]
    fn a_bitmap_that_is_not_square_is_centred_on_a_square() {
        let img = image::RgbaImage::from_pixel(40, 20, image::Rgba([0, 255, 0, 255]));
        let out = fit(img, 32);
        assert_eq!(out.size, [32, 32]);
        assert_eq!(out.pixels[0].a(), 0, "above the picture stays clear");
        assert_eq!(out.pixels[16 * 32 + 16].g(), 255, "the picture sits in the middle");
    }

    /// No setting: Explorer's own icons first, the desktop's behind them, and the desktop's is
    /// followed. A list of names is used as written; "desktop" in it stands for the desktop's.
    #[test]
    fn the_setting_resolves_to_own_icons_then_the_desktops() {
        let (themes, follows) = resolve(&[]);
        assert_eq!(themes[0], OWN_THEME);
        assert!(themes.len() > 1 && themes.last().unwrap() == "hicolor");
        assert!(follows);

        let mine = vec!["breeze-dark-walther".to_string(), "hicolor".to_string()];
        assert_eq!(resolve(&mine), (mine.clone(), false));

        let (themes, follows) = resolve(&["desktop".to_string()]);
        assert!(!themes.contains(&OWN_THEME.to_string()));
        assert!(follows);
    }

    /// A Breeze folder: the body takes the folder color, the symbol on it stays white, and a
    /// part without a class is left alone.
    #[test]
    fn a_folder_body_is_painted_and_its_symbol_stays_white() {
        let svg = r#"<svg><path style="fill:currentColor" class="ColorScheme-Accent"/><path style="fill-opacity:0.33"/><path class="ColorScheme-Text" style="fill:currentColor"/></svg>"#;
        let out = paint_own(svg, Path::new("/nowhere/folder.svg"), "#e68e0d", ON_FOLDER);
        assert_eq!(
            out,
            r##"<svg><path style="fill:#e68e0d" class="ColorScheme-Accent"/><path style="fill-opacity:0.33"/><path class="ColorScheme-Text" style="fill:#fcfcfc"/></svg>"##
        );
    }

    /// A small one-color folder (the sidebar's) is drawn in the folder color as a whole, also
    /// when it is reached through a file type that links to it; a file type of its own is not.
    #[test]
    fn small_folders_take_the_folder_color_and_file_types_do_not() {
        let dir = std::env::temp_dir().join(format!("vx-own-{}", std::process::id()));
        let theme = dir.join("icons").join(OWN_THEME);
        let svg = r#"<svg><path class="ColorScheme-Text" style="fill:currentColor"/></svg>"#;
        for rel in ["places/22/folder.svg", "mimetypes/22/text-plain.svg"] {
            std::fs::create_dir_all(theme.join(rel).parent().unwrap()).unwrap();
            std::fs::write(theme.join(rel), svg).unwrap();
        }
        let link = theme.join("mimetypes/22/inode-directory.svg");
        let _ = std::fs::remove_file(&link);
        std::os::unix::fs::symlink("../../places/22/folder.svg", &link).unwrap();

        assert!(from_own_theme(&link));
        assert!(!from_own_theme(Path::new("/usr/share/icons/breeze-dark/places/22/folder.svg")));
        assert!(paint_own(svg, &theme.join("places/22/folder.svg"), "#7aa2f7", ON_FOLDER).contains("#7aa2f7"));
        assert!(paint_own(svg, &link, "#7aa2f7", ON_FOLDER).contains("#7aa2f7"));
        assert_eq!(paint_own(svg, &theme.join("mimetypes/22/text-plain.svg"), "#7aa2f7", ON_FOLDER), svg);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The symbol on a colored folder: white on any dark palette, Breeze Dark's included, and
    /// the text color on a light one, as Breeze Light draws it, unless the folder is too dark.
    #[test]
    fn the_symbol_on_a_folder_follows_the_palette() {
        assert_eq!(on_folder([0xfc, 0xfc, 0xfc], "#3daee9"), ON_FOLDER);
        assert_eq!(on_folder([0xbd, 0x97, 0xf7], "#bd97f7"), ON_FOLDER);
        assert_eq!(on_folder([0x23, 0x26, 0x29], "#3daee9"), "#232629");
        assert_eq!(on_folder([0x23, 0x26, 0x29], "#1d2a3a"), ON_FOLDER);
    }
}
