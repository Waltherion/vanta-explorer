//! Office documents read by ourselves: Word and Writer text with its formatting, tables and
//! pictures, Excel and Calc sheets as a grid, PowerPoint and Impress slides. No office suite is
//! involved. Each is a zip of XML, and what a preview needs from it is a small part of either
//! standard.
//!
//! Parsing gives a `RichDoc`: plain data, built on the preview thread. `view` draws it.

mod docx;
mod odf;
mod pptx;
pub mod view;
mod xlsx;

use std::collections::HashMap;
use std::io::Read;
use std::path::Path;

/// More than this many blocks and the rest is left out, with a note: a preview of the first
/// hundred pages says what a document is as well as all of them.
const MAX_BLOCKS: usize = 4000;
/// Rows and columns of a sheet shown at most.
const MAX_ROWS: usize = 1000;
const MAX_COLS: usize = 50;
/// The XML of one part is read up to this; a bigger one is not a document a preview can help with.
const MAX_XML: u64 = 96 << 20;
/// Pictures up to this size are shown; bigger ones get a placeholder.
const MAX_PICTURE: u64 = 40 << 20;
/// Pictures are kept no wider or taller than this. The preview pane is narrower than it.
const PICTURE_PX: u32 = 1200;
/// At most this many pictures per document are decoded.
const MAX_PICTURES: usize = 80;

pub struct RichDoc {
    pub blocks: Vec<Block>,
    pub images: Vec<egui::ColorImage>,
    /// For a workbook: the names of its sheets. The shown sheet is the preview's page.
    pub sheets: Option<Vec<String>>,
    /// Said under the document, e.g. that it was cut short.
    pub note: Option<String>,
    /// The page the text is set on.
    pub page: PageSetup,
    /// The size of the document's normal text, in points.
    pub base_pt: f32,
    /// What stands at the top and the bottom of every page.
    pub header: Option<Running>,
    pub footer: Option<Running>,
    /// For a presentation: its slides, each a page of `page`'s size.
    pub slides: Option<Vec<Slide>>,
}

/// A slide: its background and its shapes, bottom first.
pub struct Slide {
    pub background: [u8; 3],
    /// A picture over the whole slide.
    pub background_image: Option<usize>,
    pub shapes: Vec<Shape>,
}

#[derive(Clone)]
pub struct Shape {
    /// Left, top, width, height in points.
    pub rect: [f32; 4],
    pub geom: Geom,
    pub fill: Option<[u8; 3]>,
    /// Color and width in points.
    pub line: Option<([u8; 3], f32)>,
    pub content: ShapeContent,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Geom {
    Rect,
    RoundRect,
    Ellipse,
    /// From one corner of the rectangle to the other, flipped as the document says.
    Line { flip_h: bool, flip_v: bool },
}

#[derive(Clone)]
pub enum ShapeContent {
    Empty,
    Text(TextBox),
    Image(usize),
    Table(Table),
    /// Something not drawn: a chart, a diagram.
    Missing(String),
}

#[derive(Clone)]
pub struct TextBox {
    pub blocks: Vec<Block>,
    pub anchor: VAnchor,
    /// Left, top, right, bottom, points.
    pub inset: [f32; 4],
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum VAnchor {
    #[default]
    Top,
    Middle,
    Bottom,
}

/// A header or a footer.
#[derive(Default)]
pub struct Running {
    pub blocks: Vec<Block>,
    /// The first page's own, when the document gives the first page another (empty: none).
    pub first: Option<Vec<Block>>,
}

impl Running {
    pub fn for_page(&self, page: usize) -> &[Block] {
        match (&self.first, page) {
            (Some(first), 0) => first,
            _ => &self.blocks,
        }
    }
}

/// A number filled in when the page is drawn.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Field {
    Page,
    Pages,
}

impl Field {
    /// The field a Word field instruction (`PAGE`, `NUMPAGES \* Arabic`) is, if one we fill in.
    fn from_instruction(instr: &str) -> Option<Field> {
        match instr.split_whitespace().next()?.to_ascii_uppercase().as_str() {
            "PAGE" => Some(Field::Page),
            "NUMPAGES" | "SECTIONPAGES" => Some(Field::Pages),
            _ => None,
        }
    }
}

/// A page's size and margins, in points.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PageSetup {
    pub width: f32,
    pub height: f32,
    /// Top, right, bottom, left.
    pub margin: [f32; 4],
    /// From the top of the page to the header, and from the bottom to the footer.
    pub header: f32,
    pub footer: f32,
}

impl Default for PageSetup {
    /// A4 with an inch all round, what a document that does not say is printed on here.
    fn default() -> Self {
        Self { width: 595.3, height: 841.9, margin: [72.0; 4], header: 36.0, footer: 36.0 }
    }
}

impl PageSetup {
    /// The setup unless it is nonsense (no room for text, a page a meter wide), else the default.
    fn sane(self) -> Self {
        let [t, r, b, l] = self.margin;
        let ok = (100.0..=3000.0).contains(&self.width)
            && (100.0..=3000.0).contains(&self.height)
            && self.margin.iter().all(|m| (0.0..=self.width.min(self.height) / 2.5).contains(m))
            && self.width - l - r >= 50.0
            && self.height - t - b >= 50.0
            && (0.0..=self.height / 3.0).contains(&self.header)
            && (0.0..=self.height / 3.0).contains(&self.footer);
        if ok { self } else { Self::default() }
    }
}

#[derive(Clone)]
pub enum Block {
    Para(Para),
    Table(Table),
    Sheet(Sheet),
    PageBreak,
}

#[derive(Clone, Default)]
pub struct Para {
    pub inlines: Vec<Inline>,
    /// 0 for a title, 1 to 6 for headings.
    pub heading: Option<u8>,
    pub align: Align,
    /// Indentation in steps (list levels, or an indented paragraph).
    pub level: u8,
    /// The bullet or number in front of a list item.
    pub marker: Option<String>,
    /// Space above and below, in points; `None` is the view's default.
    pub before: Option<f32>,
    pub after: Option<f32>,
    /// Line spacing as a multiple of single spacing.
    pub line: Option<f32>,
    /// No space between this and a neighbor of the same kind (Word's list paragraphs).
    pub contextual: bool,
    /// Where the text starts, points from the left; `None`: by `level`.
    pub left: Option<f32>,
    /// How far left of the text the marker hangs, points.
    pub hang: Option<f32>,
}

#[derive(Clone)]
pub enum Inline {
    Text(Span),
    /// A picture, by index into `RichDoc::images`, at the size the document gives it in points.
    Image { idx: usize, w: f32, h: f32 },
    /// Something there that is not shown: a chart, a picture in a format we do not read.
    Missing(String),
    /// The page number or the number of pages.
    Field(Field, SpanStyle),
}

#[derive(Clone)]
pub struct Span {
    pub text: String,
    pub style: SpanStyle,
}

#[derive(Clone, Default, PartialEq, Debug)]
pub struct SpanStyle {
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strike: bool,
    pub color: Option<[u8; 3]>,
    /// The size relative to the document's normal text; `None` is normal.
    pub size: Option<f32>,
}

#[derive(Clone, Copy, Default, PartialEq, Debug)]
pub enum Align {
    #[default]
    Left,
    Center,
    Right,
    Justify,
}

#[derive(Clone)]
pub struct Table {
    /// Column widths, in any unit: only their proportions are used.
    pub widths: Vec<f32>,
    pub rows: Vec<Vec<Cell>>,
}

#[derive(Clone)]
pub struct Cell {
    pub blocks: Vec<Block>,
    /// How many grid columns it covers.
    pub span: usize,
    pub fill: Option<[u8; 3]>,
}

#[derive(Clone)]
pub struct Sheet {
    /// Row number (1-based, as the sheet numbers them) and its cells, column A first.
    pub rows: Vec<(u32, Vec<SheetCell>)>,
    pub cols: usize,
    pub more_rows: bool,
    pub more_cols: bool,
}

#[derive(Clone, Default, Debug, PartialEq)]
pub struct SheetCell {
    pub text: String,
    /// Numbers and dates stand to the right, as a spreadsheet shows them.
    pub right: bool,
}

/// The locale numbers in sheets are shown for (the `numberLocale` setting); empty: the system's.
static NUMBER_LOCALE: std::sync::RwLock<String> = std::sync::RwLock::new(String::new());

/// Sets the locale numbers are shown for. Returns whether it changed, so a sheet on screen can
/// be read again.
pub fn set_number_locale(name: &str) -> bool {
    let Ok(mut current) = NUMBER_LOCALE.write() else { return false };
    let name = name.trim();
    if *current == name {
        return false;
    }
    *current = name.to_string();
    true
}

fn number_locale() -> String {
    NUMBER_LOCALE.read().map(|s| s.clone()).unwrap_or_default()
}

/// Whether we read this kind of document ourselves.
pub fn handles(ext: &str) -> bool {
    matches!(ext, "docx" | "xlsx" | "pptx" | "odt" | "ods" | "odp")
}

/// The document at `path`; `page` is the sheet of a workbook.
pub fn render(path: &Path, ext: &str, page: u32) -> Result<RichDoc, String> {
    let mut head = [0u8; 4];
    if let Ok(mut f) = std::fs::File::open(path) {
        let _ = f.read_exact(&mut head);
    }
    // A password-protected Office file is not a zip but an OLE container around the encrypted
    // one, and so is a .doc renamed .docx.
    if head == [0xD0, 0xCF, 0x11, 0xE0] {
        return Err("encrypted, or an old binary Office file (.doc, .xls)".into());
    }
    let mut pkg = Package::open(path)?;
    match ext {
        "docx" => docx::read(&mut pkg),
        "xlsx" => xlsx::read(&mut pkg, page),
        "pptx" => pptx::read(&mut pkg),
        "odt" => odf::text(&mut pkg),
        "ods" => odf::sheet(&mut pkg, page),
        "odp" => odf::presentation(&mut pkg),
        _ => Err("not an office document".into()),
    }
}

/// The zip a document is, with the reads it needs capped.
struct Package {
    zip: zip::ZipArchive<std::fs::File>,
}

impl Package {
    fn open(path: &Path) -> Result<Self, String> {
        let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
        let zip = zip::ZipArchive::new(file).map_err(|_| "not a valid document (no zip inside)".to_string())?;
        Ok(Self { zip })
    }

    fn bytes(&mut self, name: &str, cap: u64) -> Option<Vec<u8>> {
        let entry = self.zip.by_name(name).ok()?;
        if entry.size() > cap {
            return None;
        }
        let mut out = Vec::with_capacity(entry.size() as usize);
        entry.take(cap).read_to_end(&mut out).ok()?;
        Some(out)
    }

    fn text(&mut self, name: &str) -> Option<String> {
        self.bytes(name, MAX_XML).map(|b| String::from_utf8_lossy(&b).into_owned())
    }

    fn has(&mut self, name: &str) -> bool {
        self.zip.by_name(name).is_ok()
    }
}

/// Parsed XML, DTDs refused (so no entity expansion), with a cap on the number of nodes.
fn xml(text: &str) -> Result<roxmltree::Document<'_>, String> {
    let opts = roxmltree::ParsingOptions { allow_dtd: false, nodes_limit: 8_000_000, ..Default::default() };
    roxmltree::Document::parse_with_options(text, opts).map_err(|e| format!("broken XML: {e}"))
}

/// The pictures of a document, each decoded once however often it appears.
#[derive(Default)]
struct Pictures {
    images: Vec<egui::ColorImage>,
    by_name: HashMap<String, Option<usize>>,
}

impl Pictures {
    /// The picture at `name` inside the package: its index, or a word for what it is when it
    /// cannot be shown.
    fn get(&mut self, pkg: &mut Package, name: &str) -> Result<usize, String> {
        let kind = || {
            let ext = name.rsplit('.').next().unwrap_or("").to_ascii_uppercase();
            if ext.is_empty() { "picture".to_string() } else { format!("picture ({ext})") }
        };
        if let Some(hit) = self.by_name.get(name) {
            return hit.ok_or_else(kind);
        }
        let got = if self.images.len() >= MAX_PICTURES {
            None
        } else {
            pkg.bytes(name, MAX_PICTURE).and_then(|b| decode_picture(&b))
        };
        let idx = got.map(|img| {
            self.images.push(img);
            self.images.len() - 1
        });
        self.by_name.insert(name.to_string(), idx);
        idx.ok_or_else(kind)
    }
}

/// A picture as egui wants it, no larger than the preview could use. EMF and WMF, the vector
/// formats of old Office drawings, are not read.
fn decode_picture(bytes: &[u8]) -> Option<egui::ColorImage> {
    let mut reader = image::ImageReader::new(std::io::Cursor::new(bytes)).with_guessed_format().ok()?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(20_000);
    limits.max_image_height = Some(20_000);
    limits.max_alloc = Some(512 << 20);
    reader.limits(limits);
    let img = reader.decode().ok()?;
    let img = if img.width() > PICTURE_PX || img.height() > PICTURE_PX { img.thumbnail(PICTURE_PX, PICTURE_PX) } else { img };
    let rgba = img.to_rgba8();
    Some(egui::ColorImage::from_rgba_unmultiplied([rgba.width() as usize, rgba.height() as usize], rgba.as_raw()))
}

/// `#rrggbb` or `rrggbb`; anything else (Word's `auto`, a theme color) is none.
fn hex_color(s: &str) -> Option<[u8; 3]> {
    let h = s.trim().trim_start_matches('#');
    if h.len() != 6 {
        return None;
    }
    let v = u32::from_str_radix(h, 16).ok()?;
    Some([(v >> 16) as u8, (v >> 8) as u8, v as u8])
}

/// A path inside the package, relative to the part that refers to it (`word/` for
/// `word/document.xml`), with `..` resolved. A leading `/` is from the package root.
fn resolve(base_dir: &str, target: &str) -> String {
    let mut parts: Vec<&str> = if target.starts_with('/') { Vec::new() } else { base_dir.split('/').filter(|p| !p.is_empty()).collect() };
    for p in target.split('/') {
        match p {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            p => parts.push(p),
        }
    }
    parts.join("/")
}

/// The relationships of a part (`word/_rels/document.xml.rels`): id to the part it points at,
/// external links left out.
fn relationships(pkg: &mut Package, rels: &str, base_dir: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let Some(text) = pkg.text(rels) else { return out };
    let Ok(doc) = xml(&text) else { return out };
    for r in doc.descendants().filter(|n| n.is_element() && n.tag_name().name() == "Relationship") {
        if r.attribute("TargetMode") == Some("External") {
            continue;
        }
        if let (Some(id), Some(target)) = (r.attribute("Id"), r.attribute("Target")) {
            out.insert(id.to_string(), resolve(base_dir, target));
        }
    }
    out
}

/// A list counter written the way a list style asks for it.
fn format_number(n: u32, fmt: &str) -> String {
    match fmt {
        "lowerLetter" | "a" => letters(n, false),
        "upperLetter" | "A" => letters(n, true),
        "lowerRoman" | "i" => roman(n).to_lowercase(),
        "upperRoman" | "I" => roman(n),
        "decimalZero" => format!("{n:02}"),
        _ => n.to_string(),
    }
}

fn letters(n: u32, upper: bool) -> String {
    // a..z, then aa, bb, as Word does.
    let n = n.max(1) - 1;
    let c = (b'a' + (n % 26) as u8) as char;
    let s: String = std::iter::repeat_n(c, (n / 26 + 1) as usize).collect();
    if upper { s.to_uppercase() } else { s }
}

fn roman(mut n: u32) -> String {
    const R: [(u32, &str); 13] = [(1000, "M"), (900, "CM"), (500, "D"), (400, "CD"), (100, "C"), (90, "XC"), (50, "L"), (40, "XL"), (10, "X"), (9, "IX"), (5, "V"), (4, "IV"), (1, "I")];
    let mut s = String::new();
    for (v, r) in R {
        while n >= v {
            s.push_str(r);
            n -= v;
        }
    }
    s
}

/// A bullet as it can be drawn. Word writes its bullets in the Symbol and Wingdings fonts, at
/// code points that mean nothing in any other font.
fn bullet(text: &str) -> String {
    match text.chars().next() {
        None => "•".into(),
        Some('o') => "◦".into(),
        Some(c) if ('\u{F000}'..='\u{F0FF}').contains(&c) => match c as u32 & 0xFF {
            0xA7 => "▪".into(),
            0xD8 | 0xE0 => "➢".into(),
            0xFC => "✓".into(),
            _ => "•".into(),
        },
        Some('\u{2022}' | '\u{25CF}' | '\u{25AA}' | '\u{25E6}' | '\u{2013}' | '-' | '*' | '\u{27A2}' | '\u{2713}') => text.to_string(),
        Some(_) => "•".into(),
    }
}

/// Appends text to the last span when the style is the same, so a paragraph Word wrote as forty
/// runs is one span.
fn push_text(inlines: &mut Vec<Inline>, text: &str, style: &SpanStyle) {
    if text.is_empty() {
        return;
    }
    if let Some(Inline::Text(last)) = inlines.last_mut() {
        if last.style == *style {
            last.text.push_str(text);
            return;
        }
    }
    inlines.push(Inline::Text(Span { text: text.to_string(), style: style.clone() }));
}

/// A1-style column letters to a 0-based index.
fn column_index(cell_ref: &str) -> Option<usize> {
    let mut n = 0usize;
    let mut any = false;
    for c in cell_ref.chars() {
        if c.is_ascii_alphabetic() {
            n = n * 26 + (c.to_ascii_uppercase() as usize - 'A' as usize + 1);
            any = true;
        } else {
            break;
        }
    }
    any.then(|| n - 1)
}

/// 0-based column index to its letters.
pub fn column_name(mut i: usize) -> String {
    let mut s = Vec::new();
    loop {
        s.push(b'A' + (i % 26) as u8);
        if i < 26 {
            break;
        }
        i = i / 26 - 1;
    }
    s.reverse();
    String::from_utf8(s).unwrap_or_default()
}

/// A number as a sheet shows it without a format: whole numbers without a point, others with at
/// most ten significant digits and no trailing zeros.
fn plain_number(v: f64) -> String {
    if v.fract() == 0.0 && v.abs() < 1e15 {
        return format!("{}", v as i64);
    }
    let s = format!("{:.*}", (10 - v.abs().log10().ceil().max(0.0) as usize).min(10), v);
    let s = s.trim_end_matches('0').trim_end_matches('.');
    s.to_string()
}

#[cfg(test)]
mod tests;
