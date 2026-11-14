//! Icon pipeline: freedesktop lookup -> resvg rasterization at PHYSICAL pixel size -> egui
//! texture cache.
//!
//! Two load-bearing rules:
//! - Always rasterize at `logical_size * pixels_per_point` (fractional scales such as 1.25
//!   included; scaling a bitmap afterwards is what makes icons soft). Cache key includes the px size.
//! - Resolution consults an override layer BEFORE the MIME lookup, that is the hook the
//!   per-drive icon picker and any future per-item icons plug into.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

/// Used until the settings have been read, and if they name no theme at all.
/// Used when the settings name no icon theme: the desktop's own, then hicolor.
fn fallback_themes() -> Vec<String> {
    let mut out = vec![vanta_config::icon_theme::desktop()];
    if out[0] != "hicolor" {
        out.push("hicolor".into());
    }
    out
}

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
}

impl IconCache {
    pub fn new(themes: Vec<String>) -> Self {
        let themes = if themes.is_empty() { fallback_themes() } else { themes };
        Self {
            textures: HashMap::new(),
            mime_db: xdg_mime::SharedMimeInfo::new(),
            overrides: HashMap::new(),
            theme_stamp: icon_theme_stamp(&themes),
            themes,
            last_check: Instant::now(),
        }
    }

    /// The settings named other themes: look in those from now on, and forget what was drawn
    /// from the old ones.
    pub fn set_themes(&mut self, themes: Vec<String>) {
        let themes = if themes.is_empty() { fallback_themes() } else { themes };
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
            if p.extension().is_some_and(|e| e == "svg") {
                return Some(p);
            }
        }
    }
    None
}
