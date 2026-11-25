use super::*;

/// A document package written to a temporary file, from (name, contents) pairs.
fn package(name: &str, parts: &[(&str, Vec<u8>)]) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("vx-office-{}-{name}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join(name);
    let mut zip = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
    let opts = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    for (n, data) in parts {
        zip.start_file(*n, opts).unwrap();
        std::io::Write::write_all(&mut zip, data).unwrap();
    }
    zip.finish().unwrap();
    path
}

fn s(x: &str) -> Vec<u8> {
    x.as_bytes().to_vec()
}

fn png() -> Vec<u8> {
    let img = image::RgbaImage::from_pixel(4, 2, image::Rgba([200, 30, 30, 255]));
    let mut out = Vec::new();
    img.write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png).unwrap();
    out
}

fn texts(p: &Para) -> String {
    p.inlines.iter().filter_map(|i| if let Inline::Text(s) = i { Some(s.text.as_str()) } else { None }).collect()
}

fn paras(blocks: &[Block]) -> Vec<&Para> {
    blocks.iter().filter_map(|b| if let Block::Para(p) = b { Some(p) } else { None }).collect()
}

const W: &str = r#"xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main""#;

#[test]
fn a_word_document_keeps_its_headings_formatting_lists_tables_and_pictures() {
    let document = format!(
        r#"<w:document {W}><w:body>
        <w:p><w:pPr><w:pStyle w:val="Overskrift1"/></w:pPr><w:r><w:t>Kapitel</w:t></w:r></w:p>
        <w:p><w:r><w:t xml:space="preserve">Plain </w:t></w:r><w:r><w:rPr><w:b/></w:rPr><w:t>bold</w:t></w:r><w:r><w:rPr><w:i/><w:color w:val="C0392B"/></w:rPr><w:t>red</w:t></w:r></w:p>
        <w:p><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="2"/></w:numPr></w:pPr><w:r><w:t>one</w:t></w:r></w:p>
        <w:p><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="2"/></w:numPr></w:pPr><w:r><w:t>two</w:t></w:r></w:p>
        <w:p><w:pPr><w:numPr><w:ilvl w:val="1"/><w:numId w:val="2"/></w:numPr></w:pPr><w:r><w:t>two-a</w:t></w:r></w:p>
        <w:p><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="1"/></w:numPr></w:pPr><w:r><w:t>dot</w:t></w:r></w:p>
        <w:tbl><w:tblGrid><w:gridCol w:w="1000"/><w:gridCol w:w="3000"/></w:tblGrid>
          <w:tr><w:tc><w:tcPr><w:gridSpan w:val="2"/><w:shd w:fill="D9D9D9"/></w:tcPr><w:p><w:r><w:t>wide</w:t></w:r></w:p></w:tc></w:tr>
          <w:tr><w:tc><w:p><w:r><w:t>a</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>b</w:t></w:r></w:p></w:tc></w:tr>
        </w:tbl>
        <w:p><w:r><w:drawing><wp:inline><wp:extent cx="1270000" cy="635000"/><a:graphic><a:graphicData><a:blip r:embed="rId7"/></a:graphicData></a:graphic></wp:inline></w:drawing></w:r></w:p>
        <w:p><w:r><w:br w:type="page"/></w:r></w:p>
        </w:body></w:document>"#
    );
    let styles = format!(
        r#"<w:styles {W}><w:docDefaults><w:rPrDefault><w:rPr><w:sz w:val="22"/></w:rPr></w:rPrDefault></w:docDefaults>
        <w:style w:type="paragraph" w:default="1" w:styleId="Normal"><w:name w:val="Normal"/></w:style>
        <w:style w:type="paragraph" w:styleId="Overskrift1"><w:name w:val="heading 1"/><w:basedOn w:val="Normal"/><w:rPr><w:sz w:val="32"/></w:rPr></w:style></w:styles>"#
    );
    let numbering = format!(
        r#"<w:numbering {W}>
        <w:abstractNum w:abstractNumId="0"><w:lvl w:ilvl="0"><w:numFmt w:val="bullet"/><w:lvlText w:val="&#xF0B7;"/></w:lvl></w:abstractNum>
        <w:abstractNum w:abstractNumId="1"><w:lvl w:ilvl="0"><w:start w:val="1"/><w:numFmt w:val="decimal"/><w:lvlText w:val="%1."/></w:lvl><w:lvl w:ilvl="1"><w:start w:val="1"/><w:numFmt w:val="lowerLetter"/><w:lvlText w:val="%1.%2)"/></w:lvl></w:abstractNum>
        <w:num w:numId="1"><w:abstractNumId w:val="0"/></w:num><w:num w:numId="2"><w:abstractNumId w:val="1"/></w:num></w:numbering>"#
    );
    let rels = r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId7" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="media/pic.png"/></Relationships>"#;
    let path = package(
        "t.docx",
        &[("word/document.xml", s(&document)), ("word/styles.xml", s(&styles)), ("word/numbering.xml", s(&numbering)), ("word/_rels/document.xml.rels", s(rels)), ("word/media/pic.png", png())],
    );
    let doc = render(&path, "docx", 0).unwrap();
    let ps = paras(&doc.blocks);

    assert_eq!(ps[0].heading, Some(1), "a localized style id is still found by its built-in name");
    assert_eq!(texts(ps[0]), "Kapitel");

    let spans: Vec<&Span> = ps[1].inlines.iter().filter_map(|i| if let Inline::Text(s) = i { Some(s) } else { None }).collect();
    assert_eq!(spans.len(), 3);
    assert!(spans[1].style.bold && !spans[0].style.bold);
    assert!(spans[2].style.italic);
    assert_eq!(spans[2].style.color, Some([0xC0, 0x39, 0x2B]));

    let markers: Vec<Option<&str>> = ps[2..6].iter().map(|p| p.marker.as_deref()).collect();
    assert_eq!(markers, [Some("1."), Some("2."), Some("2.a)"), Some("•")]);
    assert_eq!(ps[4].level, 2);

    let Some(Block::Table(t)) = doc.blocks.iter().find(|b| matches!(b, Block::Table(_))) else { panic!("no table") };
    assert_eq!(t.widths, [1000.0, 3000.0]);
    assert_eq!(t.rows.len(), 2);
    assert_eq!(t.rows[0][0].span, 2);
    assert_eq!(t.rows[0][0].fill, Some([0xD9, 0xD9, 0xD9]));
    assert_eq!(t.rows[1].len(), 2);

    assert_eq!(doc.images.len(), 1);
    let pic = ps.iter().flat_map(|p| &p.inlines).find_map(|i| if let Inline::Image { idx, w, h } = i { Some((*idx, *w, *h)) } else { None });
    assert_eq!(pic, Some((0, 100.0, 50.0)), "the size from wp:extent, in points");
    assert!(matches!(doc.blocks.last(), Some(Block::PageBreak)));
}

#[test]
fn a_missing_picture_is_named_rather_than_dropped() {
    let document = format!(r#"<w:document {W}><w:body><w:p><w:r><w:drawing><a:blip r:embed="rId1"/></w:drawing></w:r></w:p></w:body></w:document>"#);
    let rels = r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Target="media/old.emf"/></Relationships>"#;
    let path = package("emf.docx", &[("word/document.xml", s(&document)), ("word/_rels/document.xml.rels", s(rels)), ("word/media/old.emf", vec![1, 2, 3])]);
    let doc = render(&path, "docx", 0).unwrap();
    let p = paras(&doc.blocks)[0];
    assert!(matches!(&p.inlines[0], Inline::Missing(w) if w == "picture (EMF)"));
}

#[test]
fn an_encrypted_or_binary_office_file_says_so() {
    let dir = std::env::temp_dir().join(format!("vx-office-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("locked.docx");
    std::fs::write(&path, [0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1]).unwrap();
    assert!(render(&path, "docx", 0).err().unwrap().contains("encrypted"));
}

const XL: &str = r#"xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships""#;

#[test]
fn a_workbook_shows_strings_numbers_dates_and_its_sheet_names() {
    let workbook = format!(r#"<workbook {XL}><sheets><sheet name="Budget" sheetId="1" r:id="rId1"/><sheet name="Noter" sheetId="2" r:id="rId2"/></sheets></workbook>"#);
    let rels = r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Target="worksheets/sheet1.xml"/><Relationship Id="rId2" Target="/xl/worksheets/sheet2.xml"/></Relationships>"#;
    let shared = format!(r#"<sst {XL}><si><t>Navn</t></si><si><r><t>Rig </t></r><r><t>tekst</t></r></si></sst>"#);
    let styles = format!(r#"<styleSheet {XL}><numFmts><numFmt numFmtId="164" formatCode="dd/mm/yyyy"/></numFmts><cellXfs><xf numFmtId="0"/><xf numFmtId="14"/><xf numFmtId="164"/><xf numFmtId="10"/></cellXfs></styleSheet>"#);
    let sheet1 = format!(
        r#"<worksheet {XL}><sheetData>
        <row r="1"><c r="A1" t="s"><v>0</v></c><c r="C1" t="s"><v>1</v></c></row>
        <row r="3"><c r="A3"><v>42</v></c><c r="B3" s="1"><v>46348</v></c><c r="C3" s="2"><v>45000</v></c><c r="D3" s="3"><v>0.25</v></c><c r="E3" t="b"><v>1</v></c><c r="F3" t="str"><v>formel</v></c><c r="G3"><v>3.14159</v></c></row>
        </sheetData></worksheet>"#
    );
    let sheet2 = format!(r#"<worksheet {XL}><sheetData><row r="1"><c r="A1" t="inlineStr"><is><t>Andet ark</t></is></c></row></sheetData></worksheet>"#);
    let path = package(
        "t.xlsx",
        &[
            ("xl/workbook.xml", s(&workbook)),
            ("xl/_rels/workbook.xml.rels", s(rels)),
            ("xl/sharedStrings.xml", s(&shared)),
            ("xl/styles.xml", s(&styles)),
            ("xl/worksheets/sheet1.xml", s(&sheet1)),
            ("xl/worksheets/sheet2.xml", s(&sheet2)),
        ],
    );
    let doc = render(&path, "xlsx", 0).unwrap();
    assert_eq!(doc.sheets.as_deref(), Some(&["Budget".to_string(), "Noter".to_string()][..]));
    let Block::Sheet(sh) = &doc.blocks[0] else { panic!("not a sheet") };
    assert_eq!(sh.rows.len(), 3, "the empty row 2 is shown, as a sheet shows it");
    assert_eq!(sh.rows[1], (2, vec![]));
    let row1: Vec<&str> = sh.rows[0].1.iter().map(|c| c.text.as_str()).collect();
    assert_eq!(row1, ["Navn", "", "Rig tekst"]);
    let row3: Vec<&str> = sh.rows[2].1.iter().map(|c| c.text.as_str()).collect();
    assert_eq!(row3, ["42", "2026-11-22", "2023-03-15", "25.00%", "TRUE", "formel", "3.14159"]);
    assert!(sh.rows[2].1[0].right && !sh.rows[2].1[5].right);

    let second = render(&path, "xlsx", 1).unwrap();
    let Block::Sheet(sh2) = &second.blocks[0] else { panic!() };
    assert_eq!(sh2.rows[0].1[0].text, "Andet ark");
}

const ODF: &str = r#"xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0" xmlns:table="urn:oasis:names:tc:opendocument:xmlns:table:1.0" xmlns:style="urn:oasis:names:tc:opendocument:xmlns:style:1.0" xmlns:fo="urn:oasis:names:tc:opendocument:xmlns:xsl-fo-compatible:1.0" xmlns:draw="urn:oasis:names:tc:opendocument:xmlns:drawing:1.0" xmlns:xlink="http://www.w3.org/1999/xlink" xmlns:svg="urn:oasis:names:tc:opendocument:xmlns:svg-compatible:1.0""#;

#[test]
fn a_writer_document_keeps_headings_spans_lists_tables_and_pictures() {
    let content = format!(
        r##"<office:document-content {ODF}><office:automatic-styles>
          <style:style style:name="T1" style:family="text"><style:text-properties fo:font-weight="bold" fo:color="#1155cc"/></style:style>
          <style:style style:name="P1" style:family="paragraph"><style:paragraph-properties fo:text-align="center"/></style:style>
          <text:list-style style:name="L1"><text:list-level-style-number text:level="1" style:num-format="1" style:num-suffix="."/><text:list-level-style-bullet text:level="2" text:bullet-char="•"/></text:list-style>
        </office:automatic-styles><office:body><office:text>
          <text:h text:outline-level="2">Afsnit</text:h>
          <text:p text:style-name="P1">Før <text:span text:style-name="T1">fed</text:span>  efter<text:s text:c="2"/>slut</text:p>
          <text:list text:style-name="L1">
            <text:list-item><text:p>først</text:p></text:list-item>
            <text:list-item><text:p>andet</text:p><text:list><text:list-item><text:p>under</text:p></text:list-item></text:list></text:list-item>
          </text:list>
          <table:table><table:table-column table:number-columns-repeated="2"/>
            <table:table-row><table:table-cell><text:p>x</text:p></table:table-cell><table:table-cell><text:p>y</text:p></table:table-cell></table:table-row>
          </table:table>
          <text:p><draw:frame svg:width="2in" svg:height="1in"><draw:image xlink:href="Pictures/p.png"/></draw:frame></text:p>
        </office:text></office:body></office:document-content>"##
    );
    let path = package("t.odt", &[("content.xml", s(&content)), ("Pictures/p.png", png())]);
    let doc = render(&path, "odt", 0).unwrap();
    let ps = paras(&doc.blocks);
    assert_eq!(ps[0].heading, Some(2));
    assert_eq!(texts(ps[1]), "Før fed efter  slut", "runs of spaces collapse, text:s does not");
    assert_eq!(ps[1].align, Align::Center);
    let bold = ps[1].inlines.iter().find_map(|i| if let Inline::Text(s) = i { s.style.bold.then_some(s) } else { None }).unwrap();
    assert_eq!(bold.text, "fed");
    assert_eq!(bold.style.color, Some([0x11, 0x55, 0xcc]));
    let markers: Vec<Option<&str>> = ps[2..5].iter().map(|p| p.marker.as_deref()).collect();
    assert_eq!(markers, [Some("1."), Some("2."), Some("•")]);
    assert!(doc.blocks.iter().any(|b| matches!(b, Block::Table(t) if t.rows[0].len() == 2)));
    assert_eq!(doc.images.len(), 1);
    assert!(ps.iter().flat_map(|p| &p.inlines).any(|i| matches!(i, Inline::Image { w, .. } if *w == 144.0)));
}

#[test]
fn a_calc_sheet_counts_repeated_rows_without_building_them() {
    let content = format!(
        r#"<office:document-content {ODF}><office:body><office:spreadsheet>
          <table:table table:name="Ark1">
            <table:table-row><table:table-cell office:value-type="string"><text:p>Tekst</text:p></table:table-cell><table:table-cell table:number-columns-repeated="2"/><table:table-cell office:value-type="float" office:value="7"><text:p>7,00 kr.</text:p></table:table-cell></table:table-row>
            <table:table-row table:number-rows-repeated="3"><table:table-cell table:number-columns-repeated="1024"/></table:table-row>
            <table:table-row><table:table-cell><text:p>Sidst</text:p></table:table-cell></table:table-row>
            <table:table-row table:number-rows-repeated="1048570"><table:table-cell table:number-columns-repeated="1024"/></table:table-row>
          </table:table>
          <table:table table:name="Ark2"/>
        </office:spreadsheet></office:body></office:document-content>"#
    );
    let path = package("t.ods", &[("content.xml", s(&content))]);
    let doc = render(&path, "ods", 0).unwrap();
    assert_eq!(doc.sheets.as_deref(), Some(&["Ark1".to_string(), "Ark2".to_string()][..]));
    let Block::Sheet(sh) = &doc.blocks[0] else { panic!() };
    assert_eq!(sh.rows.len(), 5);
    let first: Vec<(&str, bool)> = sh.rows[0].1.iter().map(|c| (c.text.as_str(), c.right)).collect();
    assert_eq!(first, [("Tekst", false), ("", false), ("", false), ("7,00 kr.", true)]);
    assert_eq!(sh.rows[4], (5, vec![SheetCell { text: "Sidst".into(), right: false }]));
    assert!(!sh.more_rows, "the empty filler at the end is not content");
}

#[test]
fn the_small_helpers() {
    assert_eq!(column_index("A1"), Some(0));
    assert_eq!(column_index("AA10"), Some(26));
    assert_eq!(column_name(0), "A");
    assert_eq!(column_name(26), "AA");
    assert_eq!(format_number(4, "lowerRoman"), "iv");
    assert_eq!(format_number(28, "upperLetter"), "BB");
    assert_eq!(plain_number(42.0), "42");
    assert_eq!(plain_number(0.1 + 0.2), "0.3");
    assert_eq!(xlsx::kind_for_test("dd/mm/yyyy hh:mm"), "datetime");
    assert_eq!(xlsx::kind_for_test("[$-406]d. mmmm yyyy"), "date");
    assert_eq!(xlsx::kind_for_test("0.00%"), "percent");
    assert_eq!(xlsx::kind_for_test("\"kr\" #,##0.00"), "general");
    assert_eq!(xlsx::date_for_test(1.0).as_deref(), Some("1899-12-31"));
    assert_eq!(resolve("word", "../media/x.png"), "media/x.png");
    assert_eq!(resolve("xl", "/xl/worksheets/s.xml"), "xl/worksheets/s.xml");
    assert_eq!(bullet("\u{F0B7}"), "•");
    assert_eq!(bullet("o"), "◦");
}

#[test]
fn a_word_document_gives_its_page_and_paragraph_spacing() {
    let styles = format!(
        r#"<w:styles {W}><w:docDefaults><w:rPrDefault><w:rPr><w:sz w:val="22"/></w:rPr></w:rPrDefault><w:pPrDefault><w:pPr><w:spacing w:after="160" w:line="259" w:lineRule="auto"/></w:pPr></w:pPrDefault></w:docDefaults>
        <w:style w:type="paragraph" w:styleId="List"><w:name w:val="List Paragraph"/><w:pPr><w:contextualSpacing/></w:pPr></w:style></w:styles>"#
    );
    let document = format!(
        r#"<w:document {W}><w:body>
          <w:p><w:r><w:t>normal</w:t></w:r></w:p>
          <w:p><w:pPr><w:spacing w:before="240"/></w:pPr><w:r><w:t>luft</w:t></w:r></w:p>
          <w:p><w:pPr><w:pStyle w:val="List"/></w:pPr><w:r><w:t>punkt</w:t></w:r></w:p>
          <w:sectPr><w:pgSz w:w="12240" w:h="15840"/><w:pgMar w:top="1440" w:right="1440" w:bottom="-1440" w:left="1800"/></w:sectPr>
        </w:body></w:document>"#
    );
    let path = package("page.docx", &[("word/document.xml", s(&document)), ("word/styles.xml", s(&styles))]);
    let doc = render(&path, "docx", 0).unwrap();
    assert_eq!(doc.page, PageSetup { width: 612.0, height: 792.0, margin: [72.0, 72.0, 72.0, 90.0], ..PageSetup::default() });
    assert_eq!(doc.base_pt, 11.0);
    let p = paras(&doc.blocks);
    assert_eq!((p[0].before, p[0].after), (None, Some(8.0)));
    assert!((p[0].line.unwrap() - 259.0 / 240.0).abs() < 1e-4);
    assert_eq!(p[1].before, Some(12.0));
    assert!(!p[1].contextual && p[2].contextual);
}

#[test]
fn a_writer_document_gives_its_page() {
    let styles = format!(
        r#"<office:document-styles {ODF}><office:automatic-styles>
          <style:page-layout style:name="pm1"><style:page-layout-properties fo:page-width="21cm" fo:page-height="29.7cm" fo:margin="2cm" fo:margin-left="2.54cm"/></style:page-layout>
        </office:automatic-styles><office:master-styles><style:master-page style:name="Standard" style:page-layout-name="pm1"/></office:master-styles></office:document-styles>"#
    );
    let content = format!(r#"<office:document-content {ODF}><office:body><office:text><text:p>x</text:p></office:text></office:body></office:document-content>"#);
    let path = package("page.odt", &[("content.xml", s(&content)), ("styles.xml", s(&styles))]);
    let doc = render(&path, "odt", 0).unwrap();
    let cm = 72.0 / 2.54;
    assert!((doc.page.width - 21.0 * cm).abs() < 0.01 && (doc.page.height - 29.7 * cm).abs() < 0.01);
    assert!((doc.page.margin[0] - 2.0 * cm).abs() < 0.01 && (doc.page.margin[3] - 72.0).abs() < 0.01);
}

#[test]
fn a_nonsense_page_is_the_default_one() {
    assert_eq!(PageSetup { width: 600.0, height: 800.0, margin: [500.0, 0.0, 0.0, 0.0], ..PageSetup::default() }.sane(), PageSetup::default());
    assert_eq!(PageSetup { width: 20.0, height: 800.0, margin: [0.0; 4], ..PageSetup::default() }.sane(), PageSetup::default());
}

#[test]
fn numbers_show_in_their_format_with_the_systems_separators() {
    let us = |n: f64, code: &str| xlsx::format_for_test(n, code, "en_US.UTF-8");
    let da = |n: f64, code: &str| xlsx::format_for_test(n, code, "da_DK.UTF-8");
    assert_eq!(us(1234567.891, "#,##0.00"), "1,234,567.89");
    assert_eq!(da(1234567.891, "#,##0.00"), "1.234.567,89");
    assert_eq!(us(0.256, "0.0%"), "25.6%");
    assert_eq!(us(5.0, "0.00"), "5.00");
    assert_eq!(us(0.5, "#.##"), ".5");
    assert_eq!(us(1234.5, "#,##0"), "1,235");
    assert_eq!(da(1250.0, r#"#,##0.00 "kr.""#), "1.250,00 kr.");
    assert_eq!(da(1250.0, "[$kr.-406] #,##0.00"), "kr. 1.250,00");
    assert_eq!(us(-1250.0, r#"#,##0.00;[Red]\(#,##0.00\)"#), "(1,250.00)");
    assert_eq!(us(-3.0, "0.0"), "-3.0");
    assert_eq!(da(0.0, r#"_-* #,##0.00 "kr."_-;-* #,##0.00 "kr."_-;_-* "-"?? "kr."_-;_-@_-"#), "- kr.");
    assert_eq!(da(-42.5, r#"_-* #,##0.00 "kr."_-;-* #,##0.00 "kr."_-;_-* "-"?? "kr."_-;_-@_-"#), "-42,50 kr.");
    assert_eq!(us(12345.0, "0.00E+00"), "1.23E+04");
    assert_eq!(us(1500000.0, "#,##0,,\"M\""), "2M");
    assert_eq!(us(7.0, "General"), "7");
    assert_eq!(da(2.5, "@"), "2,5");
    assert_eq!(xlsx::format_for_test(1234.5, "#,##0.0", "da-DK"), "1.234,5", "a locale written with a hyphen");
    assert_eq!(xlsx::format_for_test(1234.5, "#,##0.0", "de_CH.UTF-8"), "1'234.5");
}

fn inline_kinds(blocks: &[Block]) -> Vec<String> {
    paras(blocks)
        .iter()
        .flat_map(|p| p.inlines.iter())
        .map(|i| match i {
            Inline::Text(s) => s.text.clone(),
            Inline::Field(Field::Page, _) => "{page}".into(),
            Inline::Field(Field::Pages, _) => "{pages}".into(),
            Inline::Image { .. } => "{image}".into(),
            Inline::Missing(w) => format!("[{w}]"),
        })
        .collect()
}

#[test]
fn a_word_document_has_its_header_footer_and_page_numbers() {
    let document = format!(
        r#"<w:document {W}><w:body><w:p><w:r><w:t>krop</w:t></w:r></w:p>
          <w:sectPr><w:headerReference w:type="default" r:id="rH"/><w:footerReference w:type="default" r:id="rF"/><w:footerReference w:type="first" r:id="rF1"/><w:titlePg/>
          <w:pgMar w:top="1440" w:right="1440" w:bottom="1440" w:left="1440" w:header="600" w:footer="500"/></w:sectPr>
        </w:body></w:document>"#
    );
    let rels = r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rH" Target="header1.xml"/><Relationship Id="rF" Target="footer1.xml"/><Relationship Id="rF1" Target="footer2.xml"/></Relationships>"#;
    let header = format!(r#"<w:hdr {W}><w:p><w:r><w:t>Rapport</w:t></w:r><w:r><w:drawing><a:blip r:embed="rLogo"/></w:drawing></w:r></w:p></w:hdr>"#);
    let header_rels = r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rLogo" Target="media/logo.png"/></Relationships>"#;
    let footer = format!(
        r#"<w:ftr {W}><w:p><w:r><w:t xml:space="preserve">Side </w:t></w:r><w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:instrText> PAGE </w:instrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:t>9</w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r><w:r><w:t xml:space="preserve"> af </w:t></w:r><w:fldSimple w:instr=" NUMPAGES "><w:r><w:t>9</w:t></w:r></w:fldSimple></w:p></w:ftr>"#
    );
    let first = format!(r#"<w:ftr {W}><w:p><w:r><w:t>Forside</w:t></w:r></w:p></w:ftr>"#);
    let path = package(
        "hf.docx",
        &[
            ("word/document.xml", s(&document)),
            ("word/_rels/document.xml.rels", s(rels)),
            ("word/header1.xml", s(&header)),
            ("word/_rels/header1.xml.rels", s(header_rels)),
            ("word/media/logo.png", png()),
            ("word/footer1.xml", s(&footer)),
            ("word/footer2.xml", s(&first)),
        ],
    );
    let doc = render(&path, "docx", 0).unwrap();
    assert_eq!((doc.page.header, doc.page.footer), (30.0, 25.0));
    let h = doc.header.as_ref().unwrap();
    assert_eq!(inline_kinds(&h.blocks), ["Rapport", "{image}"], "a header's picture is found through its own relationships");
    assert!(h.first.as_ref().is_some_and(|f| f.is_empty()), "a first page of its own, without a header");
    let f = doc.footer.as_ref().unwrap();
    assert_eq!(inline_kinds(&f.blocks), ["Side ", "{page}", " af ", "{pages}"], "the saved results give way to the fields");
    assert_eq!(inline_kinds(f.for_page(0)), ["Forside"]);
    assert_eq!(inline_kinds(f.for_page(3)), ["Side ", "{page}", " af ", "{pages}"]);
    assert_eq!(doc.images.len(), 1);
}

#[test]
fn a_writer_document_has_its_header_and_the_text_starts_below_it() {
    let styles = format!(
        r#"<office:document-styles {ODF}><office:automatic-styles>
          <style:page-layout style:name="pm1"><style:page-layout-properties fo:page-width="21cm" fo:page-height="29.7cm" fo:margin="2cm"/>
            <style:header-style><style:header-footer-properties fo:min-height="1cm" fo:margin-bottom="0.5cm"/></style:header-style></style:page-layout>
        </office:automatic-styles><office:master-styles><style:master-page style:name="Standard" style:page-layout-name="pm1">
          <style:header><text:p>Side <text:page-number>1</text:page-number> af <text:page-count>3</text:page-count></text:p></style:header>
          <style:footer style:display="false"><text:p>skjult</text:p></style:footer>
        </style:master-page></office:master-styles></office:document-styles>"#
    );
    let content = format!(r#"<office:document-content {ODF}><office:body><office:text><text:p>x</text:p></office:text></office:body></office:document-content>"#);
    let path = package("hf.odt", &[("content.xml", s(&content)), ("styles.xml", s(&styles))]);
    let doc = render(&path, "odt", 0).unwrap();
    let cm = 72.0 / 2.54;
    assert_eq!(inline_kinds(&doc.header.as_ref().unwrap().blocks), ["Side ", "{page}", " af ", "{pages}"]);
    assert!(doc.footer.is_none(), "a footer switched off is none");
    assert!((doc.page.header - 2.0 * cm).abs() < 0.01, "the header at the top margin");
    assert!((doc.page.margin[0] - 3.5 * cm).abs() < 0.01, "and the text below it");
}

const P: &str = r#"xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main""#;

fn rels(items: &[(&str, &str, &str)]) -> Vec<u8> {
    let body: String = items.iter().map(|(id, kind, target)| format!(r#"<Relationship Id="{id}" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/{kind}" Target="{target}"/>"#)).collect();
    s(&format!(r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">{body}</Relationships>"#))
}

#[test]
fn a_presentation_has_its_slides_with_what_layout_master_and_theme_give_them() {
    let presentation = format!(r#"<p:presentation {P}><p:sldIdLst><p:sldId id="256" r:id="rS1"/></p:sldIdLst><p:sldSz cx="9144000" cy="5143500"/></p:presentation>"#);
    let theme = r#"<a:theme xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"><a:themeElements><a:clrScheme name="t"><a:dk1><a:srgbClr val="111111"/></a:dk1><a:lt1><a:srgbClr val="FAFAFA"/></a:lt1><a:dk2><a:srgbClr val="222222"/></a:dk2><a:lt2><a:srgbClr val="EEEEEE"/></a:lt2><a:accent1><a:srgbClr val="FF0000"/></a:accent1></a:clrScheme></a:themeElements></a:theme>"#;
    let master = format!(
        r#"<p:sldMaster {P}><p:cSld><p:bg><p:bgRef idx="1001"><a:schemeClr val="bg1"/></p:bgRef></p:bg><p:spTree>
          <p:sp><p:nvSpPr><p:cNvPr id="2" name="t"/><p:cNvSpPr/><p:nvPr><p:ph type="title"/></p:nvPr></p:nvSpPr><p:spPr><a:xfrm><a:off x="127000" y="254000"/><a:ext cx="6350000" cy="1270000"/></a:xfrm></p:spPr><p:txBody><a:bodyPr anchor="b"/><a:p><a:r><a:t>Master title prompt</a:t></a:r></a:p></p:txBody></p:sp>
          <p:sp><p:nvSpPr><p:cNvPr id="3" name="b"/><p:cNvSpPr/><p:nvPr><p:ph type="body" idx="1"/></p:nvPr></p:nvSpPr><p:spPr><a:xfrm><a:off x="127000" y="1524000"/><a:ext cx="6350000" cy="2540000"/></a:xfrm></p:spPr></p:sp>
          <p:sp><p:nvSpPr><p:cNvPr id="4" name="logo"/><p:cNvSpPr/><p:nvPr/></p:nvSpPr><p:spPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="127000" cy="127000"/></a:xfrm><a:prstGeom prst="ellipse"/><a:solidFill><a:schemeClr val="accent1"><a:lumMod val="50000"/></a:schemeClr></a:solidFill></p:spPr></p:sp>
        </p:spTree></p:cSld><p:clrMap bg1="lt1" tx1="dk1" bg2="lt2" tx2="dk2"/>
        <p:txStyles><p:titleStyle><a:lvl1pPr algn="ctr"><a:defRPr sz="4000"><a:solidFill><a:schemeClr val="tx1"/></a:solidFill></a:defRPr></a:lvl1pPr></p:titleStyle>
        <p:bodyStyle><a:lvl1pPr marL="228600" indent="-228600"><a:buChar char="•"/><a:defRPr sz="2400"><a:solidFill><a:schemeClr val="tx2"/></a:solidFill></a:defRPr></a:lvl1pPr><a:lvl2pPr marL="685800" indent="-228600"><a:buAutoNum type="alphaLcParenR"/><a:defRPr sz="2000"/></a:lvl2pPr></p:bodyStyle></p:txStyles></p:sldMaster>"#
    );
    let layout = format!(
        r#"<p:sldLayout {P}><p:cSld><p:spTree>
          <p:sp><p:nvSpPr><p:cNvPr id="2" name="t"/><p:cNvSpPr/><p:nvPr><p:ph type="title"/></p:nvPr></p:nvSpPr><p:spPr/><p:txBody><a:bodyPr/><a:lstStyle><a:lvl1pPr algn="l"/></a:lstStyle><a:p><a:r><a:t>Layout prompt</a:t></a:r></a:p></p:txBody></p:sp>
          <p:sp><p:nvSpPr><p:cNvPr id="3" name="b"/><p:cNvSpPr/><p:nvPr><p:ph idx="1"/></p:nvPr></p:nvSpPr><p:spPr/></p:sp>
        </p:spTree></p:cSld></p:sldLayout>"#
    );
    let slide = format!(
        r#"<p:sld {P}><p:cSld><p:spTree>
          <p:sp><p:nvSpPr><p:cNvPr id="2" name="t"/><p:cNvSpPr/><p:nvPr><p:ph type="title"/></p:nvPr></p:nvSpPr><p:spPr/><p:txBody><a:bodyPr/><a:p><a:r><a:t>Titel</a:t></a:r></a:p></p:txBody></p:sp>
          <p:sp><p:nvSpPr><p:cNvPr id="3" name="b"/><p:cNvSpPr/><p:nvPr><p:ph idx="1"/></p:nvPr></p:nvSpPr><p:spPr/><p:txBody><a:bodyPr/><a:p><a:r><a:t>punkt</a:t></a:r></a:p><a:p><a:pPr lvl="1"/><a:r><a:t>a</a:t></a:r></a:p><a:p><a:pPr lvl="1"/><a:r><a:rPr b="1" sz="1000"><a:solidFill><a:srgbClr val="00FF00"/></a:solidFill></a:rPr><a:t>b</a:t></a:r></a:p></p:txBody></p:sp>
          <p:sp><p:nvSpPr><p:cNvPr id="4" name="tom"/><p:cNvSpPr/><p:nvPr><p:ph type="dt" idx="10"/></p:nvPr></p:nvSpPr><p:spPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="12700" cy="12700"/></a:xfrm></p:spPr><p:txBody><a:bodyPr/><a:p/></p:txBody></p:sp>
          <p:grpSp><p:grpSpPr><a:xfrm><a:off x="1270000" y="1270000"/><a:ext cx="1270000" cy="1270000"/><a:chOff x="0" y="0"/><a:chExt cx="2540000" cy="2540000"/></a:xfrm></p:grpSpPr>
            <p:pic><p:nvPicPr><p:cNvPr id="5" name="pic"/><p:cNvPicPr/><p:nvPr/></p:nvPicPr><p:blipFill><a:blip r:embed="rImg"/></p:blipFill><p:spPr><a:xfrm><a:off x="254000" y="254000"/><a:ext cx="508000" cy="508000"/></a:xfrm></p:spPr></p:pic>
          </p:grpSp>
          <p:graphicFrame><p:nvGraphicFramePr><p:cNvPr id="6" name="c"/><p:cNvGraphicFramePr/><p:nvPr/></p:nvGraphicFramePr><p:xfrm><a:off x="0" y="0"/><a:ext cx="12700" cy="12700"/></p:xfrm><a:graphic><a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/chart"/></a:graphic></p:graphicFrame>
        </p:spTree></p:cSld></p:sld>"#
    );
    let path = package(
        "t.pptx",
        &[
            ("ppt/presentation.xml", s(&presentation)),
            ("ppt/_rels/presentation.xml.rels", rels(&[("rS1", "slide", "slides/slide1.xml")])),
            ("ppt/slides/slide1.xml", s(&slide)),
            ("ppt/slides/_rels/slide1.xml.rels", rels(&[("rL", "slideLayout", "../slideLayouts/slideLayout1.xml"), ("rImg", "image", "../media/p.png")])),
            ("ppt/slideLayouts/slideLayout1.xml", s(&layout)),
            ("ppt/slideLayouts/_rels/slideLayout1.xml.rels", rels(&[("rM", "slideMaster", "../slideMasters/slideMaster1.xml")])),
            ("ppt/slideMasters/slideMaster1.xml", s(&master)),
            ("ppt/slideMasters/_rels/slideMaster1.xml.rels", rels(&[("rT", "theme", "../theme/theme1.xml")])),
            ("ppt/theme/theme1.xml", s(theme)),
            ("ppt/media/p.png", png()),
        ],
    );
    let doc = render(&path, "pptx", 0).unwrap();
    assert_eq!((doc.page.width, doc.page.height), (720.0, 405.0));
    let slides = doc.slides.as_ref().unwrap();
    assert_eq!(slides.len(), 1);
    let slide = &slides[0];
    assert_eq!(slide.background, [0xFA; 3], "the background from the master's bg1, which is lt1");
    let [logo, title, body, pic, chart] = slide.shapes.as_slice() else { panic!("{} shapes: the master's own, the slide's, no empty placeholder", slide.shapes.len()) };
    assert_eq!((logo.geom, logo.fill), (Geom::Ellipse, Some([0x80, 0, 0])), "the master's own shape, in accent1 at half its luminance");
    assert_eq!(title.rect, [10.0, 20.0, 500.0, 100.0], "a placeholder where the master puts it");
    let ShapeContent::Text(t) = &title.content else { panic!() };
    assert_eq!(t.anchor, VAnchor::Bottom);
    let p = paras(&t.blocks)[0];
    assert_eq!((texts(p).as_str(), p.align), ("Titel", Align::Left), "the layout's alignment over the master's");
    let Inline::Text(span) = &p.inlines[0] else { panic!() };
    assert_eq!((span.style.size.map(|v| v * 18.0), span.style.color), (Some(40.0), Some([0x11; 3])));
    let ShapeContent::Text(b) = &body.content else { panic!() };
    let ps = paras(&b.blocks);
    assert_eq!(ps[0].marker.as_deref(), Some("•"));
    assert_eq!((ps[0].left, ps[0].hang), (Some(18.0), Some(18.0)));
    assert_eq!((ps[1].marker.as_deref(), ps[2].marker.as_deref()), (Some("a)"), Some("b)")), "numbered by level");
    let Inline::Text(own) = &ps[2].inlines[0] else { panic!() };
    assert_eq!((own.style.bold, own.style.color, own.style.size.map(|v| v * 18.0)), (true, Some([0, 0xFF, 0]), Some(10.0)), "a run's own style over all of it");
    let Inline::Text(inherited) = &ps[1].inlines[0] else { panic!() };
    assert_eq!(inherited.style.color, Some([0x11; 3]), "a level gives its own color, not the one above's: tx1 when none does");
    let Inline::Text(first) = &ps[0].inlines[0] else { panic!() };
    assert_eq!(first.style.color, Some([0x22; 3]), "tx2 from the body style");
    assert!(matches!(pic.content, ShapeContent::Image(0)));
    assert_eq!(pic.rect, [110.0, 110.0, 20.0, 20.0], "mapped through the group's frame");
    assert!(matches!(&chart.content, ShapeContent::Missing(w) if w == "chart"));
}

#[test]
fn an_impress_presentation_has_its_slides_over_its_master_page() {
    let styles = format!(
        r##"<office:document-styles {ODF} xmlns:presentation="urn:oasis:names:tc:opendocument:xmlns:presentation:1.0"><office:styles>
          <style:style style:name="standard" style:family="graphic"><style:graphic-properties draw:stroke="solid" svg:stroke-color="#3465a4" draw:fill="solid" draw:fill-color="#729fcf"/></style:style>
        </office:styles><office:automatic-styles>
          <style:page-layout style:name="PM1"><style:page-layout-properties fo:page-width="28cm" fo:page-height="15.75cm"/></style:page-layout>
          <style:style style:name="Mdp1" style:family="drawing-page"><style:drawing-page-properties draw:fill="solid" draw:fill-color="#fafafa"/></style:style>
          <style:style style:name="Mgr1" style:family="graphic"><style:graphic-properties draw:stroke="none" draw:fill-color="#2a6099"/></style:style>
        </office:automatic-styles><office:master-styles>
          <style:style style:name="Default-outline1" style:family="presentation"><style:text-properties fo:font-size="32pt"/></style:style>
          <style:style style:name="Default-outline2" style:family="presentation" style:parent-style-name="Default-outline1"><style:text-properties fo:font-size="28pt"/></style:style>
          <style:master-page style:name="Default" style:page-layout-name="PM1" draw:style-name="Mdp1">
            <draw:frame presentation:class="title" svg:x="1cm" svg:y="1cm" svg:width="20cm" svg:height="2cm"><draw:text-box><text:p>prompt</text:p></draw:text-box></draw:frame>
            <draw:custom-shape draw:style-name="Mgr1" svg:x="0cm" svg:y="15cm" svg:width="28cm" svg:height="0.75cm"><draw:enhanced-geometry draw:type="rectangle"/></draw:custom-shape>
          </style:master-page></office:master-styles></office:document-styles>"##
    );
    let content = format!(
        r##"<office:document-content {ODF} xmlns:presentation="urn:oasis:names:tc:opendocument:xmlns:presentation:1.0"><office:automatic-styles>
          <style:style style:name="dp2" style:family="drawing-page"><style:drawing-page-properties draw:fill="solid" draw:fill-color="#102030"/></style:style>
          <style:style style:name="pr1" style:family="presentation" style:parent-style-name="Default-outline1"/>
          <style:style style:name="gr1" style:family="graphic" style:parent-style-name="standard"><style:graphic-properties draw:fill-color="#ff0000"/></style:style>
          <text:list-style style:name="L1"><text:list-level-style-bullet text:level="1" text:bullet-char="●"/><text:list-level-style-bullet text:level="2" text:bullet-char="–"/></text:list-style>
        </office:automatic-styles><office:body><office:presentation>
          <draw:page draw:master-page-name="Default">
            <draw:frame presentation:style-name="pr1" presentation:class="outline" svg:x="1cm" svg:y="4cm" svg:width="20cm" svg:height="8cm"><draw:text-box>
              <text:list text:style-name="L1"><text:list-item><text:p>et</text:p><text:list><text:list-item><text:p>to</text:p></text:list-item></text:list></text:list-item></text:list>
            </draw:text-box></draw:frame>
            <draw:frame presentation:class="outline" presentation:placeholder="true" svg:x="1cm" svg:y="4cm" svg:width="20cm" svg:height="8cm"><draw:text-box/></draw:frame>
            <draw:custom-shape draw:style-name="gr1" svg:x="2cm" svg:y="2cm" svg:width="4cm" svg:height="2cm"><text:p>rund</text:p><draw:enhanced-geometry draw:type="ellipse"/></draw:custom-shape>
          </draw:page>
          <draw:page draw:master-page-name="Default" draw:style-name="dp2"/>
        </office:presentation></office:body></office:document-content>"##
    );
    let path = package("t.odp", &[("content.xml", s(&content)), ("styles.xml", s(&styles))]);
    let doc = render(&path, "odp", 0).unwrap();
    let cm = 72.0 / 2.54;
    assert!((doc.page.width - 28.0 * cm).abs() < 0.01 && (doc.page.height - 15.75 * cm).abs() < 0.01);
    let slides = doc.slides.as_ref().unwrap();
    assert_eq!(slides.len(), 2);
    assert_eq!(slides[0].background, [0xfa; 3], "the master page's background");
    assert_eq!(slides[1].background, [0x10, 0x20, 0x30], "a slide's own");
    let [band, outline, shape] = slides[0].shapes.as_slice() else { panic!("{} shapes: the master's band, no master placeholder, no empty one", slides[0].shapes.len()) };
    assert_eq!((band.fill, band.line), (Some([0x2a, 0x60, 0x99]), None));
    let ShapeContent::Text(t) = &outline.content else { panic!() };
    let ps = paras(&t.blocks);
    let size = |p: &Para| if let Inline::Text(s) = &p.inlines[0] { s.style.size.map(|v| v * 18.0) } else { None };
    assert_eq!((ps[0].marker.as_deref(), size(ps[0])), (Some("●"), Some(32.0)));
    assert_eq!((ps[1].marker.as_deref(), size(ps[1])), (Some("–"), Some(28.0)), "a second level in the master's outline2 style");
    assert_eq!((shape.geom, shape.fill, shape.line), (Geom::Ellipse, Some([0xff, 0, 0]), Some(([0x34, 0x65, 0xa4], 0.75))));
    let ShapeContent::Text(t) = &shape.content else { panic!() };
    assert_eq!((t.anchor, paras(&t.blocks)[0].align), (VAnchor::Middle, Align::Center), "text in a drawn shape sits in its middle");
}
