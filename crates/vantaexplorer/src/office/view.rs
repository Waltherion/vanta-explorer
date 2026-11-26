//! Drawing a `RichDoc` in the preview pane: a text document on its pages, as it would print, a
//! workbook's sheet as a grid.

use super::{Align, Block, Field, Geom, Inline, Para, Shape, ShapeContent, Slide, VAnchor, RichDoc, Sheet, SpanStyle, Table};
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

/// The lightness of paper and of ink on a dark page, which white and black become.
const NIGHT_PAPER: f32 = 0.13;
const NIGHT_INK: f32 = 0.86;

/// A color for a dark page: its lightness turned round (white paper becomes dark gray, black ink
/// light), its hue and how colorful it is kept, so blue text stays blue, only light.
pub fn night(c: egui::Color32) -> egui::Color32 {
    let [r, g, b, a] = night_px([c.r(), c.g(), c.b(), c.a()]);
    egui::Color32::from_rgba_unmultiplied(r, g, b, a)
}

fn night_px([r, g, b, a]: [u8; 4]) -> [u8; 4] {
    let (rf, gf, bf) = (r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0);
    let y = 0.2126 * rf + 0.7152 * gf + 0.0722 * bf;
    let to = NIGHT_PAPER + (NIGHT_INK - NIGHT_PAPER) * (1.0 - y);
    let d = to - y;
    let ch = |v: f32| ((v + d).clamp(0.0, 1.0) * 255.0).round() as u8;
    [ch(rf), ch(gf), ch(bf), a]
}

/// A rendered page made dark the same way: everything on it, pictures included.
pub fn night_image(mut img: egui::ColorImage) -> egui::ColorImage {
    for px in &mut img.pixels {
        let [r, g, b, a] = px.to_srgba_unmultiplied();
        let [r, g, b, a] = night_px([r, g, b, a]);
        *px = egui::Color32::from_rgba_unmultiplied(r, g, b, a);
    }
    img
}

/// Space around and between the pages, in pixels.
const PAGE_GAP: f32 = 10.0;
/// The ink of text whose color is automatic.
const INK: egui::Color32 = egui::Color32::from_rgb(20, 20, 20);
/// The hairlines of tables.
const RULE: egui::Color32 = egui::Color32::from_rgb(150, 150, 150);
/// How far a list's marker stands left of its text, in points.
const HANG: f32 = 18.0;
/// A list level's indent, in points.
const INDENT: f32 = 36.0;
/// A single line's height as a multiple of its text size.
const LINE: f32 = 1.17;

/// A text document as its pages: paper of the document's size with its margins, scaled to the
/// pane's width, the text set and broken into pages at that scale. `dark`: on dark paper, its
/// colors turned round by `night`, its pictures as they are.
pub fn show(ui: &mut egui::Ui, rect: egui::Rect, rich: &Rich, theme: &Theme, dark: bool) {
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect).layout(egui::Layout::top_down(egui::Align::Min)));
    if let [Block::Sheet(sheet)] = rich.doc.blocks.as_slice() {
        let body = egui::TextStyle::Body.resolve(child.style()).size;
        sheet_view(&mut child, sheet, theme, body);
        return;
    }
    if let Some(slides) = &rich.doc.slides {
        slides_view(&mut child, rich, slides, theme, dark);
        return;
    }
    let page = rich.doc.page;
    let mut shown_page = None;
    let out = egui::ScrollArea::vertical().id_salt("preview-doc").auto_shrink([false, false]).show(&mut child, |ui| {
        crate::browser::take_preview_scroll(ui);
        let width = ui.available_width();
        let s = ((width - 2.0 * PAGE_GAP) / page.width).max(0.05);
        let [mt, mr, mb, ml] = page.margin.map(|m| m * s);
        let (pw, ph) = (page.width * s, page.height * s);
        let lay = Lay { painter: ui.painter().clone(), textures: &rich.textures, s, base: rich.doc.base_pt, dark, page_no: std::cell::Cell::new((1, 1)) };
        let paper = lay.tone(egui::Color32::WHITE);
        let ink = lay.tone(INK);
        let pieces = lay.blocks(&rich.doc.blocks, pw - ml - mr);
        let (placed, pages) = paginate(&pieces, ph - mt - mb);
        let note_h = if rich.doc.note.is_some() { 24.0 } else { 0.0 };
        let total = PAGE_GAP + pages as f32 * (ph + PAGE_GAP) + note_h;
        let (area, _) = ui.allocate_exact_size(egui::vec2(width, total), egui::Sense::hover());
        let visible = ui.clip_rect();
        let page_rect = |i: usize| egui::Rect::from_min_size(egui::pos2(area.min.x + (width - pw) / 2.0, area.min.y + PAGE_GAP + i as f32 * (ph + PAGE_GAP)), egui::vec2(pw, ph));
        let painter = ui.painter();
        for i in 0..pages {
            let r = page_rect(i);
            if !r.expand(8.0).intersects(visible) {
                continue;
            }
            painter.rect_filled(r.translate(egui::vec2(0.0, 2.0)).expand(1.0), 2.0, egui::Color32::from_black_alpha(70));
            painter.rect_filled(r, 0.0, paper);
            if dark {
                // Dark paper on a dark window needs an edge the shadow cannot give it.
                painter.rect_stroke(r, 0.0, egui::Stroke::new(1.0, egui::Color32::from_white_alpha(28)), egui::StrokeKind::Outside);
            }
        }
        // Headers and footers, set for each page in view with its number.
        for i in 0..pages {
            let r = page_rect(i);
            if !r.intersects(visible) {
                continue;
            }
            lay.page_no.set((i + 1, pages));
            let pt = painter.with_clip_rect(r.intersect(visible));
            for (running, at_top) in [(&rich.doc.header, true), (&rich.doc.footer, false)] {
                let Some(blocks) = running.as_ref().map(|r| r.for_page(i)).filter(|b| !b.is_empty()) else { continue };
                let (items, h) = Lay::stack(lay.blocks(blocks, pw - ml - mr));
                let y = if at_top { page.header * s } else { ph - page.footer * s - h };
                let offset = (r.min + egui::vec2(ml, y)).to_vec2();
                for item in &items {
                    item.paint(&pt, offset, ink);
                }
            }
        }
        for p in &placed {
            let r = page_rect(p.page);
            if !r.intersects(visible) {
                continue;
            }
            let top = r.min + egui::vec2(ml, mt + p.at);
            let piece = &pieces[p.piece];
            let whole = p.y0 <= 0.0 && p.y1 >= piece.h;
            let clip = if whole { r } else { egui::Rect::from_min_max(egui::pos2(r.min.x, top.y), egui::pos2(r.max.x, top.y + p.y1 - p.y0)).intersect(r) };
            let pt = painter.with_clip_rect(clip.intersect(visible));
            let offset = top.to_vec2() - egui::vec2(0.0, p.y0);
            for item in &piece.items {
                item.paint(&pt, offset, ink);
            }
        }
        if let Some(note) = &rich.doc.note {
            let y = area.min.y + PAGE_GAP + pages as f32 * (ph + PAGE_GAP);
            painter.text(egui::pos2(area.center().x, y + 4.0), egui::Align2::CENTER_TOP, note, egui::TextStyle::Small.resolve(ui.style()), theme.text.gamma_multiply(0.6));
        }
        if pages > 1 {
            let middle = visible.center().y - area.min.y - PAGE_GAP / 2.0;
            shown_page = Some((((middle / (ph + PAGE_GAP)).floor().max(0.0) as usize).min(pages - 1) + 1, pages));
        }
    });
    if let Some((n, of)) = shown_page {
        page_badge(&child, out.inner_rect, n, of);
    }
}

/// Which page is in view, in the corner as a PDF reader says it.
fn page_badge(ui: &egui::Ui, area: egui::Rect, n: usize, of: usize) {
    let painter = ui.painter();
    let font = egui::TextStyle::Small.resolve(ui.style());
    let galley = painter.layout_no_wrap(format!("{n} / {of}"), font, egui::Color32::WHITE);
    let at = area.right_bottom() - galley.size() - egui::vec2(18.0, 10.0);
    painter.rect_filled(egui::Rect::from_min_size(at, galley.size()).expand2(egui::vec2(6.0, 3.0)), 4.0, egui::Color32::from_black_alpha(160));
    painter.galley(at, galley, egui::Color32::WHITE);
}

fn rgb(c: [u8; 3]) -> egui::Color32 {
    egui::Color32::from_rgb(c[0], c[1], c[2])
}

/// A presentation: its slides one under the other, each scaled to the pane's width. On dark
/// pages a light slide is turned dark; a slide that is dark already is left as it is.
fn slides_view(ui: &mut egui::Ui, rich: &Rich, slides: &[Slide], theme: &Theme, dark: bool) {
    let page = rich.doc.page;
    let mut shown = None;
    let out = egui::ScrollArea::vertical().id_salt("preview-slides").auto_shrink([false, false]).show(ui, |ui| {
        crate::browser::take_preview_scroll(ui);
        let width = ui.available_width();
        let s = ((width - 2.0 * PAGE_GAP) / page.width).max(0.02);
        let (pw, ph) = (page.width * s, page.height * s);
        let n = slides.len();
        let note_h = if rich.doc.note.is_some() { 24.0 } else { 0.0 };
        let total = PAGE_GAP + n as f32 * (ph + PAGE_GAP) + note_h;
        let (area, _) = ui.allocate_exact_size(egui::vec2(width, total), egui::Sense::hover());
        let visible = ui.clip_rect();
        let page_rect = |i: usize| egui::Rect::from_min_size(egui::pos2(area.min.x + (width - pw) / 2.0, area.min.y + PAGE_GAP + i as f32 * (ph + PAGE_GAP)), egui::vec2(pw, ph));
        let painter = ui.painter();
        for (i, slide) in slides.iter().enumerate() {
            let r = page_rect(i);
            if !r.expand(8.0).intersects(visible) {
                continue;
            }
            let [br, bg, bb] = slide.background;
            let light = 0.2126 * br as f32 + 0.7152 * bg as f32 + 0.0722 * bb as f32 > 128.0;
            let night = dark && light;
            let lay = Lay { painter: painter.clone(), textures: &rich.textures, s, base: rich.doc.base_pt, dark: night, page_no: std::cell::Cell::new((i + 1, n)) };
            painter.rect_filled(r.translate(egui::vec2(0.0, 2.0)).expand(1.0), 2.0, egui::Color32::from_black_alpha(70));
            painter.rect_filled(r, 0.0, lay.tone(rgb(slide.background)));
            let pt = painter.with_clip_rect(r.intersect(visible));
            if let Some(tex) = slide.background_image.and_then(|i| rich.textures.get(i)) {
                pt.image(tex.id(), r, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), egui::Color32::WHITE);
            }
            if dark {
                painter.rect_stroke(r, 0.0, egui::Stroke::new(1.0, egui::Color32::from_white_alpha(28)), egui::StrokeKind::Outside);
            }
            for shape in &slide.shapes {
                draw_shape(&pt, &lay, r.min, shape);
            }
        }
        if let Some(note) = &rich.doc.note {
            let y = area.min.y + PAGE_GAP + n as f32 * (ph + PAGE_GAP);
            painter.text(egui::pos2(area.center().x, y + 4.0), egui::Align2::CENTER_TOP, note, egui::TextStyle::Small.resolve(ui.style()), theme.text.gamma_multiply(0.6));
        }
        if n > 1 {
            let middle = visible.center().y - area.min.y - PAGE_GAP / 2.0;
            shown = Some((((middle / (ph + PAGE_GAP)).floor().max(0.0) as usize).min(n - 1) + 1, n));
        }
    });
    if let Some((n, of)) = shown {
        page_badge(ui, out.inner_rect, n, of);
    }
}

/// A shape on a slide whose top left is `origin`.
fn draw_shape(p: &egui::Painter, lay: &Lay, origin: egui::Pos2, sh: &Shape) {
    let s = lay.s;
    let [x, y, w, h] = sh.rect;
    let r = egui::Rect::from_min_size(origin + egui::vec2(x * s, y * s), egui::vec2(w * s, h * s));
    let fill = sh.fill.map(|c| lay.tone(rgb(c)));
    let stroke = sh.line.map(|(c, w)| egui::Stroke::new((w * s).max(0.75), lay.tone(rgb(c))));
    match sh.geom {
        Geom::Rect => {
            if let Some(f) = fill {
                p.rect_filled(r, 0.0, f);
            }
            if let Some(st) = stroke {
                p.rect_stroke(r, 0.0, st, egui::StrokeKind::Middle);
            }
        }
        Geom::RoundRect => {
            // PowerPoint's default corner: a sixth of the shorter side.
            let radius = (r.width().min(r.height()) / 6.0).min(255.0) as u8;
            if let Some(f) = fill {
                p.rect_filled(r, radius, f);
            }
            if let Some(st) = stroke {
                p.rect_stroke(r, radius, st, egui::StrokeKind::Middle);
            }
        }
        Geom::Ellipse => {
            if let Some(f) = fill {
                p.add(egui::Shape::ellipse_filled(r.center(), r.size() / 2.0, f));
            }
            if let Some(st) = stroke {
                p.add(egui::Shape::ellipse_stroke(r.center(), r.size() / 2.0, st));
            }
        }
        Geom::Line { flip_h, flip_v } => {
            let (x0, x1) = if flip_h { (r.max.x, r.min.x) } else { (r.min.x, r.max.x) };
            let (y0, y1) = if flip_v { (r.max.y, r.min.y) } else { (r.min.y, r.max.y) };
            let st = stroke.unwrap_or(egui::Stroke::new(1.0, lay.tone(INK)));
            p.line_segment([egui::pos2(x0, y0), egui::pos2(x1, y1)], st);
        }
    }
    let ink = lay.tone(INK);
    match &sh.content {
        ShapeContent::Empty => {}
        ShapeContent::Image(idx) => {
            if let Some(tex) = lay.textures.get(*idx) {
                p.image(tex.id(), r, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), egui::Color32::WHITE);
            }
        }
        ShapeContent::Text(tb) => {
            let [l, t, rr, b] = tb.inset.map(|v| v * s);
            let inner_w = (r.width() - l - rr).max(4.0);
            let (items, th) = Lay::stack(lay.blocks(&tb.blocks, inner_w));
            let top = match tb.anchor {
                VAnchor::Top => r.min.y + t,
                VAnchor::Middle => (r.min.y + t + r.max.y - b) / 2.0 - th / 2.0,
                VAnchor::Bottom => r.max.y - b - th,
            };
            let offset = egui::vec2(r.min.x + l, top);
            for item in &items {
                item.paint(p, offset, ink);
            }
        }
        ShapeContent::Table(t) => {
            let mut pieces = Vec::new();
            lay.table(t, r.width(), &mut pieces);
            let (items, _) = Lay::stack(pieces);
            for item in &items {
                item.paint(p, r.min.to_vec2(), ink);
            }
        }
        ShapeContent::Missing(what) => {
            let gray = lay.tone(egui::Color32::from_gray(140));
            p.extend(egui::Shape::dashed_line(&[r.left_top(), r.right_top(), r.right_bottom(), r.left_bottom(), r.left_top()], egui::Stroke::new(1.0, gray), 6.0, 4.0));
            p.text(r.center(), egui::Align2::CENTER_CENTER, format!("[{what}]"), egui::FontId::proportional((14.0 * s).clamp(8.0, 40.0)), gray);
        }
    }
}

/// Something drawn, relative to the top left of the piece it is in.
enum Item {
    Text(egui::Pos2, std::sync::Arc<egui::Galley>),
    Image(egui::TextureId, egui::Rect),
    Fill(egui::Rect, egui::Color32),
    Frame(egui::Rect, f32, egui::Color32),
}

impl Item {
    fn paint(&self, p: &egui::Painter, offset: egui::Vec2, ink: egui::Color32) {
        match self {
            Item::Text(pos, g) => p.galley(*pos + offset, g.clone(), ink),
            Item::Image(tex, r) => {
                p.image(*tex, r.translate(offset), egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), egui::Color32::WHITE);
            }
            Item::Fill(r, c) => {
                p.rect_filled(r.translate(offset), 0.0, *c);
            }
            Item::Frame(r, w, c) => {
                p.rect_stroke(r.translate(offset), 0.0, egui::Stroke::new(*w, *c), egui::StrokeKind::Middle);
            }
        }
    }

    fn shift(self, by: egui::Vec2) -> Self {
        match self {
            Item::Text(pos, g) => Item::Text(pos + by, g),
            Item::Image(t, r) => Item::Image(t, r.translate(by)),
            Item::Fill(r, c) => Item::Fill(r.translate(by), c),
            Item::Frame(r, w, c) => Item::Frame(r.translate(by), w, c),
        }
    }
}

/// A paragraph or a table row: what a page holds or, at the lines in `cuts`, splits.
#[derive(Default)]
struct Piece {
    items: Vec<Item>,
    h: f32,
    /// Where it may be split across pages: the tops of its lines.
    cuts: Vec<f32>,
    before: f32,
    after: f32,
    contextual: bool,
    /// Starts a new page.
    page_break: bool,
}

/// The space between two pieces one above the other.
fn between(above: &Piece, below: &Piece) -> f32 {
    if above.contextual && below.contextual { 0.0 } else { above.after + below.before }
}

/// Where a piece, or the part of it from `y0` to `y1`, stands: on which page, how far below the
/// top margin.
struct Placed {
    page: usize,
    piece: usize,
    y0: f32,
    y1: f32,
    at: f32,
}

/// The pieces on pages `room` high. A piece that does not fit is split at the last line that
/// does, or moved to the next page; one taller than a page with nowhere to split it is cut.
fn paginate(pieces: &[Piece], room: f32) -> (Vec<Placed>, usize) {
    let mut placed = Vec::new();
    let (mut page, mut y) = (0usize, 0.0f32);
    let mut prev: Option<&Piece> = None;
    for (i, p) in pieces.iter().enumerate() {
        if p.page_break && y > 0.0 {
            page += 1;
            y = 0.0;
        }
        if y > 0.0 {
            y += prev.map_or(0.0, |q| between(q, p));
        }
        let mut start = 0.0;
        loop {
            let rest = p.h - start;
            if y + rest <= room + 0.5 {
                placed.push(Placed { page, piece: i, y0: start, y1: p.h, at: y });
                y += rest;
                break;
            }
            let fits = room - y;
            match p.cuts.iter().copied().filter(|c| *c > start + 0.5 && *c - start <= fits).max_by(f32::total_cmp) {
                Some(cut) => {
                    placed.push(Placed { page, piece: i, y0: start, y1: cut, at: y });
                    start = cut;
                }
                None if y > 0.0 => {}
                None => {
                    let cut = start + room;
                    placed.push(Placed { page, piece: i, y0: start, y1: cut, at: 0.0 });
                    start = cut;
                }
            }
            page += 1;
            y = 0.0;
        }
        prev = Some(p);
    }
    (placed, page + 1)
}

/// Sets text at a scale: `s` pixels to the point.
struct Lay<'a> {
    painter: egui::Painter,
    textures: &'a [egui::TextureHandle],
    s: f32,
    /// The document's normal text size, points.
    base: f32,
    /// Set on dark paper.
    dark: bool,
    /// The page being set and the number of pages, for page number fields.
    page_no: std::cell::Cell<(usize, usize)>,
}

/// How much bigger than normal text a heading is when the document does not say.
fn heading_scale(h: Option<u8>) -> f32 {
    match h {
        Some(0) => 2.2,
        Some(1) => 1.6,
        Some(2) => 1.35,
        Some(3) => 1.2,
        Some(_) => 1.05,
        None => 1.0,
    }
}

impl Lay<'_> {
    /// A color of the page as it is drawn: turned round on dark paper.
    fn tone(&self, c: egui::Color32) -> egui::Color32 {
        if self.dark { night(c) } else { c }
    }

    fn blocks(&self, blocks: &[Block], width: f32) -> Vec<Piece> {
        let mut out = Vec::new();
        let mut page_break = false;
        for b in blocks {
            let at = out.len();
            match b {
                Block::Para(p) => out.push(self.para(p, width)),
                Block::Table(t) => self.table(t, width, &mut out),
                // Only a workbook has sheets, and it is drawn by `sheet_view`.
                Block::Sheet(_) => {}
                Block::PageBreak => page_break = true,
            }
            if page_break {
                if let Some(first) = out.get_mut(at) {
                    first.page_break = true;
                    page_break = false;
                }
            }
        }
        out
    }

    /// Pieces stacked as one, for a table cell.
    fn stack(pieces: Vec<Piece>) -> (Vec<Item>, f32) {
        let mut items = Vec::new();
        let mut y = 0.0;
        let mut prev: Option<Piece> = None;
        for mut p in pieces {
            if let Some(q) = &prev {
                y += between(q, &p);
            }
            items.extend(std::mem::take(&mut p.items).into_iter().map(|i| i.shift(egui::vec2(0.0, y))));
            y += p.h;
            prev = Some(p);
        }
        (items, y)
    }

    fn format(&self, st: &SpanStyle, heading: Option<u8>, line: f32) -> egui::TextFormat {
        let pt = (self.base * st.size.unwrap_or_else(|| heading_scale(heading))).clamp(3.0, 200.0);
        let size = pt * self.s;
        let bold = st.bold || (heading.is_some() && st.size.is_none());
        let family = if bold { egui::FontFamily::Name(BOLD.into()) } else { egui::FontFamily::Proportional };
        let color = self.tone(st.color.map_or(INK, |c| egui::Color32::from_rgb(c[0], c[1], c[2])));
        let stroke = egui::Stroke::new((pt * self.s / 14.0).max(0.6), color);
        egui::TextFormat {
            font_id: egui::FontId::new(size.max(1.0), family),
            color,
            italics: st.italic,
            underline: if st.underline { stroke } else { egui::Stroke::NONE },
            strikethrough: if st.strike { stroke } else { egui::Stroke::NONE },
            line_height: Some(size * LINE * line),
            ..Default::default()
        }
    }

    fn new_job(width: f32, align: Align) -> LayoutJob {
        let mut job = LayoutJob::default();
        job.wrap.max_width = width.max(4.0);
        job.halign = match align {
            Align::Center => egui::Align::Center,
            Align::Right => egui::Align::RIGHT,
            _ => egui::Align::LEFT,
        };
        job.justify = align == Align::Justify;
        job
    }

    fn para(&self, p: &Para, width: f32) -> Piece {
        let s = self.s;
        let line = p.line.unwrap_or(1.0).clamp(0.5, 4.0);
        let heading_gap = if p.heading.is_some() { self.base * 0.9 } else { 0.0 };
        let mut piece = Piece {
            before: p.before.unwrap_or(heading_gap) * s,
            after: p.after.unwrap_or(heading_gap / 3.0) * s,
            contextual: p.contextual,
            ..Default::default()
        };
        let indent = p.left.map_or(p.level as f32 * INDENT * s, |l| l * s).min(width * 0.6);
        let hang = p.hang.unwrap_or(HANG) * s;
        let avail = (width - indent).max(20.0);
        let first_style = p.inlines.iter().find_map(|i| if let Inline::Text(t) = i { Some(t.style.clone()) } else { None }).unwrap_or_default();
        let mut y = 0.0f32;
        let mut job = Self::new_job(avail, p.align);
        for inline in &p.inlines {
            match inline {
                Inline::Text(t) => job.append(&t.text, 0.0, self.format(&t.style, p.heading, line)),
                Inline::Field(field, st) => {
                    let (n, of) = self.page_no.get();
                    let text = match field {
                        Field::Page => n.to_string(),
                        Field::Pages => of.to_string(),
                    };
                    job.append(&text, 0.0, self.format(st, p.heading, line));
                }
                Inline::Missing(what) => {
                    let mut f = self.format(&SpanStyle { italic: true, ..Default::default() }, None, line);
                    f.color = self.tone(egui::Color32::from_gray(120));
                    job.append(&format!("[{what}]"), 0.0, f);
                }
                Inline::Image { idx, w, h } => {
                    self.flush(&mut piece, &mut y, &mut job, indent, avail, p.align);
                    self.image(&mut piece, &mut y, *idx, (*w, *h), indent, avail, p.align);
                    job = Self::new_job(avail, p.align);
                }
            }
        }
        self.flush(&mut piece, &mut y, &mut job, indent, avail, p.align);
        if y == 0.0 {
            // An empty paragraph is an empty line in the document.
            y = self.base * first_style.size.unwrap_or_else(|| heading_scale(p.heading)) * LINE * line * s;
        }
        if let Some(m) = &p.marker {
            let f = self.format(&SpanStyle { underline: false, strike: false, ..first_style }, p.heading, line);
            let mut mj = LayoutJob::default();
            mj.append(m, 0.0, f);
            let g = self.painter.layout_job(mj);
            let x = (indent - hang).max(0.0);
            // A marker wider than the hang pushes nothing: it is drawn over the space before.
            piece.items.insert(0, Item::Text(egui::pos2(x.min(indent - g.size().x - 2.0).max(0.0), 0.0), g));
        }
        piece.h = y;
        piece
    }

    /// Lays out and adds what is in `job`, leaving it empty for what follows.
    fn flush(&self, piece: &mut Piece, y: &mut f32, job: &mut LayoutJob, indent: f32, avail: f32, align: Align) {
        if job.text.trim().is_empty() {
            *job = Self::new_job(avail, align);
            return;
        }
        let taken = std::mem::replace(job, Self::new_job(avail, align));
        let g = self.painter.layout_job(taken);
        let x = match align {
            Align::Center => indent + avail / 2.0,
            Align::Right => indent + avail,
            _ => indent,
        };
        for row in g.rows.iter().skip(1) {
            piece.cuts.push(*y + row.pos.y);
        }
        if *y > 0.0 {
            piece.cuts.push(*y);
        }
        let h = g.size().y;
        piece.items.push(Item::Text(egui::pos2(x, *y), g));
        *y += h;
    }

    /// A picture at the size the document gives it, no wider than the text; at its own size, as
    /// pixels at 96 dpi, when the document gives none.
    #[allow(clippy::too_many_arguments)]
    fn image(&self, piece: &mut Piece, y: &mut f32, idx: usize, size: (f32, f32), indent: f32, avail: f32, align: Align) {
        let Some(tex) = self.textures.get(idx) else { return };
        let [tw, th] = tex.size();
        if tw == 0 || th == 0 {
            return;
        }
        let (w_pt, h_pt) = size;
        let want = if w_pt > 0.0 { w_pt } else { tw as f32 * 0.75 } * self.s;
        let dw = want.min(avail).max(4.0);
        let aspect = if w_pt > 0.0 && h_pt > 0.0 { h_pt / w_pt } else { th as f32 / tw as f32 };
        let dh = dw * aspect;
        let x = match align {
            Align::Center => indent + (avail - dw) / 2.0,
            Align::Right => indent + avail - dw,
            _ => indent,
        };
        if *y > 0.0 {
            piece.cuts.push(*y);
        }
        piece.items.push(Item::Image(tex.id(), egui::Rect::from_min_size(egui::pos2(x, *y), egui::vec2(dw, dh))));
        *y += dh;
    }

    /// A table as one piece per row, so a page can break between rows.
    fn table(&self, t: &Table, width: f32, out: &mut Vec<Piece>) {
        let s = self.s;
        let ncols = t.rows.iter().map(|r| r.iter().map(|c| c.span).sum::<usize>()).max().unwrap_or(0).max(1);
        let mut widths: Vec<f32> = if t.widths.len() == ncols && t.widths.iter().all(|w| *w > 0.0) { t.widths.clone() } else { vec![1.0; ncols] };
        let total: f32 = widths.iter().sum();
        for w in &mut widths {
            *w *= width / total;
        }
        // Word's default cell margins: a little over 5 pt at the sides, none above and below.
        let (pad_x, pad_y) = (5.4 * s, 1.0 * s);
        let rule = (0.5 * s).max(1.0);
        for row in &t.rows {
            let mut x = 0.0;
            let mut col = 0;
            let mut row_h = self.base * LINE * s + 2.0 * pad_y;
            let mut cells = Vec::new();
            for cell in row {
                let end = (col + cell.span).min(ncols);
                let cw: f32 = widths[col.min(ncols)..end].iter().sum::<f32>().max(4.0);
                col = end;
                let (items, h) = Self::stack(self.blocks(&cell.blocks, (cw - 2.0 * pad_x).max(4.0)));
                row_h = row_h.max(h + 2.0 * pad_y);
                cells.push((x, cw, cell.fill, items));
                x += cw;
            }
            let mut piece = Piece { h: row_h, ..Default::default() };
            let mut frames = Vec::new();
            for (x, cw, fill, items) in cells {
                let r = egui::Rect::from_min_size(egui::pos2(x, 0.0), egui::vec2(cw, row_h));
                if let Some(f) = fill {
                    piece.items.push(Item::Fill(r, self.tone(egui::Color32::from_rgb(f[0], f[1], f[2]))));
                }
                piece.items.extend(items.into_iter().map(|i| i.shift(egui::vec2(x + pad_x, pad_y))));
                frames.push(Item::Frame(r, rule, self.tone(RULE)));
            }
            piece.items.extend(frames);
            out.push(piece);
        }
        if let Some(last) = out.last_mut() {
            last.after = self.base * 0.5 * s;
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
        crate::browser::take_preview_scroll(ui);
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

#[cfg(test)]
mod tests {
    use super::*;

    fn piece(h: f32, cuts: &[f32]) -> Piece {
        Piece { h, cuts: cuts.to_vec(), ..Default::default() }
    }

    fn pages_of(placed: &[Placed]) -> Vec<(usize, usize, f32, f32)> {
        placed.iter().map(|p| (p.page, p.piece, p.y0, p.y1)).collect()
    }

    #[test]
    fn night_turns_lightness_round_and_keeps_the_hue() {
        let white = night(egui::Color32::WHITE);
        let black = night(egui::Color32::BLACK);
        assert!(white.r() < 50 && white.r() == white.g() && white.g() == white.b(), "white paper is dark gray: {white:?}");
        assert!(black.r() > 200, "black ink is light: {black:?}");
        let blue = night(egui::Color32::from_rgb(0, 0, 160));
        assert!(blue.b() > blue.r() && blue.r() > 120, "dark blue is light blue: {blue:?}");
        assert_eq!(night(egui::Color32::from_rgba_unmultiplied(255, 255, 255, 0)).a(), 0, "transparency is kept");
    }

    #[test]
    fn pieces_fill_a_page_then_split_at_a_line_or_move_on() {
        // 60 fits, 50 more does not: split at its line at 30. The rest starts page two.
        let (placed, pages) = paginate(&[piece(60.0, &[]), piece(50.0, &[30.0])], 100.0);
        assert_eq!(pages_of(&placed), vec![(0, 0, 0.0, 60.0), (0, 1, 0.0, 30.0), (1, 1, 30.0, 50.0)]);
        assert_eq!(pages, 2);
        // With no line to split at it moves whole.
        let (placed, _) = paginate(&[piece(60.0, &[]), piece(50.0, &[])], 100.0);
        assert_eq!(pages_of(&placed), vec![(0, 0, 0.0, 60.0), (1, 1, 0.0, 50.0)]);
    }

    #[test]
    fn spacing_page_breaks_and_pieces_taller_than_a_page() {
        let mut a = piece(40.0, &[]);
        a.after = 10.0;
        let mut b = piece(40.0, &[]);
        b.before = 5.0;
        let (placed, _) = paginate(&[a, b], 100.0);
        assert_eq!(placed[1].at, 55.0);
        // A page break starts a page, but not an empty one at the very start.
        let mut first = piece(10.0, &[]);
        first.page_break = true;
        let mut c = piece(10.0, &[]);
        c.page_break = true;
        let (placed, pages) = paginate(&[first, c], 100.0);
        assert_eq!((placed[0].page, placed[1].page, pages), (0, 1, 2));
        // Too tall and nowhere to split: cut at the page's height.
        let (placed, pages) = paginate(&[piece(250.0, &[])], 100.0);
        assert_eq!(pages_of(&placed), vec![(0, 0, 0.0, 100.0), (1, 0, 100.0, 200.0), (2, 0, 200.0, 250.0)]);
        assert_eq!(pages, 3);
    }
}
