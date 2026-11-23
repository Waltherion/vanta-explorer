//! Drawing a `RichDoc` in the preview pane: text in the theme's font, scaled so the document's
//! normal size is the pane's, its own colors kept where they can be read on the window.

use super::{Align, Block, Inline, Para, RichDoc, Sheet, SpanStyle, Table};
use crate::browser::Theme;
use egui::text::LayoutJob;

/// The family bold text is drawn in: the theme font's bold cut, or the theme font itself when it
/// has none. Always registered, see `main::apply_font`.
pub const BOLD: &str = "doc-bold";

/// A parsed document with its pictures uploaded.
pub struct Rich {
    pub doc: RichDoc,
    pub textures: Vec<egui::TextureHandle>,
}

impl Rich {
    pub fn new(ctx: &egui::Context, mut doc: RichDoc) -> Self {
        let textures = std::mem::take(&mut doc.images)
            .into_iter()
            .enumerate()
            .map(|(i, img)| ctx.load_texture(format!("preview-doc-{i}"), img, egui::TextureOptions::LINEAR))
            .collect();
        Self { doc, textures }
    }

    /// The name of sheet `page` of a workbook.
    pub fn sheet_name(&self, page: u32) -> Option<&str> {
        self.doc.sheets.as_ref()?.get(page as usize).map(String::as_str)
    }
}

pub fn show(ui: &mut egui::Ui, rect: egui::Rect, rich: &Rich, theme: &Theme) {
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect).layout(egui::Layout::top_down(egui::Align::Min)));
    let body = egui::TextStyle::Body.resolve(child.style()).size;
    if let [Block::Sheet(sheet)] = rich.doc.blocks.as_slice() {
        sheet_view(&mut child, sheet, theme, body);
        return;
    }
    egui::ScrollArea::vertical().id_salt("preview-doc").auto_shrink([false, false]).show(&mut child, |ui| {
        let width = (ui.available_width() - 8.0).max(60.0);
        let d = Drawer { theme, textures: &rich.textures, body };
        d.blocks(ui, &rich.doc.blocks, width);
        if let Some(note) = &rich.doc.note {
            ui.add_space(body * 0.6);
            ui.weak(note);
        }
    });
}

struct Drawer<'a> {
    theme: &'a Theme,
    textures: &'a [egui::TextureHandle],
    /// The pane's text size, which the document's normal size is drawn at.
    body: f32,
}

/// How much bigger than normal text a heading is drawn when the document does not say.
fn heading_scale(h: Option<u8>) -> f32 {
    match h {
        Some(0) => 1.9,
        Some(1) => 1.5,
        Some(2) => 1.3,
        Some(3) => 1.15,
        Some(_) => 1.05,
        None => 1.0,
    }
}

fn luminance(c: [u8; 3]) -> f32 {
    (0.2126 * c[0] as f32 + 0.7152 * c[1] as f32 + 0.0722 * c[2] as f32) / 255.0
}

impl Drawer<'_> {
    fn blocks(&self, ui: &mut egui::Ui, blocks: &[Block], width: f32) {
        for (i, b) in blocks.iter().enumerate() {
            match b {
                Block::Para(p) => self.para(ui, p, width, i == 0),
                Block::Table(t) => {
                    self.table(ui, t, width);
                    ui.add_space(self.body * 0.5);
                }
                Block::Sheet(s) => {
                    // Only a workbook has sheets, and it is drawn by `sheet_view`.
                    ui.weak(format!("({} rows of a sheet)", s.rows.len()));
                }
                Block::PageBreak => {
                    ui.add_space(self.body * 0.6);
                    let y = ui.cursor().min.y;
                    let x = ui.cursor().min.x;
                    let stroke = egui::Stroke::new(1.0, self.theme.text.gamma_multiply(0.25));
                    let dashes = egui::Shape::dashed_line(&[egui::pos2(x, y), egui::pos2(x + width, y)], stroke, 6.0, 4.0);
                    ui.painter().extend(dashes);
                    ui.add_space(self.body * 0.6);
                }
            }
        }
    }

    /// The document's color when it reads on the window, else the theme's text color: black text
    /// on a dark theme (or white on a light one) would be invisible.
    fn color(&self, c: Option<[u8; 3]>) -> egui::Color32 {
        let text = self.theme.text;
        let Some(c) = c else { return text };
        let dark_window = luminance([text.r(), text.g(), text.b()]) > 0.5;
        let l = luminance(c);
        if (dark_window && l < 0.3) || (!dark_window && l > 0.7) {
            text
        } else {
            egui::Color32::from_rgb(c[0], c[1], c[2])
        }
    }

    fn format(&self, st: &SpanStyle, heading: Option<u8>) -> egui::TextFormat {
        let size = (self.body * st.size.unwrap_or_else(|| heading_scale(heading))).clamp(self.body * 0.6, self.body * 2.6);
        let bold = st.bold || heading.is_some();
        let family = if bold { egui::FontFamily::Name(BOLD.into()) } else { egui::FontFamily::Proportional };
        let color = self.color(st.color);
        let line = egui::Stroke::new(1.0, color);
        egui::TextFormat {
            font_id: egui::FontId::new(size, family),
            color,
            italics: st.italic,
            underline: if st.underline { line } else { egui::Stroke::NONE },
            strikethrough: if st.strike { line } else { egui::Stroke::NONE },
            ..Default::default()
        }
    }

    fn new_job(width: f32, align: Align) -> LayoutJob {
        let mut job = LayoutJob::default();
        job.wrap.max_width = width;
        job.halign = match align {
            Align::Center => egui::Align::Center,
            Align::Right => egui::Align::RIGHT,
            _ => egui::Align::LEFT,
        };
        job.justify = align == Align::Justify;
        job
    }

    fn para(&self, ui: &mut egui::Ui, p: &Para, width: f32, first: bool) {
        if p.heading.is_some() && !first {
            ui.add_space(self.body * 0.5);
        }
        let indent = (p.level as f32 * self.body * 1.4).min(width * 0.5);
        let avail = (width - indent).max(30.0);
        let mut job = Self::new_job(avail, p.align);
        let mut drew = false;
        if let Some(m) = &p.marker {
            let st = p.inlines.iter().find_map(|i| if let Inline::Text(s) = i { Some(s.style.clone()) } else { None }).unwrap_or_default();
            job.append(&format!("{m}  "), 0.0, self.format(&SpanStyle { underline: false, strike: false, ..st }, p.heading));
        }
        for inline in &p.inlines {
            match inline {
                Inline::Text(s) => job.append(&s.text, 0.0, self.format(&s.style, p.heading)),
                Inline::Missing(what) => {
                    let mut f = self.format(&SpanStyle { italic: true, ..Default::default() }, None);
                    f.color = self.theme.text.gamma_multiply(0.5);
                    job.append(&format!("[{what}]"), 0.0, f);
                }
                Inline::Image { idx, w, h } => {
                    self.flush(ui, &mut job, indent, avail, p.align);
                    self.image(ui, *idx, (*w, *h), indent, avail, p.align);
                    drew = true;
                }
            }
        }
        drew |= self.flush(ui, &mut job, indent, avail, p.align);
        if !drew {
            // An empty paragraph is an empty line in the document.
            ui.add_space(self.body * heading_scale(p.heading));
        }
        ui.add_space(self.body * if p.heading.is_some() { 0.3 } else { 0.45 });
    }

    /// Lays out and draws what is in `job`, leaving it empty for what follows. True when there was
    /// something.
    fn flush(&self, ui: &mut egui::Ui, job: &mut LayoutJob, indent: f32, avail: f32, align: Align) -> bool {
        if job.text.trim().is_empty() {
            *job = Self::new_job(avail, align);
            return false;
        }
        let taken = std::mem::replace(job, Self::new_job(avail, align));
        let galley = ui.painter().layout_job(taken);
        let (rect, _) = ui.allocate_exact_size(egui::vec2(indent + avail, galley.size().y), egui::Sense::hover());
        let x = match align {
            Align::Center => rect.min.x + indent + avail / 2.0,
            Align::Right => rect.min.x + indent + avail,
            _ => rect.min.x + indent,
        };
        ui.painter().galley(egui::pos2(x, rect.min.y), galley, self.theme.text);
        true
    }

    /// A picture at the size the document gives it (`size`, points), which may stretch it; at its
    /// own size when the document gives none.
    fn image(&self, ui: &mut egui::Ui, idx: usize, size: (f32, f32), indent: f32, avail: f32, align: Align) {
        let Some(tex) = self.textures.get(idx) else { return };
        let [tw, th] = tex.size();
        if tw == 0 || th == 0 {
            return;
        }
        let (w_pt, h_pt) = size;
        // The document's size, at the scale its text is drawn at (11 pt is Word's normal size).
        let want = if w_pt > 0.0 { w_pt * self.body / 11.0 } else { tw as f32 };
        let dw = want.min(avail).max(8.0);
        let aspect = if w_pt > 0.0 && h_pt > 0.0 { h_pt / w_pt } else { th as f32 / tw as f32 };
        let dh = dw * aspect;
        let (rect, _) = ui.allocate_exact_size(egui::vec2(indent + avail, dh), egui::Sense::hover());
        let x = match align {
            Align::Center => rect.min.x + indent + (avail - dw) / 2.0,
            Align::Right => rect.min.x + indent + avail - dw,
            _ => rect.min.x + indent,
        };
        let r = egui::Rect::from_min_size(egui::pos2(x, rect.min.y), egui::vec2(dw, dh));
        ui.painter().image(tex.id(), r, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), egui::Color32::WHITE);
        ui.add_space(2.0);
    }

    fn table(&self, ui: &mut egui::Ui, t: &Table, width: f32) {
        let ncols = t.rows.iter().map(|r| r.iter().map(|c| c.span).sum::<usize>()).max().unwrap_or(0).max(1);
        let mut widths: Vec<f32> = if t.widths.len() == ncols && t.widths.iter().all(|w| *w > 0.0) { t.widths.clone() } else { vec![1.0; ncols] };
        let total: f32 = widths.iter().sum();
        for w in &mut widths {
            *w *= width / total;
        }
        let pad = 4.0;
        let stroke = egui::Stroke::new(1.0, self.theme.text.gamma_multiply(0.25));
        for row in &t.rows {
            let top = ui.cursor().min;
            let mut x = top.x;
            let mut col = 0;
            let mut row_h = self.body + 2.0 * pad;
            let mut placed = Vec::new();
            for cell in row {
                let end = (col + cell.span).min(ncols);
                let cw: f32 = widths[col.min(ncols)..end].iter().sum::<f32>().max(12.0);
                col = end;
                // Kept free for the fill, which is only known to be wanted once the row's height is.
                let slot = ui.painter().add(egui::Shape::Noop);
                let inner = egui::Rect::from_min_size(egui::pos2(x + pad, top.y + pad), egui::vec2((cw - 2.0 * pad).max(8.0), f32::INFINITY));
                let mut cu = ui.new_child(egui::UiBuilder::new().max_rect(inner).layout(egui::Layout::top_down(egui::Align::Min)));
                self.blocks(&mut cu, &cell.blocks, inner.width());
                row_h = row_h.max(cu.min_rect().height() + 2.0 * pad);
                placed.push((slot, x, cw, cell.fill));
                x += cw;
            }
            for (slot, x, cw, fill) in placed {
                let r = egui::Rect::from_min_size(egui::pos2(x, top.y), egui::vec2(cw, row_h));
                if let Some(f) = fill {
                    // A tint of the document's shading: at full strength a light gray header would
                    // put the theme's light text on a light ground.
                    ui.painter().set(slot, egui::Shape::rect_filled(r, 0.0, egui::Color32::from_rgba_unmultiplied(f[0], f[1], f[2], 40)));
                }
                ui.painter().rect_stroke(r, 0.0, stroke, egui::StrokeKind::Inside);
            }
            ui.allocate_exact_size(egui::vec2(width, row_h), egui::Sense::hover());
        }
    }
}

/// A sheet as a grid, only the visible rows laid out.
fn sheet_view(ui: &mut egui::Ui, s: &Sheet, theme: &Theme, body: f32) {
    let font = egui::FontId::proportional(body * 0.9);
    let row_h = (body * 1.6).round();
    let char_w = body * 0.55;
    if s.more_rows || s.more_cols {
        ui.weak(format!("The first {} rows and {} columns", super::MAX_ROWS, super::MAX_COLS));
    }
    if s.rows.is_empty() {
        ui.weak("The sheet is empty");
        return;
    }
    // Each column as wide as its longest text, within reason.
    let mut widths = vec![3usize; s.cols];
    for (_, cells) in &s.rows {
        for (c, cell) in cells.iter().enumerate() {
            let n = cell.text.lines().next().unwrap_or("").chars().count();
            widths[c] = widths[c].max(n.min(28));
        }
    }
    let widths: Vec<f32> = widths.iter().map(|n| *n as f32 * char_w + 12.0).collect();
    let last = s.rows.last().map(|r| r.0).unwrap_or(1);
    let head_w = (last.to_string().len().max(2) as f32) * char_w + 12.0;
    let total: f32 = head_w + widths.iter().sum::<f32>();
    let line = egui::Stroke::new(1.0, theme.text.gamma_multiply(0.15));
    let weak = theme.text.gamma_multiply(0.55);

    egui::ScrollArea::both().id_salt("preview-sheet").auto_shrink([false, false]).show_rows(ui, row_h, s.rows.len() + 1, |ui, range| {
        for i in range {
            let (rect, _) = ui.allocate_exact_size(egui::vec2(total, row_h), egui::Sense::hover());
            let p = ui.painter();
            let mut x = rect.min.x;
            let head = egui::Rect::from_min_size(egui::pos2(x, rect.min.y), egui::vec2(head_w, row_h));
            p.rect_filled(head, 0.0, theme.text.gamma_multiply(0.05));
            if i == 0 {
                p.rect_filled(rect, 0.0, theme.text.gamma_multiply(0.05));
            } else {
                p.text(head.right_center() - egui::vec2(5.0, 0.0), egui::Align2::RIGHT_CENTER, s.rows[i - 1].0.to_string(), font.clone(), weak);
            }
            x += head_w;
            for (c, w) in widths.iter().enumerate() {
                let cell = egui::Rect::from_min_size(egui::pos2(x, rect.min.y), egui::vec2(*w, row_h));
                p.line_segment([cell.right_top(), cell.right_bottom()], line);
                if i == 0 {
                    p.text(cell.center(), egui::Align2::CENTER_CENTER, super::column_name(c), font.clone(), weak);
                } else if let Some(v) = s.rows[i - 1].1.get(c).filter(|v| !v.text.is_empty()) {
                    let text = v.text.lines().next().unwrap_or("");
                    let clip = p.with_clip_rect(cell.shrink(1.0));
                    if v.right {
                        clip.text(cell.right_center() - egui::vec2(5.0, 0.0), egui::Align2::RIGHT_CENTER, text, font.clone(), theme.text);
                    } else {
                        clip.text(cell.left_center() + egui::vec2(5.0, 0.0), egui::Align2::LEFT_CENTER, text, font.clone(), theme.text);
                    }
                }
                x += w;
            }
            p.line_segment([rect.left_bottom(), rect.right_bottom()], line);
        }
    });
}
