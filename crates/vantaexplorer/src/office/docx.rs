//! Word documents (.docx): `word/document.xml` with the styles, list numbering and pictures it
//! refers to. Headers, footers, footnotes and comments are left out.

use super::{Align, Block, Cell, Inline, Package, Para, Pictures, RichDoc, SpanStyle, Table};
use roxmltree::Node;
use std::collections::HashMap;

/// Word's main namespace, transitional and strict.
fn is_w(n: Node) -> bool {
    n.is_element() && n.tag_name().namespace().is_some_and(|ns| ns.ends_with("/wordprocessingml/2006/main") || ns.ends_with("/wordprocessingml/main"))
}

fn w<'a, 'i>(n: Node<'a, 'i>, name: &str) -> bool {
    is_w(n) && n.tag_name().name() == name
}

/// The first child element in Word's namespace called `name`.
fn child<'a, 'i>(n: Node<'a, 'i>, name: &str) -> Option<Node<'a, 'i>> {
    n.children().find(|c| w(*c, name))
}

/// An attribute in Word's namespace (`w:val`), whatever prefix the file gives it.
fn attr<'a>(n: Node<'a, '_>, name: &str) -> Option<&'a str> {
    n.attributes().find(|a| a.name() == name && a.namespace().is_some_and(|ns| ns.contains("wordprocessingml"))).map(|a| a.value())
}

fn val<'a>(n: Node<'a, '_>) -> Option<&'a str> {
    attr(n, "val")
}

/// A toggle such as `<w:b/>` or `<w:b w:val="false"/>`.
fn on(n: Node) -> bool {
    !matches!(val(n), Some("0" | "false" | "off" | "none"))
}

fn local<'a>(n: Node<'a, '_>) -> &'a str {
    n.tag_name().name()
}

/// Run properties, each unset until something sets it, so styles can be laid over each other.
#[derive(Clone, Default, Debug)]
struct RProps {
    b: Option<bool>,
    i: Option<bool>,
    u: Option<bool>,
    strike: Option<bool>,
    color: Option<Option<[u8; 3]>>,
    /// Points.
    sz: Option<f32>,
}

impl RProps {
    fn read(rpr: Node) -> Self {
        let mut p = RProps::default();
        for c in rpr.children().filter(|c| is_w(*c)) {
            match local(c) {
                "b" => p.b = Some(on(c)),
                "i" => p.i = Some(on(c)),
                "u" => p.u = Some(on(c)),
                "strike" | "dstrike" => p.strike = Some(on(c)),
                "color" => p.color = Some(val(c).and_then(super::hex_color)),
                "sz" => p.sz = val(c).and_then(|v| v.parse::<f32>().ok()).map(|half| half / 2.0),
                _ => {}
            }
        }
        p
    }

    /// `other` laid over this one.
    fn over(&mut self, other: &RProps) {
        self.b = other.b.or(self.b);
        self.i = other.i.or(self.i);
        self.u = other.u.or(self.u);
        self.strike = other.strike.or(self.strike);
        self.color = other.color.or(self.color);
        self.sz = other.sz.or(self.sz);
    }

    fn style(&self, normal_pt: f32) -> SpanStyle {
        SpanStyle {
            bold: self.b.unwrap_or(false),
            italic: self.i.unwrap_or(false),
            underline: self.u.unwrap_or(false),
            strike: self.strike.unwrap_or(false),
            color: self.color.flatten(),
            size: self.sz.filter(|s| (s - normal_pt).abs() > 0.25).map(|s| s / normal_pt),
        }
    }
}

#[derive(Default)]
struct Style {
    name: String,
    based: Option<String>,
    rpr: RProps,
    jc: Option<Align>,
    num: Option<(String, u8)>,
    outline: Option<u8>,
}

/// Run properties, alignment, list (id and level) and heading level of a paragraph style.
type ParaStyle = (RProps, Option<Align>, Option<(String, u8)>, Option<u8>);

#[derive(Default)]
struct Styles {
    para: HashMap<String, Style>,
    chars: HashMap<String, Style>,
    default_para: Option<String>,
    defaults: RProps,
}

fn align(v: &str) -> Option<Align> {
    Some(match v {
        "center" => Align::Center,
        "right" | "end" => Align::Right,
        "both" | "distribute" => Align::Justify,
        "left" | "start" => Align::Left,
        _ => return None,
    })
}

/// The numbering of a paragraph (`w:numPr`): its list and level.
fn num_pr(ppr: Node) -> Option<(String, u8)> {
    let np = child(ppr, "numPr")?;
    let id = child(np, "numId").and_then(val)?.to_string();
    let lvl = child(np, "ilvl").and_then(val).and_then(|v| v.parse().ok()).unwrap_or(0);
    Some((id, lvl))
}

impl Styles {
    fn read(text: Option<String>) -> Self {
        let mut s = Styles::default();
        let Some(text) = text else { return s };
        let Ok(doc) = super::xml(&text) else { return s };
        for n in doc.root_element().children().filter(|c| is_w(*c)) {
            match local(n) {
                "docDefaults" => {
                    if let Some(rpr) = n.descendants().find(|d| w(*d, "rPr")) {
                        s.defaults = RProps::read(rpr);
                    }
                }
                "style" => {
                    let Some(id) = attr(n, "styleId") else { continue };
                    let mut st = Style {
                        name: child(n, "name").and_then(val).unwrap_or(id).to_lowercase(),
                        based: child(n, "basedOn").and_then(val).map(str::to_string),
                        rpr: child(n, "rPr").map(RProps::read).unwrap_or_default(),
                        ..Default::default()
                    };
                    if let Some(ppr) = child(n, "pPr") {
                        st.jc = child(ppr, "jc").and_then(val).and_then(align);
                        st.num = num_pr(ppr);
                        st.outline = child(ppr, "outlineLvl").and_then(val).and_then(|v| v.parse().ok());
                    }
                    match attr(n, "type") {
                        Some("paragraph") | None => {
                            if attr(n, "default").is_some_and(|d| d == "1" || d == "true") {
                                s.default_para = Some(id.to_string());
                            }
                            s.para.insert(id.to_string(), st);
                        }
                        Some("character") => {
                            s.chars.insert(id.to_string(), st);
                        }
                        _ => {}
                    }
                }
                _ => {}
            }
        }
        s
    }

    /// A style and those it is based on, the base first. Capped against loops.
    fn chain<'s>(map: &'s HashMap<String, Style>, id: &str) -> Vec<&'s Style> {
        let mut out = Vec::new();
        let mut cur = Some(id.to_string());
        while let Some(id) = cur {
            let Some(st) = map.get(&id) else { break };
            out.push(st);
            if out.len() > 20 {
                break;
            }
            cur = st.based.clone();
        }
        out.reverse();
        out
    }

    /// What a paragraph style gives: run properties, alignment, numbering and heading level.
    fn paragraph(&self, id: Option<&str>) -> ParaStyle {
        let mut rpr = self.defaults.clone();
        let (mut jc, mut num, mut heading) = (None, None, None);
        let Some(id) = id.or(self.default_para.as_deref()) else { return (rpr, jc, num, heading) };
        for st in Self::chain(&self.para, id) {
            rpr.over(&st.rpr);
            jc = st.jc.or(jc);
            num = st.num.clone().or(num);
            if let Some(h) = heading_of(&st.name, st.outline) {
                heading = Some(h);
            }
        }
        (rpr, jc, num, heading)
    }

    fn character(&self, id: &str) -> RProps {
        let mut rpr = RProps::default();
        for st in Self::chain(&self.chars, id) {
            rpr.over(&st.rpr);
        }
        rpr
    }
}

/// "Title" and "heading 1".."heading 6" by their built-in names, which stay English whatever the
/// language of the document (the style ids are the ones that get translated); else the outline
/// level a style declares.
fn heading_of(name: &str, outline: Option<u8>) -> Option<u8> {
    if name == "title" {
        return Some(0);
    }
    if let Some(n) = name.strip_prefix("heading ").and_then(|n| n.trim().parse::<u8>().ok()) {
        return Some(n.clamp(1, 6));
    }
    outline.filter(|o| *o < 9).map(|o| (o + 1).min(6))
}

struct Level {
    start: u32,
    fmt: String,
    text: String,
}

/// List numbering: which list a paragraph is in, how its levels are written, and how far each has
/// counted.
#[derive(Default)]
struct Numbering {
    /// numId to (abstract numbering, start overrides per level).
    nums: HashMap<String, (String, HashMap<u8, u32>)>,
    abstracts: HashMap<String, HashMap<u8, Level>>,
    counters: HashMap<String, [u32; 9]>,
}

impl Numbering {
    fn read(text: Option<String>) -> Self {
        let mut n = Numbering::default();
        let Some(text) = text else { return n };
        let Ok(doc) = super::xml(&text) else { return n };
        for node in doc.root_element().children().filter(|c| is_w(*c)) {
            match local(node) {
                "abstractNum" => {
                    let Some(id) = attr(node, "abstractNumId") else { continue };
                    let mut levels = HashMap::new();
                    for l in node.children().filter(|c| w(*c, "lvl")) {
                        let Some(ilvl) = attr(l, "ilvl").and_then(|v| v.parse::<u8>().ok()) else { continue };
                        levels.insert(
                            ilvl,
                            Level {
                                start: child(l, "start").and_then(val).and_then(|v| v.parse().ok()).unwrap_or(1),
                                fmt: child(l, "numFmt").and_then(val).unwrap_or("decimal").to_string(),
                                text: child(l, "lvlText").and_then(val).unwrap_or("").to_string(),
                            },
                        );
                    }
                    n.abstracts.insert(id.to_string(), levels);
                }
                "num" => {
                    let (Some(id), Some(abs)) = (attr(node, "numId"), child(node, "abstractNumId").and_then(val)) else { continue };
                    let mut starts = HashMap::new();
                    for o in node.children().filter(|c| w(*c, "lvlOverride")) {
                        if let (Some(l), Some(s)) = (attr(o, "ilvl").and_then(|v| v.parse().ok()), child(o, "startOverride").and_then(val).and_then(|v| v.parse().ok())) {
                            starts.insert(l, s);
                        }
                    }
                    n.nums.insert(id.to_string(), (abs.to_string(), starts));
                }
                _ => {}
            }
        }
        n
    }

    /// The marker of the next item in list `id` at `ilvl`, counting it.
    fn marker(&mut self, id: &str, ilvl: u8) -> Option<String> {
        let ilvl = ilvl.min(8);
        let (abs, starts) = self.nums.get(id)?;
        let levels = self.abstracts.get(abs)?;
        let level = levels.get(&ilvl)?;
        let start_of = |l: u8| starts.get(&l).copied().or(levels.get(&l).map(|x| x.start)).unwrap_or(1);
        let counters = self.counters.entry(id.to_string()).or_insert([0; 9]);
        counters[ilvl as usize] = if counters[ilvl as usize] == 0 { start_of(ilvl) } else { counters[ilvl as usize] + 1 };
        for deeper in counters.iter_mut().skip(ilvl as usize + 1) {
            *deeper = 0;
        }
        match level.fmt.as_str() {
            "none" => None,
            "bullet" => Some(super::bullet(&level.text)),
            _ => {
                let mut text = level.text.clone();
                for l in 0..=ilvl {
                    let n = if counters[l as usize] == 0 { start_of(l) } else { counters[l as usize] };
                    let fmt = levels.get(&l).map(|x| x.fmt.as_str()).unwrap_or("decimal");
                    text = text.replace(&format!("%{}", l + 1), &super::format_number(n, fmt));
                }
                Some(text)
            }
        }
    }
}

struct Reader<'p> {
    pkg: &'p mut Package,
    styles: Styles,
    numbering: Numbering,
    rels: HashMap<String, String>,
    pictures: Pictures,
    normal_pt: f32,
    blocks: usize,
}

pub fn read(pkg: &mut Package) -> Result<RichDoc, String> {
    let main = main_part(pkg);
    let text = pkg.text(&main).ok_or("no document text in the file")?;
    let doc = super::xml(&text)?;
    let dir = main.rsplit_once('/').map(|(d, _)| d.to_string()).unwrap_or_default();
    let (rels_name, styles_name, numbering_name) = (format!("{dir}/_rels/{}.rels", main.rsplit('/').next().unwrap_or("document.xml")), format!("{dir}/styles.xml"), format!("{dir}/numbering.xml"));
    let rels = super::relationships(pkg, &rels_name, &dir);
    let styles = Styles::read(pkg.text(&styles_name));
    let numbering = Numbering::read(pkg.text(&numbering_name));
    let normal_pt = styles.paragraph(None).0.sz.unwrap_or(11.0).max(4.0);
    let mut r = Reader { pkg, styles, numbering, rels, pictures: Pictures::default(), normal_pt, blocks: 0 };
    let body = doc.root_element().children().find(|c| w(*c, "body")).ok_or("no document body")?;
    let mut blocks = Vec::new();
    r.body(body, &mut blocks);
    let note = (r.blocks > super::MAX_BLOCKS).then(|| "The rest of the document is not shown.".to_string());
    Ok(RichDoc { blocks, images: r.pictures.images, sheets: None, note })
}

/// Where the document text is: the package says so in `_rels/.rels`; `word/document.xml` is what
/// it says in practice.
fn main_part(pkg: &mut Package) -> String {
    if let Some(text) = pkg.text("_rels/.rels") {
        if let Ok(doc) = super::xml(&text) {
            if let Some(t) = doc
                .descendants()
                .filter(|n| n.tag_name().name() == "Relationship")
                .find(|n| n.attribute("Type").is_some_and(|t| t.ends_with("/officeDocument")))
                .and_then(|n| n.attribute("Target"))
            {
                let t = super::resolve("", t);
                if pkg.has(&t) {
                    return t;
                }
            }
        }
    }
    "word/document.xml".into()
}

impl Reader<'_> {
    fn full(&self) -> bool {
        self.blocks > super::MAX_BLOCKS
    }

    /// Block-level content: the body, a table cell, a text box.
    fn body(&mut self, node: Node, out: &mut Vec<Block>) {
        for c in node.children().filter(|c| c.is_element()) {
            if self.full() {
                return;
            }
            if is_w(c) {
                match local(c) {
                    "p" => self.paragraph(c, out),
                    "tbl" => {
                        self.blocks += 1;
                        let t = self.table(c);
                        out.push(Block::Table(t));
                    }
                    "sdt" => {
                        if let Some(content) = child(c, "sdtContent") {
                            self.body(content, out);
                        }
                    }
                    "customXml" | "ins" | "smartTag" | "sdtContent" => self.body(c, out),
                    _ => {}
                }
            } else if let Some(choice) = alternate_choice(c) {
                self.body(choice, out);
            }
        }
    }

    fn paragraph(&mut self, p: Node, out: &mut Vec<Block>) {
        self.blocks += 1;
        let ppr = child(p, "pPr");
        let style_id = ppr.and_then(|pp| child(pp, "pStyle")).and_then(val);
        let (rpr, mut jc, mut num, heading) = self.styles.paragraph(style_id);
        let mut ind = 0.0f32;
        if let Some(pp) = ppr {
            if let Some(j) = child(pp, "jc").and_then(val).and_then(align) {
                jc = Some(j);
            }
            if let Some(n) = num_pr(pp) {
                num = Some(n);
            }
            if let Some(i) = child(pp, "ind") {
                ind = attr(i, "left").or(attr(i, "start")).and_then(|v| v.parse::<f32>().ok()).unwrap_or(0.0);
            }
        }
        let mut para = Para { heading, align: jc.unwrap_or_default(), ..Default::default() };
        if let Some((id, lvl)) = num.filter(|(id, _)| id != "0") {
            para.marker = self.numbering.marker(&id, lvl);
            para.level = lvl + 1;
        } else if ind > 0.0 {
            // Twentieths of a point; a step is half an inch.
            para.level = ((ind / 720.0).round() as u8).min(6);
        }
        let mut extra = Vec::new();
        let mut page_break = false;
        self.inline(p, &rpr, &mut para.inlines, &mut extra, &mut page_break);
        out.push(Block::Para(para));
        out.extend(extra);
        if page_break {
            out.push(Block::PageBreak);
        }
    }

    /// Runs and what wraps them in a paragraph.
    fn inline(&mut self, node: Node, base: &RProps, inlines: &mut Vec<Inline>, extra: &mut Vec<Block>, page_break: &mut bool) {
        for c in node.children().filter(|c| c.is_element()) {
            if is_w(c) {
                match local(c) {
                    "r" => self.run(c, base, inlines, extra, page_break),
                    "hyperlink" | "smartTag" | "ins" | "customXml" | "fldSimple" | "sdtContent" | "bdo" | "dir" | "moveTo" => self.inline(c, base, inlines, extra, page_break),
                    "sdt" => {
                        if let Some(content) = child(c, "sdtContent") {
                            self.inline(content, base, inlines, extra, page_break);
                        }
                    }
                    _ => {}
                }
            } else if c.tag_name().namespace().is_some_and(|ns| ns.ends_with("/math")) && matches!(local(c), "oMath" | "oMathPara") {
                // An equation, as its text: better than a hole where it was.
                let text: String = c.descendants().filter(|d| d.is_element() && local(*d) == "t").filter_map(|d| d.text()).collect();
                let mut st = base.style(self.normal_pt);
                st.italic = true;
                super::push_text(inlines, &text, &st);
            } else if let Some(choice) = alternate_choice(c) {
                self.inline(choice, base, inlines, extra, page_break);
            }
        }
    }

    fn run(&mut self, r: Node, base: &RProps, inlines: &mut Vec<Inline>, extra: &mut Vec<Block>, page_break: &mut bool) {
        let mut rpr = base.clone();
        if let Some(rp) = child(r, "rPr") {
            if let Some(cs) = child(rp, "rStyle").and_then(val) {
                rpr.over(&self.styles.character(cs));
            }
            rpr.over(&RProps::read(rp));
        }
        let style = rpr.style(self.normal_pt);
        self.run_content(r, &style, inlines, extra, page_break);
    }

    fn run_content(&mut self, r: Node, style: &SpanStyle, inlines: &mut Vec<Inline>, extra: &mut Vec<Block>, page_break: &mut bool) {
        for c in r.children().filter(|c| c.is_element()) {
            if is_w(c) {
                match local(c) {
                    "t" => super::push_text(inlines, c.text().unwrap_or(""), style),
                    "tab" | "ptab" => super::push_text(inlines, "    ", style),
                    "br" => {
                        if attr(c, "type") == Some("page") {
                            *page_break = true;
                        } else {
                            super::push_text(inlines, "\n", style);
                        }
                    }
                    "cr" => super::push_text(inlines, "\n", style),
                    "noBreakHyphen" => super::push_text(inlines, "-", style),
                    "sym" => {
                        if let Some(ch) = attr(c, "char").and_then(|h| u32::from_str_radix(h, 16).ok()).and_then(char::from_u32) {
                            let s = if ('\u{F000}'..='\u{F0FF}').contains(&ch) { super::bullet(&ch.to_string()) } else { ch.to_string() };
                            super::push_text(inlines, &s, style);
                        }
                    }
                    "drawing" | "pict" | "object" => self.drawing(c, inlines, extra),
                    _ => {}
                }
            } else if let Some(choice) = alternate_choice(c) {
                self.run_content(choice, style, inlines, extra, page_break);
            }
        }
    }

    /// A picture, a chart, or a text box, wherever in a drawing it is.
    fn drawing(&mut self, d: Node, inlines: &mut Vec<Inline>, extra: &mut Vec<Block>) {
        // Text boxes: their paragraphs come after the one they are anchored in.
        let boxes: Vec<Node> = d.descendants().filter(|n| w(*n, "txbxContent")).collect();
        for b in boxes.iter().filter(|b| !b.ancestors().skip(1).any(|a| w(a, "txbxContent"))) {
            self.body(*b, extra);
        }
        let blip = d.descendants().find(|n| n.is_element() && matches!(local(*n), "blip" | "imagedata"));
        let rid = blip.and_then(|b| b.attributes().find(|a| matches!(a.name(), "embed" | "id") && a.namespace().is_some_and(|ns| ns.contains("relationships"))).map(|a| a.value()));
        if let Some(target) = rid.and_then(|id| self.rels.get(id)).cloned() {
            let (w, h) = extent(d);
            match self.pictures.get(self.pkg, &target) {
                Ok(idx) => inlines.push(Inline::Image { idx, w, h }),
                Err(what) => inlines.push(Inline::Missing(what)),
            }
            return;
        }
        if d.descendants().any(|n| n.is_element() && local(n) == "chart") {
            inlines.push(Inline::Missing("chart".into()));
        }
    }

    fn table(&mut self, t: Node) -> Table {
        let widths = child(t, "tblGrid")
            .map(|g| g.children().filter(|c| w(*c, "gridCol")).map(|c| attr(c, "w").and_then(|v| v.parse::<f32>().ok()).unwrap_or(0.0)).collect())
            .unwrap_or_default();
        let mut rows = Vec::new();
        for tr in rows_of(t) {
            let mut cells = Vec::new();
            for tc in cells_of(tr) {
                let tcpr = child(tc, "tcPr");
                let span = tcpr.and_then(|p| child(p, "gridSpan")).and_then(val).and_then(|v| v.parse().ok()).unwrap_or(1usize).max(1);
                let fill = tcpr.and_then(|p| child(p, "shd")).and_then(|s| attr(s, "fill")).and_then(super::hex_color);
                // A cell merged into the one above shows nothing of its own.
                let merged = tcpr.and_then(|p| child(p, "vMerge")).is_some_and(|m| val(m) != Some("restart"));
                let mut blocks = Vec::new();
                if !merged {
                    self.body(tc, &mut blocks);
                }
                cells.push(Cell { blocks, span, fill });
            }
            rows.push(cells);
        }
        Table { widths, rows }
    }
}

/// The rows of a table, through any content control around them.
fn rows_of<'a, 'i>(t: Node<'a, 'i>) -> Vec<Node<'a, 'i>> {
    let mut out = Vec::new();
    for c in t.children().filter(|c| is_w(*c)) {
        match local(c) {
            "tr" => out.push(c),
            "sdt" | "customXml" => {
                if let Some(content) = child(c, "sdtContent").or(Some(c)) {
                    out.extend(content.children().filter(|r| w(*r, "tr")));
                }
            }
            _ => {}
        }
    }
    out
}

fn cells_of<'a, 'i>(tr: Node<'a, 'i>) -> Vec<Node<'a, 'i>> {
    let mut out = Vec::new();
    for c in tr.children().filter(|c| is_w(*c)) {
        match local(c) {
            "tc" => out.push(c),
            "sdt" | "customXml" => {
                if let Some(content) = child(c, "sdtContent").or(Some(c)) {
                    out.extend(content.children().filter(|r| w(*r, "tc")));
                }
            }
            _ => {}
        }
    }
    out
}

/// For `mc:AlternateContent`, the choice to read (Word's own newer form; its fallback is the
/// same thing again in older markup).
fn alternate_choice<'a, 'i>(n: Node<'a, 'i>) -> Option<Node<'a, 'i>> {
    if !(n.is_element() && local(n) == "AlternateContent") {
        return None;
    }
    n.children().find(|c| c.is_element() && local(*c) == "Choice").or_else(|| n.children().find(|c| c.is_element() && local(*c) == "Fallback"))
}

/// The size a drawing is shown at, in points: `wp:extent` in EMU (12700 to a point), or a VML
/// `style="width:...;height:..."`.
fn extent(d: Node) -> (f32, f32) {
    if let Some(e) = d.descendants().find(|n| n.is_element() && local(*n) == "extent") {
        let get = |k| e.attribute(k).and_then(|v: &str| v.parse::<f32>().ok()).unwrap_or(0.0) / 12700.0;
        let (w, h) = (get("cx"), get("cy"));
        if w > 0.0 && h > 0.0 {
            return (w, h);
        }
    }
    if let Some(style) = d.descendants().filter_map(|n| n.attribute("style")).find(|s| s.contains("width")) {
        let get = |k: &str| {
            style.split(';').find_map(|kv| kv.split_once(':').filter(|(key, _)| key.trim() == k)).map(|(_, v)| v.trim()).and_then(css_points)
        };
        if let (Some(w), Some(h)) = (get("width"), get("height")) {
            return (w, h);
        }
    }
    (0.0, 0.0)
}

/// A CSS length in points.
fn css_points(v: &str) -> Option<f32> {
    let (num, unit) = v.split_at(v.find(|c: char| c.is_ascii_alphabetic()).unwrap_or(v.len()));
    let n: f32 = num.trim().parse().ok()?;
    Some(match unit {
        "pt" | "" => n,
        "in" => n * 72.0,
        "cm" => n * 72.0 / 2.54,
        "mm" => n * 72.0 / 25.4,
        "px" => n * 0.75,
        _ => return None,
    })
}
