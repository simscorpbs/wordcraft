//! Spacing between table cells (`w:tblCellSpacing` in `w:tblPr`, `w:tblPrEx` and `w:trPr`,
//! ECMA-376 Part 1 §17.4.44–46). Fixtures are hand-written from the spec.

use std::io::{Read, Write};

fn docx(body: &str) -> Vec<u8> {
    let root = r#"<?xml version="1.0" encoding="UTF-8"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let doc = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>{body}</w:body></w:document>"#
    );
    let mut zw = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    for (name, bytes) in [("_rels/.rels", root.as_bytes()), ("word/document.xml", doc.as_bytes())] {
        zw.start_file(name, zip::write::SimpleFileOptions::default()).unwrap();
        zw.write_all(bytes).unwrap();
    }
    zw.finish().unwrap().into_inner()
}

fn document_xml(bytes: &[u8]) -> String {
    let mut z = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
    let mut s = String::new();
    z.by_name("word/document.xml").unwrap().read_to_string(&mut s).unwrap();
    s
}

const SPACING: &str = r#"<w:tblCellSpacing w:w="40" w:type="dxa"/>"#;

fn table() -> String {
    let cell = r#"<w:tc><w:tcPr><w:tcW w:w="2000" w:type="dxa"/></w:tcPr><w:p><w:r><w:t>x</w:t></w:r></w:p></w:tc>"#;
    format!(
        r#"<w:tbl><w:tblPr><w:tblW w:w="0" w:type="auto"/><w:jc w:val="center"/>{SPACING}<w:tblInd w:w="100" w:type="dxa"/></w:tblPr><w:tblGrid><w:gridCol w:w="2000"/><w:gridCol w:w="2000"/></w:tblGrid><w:tr><w:trPr>{SPACING}</w:trPr>{cell}{cell}</w:tr><w:tr><w:tblPrEx><w:tblCellSpacing w:w="60" w:type="dxa"/></w:tblPrEx><w:trPr><w:tblCellSpacing w:w="60" w:type="dxa"/></w:trPr>{cell}{cell}</w:tr></w:tbl><w:p/>"#
    )
}

/// Cell spacing was dropped on open, so a saved table lost the gaps between its cells in Word.
#[test]
fn cell_spacing_survives_open_and_save() {
    let d = wordcraft_docx::read(&docx(&table())).unwrap();
    let check = |d: &wordcraft_doc::Document| {
        let t = d.body.iter().find_map(|b| b.as_table()).unwrap();
        assert_eq!(t.props.cell_spacing, Some(2.0));
        assert_eq!(t.rows[0].props.cell_spacing, Some(2.0));
        assert_eq!(t.rows[1].props.cell_spacing, Some(3.0));
        assert_eq!(t.rows[1].props.exceptions.as_ref().and_then(|x| x.cell_spacing), Some(3.0));
    };
    check(&d);
    let out = wordcraft_docx::write(&d).unwrap();
    let xml = document_xml(&out);
    // In the schema's order: after `w:jc`, before `w:tblInd`; last in `w:trPr` here.
    assert!(xml.contains(&format!(r#"<w:jc w:val="center"/>{SPACING}<w:tblInd"#)), "{xml}");
    assert!(xml.contains(&format!(r#"<w:trPr>{SPACING}</w:trPr>"#)), "{xml}");
    assert!(xml.contains(r#"<w:tblPrEx><w:tblCellSpacing w:w="60" w:type="dxa"/></w:tblPrEx>"#), "{xml}");
    check(&wordcraft_docx::read(&out).unwrap());
}

/// Spacing in another unit, of zero or junk, is none and writes nothing.
#[test]
fn unusable_cell_spacing_is_none() {
    for spacing in [r#"<w:tblCellSpacing w:w="0" w:type="dxa"/>"#, r#"<w:tblCellSpacing w:w="0" w:type="nil"/>"#, r#"<w:tblCellSpacing w:w="x"/>"#] {
        let body = table().replace(SPACING, spacing);
        let d = wordcraft_docx::read(&docx(&body)).unwrap();
        let t = d.body.iter().find_map(|b| b.as_table()).unwrap();
        assert_eq!(t.props.cell_spacing, None, "{spacing}");
        assert_eq!(t.rows[0].props.cell_spacing, None, "{spacing}");
        let xml = document_xml(&wordcraft_docx::write(&d).unwrap());
        assert_eq!(xml.matches("w:tblCellSpacing").count(), 2, "only the second row's: {xml}");
    }
}
