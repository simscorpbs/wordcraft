//! Percentage shading patterns (`w:shd w:val="pctN"`, ECMA-376 Part 1 §17.18.78): N % of the
//! pattern colour over the fill. Fixtures are hand-written from the spec.

use std::io::{Read, Write};

use wordcraft_doc::props::Rgb;

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

fn shaded_para(shd: &str) -> String {
    format!(r#"<w:p><w:pPr>{shd}</w:pPr><w:r><w:t>x</w:t></w:r></w:p>"#)
}

/// A 25 % pattern in automatic colours was read as no shading at all (Word shows it light grey),
/// and a pattern over a white fill as plain white.
#[test]
fn percentage_patterns_read_as_their_blend() {
    let cases = [
        (r#"<w:shd w:val="pct25" w:color="auto" w:fill="auto"/>"#, Some(Rgb(191, 191, 191))),
        (r#"<w:shd w:val="pct50" w:color="FF0000" w:fill="FFFFFF"/>"#, Some(Rgb(255, 128, 128))),
        (r#"<w:shd w:val="pct20" w:color="000000" w:fill="0000FF"/>"#, Some(Rgb(0, 0, 204))),
        (r#"<w:shd w:val="pct12" w:color="auto" w:fill="FFFFFF"/>"#, Some(Rgb(223, 223, 223))),
        // Unchanged: a clear fill, and no pattern without a fill.
        (r#"<w:shd w:val="clear" w:color="auto" w:fill="FFFF00"/>"#, Some(Rgb(255, 255, 0))),
        (r#"<w:shd w:val="clear" w:color="auto" w:fill="auto"/>"#, None),
        (r#"<w:shd w:val="pctjunk" w:color="auto" w:fill="auto"/>"#, None),
    ];
    let body: String = cases.iter().map(|(shd, _)| shaded_para(shd)).collect();
    let d = wordcraft_docx::read(&docx(&body)).unwrap();
    let got: Vec<Option<Rgb>> = d.body.iter().filter_map(|b| b.as_para()).map(|p| p.props.shading).collect();
    let want: Vec<Option<Rgb>> = cases.iter().map(|(_, c)| *c).collect();
    assert_eq!(got, want);
    // Saved as the colour shown, so Word shows the same shade.
    let xml = document_xml(&wordcraft_docx::write(&d).unwrap());
    assert!(xml.contains(r#"w:fill="BFBFBF""#), "{xml}");
}

/// Runs and table cells take the same blend.
#[test]
fn run_and_cell_patterns_blend() {
    let body = r#"<w:tbl><w:tblGrid><w:gridCol w:w="2000"/></w:tblGrid><w:tr><w:tc><w:tcPr><w:shd w:val="pct20" w:color="auto" w:fill="auto"/></w:tcPr><w:p><w:r><w:rPr><w:shd w:val="pct75" w:color="auto" w:fill="FFFFFF"/></w:rPr><w:t>x</w:t></w:r></w:p></w:tc></w:tr></w:tbl><w:p/>"#;
    let d = wordcraft_docx::read(&docx(body)).unwrap();
    let t = d.body.iter().find_map(|b| b.as_table()).unwrap();
    let cell = &t.rows[0].cells[0];
    assert_eq!(cell.props.shading, Some(Rgb(204, 204, 204)));
    let p = cell.blocks.iter().find_map(|b| b.as_para()).unwrap();
    assert_eq!(p.runs[0].props.shading, Some(Rgb(64, 64, 64)));
}
