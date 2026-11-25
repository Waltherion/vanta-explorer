//! PowerPoint presentations (.pptx): each slide as a page, its shapes where the slide puts them.
//! A slide's placeholders take their place, frame and text style from its layout and the layout's
//! master, and the colors they name from the master's theme. What is drawn: backgrounds, filled
//! and outlined rectangles, rounded rectangles, ellipses and lines, text, pictures and tables.
//! Charts, diagrams and embedded objects are named. Speaker notes and animations are left out.

use super::{Align, Block, Cell, Geom, Inline, PageSetup, Package, Para, Pictures, RichDoc, Shape, ShapeContent, Slide, SpanStyle, Table, TextBox, VAnchor};
use roxmltree::Node;
use std::collections::HashMap;

/// English Metric Units to the point.
const EMU_PT: f32 = 12700.0;
/// The text size of a slide's normal text, which `SpanStyle::size` is relative to.
pub const BASE_PT: f32 = 18.0;
/// More slides than this and the rest are left out.
const MAX_SLIDES: usize = 300;
/// PowerPoint's own default table style, "Medium Style 2 - Accent 1".
const MEDIUM_STYLE_2: &str = "{5C22544A-7EE6-4342-B048-85BDC9FD1C3A}";

fn local<'a>(n: Node<'a, '_>) -> &'a str {
    n.tag_name().name()
}

fn el<'a, 'i>(n: Node<'a, 'i>, name: &str) -> Option<Node<'a, 'i>> {
    n.children().find(|c| c.is_element() && local(*c) == name)
}

fn els<'a, 'i>(n: Node<'a, 'i>, name: &'static str) -> impl Iterator<Item = Node<'a, 'i>> {
    n.children().filter(move |c| c.is_element() && local(*c) == name)
}

/// An attribute by its local name, whatever its namespace.
fn attr<'a>(n: Node<'a, '_>, name: &str) -> Option<&'a str> {
    n.attributes().find(|a| a.name() == name).map(|a| a.value())
}

fn num(n: Node, name: &str) -> Option<f32> {
    attr(n, name).and_then(|v| v.parse::<f32>().ok())
}

fn emu(n: Node, name: &str) -> Option<f32> {
    num(n, name).map(|v| v / EMU_PT)
}

fn flag(n: Node, name: &str) -> Option<bool> {
    attr(n, name).map(|v| v == "1" || v == "true")
}

/// The relationship id an element points with (`r:embed`, `r:id`).
fn rel_id<'a>(n: Node<'a, '_>, name: &str) -> Option<&'a str> {
    n.attributes().find(|a| a.name() == name && a.namespace().is_some_and(|ns| ns.contains("relationships"))).map(|a| a.value())
}

// --- colors ------------------------------------------------------------------------------------

/// The theme's colors under every name a document uses for them: the scheme's own (`dk1`,
/// `accent1`) and the master's mapping of them (`tx1`, `bg1`).
#[derive(Clone, Default)]
struct Colors(HashMap<String, [u8; 3]>);

impl Colors {
    fn read(theme: Option<&str>, clr_map: Option<Node>) -> Colors {
        let mut map = HashMap::new();
        if let Some(doc) = theme.and_then(|t| super::xml(t).ok()) {
            if let Some(scheme) = doc.descendants().find(|n| n.is_element() && local(*n) == "clrScheme") {
                for c in scheme.children().filter(|c| c.is_element()) {
                    if let Some(rgb) = c.children().find(|x| x.is_element()).and_then(|x| plain_color(x, &Colors::default())) {
                        map.insert(local(c).to_string(), rgb);
                    }
                }
            }
        }
        let mut colors = Colors(map);
        let defaults = [("bg1", "lt1"), ("tx1", "dk1"), ("bg2", "lt2"), ("tx2", "dk2")];
        for (alias, target) in defaults {
            let target = clr_map.and_then(|m| attr(m, alias)).unwrap_or(target);
            if let Some(c) = colors.0.get(target).copied() {
                colors.0.insert(alias.to_string(), c);
            }
        }
        colors
    }

    fn get(&self, name: &str) -> Option<[u8; 3]> {
        self.0.get(name).copied()
    }
}

/// A color element (`a:srgbClr`, `a:schemeClr`, ...) with its modifiers applied.
fn plain_color(c: Node, colors: &Colors) -> Option<[u8; 3]> {
    let base = match local(c) {
        "srgbClr" => super::hex_color(attr(c, "val")?),
        "schemeClr" => colors.get(attr(c, "val")?),
        "sysClr" => attr(c, "lastClr").and_then(super::hex_color).or_else(|| match attr(c, "val") {
            Some("window") => Some([255; 3]),
            _ => Some([0; 3]),
        }),
        "prstClr" => Some(match attr(c, "val")? {
            "white" => [255, 255, 255],
            "red" => [255, 0, 0],
            "green" => [0, 128, 0],
            "blue" => [0, 0, 255],
            "yellow" => [255, 255, 0],
            "gray" | "grey" => [128, 128, 128],
            _ => [0, 0, 0],
        }),
        "scrgbClr" => {
            let ch = |n: &str| (num(c, n).unwrap_or(0.0) / 100_000.0).clamp(0.0, 1.0);
            let s = |v: f32| (if v <= 0.003_130_8 { v * 12.92 } else { 1.055 * v.powf(1.0 / 2.4) - 0.055 } * 255.0).round() as u8;
            Some([s(ch("r")), s(ch("g")), s(ch("b"))])
        }
        _ => None,
    }?;
    Some(modify(base, c))
}

/// The modifiers of a color element: lighter, darker, and their luminance forms.
fn modify(rgb: [u8; 3], c: Node) -> [u8; 3] {
    let mut f = rgb.map(|v| v as f32 / 255.0);
    for m in c.children().filter(|m| m.is_element()) {
        let v = num(m, "val").unwrap_or(100_000.0) / 100_000.0;
        match local(m) {
            "tint" => f = f.map(|x| x + (1.0 - x) * (1.0 - v)),
            "shade" => f = f.map(|x| x * v),
            "lumMod" | "lumOff" => {
                let (h, s, mut l) = hsl(f);
                if local(m) == "lumMod" {
                    l *= v;
                } else {
                    l += v;
                }
                f = rgb_of(h, s, l.clamp(0.0, 1.0));
            }
            _ => {}
        }
    }
    f.map(|x| (x.clamp(0.0, 1.0) * 255.0).round() as u8)
}

fn hsl([r, g, b]: [f32; 3]) -> (f32, f32, f32) {
    let (max, min) = (r.max(g).max(b), r.min(g).min(b));
    let l = (max + min) / 2.0;
    if max == min {
        return (0.0, 0.0, l);
    }
    let d = max - min;
    let s = if l > 0.5 { d / (2.0 - max - min) } else { d / (max + min) };
    let h = if max == r {
        (g - b) / d + if g < b { 6.0 } else { 0.0 }
    } else if max == g {
        (b - r) / d + 2.0
    } else {
        (r - g) / d + 4.0
    };
    (h / 6.0, s, l)
}

fn rgb_of(h: f32, s: f32, l: f32) -> [f32; 3] {
    if s == 0.0 {
        return [l; 3];
    }
    let q = if l < 0.5 { l * (1.0 + s) } else { l + s - l * s };
    let p = 2.0 * l - q;
    let hue = |mut t: f32| {
        t = t.rem_euclid(1.0);
        if t < 1.0 / 6.0 {
            p + (q - p) * 6.0 * t
        } else if t < 0.5 {
            q
        } else if t < 2.0 / 3.0 {
            p + (q - p) * (2.0 / 3.0 - t) * 6.0
        } else {
            p
        }
    };
    [hue(h + 1.0 / 3.0), hue(h), hue(h - 1.0 / 3.0)]
}

/// The color a parent element holds (`a:solidFill`, `a:fontRef`, a gradient stop).
fn color_in(parent: Node, colors: &Colors) -> Option<[u8; 3]> {
    parent.children().filter(|c| c.is_element()).find_map(|c| plain_color(c, colors))
}

/// What a fill says: `None` nothing, `Some(None)` no fill, `Some(Some(c))` a color. A gradient is
/// its first stop, a pattern its foreground.
fn fill_in(parent: Node, colors: &Colors) -> Option<Option<[u8; 3]>> {
    for c in parent.children().filter(|c| c.is_element()) {
        match local(c) {
            "noFill" => return Some(None),
            "solidFill" => return Some(color_in(c, colors)),
            "gradFill" => return Some(c.descendants().find(|g| g.is_element() && local(*g) == "gs").and_then(|g| color_in(g, colors))),
            "pattFill" => return Some(el(c, "fgClr").and_then(|f| color_in(f, colors))),
            _ => {}
        }
    }
    None
}

// --- text styles -------------------------------------------------------------------------------

#[derive(Clone, Default, Debug)]
struct RunProps {
    /// Points.
    sz: Option<f32>,
    b: Option<bool>,
    i: Option<bool>,
    u: Option<bool>,
    strike: Option<bool>,
    color: Option<[u8; 3]>,
}

impl RunProps {
    fn read(n: Node, colors: &Colors) -> RunProps {
        RunProps {
            sz: num(n, "sz").map(|v| v / 100.0),
            b: flag(n, "b"),
            i: flag(n, "i"),
            u: attr(n, "u").map(|v| v != "none"),
            strike: attr(n, "strike").map(|v| v != "noStrike"),
            color: el(n, "solidFill").and_then(|f| color_in(f, colors)),
        }
    }

    fn over(&mut self, o: &RunProps) {
        self.sz = o.sz.or(self.sz);
        self.b = o.b.or(self.b);
        self.i = o.i.or(self.i);
        self.u = o.u.or(self.u);
        self.strike = o.strike.or(self.strike);
        self.color = o.color.or(self.color);
    }
}

#[derive(Clone, Debug)]
enum Bullet {
    None,
    Char(String),
    /// A numbering scheme (`arabicPeriod`) and where it starts.
    Auto(String, u32),
}

/// Space before or after a paragraph, or between its lines.
#[derive(Clone, Copy, Debug)]
enum Space {
    Pt(f32),
    /// A fraction of a line.
    Lines(f32),
}

fn space(n: Option<Node>) -> Option<Space> {
    let n = n?;
    if let Some(p) = el(n, "spcPts") {
        return num(p, "val").map(|v| Space::Pt(v / 100.0));
    }
    el(n, "spcPct").and_then(|p| num(p, "val")).map(|v| Space::Lines(v / 100_000.0))
}

/// A list level's paragraph and text properties.
#[derive(Clone, Default, Debug)]
struct Level {
    algn: Option<Align>,
    /// Points.
    mar_l: Option<f32>,
    indent: Option<f32>,
    bullet: Option<Bullet>,
    before: Option<Space>,
    after: Option<Space>,
    line: Option<Space>,
    rpr: RunProps,
}

impl Level {
    /// An `a:lvlNpPr` or an `a:pPr`.
    fn read(n: Node, colors: &Colors) -> Level {
        let bullet = n.children().filter(|c| c.is_element()).find_map(|c| match local(c) {
            "buNone" => Some(Bullet::None),
            "buChar" => Some(Bullet::Char(super::bullet(attr(c, "char").unwrap_or("•")))),
            "buAutoNum" => Some(Bullet::Auto(attr(c, "type").unwrap_or("arabicPeriod").to_string(), num(c, "startAt").map_or(1, |v| v as u32))),
            _ => None,
        });
        Level {
            algn: attr(n, "algn").and_then(|a| {
                Some(match a {
                    "l" => Align::Left,
                    "ctr" => Align::Center,
                    "r" => Align::Right,
                    "just" | "dist" | "thaiDist" | "justLow" => Align::Justify,
                    _ => return None,
                })
            }),
            mar_l: emu(n, "marL"),
            indent: emu(n, "indent"),
            bullet,
            before: space(el(n, "spcBef")),
            after: space(el(n, "spcAft")),
            line: space(el(n, "lnSpc")),
            rpr: el(n, "defRPr").map(|r| RunProps::read(r, colors)).unwrap_or_default(),
        }
    }

    fn over(&mut self, o: &Level) {
        self.algn = o.algn.or(self.algn);
        self.mar_l = o.mar_l.or(self.mar_l);
        self.indent = o.indent.or(self.indent);
        self.bullet = o.bullet.clone().or(self.bullet.take());
        self.before = o.before.or(self.before);
        self.after = o.after.or(self.after);
        self.line = o.line.or(self.line);
        self.rpr.over(&o.rpr);
    }
}

/// Nine list levels.
#[derive(Clone, Debug)]
struct ListStyle(Vec<Level>);

impl Default for ListStyle {
    fn default() -> Self {
        ListStyle(vec![Level::default(); 9])
    }
}

impl ListStyle {
    /// From an element holding `a:lvl1pPr`..`a:lvl9pPr` (`a:lstStyle`, `p:bodyStyle`).
    fn read(n: Option<Node>, colors: &Colors) -> ListStyle {
        let ListStyle(mut levels) = ListStyle::default();
        if let Some(n) = n {
            for (i, l) in levels.iter_mut().enumerate() {
                if let Some(p) = el(n, &format!("lvl{}pPr", i + 1)) {
                    *l = Level::read(p, colors);
                }
            }
        }
        ListStyle(levels)
    }

    fn over(&mut self, o: &ListStyle) {
        for (a, b) in self.0.iter_mut().zip(&o.0) {
            a.over(b);
        }
    }
}

/// How a text frame sits in its shape.
#[derive(Clone, Copy, Default, Debug)]
struct BodyProps {
    anchor: Option<VAnchor>,
    /// Left, top, right, bottom, points.
    ins: [Option<f32>; 4],
    font_scale: Option<f32>,
    line_reduction: Option<f32>,
}

impl BodyProps {
    fn read(n: Node) -> BodyProps {
        let auto = el(n, "normAutofit");
        BodyProps {
            anchor: attr(n, "anchor").map(|a| match a {
                "ctr" => VAnchor::Middle,
                "b" => VAnchor::Bottom,
                _ => VAnchor::Top,
            }),
            ins: [emu(n, "lIns"), emu(n, "tIns"), emu(n, "rIns"), emu(n, "bIns")],
            font_scale: auto.and_then(|a| num(a, "fontScale")).map(|v| v / 100_000.0),
            line_reduction: auto.and_then(|a| num(a, "lnSpcReduction")).map(|v| v / 100_000.0),
        }
    }

    fn over(&mut self, o: &BodyProps) {
        self.anchor = o.anchor.or(self.anchor);
        for (a, b) in self.ins.iter_mut().zip(o.ins) {
            *a = b.or(*a);
        }
        self.font_scale = o.font_scale.or(self.font_scale);
        self.line_reduction = o.line_reduction.or(self.line_reduction);
    }
}

// --- placeholders, layouts, masters ------------------------------------------------------------

/// A placeholder's kind (`title`, `body`, `dt`...) and index.
#[derive(Clone, Debug)]
struct Ph {
    kind: String,
    idx: u32,
}

fn ph_of(shape: Node) -> Option<Ph> {
    let nv = shape.children().find(|c| c.is_element() && local(*c).starts_with("nv"))?;
    let ph = el(el(nv, "nvPr")?, "ph")?;
    Some(Ph { kind: attr(ph, "type").unwrap_or("obj").to_string(), idx: num(ph, "idx").map_or(0, |v| v as u32) })
}

/// The kind a master places a placeholder of this kind under.
fn master_kind(kind: &str) -> &str {
    match kind {
        "ctrTitle" | "title" => "title",
        "dt" | "ftr" | "sldNum" | "hdr" => kind,
        _ => "body",
    }
}

/// What a layout's or master's placeholder gives the slide's: frame, text frame, text style.
#[derive(Clone, Debug)]
struct PhInfo {
    ph: Ph,
    rect: Option<[f32; 4]>,
    body: BodyProps,
    list: ListStyle,
}

/// A layout or a master: its placeholders, its background, and the shapes it draws on every
/// slide that uses it.
#[derive(Default)]
struct Frame {
    placeholders: Vec<PhInfo>,
    background: Option<Background>,
    shapes: Vec<Shape>,
    /// For a layout: whether the master's shapes are drawn too.
    show_master: bool,
}

#[derive(Clone, Copy, Debug)]
enum Background {
    Color([u8; 3]),
    Image(usize),
}

struct Master {
    frame: Frame,
    title: ListStyle,
    body: ListStyle,
    other: ListStyle,
    colors: Colors,
}

/// A shape's place: offset and size, mapped through the groups it is in.
#[derive(Clone, Copy)]
struct Xf {
    off: [f32; 2],
    scale: [f32; 2],
    child_off: [f32; 2],
}

impl Xf {
    const IDENTITY: Xf = Xf { off: [0.0; 2], scale: [1.0; 2], child_off: [0.0; 2] };

    fn map(&self, [x, y, w, h]: [f32; 4]) -> [f32; 4] {
        [self.off[0] + (x - self.child_off[0]) * self.scale[0], self.off[1] + (y - self.child_off[1]) * self.scale[1], w * self.scale[0], h * self.scale[1]]
    }
}

/// An `a:xfrm` or `p:xfrm`: offset and extent, points.
fn xfrm_rect(x: Node) -> Option<[f32; 4]> {
    let off = el(x, "off")?;
    let ext = el(x, "ext")?;
    Some([emu(off, "x")?, emu(off, "y")?, emu(ext, "cx")?, emu(ext, "cy")?])
}

// --- reading -----------------------------------------------------------------------------------

struct Reader<'p> {
    pkg: &'p mut Package,
    pictures: Pictures,
    defaults: ListStyle,
}

/// Where a part's relationships are, and those relationships.
fn part_rels(pkg: &mut Package, part: &str) -> HashMap<String, String> {
    let (dir, name) = part.rsplit_once('/').unwrap_or(("", part));
    super::relationships(pkg, &format!("{dir}/_rels/{name}.rels"), dir)
}

/// The relationship target of a given type (`/slideLayout`) in a part's relationships file.
fn rel_of_type(pkg: &mut Package, part: &str, kind: &str) -> Option<String> {
    let (dir, name) = part.rsplit_once('/').unwrap_or(("", part));
    let text = pkg.text(&format!("{dir}/_rels/{name}.rels"))?;
    let doc = super::xml(&text).ok()?;
    let target = doc.descendants().find(|n| local(*n) == "Relationship" && attr(*n, "Type").is_some_and(|t| t.ends_with(kind)))?.attribute("Target")?.to_string();
    Some(super::resolve(dir, &target))
}

pub fn read(pkg: &mut Package) -> Result<RichDoc, String> {
    let pres_text = pkg.text("ppt/presentation.xml").ok_or("no presentation in the file")?;
    let pres = super::xml(&pres_text)?;
    let root = pres.root_element();
    let size = el(root, "sldSz");
    let page = PageSetup {
        width: size.and_then(|s| emu(s, "cx")).unwrap_or(720.0),
        height: size.and_then(|s| emu(s, "cy")).unwrap_or(540.0),
        margin: [0.0; 4],
        header: 0.0,
        footer: 0.0,
    };
    let rels = super::relationships(pkg, "ppt/_rels/presentation.xml.rels", "ppt");
    let slide_parts: Vec<String> = el(root, "sldIdLst").into_iter().flat_map(|l| els(l, "sldId")).filter_map(|s| rel_id(s, "id")).filter_map(|id| rels.get(id).cloned()).collect();
    if slide_parts.is_empty() {
        return Err("the presentation has no slides".into());
    }
    let mut r = Reader { pkg, pictures: Pictures::default(), defaults: ListStyle::default() };
    let mut masters: HashMap<String, Master> = HashMap::new();
    let mut layouts: HashMap<String, Frame> = HashMap::new();
    let mut slides = Vec::new();
    for part in slide_parts.iter().take(MAX_SLIDES) {
        let Some(layout_part) = rel_of_type(r.pkg, part, "/slideLayout") else { continue };
        let Some(master_part) = rel_of_type(r.pkg, &layout_part, "/slideMaster") else { continue };
        if !masters.contains_key(&master_part) {
            let m = r.master(&master_part, el(root, "defaultTextStyle"));
            masters.insert(master_part.clone(), m);
        }
        let master = &masters[&master_part];
        if !layouts.contains_key(&layout_part) {
            let l = r.layout(&layout_part, master);
            layouts.insert(layout_part.clone(), l);
        }
        let layout = &layouts[&layout_part];
        if let Some(slide) = r.slide(part, layout, master) {
            slides.push(slide);
        }
    }
    let note = (slide_parts.len() > MAX_SLIDES).then(|| format!("The first {MAX_SLIDES} slides"));
    Ok(RichDoc {
        blocks: Vec::new(),
        images: r.pictures.images,
        sheets: None,
        note,
        page,
        base_pt: BASE_PT,
        header: None,
        footer: None,
        slides: Some(slides),
    })
}

impl Reader<'_> {
    fn master(&mut self, part: &str, default_style: Option<Node>) -> Master {
        let theme_part = rel_of_type(self.pkg, part, "/theme");
        let theme = theme_part.and_then(|t| self.pkg.text(&t));
        let text = self.pkg.text(part).unwrap_or_default();
        let doc = super::xml(&text).ok();
        let root = doc.as_ref().map(|d| d.root_element());
        let colors = Colors::read(theme.as_deref(), root.and_then(|r| el(r, "clrMap")));
        self.defaults = ListStyle::read(default_style, &colors);
        let styles = root.and_then(|r| el(r, "txStyles"));
        let style = |name: &str| {
            let mut s = self.defaults.clone();
            s.over(&ListStyle::read(styles.and_then(|t| el(t, name)), &colors));
            s
        };
        let (title, body, other) = (style("titleStyle"), style("bodyStyle"), style("otherStyle"));
        let mut master = Master { frame: Frame::default(), title, body, other, colors };
        if let Some(root) = root {
            master.frame = self.frame(root, part, &master, None);
        }
        master
    }

    fn layout(&mut self, part: &str, master: &Master) -> Frame {
        let Some(text) = self.pkg.text(part) else { return Frame::default() };
        let Ok(doc) = super::xml(&text) else { return Frame::default() };
        let mut f = self.frame(doc.root_element(), part, master, Some(&master.frame));
        f.show_master = flag(doc.root_element(), "showMasterSp").unwrap_or(true);
        f
    }

    /// A layout's or master's placeholders, background and own shapes. `under`: the master,
    /// whose placeholders a layout's fill in.
    fn frame(&mut self, root: Node, part: &str, master: &Master, under: Option<&Frame>) -> Frame {
        let rels = part_rels(self.pkg, part);
        let mut frame = Frame { background: self.background(root, &rels, &master.colors), ..Default::default() };
        let Some(tree) = el(root, "cSld").and_then(|c| el(c, "spTree")) else { return frame };
        for sp in tree.children().filter(|c| c.is_element()) {
            if let Some(ph) = ph_of(sp) {
                let inherited = under.and_then(|u| find_ph(&u.placeholders, &ph, true));
                let sppr = el(sp, "spPr");
                let own_rect = sppr.and_then(|p| el(p, "xfrm")).and_then(xfrm_rect);
                let mut body = inherited.map(|i| i.body).unwrap_or_default();
                let mut list = inherited.map(|i| i.list.clone()).unwrap_or_default();
                if let Some(tx) = el(sp, "txBody") {
                    if let Some(bp) = el(tx, "bodyPr") {
                        body.over(&BodyProps::read(bp));
                    }
                    list.over(&ListStyle::read(el(tx, "lstStyle"), &master.colors));
                }
                frame.placeholders.push(PhInfo { ph, rect: own_rect.or(inherited.and_then(|i| i.rect)), body, list });
            }
        }
        // Its own shapes, the placeholders aside: they only lend the slides their place and style.
        let mut shapes = Vec::new();
        self.shapes(tree, &rels, master, None, Xf::IDENTITY, &mut shapes);
        frame.shapes = shapes;
        frame
    }

    fn background(&mut self, root: Node, rels: &HashMap<String, String>, colors: &Colors) -> Option<Background> {
        let bg = el(root, "cSld").and_then(|c| el(c, "bg"))?;
        if let Some(pr) = el(bg, "bgPr") {
            if let Some(blip) = el(pr, "blipFill").and_then(|b| el(b, "blip")).and_then(|b| rel_id(b, "embed")).and_then(|id| rels.get(id)) {
                if let Ok(idx) = self.pictures.get(self.pkg, blip) {
                    return Some(Background::Image(idx));
                }
            }
            return fill_in(pr, colors).flatten().map(Background::Color);
        }
        el(bg, "bgRef").and_then(|r| color_in(r, colors)).map(Background::Color)
    }

    fn slide(&mut self, part: &str, layout: &Frame, master: &Master) -> Option<Slide> {
        let text = self.pkg.text(part)?;
        let doc = super::xml(&text).ok()?;
        let root = doc.root_element();
        let rels = part_rels(self.pkg, part);
        let background = self.background(root, &rels, &master.colors).or(layout.background).or(master.frame.background);
        let mut shapes = Vec::new();
        if layout.show_master && flag(root, "showMasterSp").unwrap_or(true) {
            shapes.extend(master.frame.shapes.iter().cloned());
        }
        shapes.extend(layout.shapes.iter().cloned());
        if let Some(tree) = el(root, "cSld").and_then(|c| el(c, "spTree")) {
            self.shapes(tree, &rels, master, Some(layout), Xf::IDENTITY, &mut shapes);
        }
        let (background, background_image) = match background {
            Some(Background::Color(c)) => (c, None),
            Some(Background::Image(i)) => (master.colors.get("bg1").unwrap_or([255; 3]), Some(i)),
            None => (master.colors.get("bg1").unwrap_or([255; 3]), None),
        };
        Some(Slide { background, background_image, shapes })
    }

    /// The shapes of a shape tree or group. `layout`: set for a slide, whose placeholders are
    /// drawn; a layout's or master's are not.
    fn shapes(&mut self, tree: Node, rels: &HashMap<String, String>, master: &Master, layout: Option<&Frame>, xf: Xf, out: &mut Vec<Shape>) {
        for c in tree.children().filter(|c| c.is_element()) {
            match local(c) {
                "AlternateContent" => {
                    if let Some(alt) = el(c, "Fallback").or_else(|| el(c, "Choice")) {
                        self.shapes(alt, rels, master, layout, xf, out);
                    }
                }
                "grpSp" => {
                    let Some(x) = el(c, "grpSpPr").and_then(|p| el(p, "xfrm")) else {
                        self.shapes(c, rels, master, layout, xf, out);
                        continue;
                    };
                    let (Some([ox, oy, w, h]), Some(choff), Some(chext)) = (xfrm_rect(x), el(x, "chOff"), el(x, "chExt")) else {
                        self.shapes(c, rels, master, layout, xf, out);
                        continue;
                    };
                    let (cx, cy) = (emu(choff, "x").unwrap_or(0.0), emu(choff, "y").unwrap_or(0.0));
                    let (cw, ch) = (emu(chext, "cx").unwrap_or(w), emu(chext, "cy").unwrap_or(h));
                    let [mx, my, mw, mh] = xf.map([ox, oy, w, h]);
                    let inner = Xf {
                        off: [mx, my],
                        scale: [if cw > 0.0 { mw / cw } else { 1.0 }, if ch > 0.0 { mh / ch } else { 1.0 }],
                        child_off: [cx, cy],
                    };
                    self.shapes(c, rels, master, layout, inner, out);
                }
                "sp" | "cxnSp" | "pic" | "graphicFrame" => {
                    let ph = ph_of(c);
                    if ph.is_some() && layout.is_none() {
                        continue;
                    }
                    if let Some(s) = self.shape(c, rels, master, layout, ph, xf) {
                        out.push(s);
                    }
                }
                _ => {}
            }
        }
    }

    fn shape(&mut self, c: Node, rels: &HashMap<String, String>, master: &Master, layout: Option<&Frame>, ph: Option<Ph>, xf: Xf) -> Option<Shape> {
        let colors = &master.colors;
        let from_layout = ph.as_ref().and_then(|p| layout.and_then(|l| find_ph(&l.placeholders, p, false)));
        let from_master = ph.as_ref().and_then(|p| find_ph(&master.frame.placeholders, p, true));
        let inherited = from_layout.or(from_master);
        let sppr = el(c, "spPr");
        let own_rect = match local(c) {
            "graphicFrame" => el(c, "xfrm").and_then(xfrm_rect),
            _ => sppr.and_then(|p| el(p, "xfrm")).and_then(xfrm_rect),
        };
        let rect = xf.map(own_rect.or(inherited.and_then(|i| i.rect))?);
        let style = el(c, "style");
        let style_color = |name: &str| style.and_then(|s| el(s, name)).filter(|r| num(*r, "idx").unwrap_or(0.0) > 0.0).and_then(|r| color_in(r, colors));
        let fill = sppr.and_then(|p| fill_in(p, colors)).unwrap_or_else(|| style_color("fillRef"));
        let ln = sppr.and_then(|p| el(p, "ln"));
        let line_width = ln.and_then(|l| emu(l, "w")).unwrap_or(0.75).max(0.25);
        let line = match ln.and_then(|l| fill_in(l, colors)) {
            Some(c) => c,
            None => style_color("lnRef"),
        }
        .map(|c| (c, line_width));
        let prst = sppr.and_then(|p| el(p, "prstGeom")).and_then(|g| attr(g, "prst")).unwrap_or("rect");
        let xfrm = sppr.and_then(|p| el(p, "xfrm"));
        let geom = match (local(c), prst) {
            ("cxnSp", _) | (_, "line" | "straightConnector1") => Geom::Line {
                flip_h: xfrm.and_then(|x| flag(x, "flipH")).unwrap_or(false),
                flip_v: xfrm.and_then(|x| flag(x, "flipV")).unwrap_or(false),
            },
            (_, "roundRect") => Geom::RoundRect,
            (_, "ellipse" | "circle") => Geom::Ellipse,
            _ => Geom::Rect,
        };
        let content = match local(c) {
            "pic" => {
                let target = el(c, "blipFill").and_then(|b| el(b, "blip")).and_then(|b| rel_id(b, "embed")).and_then(|id| rels.get(id)).cloned();
                match target.map(|t| self.pictures.get(self.pkg, &t)) {
                    Some(Ok(idx)) => ShapeContent::Image(idx),
                    Some(Err(what)) => ShapeContent::Missing(what),
                    None => ShapeContent::Empty,
                }
            }
            "graphicFrame" => {
                let data = c.descendants().find(|n| n.is_element() && local(*n) == "graphicData");
                let uri = data.and_then(|d| attr(d, "uri")).unwrap_or("");
                if let Some(tbl) = data.and_then(|d| el(d, "tbl")) {
                    ShapeContent::Table(self.table(tbl, colors))
                } else if uri.ends_with("/chart") || uri.contains("chartex") {
                    ShapeContent::Missing("chart".into())
                } else if uri.ends_with("/diagram") {
                    ShapeContent::Missing("diagram".into())
                } else {
                    ShapeContent::Missing("object".into())
                }
            }
            _ => match el(c, "txBody") {
                Some(tx) => {
                    let kind = ph.as_ref().map(|p| master_kind(&p.kind));
                    let mut list = match kind {
                        Some("title") => master.title.clone(),
                        Some("body") => master.body.clone(),
                        Some(_) => master.other.clone(),
                        None => self.defaults.clone(),
                    };
                    let mut body = BodyProps::default();
                    for i in [from_master, from_layout].into_iter().flatten() {
                        list.over(&i.list);
                        body.over(&i.body);
                    }
                    if let Some(bp) = el(tx, "bodyPr") {
                        body.over(&BodyProps::read(bp));
                    }
                    let font_color = style.and_then(|s| el(s, "fontRef")).and_then(|r| color_in(r, colors));
                    let tb = text_body(tx, list, body, font_color, colors);
                    let blank = tb.blocks.iter().all(|b| matches!(b, Block::Para(p) if p.inlines.iter().all(|i| matches!(i, Inline::Text(s) if s.text.trim().is_empty()))));
                    if blank { ShapeContent::Empty } else { ShapeContent::Text(tb) }
                }
                None => ShapeContent::Empty,
            },
        };
        // A placeholder with nothing in it shows nothing on a slide; an empty text box neither.
        if matches!(content, ShapeContent::Empty) && fill.is_none() && line.is_none() {
            return None;
        }
        Some(Shape { rect, geom, fill, line, content })
    }

    fn table(&mut self, tbl: Node, colors: &Colors) -> Table {
        let widths: Vec<f32> = el(tbl, "tblGrid").into_iter().flat_map(|g| els(g, "gridCol")).map(|g| emu(g, "w").unwrap_or(0.0)).collect();
        let pr = el(tbl, "tblPr");
        let styled = pr.and_then(|p| el(p, "tableStyleId")).and_then(|s| s.text()).is_some_and(|s| s.trim() == MEDIUM_STYLE_2);
        let first_row = styled && pr.and_then(|p| flag(p, "firstRow")).unwrap_or(false);
        let banded = styled && pr.and_then(|p| flag(p, "bandRow")).unwrap_or(false);
        let accent = colors.get("accent1").unwrap_or([0x4F, 0x81, 0xBD]);
        let light = colors.get("lt1").unwrap_or([255; 3]);
        let tint = |t: f32| accent.map(|v| (v as f32 + (255.0 - v as f32) * (1.0 - t)).round() as u8);
        let mut rows = Vec::new();
        for (r, tr) in els(tbl, "tr").enumerate() {
            let header = first_row && r == 0;
            let data_row = if first_row { r.saturating_sub(1) } else { r };
            let style_fill = if !styled {
                None
            } else if header {
                Some(accent)
            } else if banded && data_row % 2 == 0 {
                Some(tint(0.4))
            } else {
                Some(tint(0.2))
            };
            let mut cells = Vec::new();
            for tc in els(tr, "tc") {
                if flag(tc, "hMerge").unwrap_or(false) {
                    continue;
                }
                let span = num(tc, "gridSpan").map_or(1, |v| v.max(1.0) as usize);
                let tcpr = el(tc, "tcPr");
                let fill = match tcpr.and_then(|p| fill_in(p, colors)) {
                    Some(f) => f,
                    None => style_fill,
                };
                let mut list = self.defaults.clone();
                if header {
                    let mut l = Level::default();
                    l.rpr.b = Some(true);
                    l.rpr.color = Some(light);
                    for level in &mut list.0 {
                        level.over(&l);
                    }
                }
                let blocks = if flag(tc, "vMerge").unwrap_or(false) {
                    Vec::new()
                } else {
                    el(tc, "txBody").map(|tx| text_body(tx, list, BodyProps::default(), None, colors).blocks).unwrap_or_default()
                };
                cells.push(Cell { blocks, span, fill });
            }
            rows.push(cells);
        }
        Table { widths, rows }
    }
}

/// The placeholder in a layout or master that one on a slide (or layout) takes after: the same
/// index, else the same kind. `by_kind`: a master is searched by kind alone.
fn find_ph<'a>(list: &'a [PhInfo], ph: &Ph, by_kind: bool) -> Option<&'a PhInfo> {
    if by_kind {
        let kind = master_kind(&ph.kind);
        return list.iter().find(|p| master_kind(&p.ph.kind) == kind);
    }
    list.iter().find(|p| ph.idx != 0 && p.ph.idx == ph.idx).or_else(|| list.iter().find(|p| p.ph.kind == ph.kind)).or_else(|| list.iter().find(|p| master_kind(&p.ph.kind) == master_kind(&ph.kind) && p.ph.idx == ph.idx))
}

/// The paragraphs of a text body in the shape's styles.
fn text_body(tx: Node, mut list: ListStyle, body: BodyProps, font_color: Option<[u8; 3]>, colors: &Colors) -> TextBox {
    list.over(&ListStyle::read(el(tx, "lstStyle"), colors));
    let scale = body.font_scale.unwrap_or(1.0);
    let reduce = body.line_reduction.unwrap_or(0.0);
    let mut counters = [0u32; 9];
    let mut blocks = Vec::new();
    for p in els(tx, "p") {
        let ppr = el(p, "pPr");
        let lvl = ppr.and_then(|pp| num(pp, "lvl")).map_or(0, |v| (v as usize).min(8));
        let mut level = list.0[lvl].clone();
        // A shape's own text color (its style's font reference) beats the list style's; the
        // runs' own beat it.
        if let Some(c) = font_color {
            level.rpr.color = Some(c);
        }
        if let Some(pp) = ppr {
            level.over(&Level::read(pp, colors));
        }
        let size_of = |r: &RunProps| r.sz.unwrap_or(18.0) * scale;
        let style_of = |r: &RunProps| SpanStyle {
            bold: r.b.unwrap_or(false),
            italic: r.i.unwrap_or(false),
            underline: r.u.unwrap_or(false),
            strike: r.strike.unwrap_or(false),
            // Text nothing gives a color is in the theme's text color.
            color: Some(r.color.or(colors.get("tx1")).unwrap_or([0, 0, 0])),
            size: Some(size_of(r) / BASE_PT),
        };
        let mut inlines = Vec::new();
        for c in p.children().filter(|c| c.is_element()) {
            match local(c) {
                "r" | "fld" => {
                    let mut rp = level.rpr.clone();
                    if let Some(rpr) = el(c, "rPr") {
                        rp.over(&RunProps::read(rpr, colors));
                    }
                    let text = el(c, "t").and_then(|t| t.text()).unwrap_or("");
                    super::push_text(&mut inlines, text, &style_of(&rp));
                }
                "br" => {
                    let mut rp = level.rpr.clone();
                    if let Some(rpr) = el(c, "rPr") {
                        rp.over(&RunProps::read(rpr, colors));
                    }
                    super::push_text(&mut inlines, "\n", &style_of(&rp));
                }
                _ => {}
            }
        }
        let has_text = inlines.iter().any(|i| matches!(i, Inline::Text(s) if !s.text.trim().is_empty()));
        // Size of the paragraph's text, for spacing given in lines; an empty one is as high as the
        // size it ends with.
        let end = el(p, "endParaRPr").map(|e| RunProps::read(e, colors)).unwrap_or_default();
        let mut end_rp = level.rpr.clone();
        end_rp.over(&end);
        let size = inlines.iter().find_map(|i| if let Inline::Text(s) = i { s.style.size.map(|v| v * BASE_PT) } else { None }).unwrap_or_else(|| size_of(&end_rp));
        if !has_text && inlines.is_empty() {
            // Keeps the empty line's height in the paragraph's size.
            super::push_text(&mut inlines, " ", &style_of(&end_rp));
        }
        let marker = if !has_text {
            None
        } else {
            match &level.bullet {
                Some(Bullet::Char(c)) => Some(c.clone()),
                Some(Bullet::Auto(scheme, start)) => {
                    counters[lvl] += 1;
                    for deeper in counters.iter_mut().skip(lvl + 1) {
                        *deeper = 0;
                    }
                    Some(auto_number(scheme, start + counters[lvl] - 1))
                }
                _ => None,
            }
        };
        if !matches!(level.bullet, Some(Bullet::Auto(..))) {
            counters[lvl] = 0;
        }
        let pts = |s: Option<Space>| s.map(|s| match s {
            Space::Pt(p) => p * scale,
            Space::Lines(l) => l * size * 1.2,
        });
        let line = level.line.map(|s| match s {
            Space::Lines(l) => l,
            Space::Pt(p) => p / (size * 1.2),
        });
        let line = line.map(|l| l - reduce).or((reduce > 0.0).then_some(1.0 - reduce));
        let left = level.mar_l.unwrap_or(0.0);
        let indent = level.indent.unwrap_or(0.0);
        blocks.push(Block::Para(Para {
            inlines,
            align: level.algn.unwrap_or_default(),
            marker,
            before: pts(level.before),
            after: pts(level.after),
            line,
            left: Some(left),
            hang: Some((-indent).max(0.0)),
            ..Default::default()
        }));
    }
    let [l, t, r, b] = body.ins;
    TextBox { blocks, anchor: body.anchor.unwrap_or(VAnchor::Top), inset: [l.unwrap_or(7.2), t.unwrap_or(3.6), r.unwrap_or(7.2), b.unwrap_or(3.6)] }
}

/// An automatic list number as its scheme writes it (`arabicPeriod` 3 is "3.").
fn auto_number(scheme: &str, n: u32) -> String {
    let (fmt, rest) = if let Some(r) = scheme.strip_prefix("arabic") {
        ("decimal", r)
    } else if let Some(r) = scheme.strip_prefix("alphaLc") {
        ("lowerLetter", r)
    } else if let Some(r) = scheme.strip_prefix("alphaUc") {
        ("upperLetter", r)
    } else if let Some(r) = scheme.strip_prefix("romanLc") {
        ("lowerRoman", r)
    } else if let Some(r) = scheme.strip_prefix("romanUc") {
        ("upperRoman", r)
    } else {
        ("decimal", "Period")
    };
    let v = super::format_number(n, fmt);
    match rest {
        "ParenR" => format!("{v})"),
        "ParenBoth" => format!("({v})"),
        "Plain" => v,
        _ => format!("{v}."),
    }
}
