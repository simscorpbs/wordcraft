//! Page border placement (`w:pgBorders w:offsetFrom`, ECMA-376 Part 1 §17.6.10): measured from
//! the page edge or from the text, `text` when the attribute is omitted. Fixtures are hand-written
//! from the spec.

use std::io::{Read, Write};

use wordcraft_doc::props::{Border, Borders};

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

fn section(offset: &str) -> String {
    let side = |n: &str| format!(r#"<w:{n} w:val="single" w:sz="4" w:space="24" w:color="auto"/>"#);
    format!(r#"<w:sectPr><w:pgBorders{offset}>{}{}{}{}</w:pgBorders></w:sectPr>"#, side("top"), side("left"), side("bottom"), side("right"))
}

/// A border measured from the page edge was saved as measured from the text, which moves it
/// inside the margins in Word; the omitted attribute means the text (§17.6.10).
#[test]
fn page_border_offset_survives_open_and_save() {
    for (attr, from_text, written) in [
        (r#" w:offsetFrom="page""#, false, r#"w:offsetFrom="page""#),
        (r#" w:offsetFrom="text""#, true, r#"w:offsetFrom="text""#),
        ("", true, r#"w:offsetFrom="text""#),
    ] {
        let body = format!(r#"<w:p><w:r><w:t>A</w:t></w:r></w:p>{}"#, section(attr));
        let d = wordcraft_docx::read(&docx(&body)).unwrap();
        assert!(d.last_section.page_borders.is_some());
        assert_eq!(d.last_section.page_borders_from_text, from_text, "{attr:?}");
        let out = wordcraft_docx::write(&d).unwrap();
        let xml = document_xml(&out);
        assert!(xml.contains(written), "{attr:?}: {xml}");
        assert_eq!(wordcraft_docx::read(&out).unwrap().last_section.page_borders_from_text, from_text, "{attr:?}");
    }
}

/// Borders made in WordCraft are measured from the page edge, as laid out, and saved so.
#[test]
fn new_page_borders_are_saved_from_the_page_edge() {
    let mut d = wordcraft_doc::Document::from_text("A");
    d.last_section.page_borders = Some(Borders::box_(Border::single(1.0)));
    let xml = document_xml(&wordcraft_docx::write(&d).unwrap());
    assert!(xml.contains(r#"<w:pgBorders w:offsetFrom="page">"#), "{xml}");
}
