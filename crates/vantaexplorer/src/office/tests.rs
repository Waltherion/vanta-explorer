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
    assert_eq!(row3, ["42", "2026-11-22", "2023-03-15", "25%", "TRUE", "formel", "3.14159"]);
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
