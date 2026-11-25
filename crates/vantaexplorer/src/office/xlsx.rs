//! Excel workbooks (.xlsx): one sheet at a time, as the grid of what its cells show. Formulas
//! show the value Excel saved with them, in the cell's number format; dates are recognized by
//! theirs and written ISO.

use super::{Block, Package, RichDoc, Sheet, SheetCell};
use roxmltree::Node;
use std::collections::HashMap;

fn local<'a>(n: Node<'a, '_>) -> &'a str {
    n.tag_name().name()
}

fn el<'a, 'i>(n: Node<'a, 'i>, name: &str) -> Option<Node<'a, 'i>> {
    n.children().find(|c| c.is_element() && local(*c) == name)
}

/// An attribute by its local name, whatever namespace (`r:id`).
fn attr<'a>(n: Node<'a, '_>, name: &str) -> Option<&'a str> {
    n.attributes().find(|a| a.name() == name).map(|a| a.value())
}

pub fn read(pkg: &mut Package, page: u32) -> Result<RichDoc, String> {
    let wb_text = pkg.text("xl/workbook.xml").ok_or("no workbook in the file")?;
    let wb = super::xml(&wb_text)?;
    let date1904 = wb.descendants().find(|n| local(*n) == "workbookPr").and_then(|n| attr(n, "date1904")).is_some_and(|v| v == "1" || v == "true");
    let sheets: Vec<(String, String)> = wb
        .descendants()
        .filter(|n| n.is_element() && local(*n) == "sheet")
        .filter_map(|n| Some((attr(n, "name")?.to_string(), n.attributes().find(|a| a.name() == "id" && a.namespace().is_some())?.value().to_string())))
        .collect();
    if sheets.is_empty() {
        return Err("the workbook has no sheets".into());
    }
    let rels = super::relationships(pkg, "xl/_rels/workbook.xml.rels", "xl");
    let shared = shared_strings(pkg);
    let formats = cell_formats(pkg);
    let (_, rid) = &sheets[(page as usize).min(sheets.len() - 1)];
    let part = rels.get(rid).cloned().ok_or("a sheet the workbook lists is missing")?;
    let text = pkg.text(&part).ok_or("a sheet the workbook lists is missing")?;
    let doc = super::xml(&text)?;
    let sheet = sheet(&doc, &shared, &formats, date1904, Seps::system());
    Ok(RichDoc { blocks: vec![Block::Sheet(sheet)], images: Vec::new(), sheets: Some(sheets.into_iter().map(|(n, _)| n).collect()), note: None, page: Default::default(), base_pt: 11.0, header: None, footer: None, slides: None })
}

/// The text of every shared string, in order. Rich text is flattened; phonetic runs are left out.
fn shared_strings(pkg: &mut Package) -> Vec<String> {
    let Some(text) = pkg.text("xl/sharedStrings.xml") else { return Vec::new() };
    let Ok(doc) = super::xml(&text) else { return Vec::new() };
    doc.root_element().children().filter(|n| n.is_element() && local(*n) == "si").map(string_item).collect()
}

/// The text of an `si` or an inline `is`: its `t`, or the `t` of each of its runs.
fn string_item(si: Node) -> String {
    si.descendants()
        .filter(|n| n.is_element() && local(*n) == "t" && !n.ancestors().any(|a| a.is_element() && local(a) == "rPh"))
        .filter_map(|n| n.text())
        .collect()
}

#[derive(Clone, PartialEq, Debug)]
enum Kind {
    /// A number in a format code; `None` is the general format.
    Number(Option<String>),
    Date,
    DateTime,
    Time,
}

/// What each cell style index shows numbers as.
fn cell_formats(pkg: &mut Package) -> Vec<Kind> {
    let Some(text) = pkg.text("xl/styles.xml") else { return Vec::new() };
    let Ok(doc) = super::xml(&text) else { return Vec::new() };
    let root = doc.root_element();
    let custom: HashMap<u32, String> = el(root, "numFmts")
        .map(|f| {
            f.children()
                .filter(|n| n.is_element() && local(*n) == "numFmt")
                .filter_map(|n| Some((attr(n, "numFmtId")?.parse().ok()?, attr(n, "formatCode")?.to_string())))
                .collect()
        })
        .unwrap_or_default();
    let Some(xfs) = el(root, "cellXfs") else { return Vec::new() };
    xfs.children()
        .filter(|n| n.is_element() && local(*n) == "xf")
        .map(|xf| {
            let id: u32 = attr(xf, "numFmtId").and_then(|v| v.parse().ok()).unwrap_or(0);
            match custom.get(&id) {
                Some(code) => kind_of_code(code),
                None => builtin_kind(id),
            }
        })
        .collect()
}

fn builtin_kind(id: u32) -> Kind {
    match id {
        14..=17 => Kind::Date,
        18..=21 | 45..=47 => Kind::Time,
        22 => Kind::DateTime,
        _ => Kind::Number(builtin_code(id).map(str::to_string)),
    }
}

/// A custom number format by what it shows: date and time letters outside quotes and brackets
/// make a date; a `%` a percentage.
fn kind_of_code(code: &str) -> Kind {
    let mut plain = String::new();
    let (mut quoted, mut bracket) = (false, false);
    let mut chars = code.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => quoted = !quoted,
            '[' if !quoted => bracket = true,
            ']' if !quoted => bracket = false,
            '\\' => {
                chars.next();
            }
            c if !quoted && !bracket => plain.push(c.to_ascii_lowercase()),
            _ => {}
        }
    }
    let plain = plain.split(';').next().unwrap_or("");
    let date = plain.contains('y') || plain.contains('d') || (plain.contains('m') && !plain.contains('h') && !plain.contains('s'));
    let time = plain.contains('h') || plain.contains('s');
    match (date, time) {
        (true, true) => Kind::DateTime,
        (true, false) => Kind::Date,
        (false, true) => Kind::Time,
        _ if plain.trim().eq_ignore_ascii_case("general") => Kind::Number(None),
        _ => Kind::Number(Some(code.to_string())),
    }
}

fn sheet(doc: &roxmltree::Document, shared: &[String], formats: &[Kind], date1904: bool, seps: Seps) -> Sheet {
    let mut rows: Vec<(u32, Vec<SheetCell>)> = Vec::new();
    let mut cols = 0usize;
    let (mut more_rows, mut more_cols) = (false, false);
    let Some(data) = doc.descendants().find(|n| n.is_element() && local(*n) == "sheetData") else {
        return Sheet { rows, cols, more_rows, more_cols };
    };
    let mut next_row = 1u32;
    for row in data.children().filter(|n| n.is_element() && local(*n) == "row") {
        let num = attr(row, "r").and_then(|v| v.parse().ok()).unwrap_or(next_row);
        next_row = num + 1;
        if num as usize > super::MAX_ROWS {
            more_rows = true;
            break;
        }
        let mut cells: Vec<SheetCell> = Vec::new();
        let mut next_col = 0usize;
        for c in row.children().filter(|n| n.is_element() && local(*n) == "c") {
            let col = attr(c, "r").and_then(super::column_index).unwrap_or(next_col);
            next_col = col + 1;
            if col >= super::MAX_COLS {
                more_cols = true;
                continue;
            }
            let cell = value(c, shared, formats, date1904, seps);
            if cell.text.is_empty() {
                continue;
            }
            if cells.len() <= col {
                cells.resize(col + 1, SheetCell::default());
            }
            cells[col] = cell;
        }
        if cells.is_empty() {
            continue;
        }
        cols = cols.max(cells.len());
        rows.push((num, cells));
    }
    // Empty rows between those with something in them are shown, as a sheet shows them.
    let mut dense = Vec::new();
    let mut expect = 1u32;
    for (num, cells) in rows {
        while expect < num && dense.len() < super::MAX_ROWS {
            dense.push((expect, Vec::new()));
            expect += 1;
        }
        dense.push((num, cells));
        expect = num + 1;
    }
    Sheet { rows: dense, cols, more_rows, more_cols }
}

/// What a cell shows.
fn value(c: Node, shared: &[String], formats: &[Kind], date1904: bool, seps: Seps) -> SheetCell {
    let t = attr(c, "t").unwrap_or("n");
    let v = el(c, "v").and_then(|v| v.text()).unwrap_or("");
    let text = |s: String| SheetCell { text: s, right: false };
    match t {
        "s" => text(v.trim().parse::<usize>().ok().and_then(|i| shared.get(i)).cloned().unwrap_or_default()),
        "inlineStr" => text(el(c, "is").map(string_item).unwrap_or_default()),
        "str" => text(v.to_string()),
        "b" => SheetCell { text: if v.trim() == "1" { "TRUE" } else { "FALSE" }.into(), right: false },
        "e" => text(v.to_string()),
        // An ISO date written as text (`t="d"`), as newer Excel can.
        "d" => SheetCell { text: v.replace('T', " "), right: true },
        _ => {
            let Ok(n) = v.trim().parse::<f64>() else { return text(v.to_string()) };
            let kind = attr(c, "s").and_then(|s| s.parse::<usize>().ok()).and_then(|i| formats.get(i)).cloned().unwrap_or(Kind::Number(None));
            SheetCell { text: number(n, &kind, date1904, seps), right: true }
        }
    }
}

fn number(n: f64, kind: &Kind, date1904: bool, seps: Seps) -> String {
    match kind {
        Kind::Number(None) => general(n, seps),
        Kind::Number(Some(code)) => format_code(n, code, seps).trim().to_string(),
        Kind::Date | Kind::DateTime | Kind::Time => serial_date(n, kind, date1904).unwrap_or_else(|| general(n, seps)),
    }
}

/// A spreadsheet date: days since 1899-12-30 (since 1904-01-01 in old Mac workbooks), the
/// fraction being the time of day. Written ISO, as the rest of Vanta writes dates.
fn serial_date(n: f64, kind: &Kind, date1904: bool) -> Option<String> {
    let epoch = if date1904 { chrono::NaiveDate::from_ymd_opt(1904, 1, 1)? } else { chrono::NaiveDate::from_ymd_opt(1899, 12, 30)? };
    let days = n.floor();
    let secs = ((n - days) * 86400.0).round() as i64;
    let date = epoch.checked_add_signed(chrono::Duration::days(days as i64))?;
    let time = format!("{:02}:{:02}", secs / 3600 % 24, secs / 60 % 60);
    Some(match kind {
        Kind::Time => time,
        Kind::DateTime => format!("{} {time}", date.format("%Y-%m-%d")),
        _ => date.format("%Y-%m-%d").to_string(),
    })
}

/// The decimal mark and thousands separator numbers are shown with: the `numberLocale` setting's,
/// else the system's, as a spreadsheet program on this machine would show them (the format codes
/// in the file are always written with `.` and `,`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Seps {
    decimal: char,
    thousands: &'static str,
}

impl Seps {
    pub(super) const US: Seps = Seps { decimal: '.', thousands: "," };

    fn system() -> Seps {
        // Tests read workbooks the same whatever the machine's language.
        if cfg!(test) {
            return Seps::US;
        }
        let chosen = super::number_locale();
        if !chosen.is_empty() {
            return Seps::for_locale(&chosen);
        }
        let var = ["LC_ALL", "LC_NUMERIC", "LANG"].iter().filter_map(|k| std::env::var(k).ok()).find(|v| !v.is_empty()).unwrap_or_default();
        Seps::for_locale(&var)
    }

    /// For a locale name such as `da_DK.UTF-8`: by its language, and for Swiss German its
    /// country.
    pub(super) fn for_locale(name: &str) -> Seps {
        // `da-DK` as well as `da_DK`.
        let name = &name.replace('-', "_");
        let lang = name.split(['_', '.', '@']).next().unwrap_or("").to_ascii_lowercase();
        let lang = lang.as_str();
        if name.starts_with("de_CH") || name.starts_with("it_CH") {
            return Seps { decimal: '.', thousands: "'" };
        }
        match lang {
            "da" | "de" | "nl" | "es" | "it" | "pt" | "id" | "tr" | "el" | "ro" | "hr" | "sl" | "sr" | "is" => Seps { decimal: ',', thousands: "." },
            "fr" | "sv" | "nb" | "nn" | "no" | "fi" | "pl" | "ru" | "uk" | "cs" | "sk" | "hu" | "et" | "lv" | "lt" | "bg" => Seps { decimal: ',', thousands: "\u{202F}" },
            _ => Seps::US,
        }
    }
}

/// A number as a format code shows it (`#,##0.00`, `0%`, `"kr" #,##0;[Red]-#,##0`,
/// `0.00E+00`). Colors and padding are left out; a section that is text (`@`) shows the number
/// plainly.
pub(super) fn format_code(n: f64, code: &str, seps: Seps) -> String {
    let sections = split_sections(code);
    let (section, negative_has_own) = match sections.len() {
        _ if n < 0.0 && sections.len() >= 2 => (&sections[1], true),
        _ if n == 0.0 && sections.len() >= 3 => (&sections[2], true),
        _ => (&sections[0], false),
    };
    let value = if negative_has_own { n.abs() } else { n };
    let parsed = Section::parse(section);
    if !parsed.has_digits {
        // All text (a section such as "-" for zero) or the general format.
        return if parsed.general {
            format!("{}{}{}", parsed.prefix, general(value, seps), parsed.suffix)
        } else if parsed.prefix.is_empty() && parsed.suffix.is_empty() {
            general(value, seps)
        } else {
            format!("{}{}", parsed.prefix, parsed.suffix)
        };
    }
    let sign = if value < 0.0 { "-" } else { "" };
    let body = parsed.number(value.abs(), seps);
    format!("{sign}{}{body}{}", parsed.prefix, parsed.suffix)
}

/// A number in the general format, with the system's decimal mark.
pub(super) fn general(n: f64, seps: Seps) -> String {
    super::plain_number(n).replace('.', &seps.decimal.to_string())
}

/// The sections of a code, split at `;` outside quotes and brackets.
fn split_sections(code: &str) -> Vec<String> {
    let mut out = vec![String::new()];
    let (mut quoted, mut bracket, mut escaped) = (false, false, false);
    for c in code.chars() {
        if escaped {
            escaped = false;
        } else if c == '\\' && !quoted {
            escaped = true;
        } else if c == '"' {
            quoted = !quoted;
        } else if c == '[' && !quoted {
            bracket = true;
        } else if c == ']' && !quoted {
            bracket = false;
        } else if c == ';' && !quoted && !bracket {
            out.push(String::new());
            continue;
        }
        if let Some(s) = out.last_mut() {
            s.push(c);
        }
    }
    out
}

#[derive(Default, Debug)]
struct Section {
    prefix: String,
    suffix: String,
    has_digits: bool,
    general: bool,
    /// `0`s before the decimal point: the least number of integer digits.
    min_int: usize,
    grouping: bool,
    min_dec: usize,
    max_dec: usize,
    percent: u32,
    /// Trailing commas: each divides by a thousand.
    scale: u32,
    /// Digits of the exponent, when scientific.
    exponent: Option<usize>,
    exp_plus: bool,
}

impl Section {
    fn parse(s: &str) -> Section {
        let mut out = Section::default();
        let chars: Vec<char> = s.chars().collect();
        let mut i = 0;
        let (mut in_dec, mut in_exp) = (false, false);
        let mut pending_commas = 0u32;
        let mut exp_digits = 0usize;
        fn lit(out: &mut Section, text: &str) {
            if out.has_digits {
                out.suffix.push_str(text);
            } else {
                out.prefix.push_str(text);
            }
        }
        while i < chars.len() {
            let c = chars[i];
            match c {
                '"' => {
                    let end = chars[i + 1..].iter().position(|&q| q == '"').map_or(chars.len(), |p| i + 1 + p);
                    let text: String = chars[i + 1..end.min(chars.len())].iter().collect();
                    lit(&mut out, &text);
                    i = end + 1;
                    continue;
                }
                '\\' => {
                    if let Some(&n) = chars.get(i + 1) {
                        lit(&mut out, &n.to_string());
                    }
                    i += 2;
                    continue;
                }
                '[' => {
                    let end = chars[i..].iter().position(|&q| q == ']').map_or(chars.len(), |p| i + p);
                    let inner: String = chars[i + 1..end.min(chars.len())].iter().collect();
                    // `[$kr.-406]`: a currency symbol and a locale; `[Red]`, `[>100]`: not shown.
                    if let Some(cur) = inner.strip_prefix('$') {
                        let symbol = cur.split('-').next().unwrap_or("");
                        lit(&mut out, symbol);
                    }
                    i = end + 1;
                    continue;
                }
                // Padding as wide as the next character, and a fill to the cell's width.
                '_' => {
                    lit(&mut out, " ");
                    i += 2;
                    continue;
                }
                '*' => {
                    i += 2;
                    continue;
                }
                '0' | '#' | '?' => {
                    if in_exp {
                        exp_digits += 1;
                    } else if in_dec {
                        out.max_dec += 1;
                        if c == '0' {
                            out.min_dec += 1;
                        }
                    } else {
                        if pending_commas > 0 {
                            out.grouping = true;
                            pending_commas = 0;
                        }
                        if c == '0' {
                            out.min_int += 1;
                        }
                    }
                    out.has_digits = true;
                }
                '.' if !in_exp && !out.suffix_started() => in_dec = true,
                ',' if out.has_digits && !in_dec && !in_exp => pending_commas += 1,
                '%' => {
                    out.percent += 1;
                    lit(&mut out, "%");
                }
                'E' | 'e' if out.has_digits && matches!(chars.get(i + 1), Some('+' | '-')) => {
                    in_exp = true;
                    out.exp_plus = chars[i + 1] == '+';
                    i += 2;
                    continue;
                }
                _ => {
                    let rest: String = chars[i..].iter().collect();
                    if rest.to_ascii_lowercase().starts_with("general") {
                        out.general = true;
                        i += "general".len();
                        continue;
                    }
                    if c == '@' {
                        out.general = true;
                    } else {
                        lit(&mut out, &c.to_string());
                    }
                }
            }
            i += 1;
        }
        out.scale = pending_commas;
        if in_exp {
            out.exponent = Some(exp_digits.max(1));
        }
        out
    }

    fn suffix_started(&self) -> bool {
        !self.suffix.is_empty()
    }

    fn number(&self, v: f64, seps: Seps) -> String {
        let mut v = v * 100f64.powi(self.percent as i32) / 1000f64.powi(self.scale as i32);
        let mut exp_part = String::new();
        if let Some(digits) = self.exponent {
            let mut e = if v == 0.0 { 0 } else { v.log10().floor() as i32 };
            // Engineering style (`##0.0E+0`): the exponent a multiple of the integer digits.
            let step = (self.min_int.max(1)) as i32;
            if step > 1 {
                e = e.div_euclid(step) * step;
            }
            v /= 10f64.powi(e);
            let sign = if e < 0 { "-" } else if self.exp_plus { "+" } else { "" };
            exp_part = format!("E{sign}{:0digits$}", e.abs());
        }
        // Halves away from zero, as a spreadsheet rounds (Rust's formatting rounds them to even),
        // with a hair added so 1.005 is not the 1.00499... it is stored as.
        let p = 10f64.powi(self.max_dec as i32);
        let v = ((v * p) * (1.0 + 1e-13)).round() / p;
        let rounded = format!("{:.*}", self.max_dec, v);
        let (int, dec) = rounded.split_once('.').unwrap_or((&rounded, ""));
        let int = int.trim_start_matches('0');
        let int = if int.len() < self.min_int { format!("{}{int}", "0".repeat(self.min_int - int.len())) } else { int.to_string() };
        let int = if self.grouping { group(&int, seps.thousands) } else { int };
        let dec = dec.trim_end_matches('0');
        let dec = if dec.len() < self.min_dec { format!("{dec}{}", "0".repeat(self.min_dec - dec.len())) } else { dec.to_string() };
        if dec.is_empty() { format!("{int}{exp_part}") } else { format!("{int}{}{dec}{exp_part}", seps.decimal) }
    }
}

fn group(int: &str, sep: &str) -> String {
    let digits: Vec<char> = int.chars().collect();
    let mut out = String::new();
    for (i, c) in digits.iter().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push_str(sep);
        }
        out.push(*c);
    }
    out
}

/// The codes of Excel's built-in number formats that are numbers (dates are told apart before).
fn builtin_code(id: u32) -> Option<&'static str> {
    Some(match id {
        1 => "0",
        2 => "0.00",
        3 | 5 | 6 | 37 | 38 | 41 => "#,##0",
        4 | 7 | 8 | 39 | 40 | 43 => "#,##0.00",
        9 => "0%",
        10 => "0.00%",
        11 => "0.00E+00",
        48 => "##0.0E+0",
        _ => return None,
    })
}

#[cfg(test)]
pub(super) fn kind_for_test(code: &str) -> &'static str {
    match kind_of_code(code) {
        Kind::Number(Some(c)) if c.contains('%') => "percent",
        Kind::Number(_) => "general",
        Kind::Date => "date",
        Kind::DateTime => "datetime",
        Kind::Time => "time",
    }
}

#[cfg(test)]
pub(super) fn date_for_test(n: f64) -> Option<String> {
    serial_date(n, &Kind::Date, false)
}

#[cfg(test)]
pub(super) fn format_for_test(n: f64, code: &str, locale: &str) -> String {
    format_code(n, code, Seps::for_locale(locale)).trim().to_string()
}
