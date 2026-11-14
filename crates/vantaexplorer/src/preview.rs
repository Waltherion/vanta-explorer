//! Async image decode for the preview pane.
//!
//! Decoding a 60-megapixel AVIF takes tens of milliseconds, which would drop frames if it ran
//! inline. One worker thread owns the decode; the UI thread only ever polls a channel.

use crate::docs::{self, DocContent};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};
use vanta_hdr::{decode_image, HdrImage, Primaries};

/// Cap the decode so a huge photo does not become a huge GPU upload. Comfortably above any
/// preview pane size, and re-decoded at full size only if we ever add a zoom view.
const PREVIEW_MAX_DIM: u32 = 2048;

enum Content {
    Hdr(HdrImage),
    Doc(DocContent),
}

struct Loaded {
    path: PathBuf,
    content: Result<Content, String>,
    file_info: Option<String>,
}

/// What the preview pane should draw right now.
pub enum Shown {
    Nothing,
    /// HDR image lives on the GPU; the pane reserves a transparent rect for it.
    Hdr,
    /// SDR raster (PDF page, document thumbnail) as a plain egui texture.
    Raster(egui::TextureHandle),
    /// Highlighted text.
    Text(std::sync::Arc<egui::text::LayoutJob>),
}

/// Everything previewable, not just HDR formats.
pub fn supported(path: &Path) -> bool {
    vanta_hdr::decode::is_supported(path) || docs::is_doc(path) || docs::is_text(path)
}

/// State of the currently previewed file, as far as the UI is concerned.
pub enum PreviewState {
    Empty,
    /// A decode is in flight and there is nothing to show meanwhile. Which file it is lives
    /// in `Preview::requested`, so it is not repeated here.
    Loading,
    Ready(PathBuf, u32, u32, Option<String>), // path, current_page, total_pages, file_info
    Failed(PathBuf, String),
}

pub struct Preview {
    tx: Sender<(PathBuf, u32, egui::Context)>,
    rx: Receiver<Loaded>,
    pub state: PreviewState,
    pub shown: Shown,
    /// Dimensions of what is currently on the GPU, for aspect-fit.
    pub texture_size: Option<(u32, u32)>,
    pub primaries: Primaries,
    pub is_hdr: bool,
    /// What the pane last ASKED for, which during a decode is not yet what is drawn.
    /// Everything that means "the current subject" reads this: request dedupe, the stale
    /// result guard in poll, and the page controls.
    requested: Option<(PathBuf, u32)>,
}

impl Preview {
    pub fn new() -> Self {
        let (req_tx, req_rx) = channel::<(PathBuf, u32, egui::Context)>();
        let (out_tx, out_rx) = channel::<Loaded>();

        std::thread::Builder::new()
            .name("vx-preview".into())
            .spawn(move || {
                while let Ok((path, page, ctx)) = req_rx.recv() {
                    let mut path = path;
                    let mut page = page;
                    let mut ctx = ctx;
                    while let Ok(newer) = req_rx.try_recv() {
                        path = newer.0;
                        page = newer.1;
                        ctx = newer.2;
                    }
                    let content = if docs::is_doc(&path) || docs::is_text(&path) {
                        docs::render(&path, PREVIEW_MAX_DIM, page).map(Content::Doc)
                    } else {
                        decode_image(&path, PREVIEW_MAX_DIM, page)
                            .map(Content::Hdr)
                            .map_err(|e| e.to_string())
                    };
                    let file_info = std::process::Command::new("file")
                        .arg("-b")
                        .arg(&path)
                        .output()
                        .ok()
                        .and_then(|out| String::from_utf8(out.stdout).ok())
                        .map(|s| s.trim().to_string());
                    let ok = out_tx.send(Loaded { path, content, file_info }).is_ok();
                    ctx.request_repaint();
                    if !ok {
                        return;
                    }
                }
            })
            .expect("spawn preview thread");
        Self {
            tx: req_tx,
            rx: out_rx,
            state: PreviewState::Empty,
            shown: Shown::Nothing,
            texture_size: None,
            primaries: Primaries::Bt709,
            is_hdr: false,
            requested: None,
        }
    }

    pub fn current_path(&self) -> Option<&Path> {
        self.requested.as_ref().map(|(p, _)| p.as_path())
    }

    pub fn current_page(&self) -> Option<u32> {
        self.requested.as_ref().map(|(_, page)| *page)
    }

    /// Ask for `path` unless it is already the current subject.
    pub fn request(&mut self, path: &Path, page: u32, ctx: &egui::Context) {
        if self.requested.as_ref().is_some_and(|(p, pg)| p == path && *pg == page) {
            return;
        }
        // Dragging the video seek bar is a rapid series of requests for the SAME file, and
        // blanking on each one made the picture strobe: the pane stopped reserving its rect,
        // so the HDR pass had nowhere to draw and the frame already on the GPU went unused
        // until the next decode finished. Hold the last good frame instead, and only wipe
        // when the subject really changes (or when there is nothing on screen to hold).
        let hold = self.requested.as_ref().is_some_and(|(p, _)| p == path)
            && !matches!(self.shown, Shown::Nothing);
        self.requested = Some((path.to_path_buf(), page));
        if !hold {
            self.state = PreviewState::Loading;
            self.shown = Shown::Nothing;
        }
        let _ = self.tx.send((path.to_path_buf(), page, ctx.clone()));
    }

    pub fn clear(&mut self) {
        self.state = PreviewState::Empty;
        self.shown = Shown::Nothing;
        self.texture_size = None;
        self.requested = None;
    }

    /// Drain finished decodes. Returns an HDR image to upload to the GPU, if one arrived;
    /// raster and text results are absorbed here as egui-side state.
    pub fn poll(&mut self, ctx: &egui::Context) -> Option<HdrImage> {
        let mut result = None;
        while let Ok(loaded) = self.rx.try_recv() {
            // A stale result arrives whenever the selection moved on mid-decode.
            if self.current_path() != Some(loaded.path.as_path()) {
                continue;
            }
            match loaded.content {
                Ok(Content::Hdr(img)) => {
                    let pages = img.pages;
                    self.texture_size = Some((img.w, img.h));
                    self.primaries = img.primaries;
                    self.is_hdr = img.hdr;
                    self.shown = Shown::Hdr;
                    let current_page = self.current_page().unwrap_or(0);
                    self.state = PreviewState::Ready(loaded.path, current_page, pages, loaded.file_info);
                    result = Some(img);
                }
                Ok(Content::Doc(DocContent::Raster { img, total_pages })) => {
                    self.texture_size = Some((img.width() as u32, img.height() as u32));
                    self.is_hdr = false;
                    self.shown = Shown::Raster(ctx.load_texture(
                        "preview-doc",
                        img,
                        egui::TextureOptions::LINEAR,
                    ));
                    let current_page = self.current_page().unwrap_or(0);
                    self.state = PreviewState::Ready(loaded.path, current_page, total_pages, loaded.file_info);
                }
                Ok(Content::Doc(DocContent::Text(job))) => {
                    self.texture_size = None;
                    self.is_hdr = false;
                    self.shown = Shown::Text(std::sync::Arc::new(job));
                    let current_page = self.current_page().unwrap_or(0);
                    self.state = PreviewState::Ready(loaded.path, current_page, 1, loaded.file_info);
                }
                Err(e) => {
                    self.texture_size = None;
                    self.shown = Shown::Nothing;
                    self.state = PreviewState::Failed(loaded.path, e);
                }
            }
        }
        result
    }
}

/// Fit `(iw, ih)` inside `rect` preserving aspect ratio, centred.
pub fn fit(rect: egui::Rect, iw: u32, ih: u32) -> egui::Rect {
    if iw == 0 || ih == 0 {
        return rect;
    }
    let scale = (rect.width() / iw as f32).min(rect.height() / ih as f32);
    let (w, h) = (iw as f32 * scale, ih as f32 * scale);
    egui::Rect::from_min_size(
        egui::pos2(
            rect.min.x + (rect.width() - w) * 0.5,
            rect.min.y + (rect.height() - h) * 0.5,
        ),
        egui::vec2(w, h),
    )
}

pub fn primaries_index(p: Primaries) -> u32 {
    match p {
        Primaries::Bt709 => 0,
        Primaries::Bt2020 => 1,
        Primaries::DisplayP3 => 2,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Seeking a video is a rapid series of requests for the same file. Blanking on each one
    /// is what made the picture strobe, so it is pinned here: same file keeps the frame that
    /// is on screen, a different file does not.
    #[test]
    fn same_file_holds_the_last_frame_while_the_next_decodes() {
        let ctx = egui::Context::default();
        let mut p = Preview::new();
        let video = Path::new("/nonexistent/clip.mp4");

        p.request(video, 0, &ctx);
        assert!(matches!(p.state, PreviewState::Loading), "first look has nothing to hold");

        // Pretend the decode landed: this is the state the seek bar drags from.
        p.shown = Shown::Hdr;
        p.texture_size = Some((1920, 1080));
        p.state = PreviewState::Ready(video.to_path_buf(), 0, 100, None);

        p.request(video, 37, &ctx);
        assert!(matches!(p.shown, Shown::Hdr), "held frame must stay on screen while seeking");
        assert!(
            matches!(p.state, PreviewState::Ready(_, 0, 100, _)),
            "state still describes the held frame, so the pane keeps reserving its rect"
        );
        assert_eq!(p.current_page(), Some(37), "the controls follow what was asked for");
        assert_eq!(p.texture_size, Some((1920, 1080)));

        // A different file is a different subject: nothing to hold on to.
        p.request(Path::new("/nonexistent/other.png"), 0, &ctx);
        assert!(matches!(p.shown, Shown::Nothing));
        assert!(matches!(p.state, PreviewState::Loading));
    }
}
