//! Thumbnail generation and disk cache.
//!
//! Grid thumbnails go through egui as 8-bit sRGB, tone-mapped on the CPU with the SAME
//! Reinhard(Lw=6) the present shader uses, so a thumbnail and the HDR preview of the same file
//! agree. True HDR is reserved for the preview pane, where the extra range is actually visible;
//! at 128px the benefit would not pay for a GPU texture per visible cell.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::time::{Duration, Instant, SystemTime};

/// Decode cap. Generated once at the largest size the grid uses, then scaled down by egui,
/// so zooming does not re-decode.
pub const THUMB_PX: u32 = 256;
const WORKERS: usize = 3;
/// How still the view must be before any decode is started.
///
/// Scrolling past a folder of photos would otherwise queue every cell it swept over, and the
/// workers would grind through images the user had long left behind. Nothing is dispatched
/// until the set of visible cells has stopped changing for this long, so a flick through a
/// folder starts no work at all.
///
/// This exists ONLY to avoid wasted decodes. It was introduced while hunting a periodic
/// hitch and originally set to 150ms on the assumption that decoding caused it; it did not
///, the hitch was FIFO present mode blocking acquire (see vk.rs). With that understood,
/// the delay only has to outlast an actual scroll gesture, not protect the frame loop, so
/// it is short enough to be imperceptible when you stop. Set it to 0 to dispatch
/// immediately; the surrounding machinery (drop-on-scroll-away, per-frame caps) still works.
const SETTLE: Duration = Duration::from_millis(50);
/// Decodes started per frame once settled, so arriving at a big folder ramps up instead of
/// dumping the whole screen on the workers at once.
const MAX_DISPATCH_PER_FRAME: usize = 4;
/// Texture uploads per frame. Uploads run on the UI thread, so draining a burst of finished
/// decodes in one go means a single long frame. Worth keeping regardless of what the
/// periodic hitch turned out to be: unbounded work on the UI thread is its own hazard.
const MAX_UPLOADS_PER_FRAME: usize = 2;

pub enum Slot {
    Pending,
    Ready(egui::TextureHandle),
    Failed,
}

struct Done {
    key: String,
    image: Option<egui::ColorImage>,
}

pub struct Thumbs {
    tx: Sender<(String, PathBuf)>,
    rx: Receiver<Done>,
    slots: HashMap<String, Slot>,
    /// Requested this frame; anything not re-requested can be evicted.
    live: std::collections::HashSet<String>,
    /// Up to four thumbnailable files per folder, for folder-preview tiles. Cached because
    /// the alternative is a `read_dir` per folder cell per frame.
    folder_samples: HashMap<PathBuf, Vec<(PathBuf, Option<SystemTime>)>>,
    /// New scans started this frame, capped so scrolling past a wall of folders cannot stall
    /// the UI thread on syscalls. Uncached folders simply fill in over the next few frames.
    scans_this_frame: usize,
    /// Asked for but not yet handed to a worker. Held back until the view settles.
    wanted: HashMap<String, PathBuf>,
    /// Last frame's visible set, and when it last differed, our "is the user scrolling?"
    /// signal. No plumbing from the scroll area needed: while scrolling, what is on screen
    /// changes every frame; when it stops, it stops.
    prev_live: std::collections::HashSet<String>,
    last_change: Instant,
}

/// Tiles shown for a folder preview.
pub const FOLDER_TILES: usize = 4;
const MAX_SCANS_PER_FRAME: usize = 2;

impl Thumbs {
    pub fn new() -> Self {
        let (req_tx, req_rx) = channel::<(String, PathBuf)>();
        let (out_tx, out_rx) = channel::<Done>();
        let req_rx = std::sync::Arc::new(std::sync::Mutex::new(req_rx));

        for i in 0..WORKERS {
            let rx = std::sync::Arc::clone(&req_rx);
            let tx = out_tx.clone();
            std::thread::Builder::new()
                .name(format!("vx-thumb{i}"))
                .spawn(move || loop {
                    let job = { rx.lock().ok().and_then(|g| g.recv().ok()) };
                    let Some((key, path)) = job else { return };
                    let image = load_or_make(&key, &path);
                    if tx.send(Done { key, image }).is_err() {
                        return;
                    }
                })
                .expect("spawn thumbnail worker");
        }

        Self {
            tx: req_tx,
            rx: out_rx,
            slots: HashMap::new(),
            live: Default::default(),
            folder_samples: HashMap::new(),
            scans_this_frame: 0,
            wanted: HashMap::new(),
            prev_live: Default::default(),
            last_change: Instant::now(),
        }
    }

    /// Stable per (path, mtime, size). Same scheme vantapaper proved: content changes when the
    /// file is rewritten, so a stale thumbnail cannot survive an edit.
    pub fn key(path: &Path, mtime: Option<SystemTime>) -> String {
        use sha1::{Digest, Sha1};
        let secs = mtime
            .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let mut h = Sha1::new();
        h.update(path.to_string_lossy().as_bytes());
        h.update(format!(":{secs}:{THUMB_PX}").as_bytes());
        let digest = h.finalize();
        digest.iter().take(8).map(|b| format!("{b:02x}")).collect()
    }

    /// Ask for a thumbnail. Returns immediately; poll picks up the result.
    pub fn request(&mut self, key: &str, path: &Path) {
        self.live.insert(key.to_string());
        if self.slots.contains_key(key) {
            return;
        }
        self.slots.insert(key.to_string(), Slot::Pending);
        // Queued, not dispatched: end_frame decides when the view is calm enough to start.
        self.wanted.insert(key.to_string(), path.to_path_buf());
    }

    pub fn get(&self, key: &str) -> Option<&Slot> {
        self.slots.get(key)
    }

    /// Upload finished thumbnails. Returns true when anything changed.
    pub fn poll(&mut self, ctx: &egui::Context) -> bool {
        let mut changed = false;
        let mut uploaded = 0;
        while uploaded < MAX_UPLOADS_PER_FRAME {
            let Ok(done) = self.rx.try_recv() else { break };
            uploaded += 1;
            let slot = match done.image {
                Some(img) => Slot::Ready(ctx.load_texture(
                    format!("thumb:{}", done.key),
                    img,
                    egui::TextureOptions::LINEAR,
                )),
                None => Slot::Failed,
            };
            self.slots.insert(done.key, slot);
            changed = true;
        }
        if uploaded == MAX_UPLOADS_PER_FRAME {
            // More are waiting; come back for them rather than spending a longer frame here.
            ctx.request_repaint();
        }
        changed
    }

    /// Up to `FOLDER_TILES` thumbnailable files directly inside `dir`, newest scan cached.
    ///
    /// Returns an empty slice both for "no images here" and for "not scanned yet", the
    /// caller just falls back to the folder icon, and a later frame fills it in. Only the
    /// folder's own children are considered; a recursive hunt would be far too expensive to
    /// run from the paint loop.
    pub fn folder_sample(&mut self, dir: &Path) -> Vec<(PathBuf, Option<SystemTime>)> {
        if let Some(hit) = self.folder_samples.get(dir) {
            return hit.clone();
        }
        if self.scans_this_frame >= MAX_SCANS_PER_FRAME {
            return Vec::new();
        }
        self.scans_this_frame += 1;

        let mut found = Vec::new();
        if let Ok(rd) = std::fs::read_dir(dir) {
            for de in rd.flatten() {
                if found.len() >= FOLDER_TILES {
                    break;
                }
                let path = de.path();
                if de.file_type().is_ok_and(|t| t.is_file()) && thumbable(&path) {
                    let mtime = de.metadata().ok().and_then(|m| m.modified().ok());
                    found.push((path, mtime));
                }
            }
        }
        self.folder_samples.insert(dir.to_path_buf(), found.clone());
        found
    }

    /// Drop textures for cells that scrolled out of view, so a huge folder cannot grow the
    /// texture set without bound. Call once per frame after the UI has made its requests.
    pub fn end_frame(&mut self, ctx: &egui::Context) {
        self.scans_this_frame = 0;

        if self.live != self.prev_live {
            self.prev_live.clone_from(&self.live);
            self.last_change = Instant::now();
        }

        // Anything queued that has scrolled off screen is dropped WITH its slot, leaving the
        // slot behind as Pending would mean the cell never decodes if it comes back.
        let gone: Vec<String> = self
            .wanted
            .keys()
            .filter(|k| !self.live.contains(*k))
            .cloned()
            .collect();
        for key in gone {
            self.wanted.remove(&key);
            self.slots.remove(&key);
        }

        let idle = self.last_change.elapsed();
        if idle < SETTLE {
            // Still moving. Make sure a frame lands once it has been calm long enough,
            // otherwise an on-demand-render app would simply never start the work.
            ctx.request_repaint_after(SETTLE - idle);
        } else {
            let start: Vec<String> =
                self.wanted.keys().take(MAX_DISPATCH_PER_FRAME).cloned().collect();
            for key in start {
                if let Some(path) = self.wanted.remove(&key) {
                    let _ = self.tx.send((key, path));
                }
            }
            if !self.wanted.is_empty() {
                ctx.request_repaint();
            }
        }

        if self.slots.len() > 512 {
            let live = std::mem::take(&mut self.live);
            self.slots.retain(|k, _| live.contains(k));
            self.live = live;
        } else {
            self.live.clear();
        }
    }
}

fn cache_dir() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))?;
    Some(base.join("vantaexplorer/thumbs"))
}

/// Grid thumbnails cover HDR media AND documents (PDF first page, ODF/OOXML embedded
/// thumbnails). Text files stay on their MIME icon: a thumbnail of text is noise.
pub fn thumbable(path: &Path) -> bool {
    vanta_hdr::decode::is_supported(path) || crate::docs::is_doc(path)
}

/// Disk cache hit, else decode and store. Cached as 8-bit sRGB PNG: small, and inspectable
/// with any viewer when something looks wrong.
fn load_or_make(key: &str, path: &Path) -> Option<egui::ColorImage> {
    let file = cache_dir().map(|d| d.join(format!("{key}.png")));

    if let Some(f) = &file {
        if let Ok(img) = image::open(f) {
            mark_used(f);
            return Some(to_color_image(&img.to_rgba8()));
        }
    }

    if crate::docs::is_doc(path) {
        let crate::docs::DocContent::Raster { img, .. } = crate::docs::render(path, THUMB_PX, 0).ok()? else {
            return None;
        };
        let rgba: Vec<u8> = img
            .pixels
            .iter()
            .flat_map(|c| c.to_srgba_unmultiplied())
            .collect();
        let buf: image::RgbaImage =
            image::ImageBuffer::from_raw(img.width() as u32, img.height() as u32, rgba)?;
        if let Some(f) = &file {
            store(f, &buf);
        }
        return Some(to_color_image(&buf));
    }

    let hdr = vanta_hdr::decode_image(path, THUMB_PX, 0).ok()?;
    let rgba8 = tonemap_to_srgb8(&hdr);
    let buf: image::RgbaImage = image::ImageBuffer::from_raw(hdr.w, hdr.h, rgba8)?;

    if let Some(f) = &file {
        store(f, &buf);
    }
    Some(to_color_image(&buf))
}

/// Written beside its final name and renamed over it, so another window reading the same
/// thumbnail never sees half a file.
fn store(f: &Path, buf: &image::RgbaImage) {
    if let Some(parent) = f.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let tmp = f.with_extension(format!("{}.tmp", std::process::id()));
    if buf.save_with_format(&tmp, image::ImageFormat::Png).is_ok() {
        let _ = std::fs::rename(&tmp, f);
    } else {
        let _ = std::fs::remove_file(&tmp);
    }
}

/// A thumbnail's modification time is when it was last shown, at day resolution: what the
/// clean-up goes by. Reading does not move atime reliably (noatime), so it is set here.
fn mark_used(f: &Path) {
    const DAY: std::time::Duration = std::time::Duration::from_secs(24 * 3600);
    let stale = std::fs::metadata(f).and_then(|m| m.modified()).is_ok_and(|t| t.elapsed().unwrap_or_default() > DAY);
    if stale {
        if let Ok(file) = std::fs::File::options().write(true).open(f) {
            let _ = file.set_modified(std::time::SystemTime::now());
        }
    }
}

/// Thumbnails not shown for this long are deleted.
const UNUSED_FOR: std::time::Duration = std::time::Duration::from_secs(90 * 24 * 3600);
/// And beyond this much, the least recently shown go first.
const CACHE_LIMIT: u64 = 1 << 30;

/// The disk cache's clean-up, once at start on a thread of its own: thumbnails of files long
/// gone or changed (their key has moved on) would otherwise stay forever.
pub fn clean_cache() {
    let Some(dir) = cache_dir() else { return };
    let _ = std::thread::Builder::new().name("vx-thumb-clean".into()).spawn(move || prune(&dir, UNUSED_FOR, CACHE_LIMIT));
}

fn prune(dir: &Path, unused_for: std::time::Duration, limit: u64) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let mut kept: Vec<(std::time::SystemTime, u64, PathBuf)> = Vec::new();
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

/// fp16 linear (203 nits) -> 8-bit sRGB, via the same tone map as the shader's SDR branch.
fn tonemap_to_srgb8(img: &vanta_hdr::HdrImage) -> Vec<u8> {
    use vanta_hdr::color::{srgb_oetf, to_bt709, tonemap_reinhard};
    let mut out = Vec::with_capacity((img.w * img.h * 4) as usize);
    for px in img.rgba16f.chunks(4) {
        let mut c = [px[0].to_f32(), px[1].to_f32(), px[2].to_f32()];
        c = to_bt709(c, img.primaries);
        for v in &mut c {
            *v = v.max(0.0);
        }
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

fn to_color_image(img: &image::RgbaImage) -> egui::ColorImage {
    egui::ColorImage::from_rgba_unmultiplied(
        [img.width() as usize, img.height() as usize],
        img.as_raw(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_cache_loses_old_thumbnails_and_then_the_least_recently_shown() {
        let dir = std::env::temp_dir().join("vantaexplorer-thumb-prune");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let day = std::time::Duration::from_secs(24 * 3600);
        let now = std::time::SystemTime::now();
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
