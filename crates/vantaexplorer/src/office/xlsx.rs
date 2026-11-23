//! Excel workbooks (.xlsx): one sheet at a time, as the grid of what its cells show. Formulas
//! show the value Excel saved with them; dates are recognized by their number format.

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
    let sheet = sheet(&doc, &shared, &formats, date1904);
    Ok(RichDoc { blocks: vec![Block::Sheet(sheet)], images: Vec::new(), sheets: Some(sheets.into_iter().map(|(n, _)| n).collect()), note: None })
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

#[derive(Clone, Copy, PartialEq, Debug)]
enum Kind {
    General,
    Date,
    DateTime,
    Time,
    Percent,
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
        9 | 10 => Kind::Percent,
        14..=17 => Kind::Date,
        18..=21 | 45..=47 => Kind::Time,
        22 => Kind::DateTime,
        _ => Kind::General,
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
        _ if plain.contains('%') => Kind::Percent,
        _ => Kind::General,
    }
}

fn sheet(doc: &roxmltree::Document, shared: &[String], formats: &[Kind], date1904: bool) -> Sheet {
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
            let cell = value(c, shared, formats, date1904);
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
fn value(c: Node, shared: &[String], formats: &[Kind], date1904: bool) -> SheetCell {
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
            let kind = attr(c, "s").and_then(|s| s.parse::<usize>().ok()).and_then(|i| formats.get(i)).copied().unwrap_or(Kind::General);
            SheetCell { text: number(n, kind, date1904), right: true }
        }
    }
}

fn number(n: f64, kind: Kind, date1904: bool) -> String {
    match kind {
        Kind::General => super::plain_number(n),
        Kind::Percent => format!("{}%", super::plain_number(n * 100.0)),
        Kind::Date | Kind::DateTime | Kind::Time => serial_date(n, kind, date1904).unwrap_or_else(|| super::plain_number(n)),
    }
}

/// A spreadsheet date: days since 1899-12-30 (since 1904-01-01 in old Mac workbooks), the
/// fraction being the time of day. Written ISO, as the rest of Vanta writes dates.
fn serial_date(n: f64, kind: Kind, date1904: bool) -> Option<String> {
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

#[cfg(test)]
pub(super) fn kind_for_test(code: &str) -> &'static str {
    match kind_of_code(code) {
        Kind::General => "general",
        Kind::Date => "date",
        Kind::DateTime => "datetime",
        Kind::Time => "time",
        Kind::Percent => "percent",
    }
}

#[cfg(test)]
pub(super) fn date_for_test(n: f64) -> Option<String> {
    serial_date(n, Kind::Date, false)
}
