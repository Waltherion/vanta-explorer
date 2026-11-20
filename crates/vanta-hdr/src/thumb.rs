//! Thumbnails on disk, shared by every app that shows small pictures of files: Vanta Explorer's
//! grid and Vanta Viewer's gallery read and write the same cache, so a folder looked at in one
//! is already done in the other.
//!
//! A thumbnail is 8-bit sRGB, tone-mapped on the CPU with the same Reinhard the shaders use for
//! HDR on an SDR screen, and kept as a PNG: small, and inspectable with any viewer when something
//! looks wrong. Its name is a hash of the file's path, its modification time and the size it was
//! made at, so a file that is written again gets a new thumbnail rather than a stale one.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::HdrImage;

/// An 8-bit sRGB picture, unpremultiplied RGBA, row after row.
pub struct Rgba8 {
    pub w: u32,
    pub h: u32,
    pub pixels: Vec<u8>,
}

/// The name a thumbnail of `path` at `px` pixels is kept under.
pub fn key(path: &Path, mtime: Option<SystemTime>, px: u32) -> String {
    use sha1::{Digest, Sha1};
    let secs = mtime.and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok()).map(|d| d.as_secs()).unwrap_or(0);
    let mut h = Sha1::new();
    h.update(path.to_string_lossy().as_bytes());
    h.update(format!(":{secs}:{px}").as_bytes());
    h.finalize().iter().take(8).map(|b| format!("{b:02x}")).collect()
}

/// Where the thumbnails are kept. The folder carries the explorer's name because it was there
/// first; moving it would throw away every thumbnail already made.
pub fn cache_dir() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))?;
    Some(base.join("vantaexplorer/thumbs"))
}

/// The cached thumbnail under `key`, if there is one.
pub fn load(key: &str) -> Option<Rgba8> {
    let f = cache_dir()?.join(format!("{key}.png"));
    let img = image::open(&f).ok()?.to_rgba8();
    mark_used(&f);
    Some(Rgba8 { w: img.width(), h: img.height(), pixels: img.into_raw() })
}

/// Keeps `img` under `key`. Written beside its final name and renamed over it, so another window
/// reading the same thumbnail never sees half a file.
pub fn store(key: &str, img: &Rgba8) {
    let Some(dir) = cache_dir() else { return };
    let _ = std::fs::create_dir_all(&dir);
    let f = dir.join(format!("{key}.png"));
    let tmp = f.with_extension(format!("{}.tmp", std::process::id()));
    let saved = image::save_buffer_with_format(&tmp, &img.pixels, img.w, img.h, image::ExtendedColorType::Rgba8, image::ImageFormat::Png);
    if saved.is_ok() {
        let _ = std::fs::rename(&tmp, &f);
    } else {
        let _ = std::fs::remove_file(&tmp);
    }
}

/// The thumbnail of an image file at `px` pixels on its longest side: from the cache, or decoded
/// and put there. `None` when the file cannot be decoded.
pub fn of_image(path: &Path, px: u32) -> Option<Rgba8> {
    let mtime = std::fs::metadata(path).and_then(|m| m.modified()).ok();
    let key = key(path, mtime, px);
    if let Some(hit) = load(&key) {
        return Some(hit);
    }
    let img = crate::decode_image(path, px, 0).ok().filter(|i| i.valid())?;
    let out = Rgba8 { w: img.w, h: img.h, pixels: to_srgb8(&img) };
    store(&key, &out);
    Some(out)
}

/// A thumbnail's modification time is when it was last shown, at day resolution: what the
/// clean-up goes by. Reading does not move atime reliably (noatime), so it is set here.
fn mark_used(f: &Path) {
    const DAY: std::time::Duration = std::time::Duration::from_secs(24 * 3600);
    let stale = std::fs::metadata(f).and_then(|m| m.modified()).is_ok_and(|t| t.elapsed().unwrap_or_default() > DAY);
    if stale {
        if let Ok(file) = std::fs::File::options().write(true).open(f) {
            let _ = file.set_modified(SystemTime::now());
        }
    }
}

/// Thumbnails not shown for this long are deleted.
const UNUSED_FOR: std::time::Duration = std::time::Duration::from_secs(90 * 24 * 3600);
/// And beyond this much, the least recently shown go first.
const CACHE_LIMIT: u64 = 1 << 30;

/// The cache's clean-up, on a thread of its own: thumbnails of files long gone or changed (their
/// key has moved on) would otherwise stay forever. Every app that writes to the cache runs it
/// once when it starts, so it is kept in bounds whichever of them is used. Two running at once
/// do no harm: a file the other already removed is simply not there to remove.
pub fn clean_cache() {
    let Some(dir) = cache_dir() else { return };
    let _ = std::thread::Builder::new().name("thumb-clean".into()).spawn(move || prune(&dir, UNUSED_FOR, CACHE_LIMIT));
}

fn prune(dir: &Path, unused_for: std::time::Duration, limit: u64) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let mut kept: Vec<(SystemTime, u64, PathBuf)> = Vec::new();
    let mut total = 0u64;
    for entry in rd.flatten() {
        let Ok(meta) = entry.metadata() else { continue };
        let Ok(used) = meta.modified() else { continue };
        if !meta.is_file() {
            continue;
        }
        let path = entry.path();
        let age = used.elapsed().unwrap_or_default();
        // A write that never got renamed into place (the program died mid-write).
        let leftover = path.extension().is_some_and(|e| e == "tmp") && age.as_secs() > 3600;
        if leftover || age > unused_for {
            let _ = std::fs::remove_file(&path);
            continue;
        }
        total += meta.len();
        kept.push((used, meta.len(), path));
    }
    if total > limit {
        kept.sort();
        for (_, len, path) in kept {
            if total <= limit {
                break;
            }
            if std::fs::remove_file(&path).is_ok() {
                total -= len;
            }
        }
    }
}

/// fp16 linear (203 nits) to 8-bit sRGB, HDR through the same tone map as the shaders' SDR path.
pub fn to_srgb8(img: &HdrImage) -> Vec<u8> {
    use crate::color::{srgb_oetf, to_bt709, tonemap_reinhard};
    let mut out = Vec::with_capacity((img.w * img.h * 4) as usize);
    for px in img.rgba16f.as_chunks::<4>().0 {
        let mut c = to_bt709([px[0].to_f32(), px[1].to_f32(), px[2].to_f32()], img.primaries).map(|v| v.max(0.0));
        if img.hdr {
            c = tonemap_reinhard(c);
        }
        for v in c {
            out.push((srgb_oetf(v.clamp(0.0, 1.0)) * 255.0).round() as u8);
        }
        out.push((px[3].to_f32().clamp(0.0, 1.0) * 255.0).round() as u8);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_key_changes_with_the_file_and_the_size() {
        let p = Path::new("/a/b.png");
        let t = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1000);
        assert_eq!(key(p, Some(t), 256), key(p, Some(t), 256));
        assert_ne!(key(p, Some(t), 256), key(p, Some(t), 384));
        assert_ne!(key(p, Some(t), 256), key(p, Some(t + std::time::Duration::from_secs(1)), 256));
        assert_eq!(key(p, Some(t), 256).len(), 16);
    }

    #[test]
    fn the_cache_loses_old_thumbnails_and_then_the_least_recently_shown() {
        let dir = std::env::temp_dir().join(format!("vanta-hdr-thumb-prune-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let day = std::time::Duration::from_secs(24 * 3600);
        let now = SystemTime::now();
        for (name, age_days) in [("old.png", 100), ("a.png", 3), ("b.png", 2), ("c.png", 1)] {
            let f = std::fs::File::create(dir.join(name)).unwrap();
            f.set_len(100).unwrap();
            f.set_modified(now - day * age_days).unwrap();
        }
        prune(&dir, day * 90, 250);
        let mut left: Vec<String> = std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        left.sort();
        assert_eq!(left, ["b.png", "c.png"]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
