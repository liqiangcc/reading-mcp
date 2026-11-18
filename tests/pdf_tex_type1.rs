//! PDF-6b: the committed pdfTeX fixture (Type 1, built-in encoding, no
//! `/ToUnicode`) decodes to the text a reader expects.
//!
//! The fixture is `tests/fixtures/pdf_tex/type1-builtin.tex`, compiled by the
//! `PDF TeX fixture` workflow with pdfTeX defaults and `\pdfgentounicode=0`;
//! `tests/fixtures/pdf_tex/README.md` carries its `pdffonts` table.

use reading_mcp::application::ports::{Parser, RetrievedResource};
use reading_mcp::domain::{DocumentSource, MediaType, Section};
use reading_mcp::parsing::PdfParser;

fn fixture_resource() -> RetrievedResource {
    let path = "tests/fixtures/pdf_tex/type1-builtin.pdf";
    let bytes = std::fs::read(path).expect("the fixture PDF is committed");
    let source = DocumentSource(format!("file://{path}"));
    RetrievedResource {
        source: source.clone(),
        final_source: source,
        media_type: MediaType("application/pdf".into()),
        bytes,
        etag: None,
        last_modified: None,
        metadata: Default::default(),
    }
}

fn section_text(sections: &[Section], out: &mut String) {
    for section in sections {
        out.push_str(&section.content);
        out.push('\n');
        section_text(&section.children, out);
    }
}

#[tokio::test]
async fn the_pdftex_fixture_decodes_its_builtin_type1_encoding() {
    let document = PdfParser
        .parse(fixture_resource())
        .await
        .expect("the TeX fixture parses");
    let mut text = String::new();
    section_text(&document.root_sections, &mut text);
    // Inter-word spaces, superscript layout and the line breaks lopdf inserts at
    // every text object are later PDF-6 tasks, so this test only asserts the
    // characters: whitespace is dropped before matching.
    let compact = text
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect::<String>();
    let lowered = compact.to_lowercase();

    for word in ["efficient", "finance", "first", "fluffy", "affluent"] {
        assert!(lowered.contains(word), "{word:?} missing from {text:?}");
    }
    // CMMI's `period` at 0x3A used to decode as `:`.
    assert!(compact.contains("0.5%"), "0.5% missing from {text:?}");
    assert!(!compact.contains("0:5"), "0:5 is still there in {text:?}");
    // CMSY's `minus` at 0x00 used to be dropped.
    assert!(compact.contains('\u{2212}'), "no U+2212 in {text:?}");
    assert!(compact.contains("\u{2212}12"), "no -12 in {text:?}");
    assert!(compact.contains('\u{3b1}'), "no alpha in {text:?}");
    // The ligatures are plain letters, not the U+FB00 block.
    assert!(!text.contains('\u{fb00}'), "U+FB00 in {text:?}");
    assert!(!text.contains('\u{fb03}'), "U+FB03 in {text:?}");
    assert!(
        compact.contains("plaintext"),
        "plain text missing from {text:?}"
    );
}
