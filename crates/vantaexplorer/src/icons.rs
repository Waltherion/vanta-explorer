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

/// Used when the settings name no icon theme: the desktop's own, then the common themes that
/// are installed, then hicolor. The common ones are there for an icon the desktop's theme does
/// not have, so a theme with gaps never leaves a file without an icon.
fn fallback_themes() -> Vec<String> {
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
    /// Keyed by (icon name, pixel size, canonical-lookup). The flag belongs in the key:
    /// the same name at the same size resolves to DIFFERENT files depending on it.
    textures: HashMap<(String, u32, bool), Option<egui::TextureHandle>>,
    mime_db: xdg_mime::SharedMimeInfo,
    /// Per-item icon-name overrides, keyed by canonical path. Consulted before MIME.
    overrides: HashMap<PathBuf, String>,
    /// Icon themes to look in, best first, from the settings.
    themes: Vec<String>,
    /// Newest mtime seen across the watched icon files.
    theme_stamp: Option<SystemTime>,
    last_check: Instant,
    /// The settings name no theme, so `themes` follows the desktop's and is asked for again.
    follows_desktop: bool,
    last_desktop_check: Instant,
}

impl IconCache {
    pub fn new(themes: Vec<String>) -> Self {
        let follows_desktop = themes.is_empty();
        let themes = if follows_desktop { fallback_themes() } else { themes };
        Self {
            textures: HashMap::new(),
            mime_db: xdg_mime::SharedMimeInfo::new(),
            overrides: HashMap::new(),
            theme_stamp: icon_theme_stamp(&themes),
            themes,
            last_check: Instant::now(),
            follows_desktop,
            last_desktop_check: Instant::now(),
        }
    }

    /// The settings named other themes: look in those from now on, and forget what was drawn
    /// from the old ones.
    pub fn set_themes(&mut self, themes: Vec<String>) {
        self.follows_desktop = themes.is_empty();
        self.last_desktop_check = Instant::now();
        let themes = if self.follows_desktop { fallback_themes() } else { themes };
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
            let themes = fallback_themes();
            if themes != self.themes {
                log::info!("desktop icon theme is now {}; icon cache dropped", themes[0]);
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
        self.get_sized(ctx, name, logical, ppp, tint, false)
    }

    fn get_sized(
        &mut self,
        ctx: &egui::Context,
        name: &str,
        logical: f32,
        ppp: f32,
        tint: [u8; 3],
        canonical: bool,
    ) -> Option<egui::TextureId> {
        let px = (logical * ppp).round().max(1.0) as u32;
        let key = (name.to_string(), px, canonical);
        if let Some(cached) = self.textures.get(&key) {
            return cached.as_ref().map(|h| h.id());
        }

        let handle = rasterize(&self.themes, name, px, tint, canonical).map(|img| {
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
        if let Some(id) = self.get_sized(ctx, &name, logical, ppp, tint, true) {
            return Some(id);
        }
        // Some themes name a folder only `folder`, not `inode-directory`.
        if name == "inode-directory" {
            if let Some(id) = self.get_sized(ctx, "folder", logical, ppp, tint, true) {
                return Some(id);
            }
        }
        if let Some(major) = name.split('-').next() {
            let generic = format!("{major}-x-generic");
            if let Some(id) = self.get_sized(ctx, &generic, logical, ppp, tint, true) {
                return Some(id);
            }
        }
        self.get_sized(ctx, "text-x-generic", logical, ppp, tint, true)
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

fn rasterize(themes: &[String], name: &str, px: u32, tint: [u8; 3], canonical: bool) -> Option<egui::ColorImage> {
    let path = lookup(themes, name, px, canonical)?;
    if is_png(&path) {
        return bitmap(themes, name, &path, px);
    }
    let svg = std::fs::read_to_string(&path).ok()?;
    // usvg does not resolve CSS currentColor, and breeze icons rely on it, so the theme's text
    // colour is substituted in before the SVG is parsed.
    let tint_hex = format!("#{:02x}{:02x}{:02x}", tint[0], tint[1], tint[2]);
    let svg = svg.replace("currentColor", &tint_hex);

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

/// `canonical` pins the bucket for the FILE VIEW, where an icon must not change shape as the
/// grid zooms. Chrome drawn at a fixed size (the sidebar) passes false and gets the
/// size-appropriate artwork the theme intends, pinning it too made sidebar folders change.
fn lookup(themes: &[String], name: &str, px: u32, canonical: bool) -> Option<PathBuf> {
    let want = if canonical { LOOKUP_SIZE } else { px.min(u16::MAX as u32) as u16 };
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
        let larger = lookup(themes, name, 256, false)
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
            "[Icon Theme]\nName=PngOnly\nDirectories=48x48/places,256x256/places\n\n\
             [48x48/places]\nSize=48\nType=Fixed\n\n[256x256/places]\nSize=256\nType=Fixed\n",
        )
        .unwrap();
        png(&theme.join("48x48/places/folder.png"), 48, [255, 0, 0, 255]);
        png(&theme.join("256x256/places/folder.png"), 256, [0, 0, 255, 255]);
        // Read once, by the first lookup in this process; nothing else in these tests looks icons up.
        std::env::set_var("XDG_DATA_HOME", &data);

        let themes = vec!["PngOnly".to_string()];
        let small = rasterize(&themes, "folder", 24, [255, 255, 255], false).expect("a PNG icon is used");
        assert_eq!(small.size, [24, 24]);
        let large = rasterize(&themes, "folder", 128, [255, 255, 255], false).expect("a PNG icon is used");
        assert_eq!(large.size, [128, 128]);
        let mid = large.pixels[64 * 128 + 64];
        assert_eq!((mid.r(), mid.b()), (0, 255), "the 256 artwork scaled down, not the 48 scaled up");
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
}
