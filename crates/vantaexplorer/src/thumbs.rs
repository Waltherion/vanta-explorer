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
        vanta_hdr::thumb::key(path, mtime, THUMB_PX)
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


/// Grid thumbnails cover HDR media AND documents (PDF first page, ODF/OOXML embedded
/// thumbnails). Text files stay on their MIME icon: a thumbnail of text is noise.
pub fn thumbable(path: &Path) -> bool {
    vanta_hdr::decode::is_supported(path) || crate::docs::is_doc(path)
}

/// Disk cache hit, else decode and store, through the cache vanta-hdr keeps for every app.
fn load_or_make(key: &str, path: &Path) -> Option<egui::ColorImage> {
    use vanta_hdr::thumb::{self, Rgba8};
    if let Some(hit) = thumb::load(key) {
        return Some(to_color_image(&hit));
    }

    if crate::docs::is_doc(path) {
        let crate::docs::DocContent::Raster { img, .. } = crate::docs::render(path, THUMB_PX, 0).ok()? else {
            return None;
        };
        let pixels: Vec<u8> = img.pixels.iter().flat_map(|c| c.to_srgba_unmultiplied()).collect();
        let out = Rgba8 { w: img.width() as u32, h: img.height() as u32, pixels };
        thumb::store(key, &out);
        return Some(to_color_image(&out));
    }

    let hdr = vanta_hdr::decode_image(path, THUMB_PX, 0).ok()?;
    let out = Rgba8 { w: hdr.w, h: hdr.h, pixels: thumb::to_srgb8(&hdr) };
    thumb::store(key, &out);
    Some(to_color_image(&out))
}

/// The disk cache's clean-up, once at start on a thread of its own; it lives in vanta-hdr with
/// the cache itself, since Vanta Viewer runs it too.
pub fn clean_cache() {
    vanta_hdr::thumb::clean_cache();
}

fn to_color_image(img: &vanta_hdr::thumb::Rgba8) -> egui::ColorImage {
    egui::ColorImage::from_rgba_unmultiplied([img.w as usize, img.h as usize], &img.pixels)
}
