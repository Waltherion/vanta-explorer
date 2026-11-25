//! OpenDocument (.odt, .ods), as LibreOffice and OnlyOffice write it: `content.xml`, with the
//! styles of `styles.xml` behind its own.

use super::{Align, Block, Cell, Field, Geom, Inline, PageSetup, Package, Para, Running, Shape, ShapeContent, Slide, TextBox, VAnchor, Pictures, RichDoc, Sheet, SheetCell, SpanStyle, Table};
use roxmltree::Node;
use std::collections::HashMap;

fn local<'a>(n: Node<'a, '_>) -> &'a str {
    n.tag_name().name()
}

/// An element in the OpenDocument namespace `ns` (`text`, `table`, `draw`, ...). `draw` is the
/// prefix; the namespace is called `drawing`.
fn is(n: Node, ns: &str, name: &str) -> bool {
    let ns = if ns == "draw" { "drawing" } else { ns };
    n.is_element() && local(n) == name && n.tag_name().namespace().is_some_and(|u| u.ends_with(&format!(":{ns}:1.0")))
}

/// An attribute by namespace (`style`, `fo`, `svg`, `text`, `xlink`, ...) and local name. `fo` and
/// `svg` are the "-compatible" namespaces OpenDocument borrows them as.
fn attr<'a>(n: Node<'a, '_>, ns: &str, name: &str) -> Option<&'a str> {
    let ns = if ns == "draw" { "drawing" } else { ns };
    n.attributes()
        .find(|a| a.name() == name && a.namespace().is_some_and(|u| u.ends_with(&format!(":{ns}:1.0")) || u.ends_with(&format!(":{ns}-compatible:1.0")) || u.ends_with(&format!("/{ns}")) || (ns == "fo" && u.ends_with("xsl-fo-compatible:1.0"))))
        .map(|a| a.value())
}

#[derive(Clone, Default, Debug)]
struct Props {
    bold: Option<bool>,
    italic: Option<bool>,
    underline: Option<bool>,
    strike: Option<bool>,
    color: Option<[u8; 3]>,
    /// Points, or a percentage of the parent's size.
    size: Option<Size>,
    align: Option<Align>,
    outline: Option<u8>,
    /// Space above and below a paragraph, points.
    before: Option<f32>,
    after: Option<f32>,
    /// Line spacing, a multiple of single.
    line: Option<f32>,
}

#[derive(Clone, Copy, Debug)]
enum Size {
    Pt(f32),
    Percent(f32),
}

impl Props {
    fn over(&mut self, o: &Props) {
        self.bold = o.bold.or(self.bold);
        self.italic = o.italic.or(self.italic);
        self.underline = o.underline.or(self.underline);
        self.strike = o.strike.or(self.strike);
        self.color = o.color.or(self.color);
        self.size = match (o.size, self.size) {
            (Some(Size::Percent(p)), Some(Size::Pt(pt))) => Some(Size::Pt(pt * p / 100.0)),
            (Some(s), _) => Some(s),
            (None, s) => s,
        };
        self.align = o.align.or(self.align);
        self.outline = o.outline.or(self.outline);
        self.before = o.before.or(self.before);
        self.after = o.after.or(self.after);
        self.line = o.line.or(self.line);
    }

    fn style(&self, normal_pt: f32) -> SpanStyle {
        let pt = match self.size {
            Some(Size::Pt(p)) => Some(p),
            Some(Size::Percent(p)) => Some(normal_pt * p / 100.0),
            None => None,
        };
        SpanStyle {
            bold: self.bold.unwrap_or(false),
            italic: self.italic.unwrap_or(false),
            underline: self.underline.unwrap_or(false),
            strike: self.strike.unwrap_or(false),
            color: self.color,
            size: pt.filter(|p| (p - normal_pt).abs() > 0.25).map(|p| p / normal_pt),
        }
    }
}

fn read_props(style: Node) -> Props {
    let mut p = Props::default();
    for c in style.children().filter(|c| c.is_element()) {
        match local(c) {
            "text-properties" => {
                p.bold = attr(c, "fo", "font-weight").map(|w| w == "bold" || w.parse::<u32>().is_ok_and(|n| n >= 600));
                p.italic = attr(c, "fo", "font-style").map(|s| s == "italic" || s == "oblique");
                p.underline = attr(c, "style", "text-underline-style").map(|s| s != "none");
                p.strike = attr(c, "style", "text-line-through-style").map(|s| s != "none");
                p.color = attr(c, "fo", "color").and_then(super::hex_color);
                p.size = attr(c, "fo", "font-size").and_then(|s| {
                    if let Some(pc) = s.strip_suffix('%') {
                        pc.parse().ok().map(Size::Percent)
                    } else {
                        length_pt(s).map(Size::Pt)
                    }
                });
            }
            "paragraph-properties" => {
                p.align = attr(c, "fo", "text-align").and_then(|a| {
                    Some(match a {
                        "center" => Align::Center,
                        "end" | "right" => Align::Right,
                        "justify" => Align::Justify,
                        "start" | "left" => Align::Left,
                        _ => return None,
                    })
                });
                p.before = attr(c, "fo", "margin-top").and_then(length_pt);
                p.after = attr(c, "fo", "margin-bottom").and_then(length_pt);
                p.line = attr(c, "fo", "line-height").and_then(|v| match v.strip_suffix('%') {
                    Some(pc) => pc.parse::<f32>().ok().map(|p| p / 100.0),
                    // A fixed height: as a multiple of a 12 pt line.
                    None => length_pt(v).map(|pt| pt / 12.0 / 1.17),
                });
            }
            _ => {}
        }
    }
    p.outline = attr(style, "style", "default-outline-level").and_then(|v| v.parse().ok());
    p
}

/// A length with its unit (`12pt`, `2.5cm`, `1in`) in points.
fn length_pt(v: &str) -> Option<f32> {
    let v = v.trim();
    let (num, unit) = v.split_at(v.find(|c: char| c.is_ascii_alphabetic()).unwrap_or(v.len()));
    let n: f32 = num.trim().parse().ok()?;
    Some(match unit {
        "pt" | "" => n,
        "in" => n * 72.0,
        "cm" => n * 72.0 / 2.54,
        "mm" => n * 72.0 / 25.4,
        "px" => n * 0.75,
        "pc" => n * 12.0,
        _ => return None,
    })
}

struct ListLevel {
    bullet: Option<String>,
    format: String,
    prefix: String,
    suffix: String,
    start: u32,
}

#[derive(Default)]
struct Styles {
    styles: HashMap<String, (Option<String>, Props)>,
    lists: HashMap<String, HashMap<u8, ListLevel>>,
    normal_pt: f32,
}

impl Styles {
    /// The styles of `styles.xml` and those `content.xml` adds to them.
    fn read(docs: &[&roxmltree::Document]) -> Self {
        let mut s = Styles { normal_pt: 12.0, ..Default::default() };
        for doc in docs {
            for n in doc.descendants().filter(|n| n.is_element()) {
                if is(n, "style", "default-style") && attr(n, "style", "family") == Some("paragraph") {
                    if let Some(Size::Pt(pt)) = read_props(n).size {
                        s.normal_pt = pt.max(4.0);
                    }
                } else if is(n, "style", "style") {
                    if let Some(name) = attr(n, "style", "name") {
                        s.styles.insert(name.to_string(), (attr(n, "style", "parent-style-name").map(str::to_string), read_props(n)));
                    }
                } else if is(n, "text", "list-style") {
                    let Some(name) = attr(n, "style", "name") else { continue };
                    let mut levels = HashMap::new();
                    for l in n.children().filter(|c| c.is_element()) {
                        let Some(level) = attr(l, "text", "level").and_then(|v| v.parse::<u8>().ok()) else { continue };
                        let bullet = (local(l) == "list-level-style-bullet").then(|| super::bullet(attr(l, "text", "bullet-char").unwrap_or("•")));
                        levels.insert(
                            level,
                            ListLevel {
                                bullet,
                                format: attr(l, "style", "num-format").unwrap_or("").to_string(),
                                prefix: attr(l, "style", "num-prefix").unwrap_or("").to_string(),
                                suffix: attr(l, "style", "num-suffix").unwrap_or("").to_string(),
                                start: attr(l, "text", "start-value").and_then(|v| v.parse().ok()).unwrap_or(1),
                            },
                        );
                    }
                    s.lists.insert(name.to_string(), levels);
                }
            }
        }
        s
    }

    /// A style with its parents laid under it.
    fn props(&self, name: Option<&str>) -> Props {
        let mut chain = Vec::new();
        let mut cur = name.map(str::to_string);
        while let Some(n) = cur {
            let Some((parent, p)) = self.styles.get(&n) else { break };
            chain.push(p);
            if chain.len() > 20 {
                break;
            }
            cur = parent.clone();
        }
        let mut out = Props::default();
        for p in chain.into_iter().rev() {
            out.over(p);
        }
        out
    }
}

struct Reader<'p> {
    pkg: &'p mut Package,
    styles: Styles,
    pictures: Pictures,
    blocks: usize,
    /// Counters of the list being read, one per level.
    counters: Vec<u32>,
    /// What the text being read stands on: a slide shape's style.
    base: Props,
    /// In a slide's outline: its master's name, whose `-outlineN` styles give each list level.
    outline: Option<String>,
}

fn read_both(pkg: &mut Package) -> Result<(String, Option<String>), String> {
    let content = pkg.text("content.xml").ok_or("no content in the file")?;
    Ok((content, pkg.text("styles.xml")))
}

pub fn text(pkg: &mut Package) -> Result<RichDoc, String> {
    let (content, styles_text) = read_both(pkg)?;
    let doc = super::xml(&content)?;
    let styles_doc = styles_text.as_deref().and_then(|t| super::xml(t).ok());
    let mut docs = Vec::new();
    if let Some(d) = &styles_doc {
        docs.push(d);
    }
    docs.push(&doc);
    let styles = Styles::read(&docs);
    let body = doc.descendants().find(|n| is(*n, "office", "text")).ok_or("no text in the document")?;
    let mut r = Reader { pkg, styles, pictures: Pictures::default(), blocks: 0, counters: Vec::new(), base: Props::default(), outline: None };
    let mut blocks = Vec::new();
    r.body(body, &mut blocks, None, 0);
    let note = (r.blocks > super::MAX_BLOCKS).then(|| "The rest of the document is not shown.".to_string());
    let mut page = styles_doc.as_ref().map(page_setup).unwrap_or_default();
    let base_pt = r.styles.normal_pt;
    let (mut header, mut footer) = (None, None);
    if let Some(styles) = &styles_doc {
        if let Some(master) = standard_master(styles) {
            header = r.running(master, "header");
            footer = r.running(master, "footer");
            // The margins of an OpenDocument page are outside its header and footer: the text
            // starts below the header.
            let room = |kind: &str| page_layout(styles).and_then(|l| l.children().find(|c| is(*c, "style", &format!("{kind}-style")))).and_then(|s| s.children().find(|c| is(*c, "style", "header-footer-properties")));
            if header.is_some() {
                let props = room("header");
                let h = props.and_then(|p| attr(p, "fo", "min-height")).and_then(length_pt).unwrap_or(0.0).max(base_pt * 1.2);
                let gap = props.and_then(|p| attr(p, "fo", "margin-bottom")).and_then(length_pt).unwrap_or(7.0);
                page.header = page.margin[0];
                page.margin[0] += h + gap;
            }
            if footer.is_some() {
                let props = room("footer");
                let h = props.and_then(|p| attr(p, "fo", "min-height")).and_then(length_pt).unwrap_or(0.0).max(base_pt * 1.2);
                let gap = props.and_then(|p| attr(p, "fo", "margin-top")).and_then(length_pt).unwrap_or(7.0);
                page.footer = page.margin[2];
                page.margin[2] += h + gap;
            }
            page = page.sane();
        }
    }
    Ok(RichDoc { blocks, images: r.pictures.images, sheets: None, note, page, base_pt, header, footer, slides: None })
}

/// The master page called Standard, the one a document starts on, or the first.
fn standard_master<'a, 'i>(styles: &'a roxmltree::Document<'i>) -> Option<Node<'a, 'i>> {
    let masters = || styles.descendants().filter(|n| is(*n, "style", "master-page"));
    masters().find(|m| attr(*m, "style", "name") == Some("Standard")).or_else(|| masters().next())
}

/// The page layout of that master page.
fn page_layout<'a, 'i>(styles: &'a roxmltree::Document<'i>) -> Option<Node<'a, 'i>> {
    let name = standard_master(styles).and_then(|m| attr(m, "style", "page-layout-name"));
    let layouts = || styles.descendants().filter(|n| is(*n, "style", "page-layout"));
    layouts().find(|l| name.is_some() && attr(*l, "style", "name") == name).or_else(|| layouts().next())
}

/// The look of a shape or a slide: fill, outline, where its text sits.
#[derive(Clone, Default, Debug)]
struct Graphic {
    fill: Option<bool>,
    fill_color: Option<[u8; 3]>,
    stroke: Option<bool>,
    stroke_color: Option<[u8; 3]>,
    /// Points.
    stroke_width: Option<f32>,
    valign: Option<VAnchor>,
    /// Left, top, right, bottom, points.
    padding: [Option<f32>; 4],
}

impl Graphic {
    /// From a style's `style:graphic-properties` or `style:drawing-page-properties`.
    fn read(style: Node) -> Graphic {
        let mut g = Graphic::default();
        for p in style.children().filter(|c| is(*c, "style", "graphic-properties") || is(*c, "style", "drawing-page-properties")) {
            g.fill = attr(p, "draw", "fill").map(|f| f != "none").or(g.fill);
            g.fill_color = attr(p, "draw", "fill-color").and_then(super::hex_color).or(g.fill_color);
            g.stroke = attr(p, "draw", "stroke").map(|f| f != "none").or(g.stroke);
            g.stroke_color = attr(p, "svg", "stroke-color").and_then(super::hex_color).or(g.stroke_color);
            g.stroke_width = attr(p, "svg", "stroke-width").and_then(length_pt).or(g.stroke_width);
            g.valign = attr(p, "draw", "textarea-vertical-align")
                .map(|v| match v {
                    "middle" => VAnchor::Middle,
                    "bottom" => VAnchor::Bottom,
                    _ => VAnchor::Top,
                })
                .or(g.valign);
            let all = attr(p, "fo", "padding").and_then(length_pt);
            for (i, side) in ["padding-left", "padding-top", "padding-right", "padding-bottom"].iter().enumerate() {
                g.padding[i] = attr(p, "fo", side).and_then(length_pt).or(all).or(g.padding[i]);
            }
        }
        g
    }

    fn over(&mut self, o: &Graphic) {
        self.fill = o.fill.or(self.fill);
        self.fill_color = o.fill_color.or(self.fill_color);
        self.stroke = o.stroke.or(self.stroke);
        self.stroke_color = o.stroke_color.or(self.stroke_color);
        self.stroke_width = o.stroke_width.or(self.stroke_width);
        self.valign = o.valign.or(self.valign);
        for (a, b) in self.padding.iter_mut().zip(o.padding) {
            *a = b.or(*a);
        }
    }
}

/// Every style's look by name, over the default graphic style.
#[derive(Default)]
struct Graphics {
    styles: HashMap<String, (Option<String>, Graphic)>,
    default: Graphic,
}

impl Graphics {
    fn read(docs: &[&roxmltree::Document]) -> Graphics {
        let mut g = Graphics::default();
        for doc in docs {
            for n in doc.descendants().filter(|n| n.is_element()) {
                if is(n, "style", "default-style") && attr(n, "style", "family") == Some("graphic") {
                    g.default = Graphic::read(n);
                } else if is(n, "style", "style") {
                    if let Some(name) = attr(n, "style", "name") {
                        g.styles.insert(name.to_string(), (attr(n, "style", "parent-style-name").map(str::to_string), Graphic::read(n)));
                    }
                }
            }
        }
        g
    }

    /// The look of the styles named, each with its parents, later ones over earlier ones.
    fn of(&self, names: &[Option<&str>]) -> Graphic {
        let mut out = self.default.clone();
        for name in names.iter().flatten() {
            let mut chain = Vec::new();
            let mut cur = Some(name.to_string());
            while let Some(n) = cur {
                let Some((parent, g)) = self.styles.get(&n) else { break };
                chain.push(g);
                if chain.len() > 20 {
                    break;
                }
                cur = parent.clone();
            }
            for g in chain.into_iter().rev() {
                out.over(g);
            }
        }
        out
    }
}

/// More slides than this and the rest are left out.
const MAX_SLIDES: usize = 300;

/// A presentation (.odp): its slides, each with its master page's own shapes under its own.
pub fn presentation(pkg: &mut Package) -> Result<RichDoc, String> {
    let (content, styles_text) = read_both(pkg)?;
    let doc = super::xml(&content)?;
    let styles_doc = styles_text.as_deref().and_then(|t| super::xml(t).ok());
    let mut docs = Vec::new();
    if let Some(d) = &styles_doc {
        docs.push(d);
    }
    docs.push(&doc);
    let mut styles = Styles::read(&docs);
    styles.normal_pt = super::pptx::BASE_PT;
    let graphics = Graphics::read(&docs);
    let body = doc.descendants().find(|n| is(*n, "office", "presentation")).ok_or("no slides in the presentation")?;
    let mut page = styles_doc.as_ref().map(page_setup).unwrap_or(PageSetup { width: 793.7, height: 446.5, ..PageSetup::default() });
    (page.margin, page.header, page.footer) = ([0.0; 4], 0.0, 0.0);
    let mut r = Reader { pkg, styles, pictures: Pictures::default(), blocks: 0, counters: Vec::new(), base: Props::default(), outline: None };
    let pages: Vec<Node> = body.children().filter(|c| is(*c, "draw", "page")).collect();
    let mut slides = Vec::new();
    for pg in pages.iter().take(MAX_SLIDES) {
        let master_name = attr(*pg, "draw", "master-page-name").unwrap_or("Default");
        let master = styles_doc.as_ref().and_then(|d| d.descendants().find(|m| is(*m, "style", "master-page") && attr(*m, "style", "name") == Some(master_name)));
        let look = graphics.of(&[master.and_then(|m| attr(m, "draw", "style-name")), attr(*pg, "draw", "style-name")]);
        let background = look.fill_color.filter(|_| look.fill != Some(false)).unwrap_or([255; 3]);
        let mut shapes = Vec::new();
        if let Some(m) = master {
            r.slide_shapes(m, &graphics, master_name, true, &mut shapes);
        }
        r.slide_shapes(*pg, &graphics, master_name, false, &mut shapes);
        slides.push(Slide { background, background_image: None, shapes });
    }
    if slides.is_empty() {
        return Err("the presentation has no slides".into());
    }
    let note = (pages.len() > MAX_SLIDES).then(|| format!("The first {MAX_SLIDES} slides"));
    Ok(RichDoc { blocks: Vec::new(), images: r.pictures.images, sheets: None, note, page, base_pt: super::pptx::BASE_PT, header: None, footer: None, slides: Some(slides) })
}

/// A shape's frame from `svg:x`, `svg:y`, `svg:width` and `svg:height`; a rotated one gives its
/// place in `draw:transform` instead.
fn frame_rect(n: Node) -> Option<[f32; 4]> {
    let w = attr(n, "svg", "width").and_then(length_pt)?;
    let h = attr(n, "svg", "height").and_then(length_pt)?;
    let (x, y) = match (attr(n, "svg", "x").and_then(length_pt), attr(n, "svg", "y").and_then(length_pt)) {
        (Some(x), Some(y)) => (x, y),
        _ => {
            let t = attr(n, "draw", "transform")?;
            let inner = t.split("translate").nth(1)?.trim().trim_start_matches('(');
            let inner = inner.split(')').next()?;
            let mut parts = inner.split_whitespace();
            (length_pt(parts.next()?)?, length_pt(parts.next()?)?)
        }
    };
    Some([x, y, w, h])
}

impl Reader<'_> {
    /// The shapes of a slide or master page. `master`: a master page, whose placeholders only
    /// show on its slides when the slide fills them, so are not drawn from it.
    fn slide_shapes(&mut self, node: Node, graphics: &Graphics, master_name: &str, master: bool, out: &mut Vec<Shape>) {
        for c in node.children().filter(|c| c.is_element()) {
            if is(c, "draw", "g") {
                self.slide_shapes(c, graphics, master_name, master, out);
                continue;
            }
            let class = attr(c, "presentation", "class");
            // A placeholder: on the master, or left empty on a slide ("Click to add text").
            if (master && class.is_some()) || attr(c, "presentation", "placeholder") == Some("true") {
                continue;
            }
            let style_names = [attr(c, "presentation", "style-name"), attr(c, "draw", "style-name")];
            let look = graphics.of(&style_names);
            let kind = local(c);
            let (rect, geom) = match kind {
                "line" | "connector" => {
                    let p = |n: &str| attr(c, "svg", n).and_then(length_pt);
                    let (Some(x1), Some(y1), Some(x2), Some(y2)) = (p("x1"), p("y1"), p("x2"), p("y2")) else { continue };
                    ([x1.min(x2), y1.min(y2), (x2 - x1).abs(), (y2 - y1).abs()], Geom::Line { flip_h: x2 < x1, flip_v: y2 < y1 })
                }
                "frame" | "custom-shape" | "rect" | "ellipse" | "circle" | "caption" => {
                    let Some(rect) = frame_rect(c) else { continue };
                    let geom = match kind {
                        "ellipse" | "circle" => Geom::Ellipse,
                        "custom-shape" => {
                            let t = c.children().find(|g| is(*g, "draw", "enhanced-geometry")).and_then(|g| attr(g, "draw", "type")).unwrap_or("rectangle");
                            match t {
                                "ellipse" | "circle" | "ooxml-ellipse" => Geom::Ellipse,
                                "round-rectangle" | "ooxml-roundRect" => Geom::RoundRect,
                                _ => Geom::Rect,
                            }
                        }
                        _ => Geom::Rect,
                    };
                    (rect, geom)
                }
                _ => continue,
            };
            // A frame (a text box, a picture) is unfilled unless its style says; a drawn shape
            // takes LibreOffice's default, filled and outlined.
            let drawn = kind != "frame";
            let fill = if look.fill.unwrap_or(drawn) { look.fill_color } else { None };
            let line = if look.stroke.unwrap_or(drawn) && !matches!(geom, Geom::Line { .. }) || matches!(geom, Geom::Line { .. }) && look.stroke != Some(false) {
                Some((look.stroke_color.unwrap_or([0x34, 0x65, 0xA4]), look.stroke_width.unwrap_or(0.0).max(0.75)))
            } else {
                None
            };
            let content = if kind == "frame" {
                self.frame_content(c, &look, &style_names, class, master_name)
            } else {
                // Text in a drawn shape stands in its middle, both ways.
                self.text_content(c, &look, &style_names, None, master_name, VAnchor::Middle, true)
            };
            if matches!(content, ShapeContent::Empty) && fill.is_none() && line.is_none() {
                continue;
            }
            out.push(Shape { rect, geom, fill, line, content });
        }
    }

    fn frame_content(&mut self, f: Node, look: &Graphic, style_names: &[Option<&str>], class: Option<&str>, master_name: &str) -> ShapeContent {
        for c in f.children().filter(|c| c.is_element()) {
            if is(c, "draw", "image") {
                let Some(href) = c.attributes().find(|a| a.name() == "href").map(|a| a.value()) else { continue };
                if href.starts_with("http") {
                    continue;
                }
                return match self.pictures.get(self.pkg, href.trim_start_matches("./")) {
                    Ok(idx) => ShapeContent::Image(idx),
                    Err(what) => ShapeContent::Missing(what),
                };
            }
            if is(c, "draw", "text-box") {
                return self.text_content(c, look, style_names, class, master_name, VAnchor::Top, false);
            }
            if is(c, "table", "table") {
                return ShapeContent::Table(self.table(c));
            }
            if is(c, "draw", "object") || is(c, "draw", "object-ole") {
                return ShapeContent::Missing(if class == Some("chart") { "chart" } else { "object" }.into());
            }
            if is(c, "draw", "plugin") {
                return ShapeContent::Missing("media".into());
            }
        }
        ShapeContent::Empty
    }

    /// The text in a text box or a drawn shape, in the shape's style.
    #[allow(clippy::too_many_arguments)]
    fn text_content(&mut self, node: Node, look: &Graphic, style_names: &[Option<&str>], class: Option<&str>, master_name: &str, anchor: VAnchor, centered: bool) -> ShapeContent {
        let mut base = Props::default();
        if centered {
            base.align = Some(Align::Center);
        }
        for name in style_names.iter().flatten() {
            base.over(&self.styles.props(Some(name)));
        }
        self.base = base;
        self.outline = (class == Some("outline")).then(|| master_name.to_string());
        self.counters.clear();
        let mut blocks = Vec::new();
        self.body(node, &mut blocks, None, 0);
        self.base = Props::default();
        self.outline = None;
        let blank = blocks.iter().all(|b| matches!(b, Block::Para(p) if p.inlines.iter().all(|i| matches!(i, Inline::Text(s) if s.text.trim().is_empty()))));
        if blank {
            return ShapeContent::Empty;
        }
        let [l, t, r, b] = look.padding;
        ShapeContent::Text(TextBox { blocks, anchor: look.valign.unwrap_or(anchor), inset: [l.unwrap_or(7.1), t.unwrap_or(3.5), r.unwrap_or(7.1), b.unwrap_or(3.5)] })
    }
}

/// The page of the master page called Standard (the one a document starts on), or of the first.
fn page_setup(styles: &roxmltree::Document) -> PageSetup {
    let Some(layout) = page_layout(styles) else { return PageSetup::default() };
    let Some(props) = layout.children().find(|c| is(*c, "style", "page-layout-properties")) else { return PageSetup::default() };
    let mut page = PageSetup::default();
    let len = |name: &str| attr(props, "fo", name).and_then(length_pt);
    page.width = len("page-width").unwrap_or(page.width);
    page.height = len("page-height").unwrap_or(page.height);
    let all = len("margin");
    for (i, name) in ["margin-top", "margin-right", "margin-bottom", "margin-left"].iter().enumerate() {
        if let Some(v) = len(name).or(all) {
            page.margin[i] = v;
        }
    }
    page.sane()
}

impl Reader<'_> {
    /// The header or footer (`kind`) of a master page, with the first page's own when it has one
    /// (`style:header-first`). One switched off (`style:display="false"`) is none.
    fn running(&mut self, master: Node, kind: &str) -> Option<Running> {
        let shown = |n: &Node| attr(*n, "style", "display") != Some("false");
        let blocks = master.children().find(|c| is(*c, "style", kind)).filter(shown).map(|n| {
            let mut out = Vec::new();
            self.body(n, &mut out, None, 0);
            out
        });
        let first = master.children().find(|c| is(*c, "style", &format!("{kind}-first"))).map(|n| {
            let mut out = Vec::new();
            if shown(&n) {
                self.body(n, &mut out, None, 0);
            }
            out
        });
        blocks.as_ref()?;
        Some(Running { blocks: blocks.unwrap_or_default(), first })
    }

    /// Block content. `list` is the list style in effect and `depth` how deep in lists we are.
    fn body(&mut self, node: Node, out: &mut Vec<Block>, list: Option<&str>, depth: u8) {
        for c in node.children().filter(|c| c.is_element()) {
            if self.blocks > super::MAX_BLOCKS {
                return;
            }
            if is(c, "text", "p") || is(c, "text", "h") {
                self.paragraph(c, out, None, 0);
            } else if is(c, "text", "list") {
                let style = attr(c, "text", "style-name").or(list).map(str::to_string);
                if depth == 0 && attr(c, "text", "continue-numbering") != Some("true") {
                    self.counters.clear();
                }
                self.list(c, out, style.as_deref(), depth + 1);
            } else if is(c, "table", "table") {
                self.blocks += 1;
                let t = self.table(c);
                out.push(Block::Table(t));
            } else if is(c, "text", "section") || is(c, "text", "index-body") || is(c, "text", "table-of-content") || is(c, "text", "alphabetical-index") {
                self.body(c, out, list, depth);
            } else if is(c, "text", "soft-page-break") {
            } else if is(c, "draw", "frame") {
                let mut para = Para::default();
                let mut extra = Vec::new();
                self.frame(c, &mut para.inlines, &mut extra);
                out.push(Block::Para(para));
                out.extend(extra);
            }
        }
    }

    fn list(&mut self, l: Node, out: &mut Vec<Block>, style: Option<&str>, depth: u8) {
        for item in l.children().filter(|c| is(*c, "text", "list-item") || is(*c, "text", "list-header")) {
            let header = is(item, "text", "list-header");
            let mut first = true;
            for c in item.children().filter(|c| c.is_element()) {
                if is(c, "text", "p") || is(c, "text", "h") {
                    let marker = if first && !header { self.marker(style, depth) } else { None };
                    first = false;
                    self.paragraph(c, out, marker, depth);
                } else if is(c, "text", "list") {
                    let inner = attr(c, "text", "style-name").or(style).map(str::to_string);
                    self.list(c, out, inner.as_deref(), depth + 1);
                }
            }
        }
    }

    /// The marker of the next item at `depth` in a list of `style`, counting it.
    fn marker(&mut self, style: Option<&str>, depth: u8) -> Option<String> {
        let d = depth.max(1) as usize;
        if self.counters.len() < d {
            self.counters.resize(d, 0);
        }
        self.counters.truncate(d);
        let level = style.and_then(|s| self.styles.lists.get(s)).and_then(|l| l.get(&(d as u8)));
        let start = level.map(|l| l.start).unwrap_or(1);
        self.counters[d - 1] = if self.counters[d - 1] == 0 { start } else { self.counters[d - 1] + 1 };
        match level {
            Some(l) if l.bullet.is_none() && !l.format.is_empty() => Some(format!("{}{}{}", l.prefix, super::format_number(self.counters[d - 1], &l.format), l.suffix)),
            Some(l) if l.bullet.is_some() => l.bullet.clone(),
            _ => Some("•".into()),
        }
    }

    fn paragraph(&mut self, p: Node, out: &mut Vec<Block>, marker: Option<String>, depth: u8) {
        self.blocks += 1;
        let mut props = self.base.clone();
        if let Some(master) = self.outline.as_deref().filter(|_| depth > 1) {
            props.over(&self.styles.props(Some(&format!("{master}-outline{depth}"))));
        }
        props.over(&self.styles.props(attr(p, "text", "style-name")));
        let heading = if is(p, "text", "h") {
            Some(attr(p, "text", "outline-level").and_then(|v| v.parse::<u8>().ok()).or(props.outline).unwrap_or(1).clamp(1, 6))
        } else {
            props.outline.filter(|o| *o > 0).map(|o| o.min(6))
        };
        let mut para = Para {
            heading,
            align: props.align.unwrap_or_default(),
            level: depth,
            marker,
            before: props.before,
            after: props.after,
            line: props.line,
            ..Default::default()
        };
        let mut extra = Vec::new();
        self.inline(p, &props, &mut para.inlines, &mut extra);
        out.push(Block::Para(para));
        out.extend(extra);
    }

    fn inline(&mut self, node: Node, base: &Props, inlines: &mut Vec<Inline>, extra: &mut Vec<Block>) {
        let normal = self.styles.normal_pt;
        for c in node.children() {
            if c.is_text() {
                // Runs of white space in ODF are one space; real ones are written as text:s.
                let t = c.text().unwrap_or("");
                let collapsed: String = t.split_whitespace().collect::<Vec<_>>().join(" ");
                let lead = t.starts_with(char::is_whitespace) && !collapsed.is_empty();
                let trail = t.ends_with(char::is_whitespace) && !collapsed.is_empty();
                let s = format!("{}{}{}", if lead { " " } else { "" }, collapsed, if trail { " " } else { "" });
                let s = if collapsed.is_empty() && !t.is_empty() { " ".to_string() } else { s };
                super::push_text(inlines, &s, &base.style(normal));
                continue;
            }
            if !c.is_element() {
                continue;
            }
            if is(c, "text", "span") || is(c, "text", "a") || is(c, "text", "ruby-base") || is(c, "text", "meta") {
                let mut p = base.clone();
                p.over(&self.styles.props(attr(c, "text", "style-name")));
                self.inline(c, &p, inlines, extra);
            } else if is(c, "text", "s") {
                let n: usize = attr(c, "text", "c").and_then(|v| v.parse().ok()).unwrap_or(1).min(200);
                super::push_text(inlines, &" ".repeat(n), &base.style(normal));
            } else if is(c, "text", "tab") {
                super::push_text(inlines, "    ", &base.style(normal));
            } else if is(c, "text", "line-break") {
                super::push_text(inlines, "\n", &base.style(normal));
            } else if is(c, "draw", "frame") || is(c, "draw", "a") {
                self.frame(c, inlines, extra);
            } else if is(c, "text", "page-number") {
                inlines.push(Inline::Field(Field::Page, base.style(normal)));
            } else if is(c, "text", "page-count") {
                inlines.push(Inline::Field(Field::Pages, base.style(normal)));
            } else if is(c, "text", "date") || is(c, "text", "title") || is(c, "text", "sequence") || is(c, "text", "bookmark-ref") || is(c, "text", "reference-ref") {
                self.inline(c, base, inlines, extra);
            }
        }
    }

    /// A frame: a picture, or a text box whose paragraphs follow the one it is in.
    fn frame(&mut self, f: Node, inlines: &mut Vec<Inline>, extra: &mut Vec<Block>) {
        let frame = if is(f, "draw", "a") { f.children().find(|c| is(*c, "draw", "frame")).unwrap_or(f) } else { f };
        let w = attr(frame, "svg", "width").and_then(length_pt).unwrap_or(0.0);
        let h = attr(frame, "svg", "height").and_then(length_pt).unwrap_or(0.0);
        for c in frame.children().filter(|c| c.is_element()) {
            if is(c, "draw", "image") {
                if let Some(href) = c.attributes().find(|a| a.name() == "href").map(|a| a.value()) {
                    if href.starts_with("http") {
                        continue;
                    }
                    let name = href.trim_start_matches("./").to_string();
                    match self.pictures.get(self.pkg, &name) {
                        Ok(idx) => inlines.push(Inline::Image { idx, w, h }),
                        Err(what) => inlines.push(Inline::Missing(what)),
                    }
                    // The first image of a frame is the picture; others are fallbacks of it.
                    return;
                }
            } else if is(c, "draw", "text-box") {
                self.body(c, extra, None, 0);
            } else if is(c, "draw", "object") {
                inlines.push(Inline::Missing("object".into()));
            }
        }
    }

    fn table(&mut self, t: Node) -> Table {
        let mut widths = Vec::new();
        let mut rows = Vec::new();
        self.table_rows(t, &mut widths, &mut rows);
        Table { widths, rows }
    }

    fn table_rows(&mut self, t: Node, widths: &mut Vec<f32>, rows: &mut Vec<Vec<Cell>>) {
        for c in t.children().filter(|c| c.is_element()) {
            if is(c, "table", "table-column") {
                let n: usize = attr(c, "table", "number-columns-repeated").and_then(|v| v.parse().ok()).unwrap_or(1).min(super::MAX_COLS);
                widths.extend(std::iter::repeat_n(1.0, n));
            } else if is(c, "table", "table-row") {
                let mut cells = Vec::new();
                for cell in c.children().filter(|x| is(*x, "table", "table-cell") || is(*x, "table", "covered-table-cell")) {
                    let span = attr(cell, "table", "number-columns-spanned").and_then(|v| v.parse().ok()).unwrap_or(1usize).max(1);
                    let repeat: usize = attr(cell, "table", "number-columns-repeated").and_then(|v| v.parse().ok()).unwrap_or(1).min(super::MAX_COLS);
                    if is(cell, "table", "covered-table-cell") {
                        // Covered by a spanning neighbor: it has no place of its own.
                        continue;
                    }
                    for _ in 0..repeat {
                        let mut blocks = Vec::new();
                        self.body(cell, &mut blocks, None, 0);
                        cells.push(Cell { blocks, span, fill: None });
                    }
                }
                rows.push(cells);
            } else if is(c, "table", "table-rows") || is(c, "table", "table-header-rows") || is(c, "table", "table-row-group") || is(c, "table", "table-columns") || is(c, "table", "table-header-columns") || is(c, "table", "table-column-group") {
                self.table_rows(c, widths, rows);
            }
        }
    }
}

/// A Calc workbook: one sheet at a time. Cells carry the text they show, already formatted.
pub fn sheet(pkg: &mut Package, page: u32) -> Result<RichDoc, String> {
    let (content, _) = read_both(pkg)?;
    let doc = super::xml(&content)?;
    let book = doc.descendants().find(|n| is(*n, "office", "spreadsheet")).ok_or("no spreadsheet in the document")?;
    let tables: Vec<Node> = book.children().filter(|c| is(*c, "table", "table")).collect();
    if tables.is_empty() {
        return Err("the workbook has no sheets".into());
    }
    let names = tables.iter().enumerate().map(|(i, t)| attr(*t, "table", "name").map(str::to_string).unwrap_or_else(|| format!("Sheet{}", i + 1))).collect();
    let t = tables[(page as usize).min(tables.len() - 1)];
    Ok(RichDoc { blocks: vec![Block::Sheet(calc_sheet(t))], images: Vec::new(), sheets: Some(names), note: None, page: PageSetup::default(), base_pt: 11.0, header: None, footer: None, slides: None })
}

fn calc_sheet(t: Node) -> Sheet {
    let mut rows: Vec<(u32, Vec<SheetCell>)> = Vec::new();
    let (mut more_rows, mut more_cols) = (false, false);
    let mut num = 1u32;
    let mut cols = 0usize;
    let mut all_rows = Vec::new();
    collect_rows(t, &mut all_rows);
    for row in all_rows {
        // A sheet ends in a row repeated a million times to fill out the grid; empty repeats are
        // counted, never built.
        let repeat: u32 = attr(row, "table", "number-rows-repeated").and_then(|v| v.parse().ok()).unwrap_or(1).max(1);
        let mut cells = Vec::new();
        for cell in row.children().filter(|c| is(*c, "table", "table-cell") || is(*c, "table", "covered-table-cell")) {
            let rep: usize = attr(cell, "table", "number-columns-repeated").and_then(|v| v.parse().ok()).unwrap_or(1).max(1);
            let text: Vec<String> = cell.children().filter(|c| is(*c, "text", "p")).map(|p| p.descendants().filter(|d| d.is_text()).filter_map(|d| d.text()).collect()).collect();
            let text = text.join("\n");
            let right = matches!(attr(cell, "office", "value-type"), Some("float" | "percentage" | "currency" | "date" | "time"));
            for _ in 0..rep {
                if cells.len() >= super::MAX_COLS {
                    if !text.is_empty() {
                        more_cols = true;
                    }
                    break;
                }
                cells.push(SheetCell { text: text.clone(), right });
            }
        }
        while cells.last().is_some_and(|c: &SheetCell| c.text.is_empty()) {
            cells.pop();
        }
        if cells.is_empty() {
            num = num.saturating_add(repeat);
            continue;
        }
        for _ in 0..repeat.min(super::MAX_ROWS as u32) {
            if rows.len() >= super::MAX_ROWS || num as usize > super::MAX_ROWS {
                more_rows = true;
                break;
            }
            cols = cols.max(cells.len());
            rows.push((num, cells.clone()));
            num += 1;
        }
        if more_rows {
            break;
        }
    }
    let mut dense = Vec::new();
    let mut expect = 1u32;
    for (n, cells) in rows {
        while expect < n && dense.len() < super::MAX_ROWS {
            dense.push((expect, Vec::new()));
            expect += 1;
        }
        dense.push((n, cells));
        expect = n + 1;
    }
    Sheet { rows: dense, cols, more_rows, more_cols }
}

fn collect_rows<'a, 'i>(n: Node<'a, 'i>, out: &mut Vec<Node<'a, 'i>>) {
    for c in n.children().filter(|c| c.is_element()) {
        if is(c, "table", "table-row") {
            out.push(c);
        } else if is(c, "table", "table-rows") || is(c, "table", "table-header-rows") || is(c, "table", "table-row-group") {
            collect_rows(c, out);
        }
    }
}
