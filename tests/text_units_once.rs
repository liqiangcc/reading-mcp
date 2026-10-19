use std::io::{Cursor, Write};

use reading_mcp::application::ports::{DocumentReliabilityInspector, Parser, RetrievedResource};
use reading_mcp::domain::{Document, DocumentSource, MediaType};
use reading_mcp::parsing::{
    ArchiveLimits, EpubParser, MarkdownParser, PdfParser, PersistedDocumentReliabilityInspector,
    TextParser, validate_epub_document, validate_epub_document_with_units,
};
use zip::write::{SimpleFileOptions, ZipWriter};

const PLAIN_TEXT: &str = "Alpha beta gamma. Delta epsilon.\n\nZeta eta theta. Iota kappa lambda.\n";

const MARKDOWN: &str = "# Title\n\nIntro paragraph with two sentences. Second sentence.\n\n\
## List\n\n- first item\n- second item\n\n\
## Code\n\n```rust\nfn main() { println!(\"hi\"); }\n```\n";

#[tokio::test]
async fn try_text_unit_sets_matches_the_individual_builders() {
    for document in [
        parse_text().await,
        parse_markdown().await,
        parse_epub().await,
    ] {
        assert_eq!(
            document.try_text_unit_sets().expect("combined unit sets"),
            (
                document
                    .try_paragraph_text_units()
                    .expect("paragraph units"),
                document.try_sentence_text_units().expect("sentence units"),
            )
        );
    }
}

#[tokio::test]
async fn epub_validation_with_units_matches_full_validation() {
    let document = parse_epub().await;
    let (paragraphs, sentences) = document.try_text_unit_sets().expect("combined unit sets");

    assert_eq!(
        validate_epub_document_with_units(&document, &paragraphs, &sentences),
        validate_epub_document(&document)
    );
}

#[tokio::test]
async fn inspector_with_units_matches_inspect_for_text_markdown_and_epub() {
    for document in [
        parse_text().await,
        parse_markdown().await,
        parse_epub().await,
    ] {
        assert_inspector_parity(&document);
    }
}

#[tokio::test]
async fn inspector_with_units_matches_inspect_for_fallback_pdf_fixture() {
    let bytes =
        std::fs::read("tests/fixtures/scanned_pdf/pdf/F01.pdf").expect("PDF fixture should exist");
    let document = PdfParser
        .parse(pdf_resource(bytes))
        .await
        .expect("native text-layer PDF fixture should parse");
    assert_eq!(document.media_type.0, "application/pdf");
    assert_inspector_parity(&document);
}

fn assert_inspector_parity(document: &Document) {
    let (paragraphs, sentences) = document.try_text_unit_sets().expect("combined unit sets");
    let inspector = PersistedDocumentReliabilityInspector;

    assert_eq!(
        inspector.inspect_with_units(document, &paragraphs, &sentences),
        inspector.inspect(document)
    );
}

async fn parse_text() -> Document {
    TextParser
        .parse(text_resource(PLAIN_TEXT.as_bytes().to_vec()))
        .await
        .expect("plain text should parse")
}

async fn parse_markdown() -> Document {
    MarkdownParser
        .parse(text_resource(MARKDOWN.as_bytes().to_vec()))
        .await
        .expect("Markdown should parse")
}

async fn parse_epub() -> Document {
    EpubParser::new(ArchiveLimits::default())
        .parse(epub_resource(build_epub_fixture()))
        .await
        .expect("EPUB fixture should parse")
}

fn text_resource(bytes: Vec<u8>) -> RetrievedResource {
    RetrievedResource {
        source: DocumentSource("memory:text-units-once.txt".into()),
        final_source: DocumentSource("memory:text-units-once.txt".into()),
        media_type: MediaType("text/plain".into()),
        bytes,
        etag: None,
        last_modified: None,
        metadata: Default::default(),
    }
}

fn pdf_resource(bytes: Vec<u8>) -> RetrievedResource {
    RetrievedResource {
        source: DocumentSource("memory:text-units-once.pdf".into()),
        final_source: DocumentSource("memory:text-units-once.pdf".into()),
        media_type: MediaType("application/pdf".into()),
        bytes,
        etag: None,
        last_modified: None,
        metadata: Default::default(),
    }
}

fn epub_resource(bytes: Vec<u8>) -> RetrievedResource {
    RetrievedResource {
        source: DocumentSource("memory:text-units-once.epub".into()),
        final_source: DocumentSource("memory:text-units-once.epub".into()),
        media_type: MediaType("application/epub+zip".into()),
        bytes,
        etag: None,
        last_modified: None,
        metadata: Default::default(),
    }
}

fn build_epub_fixture() -> Vec<u8> {
    let entries: [(&str, &str); 5] = [
        ("mimetype", "application/epub+zip"),
        ("META-INF/container.xml", CONTAINER_XML),
        (
            "OPS/package.opf",
            r#"<?xml version="1.0"?>
<package xmlns="http://www.idpf.org/2007/opf" version="3.0">
  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:title>Unit Book</dc:title></metadata>
  <manifest>
    <item id="nav" href="nav/toc.xhtml" media-type="application/xhtml+xml" properties="nav"/>
    <item id="ch1" href="text/ch1.xhtml" media-type="application/xhtml+xml"/>
    <item id="ch2" href="text/ch2.xhtml" media-type="application/xhtml+xml"/>
  </manifest>
  <spine><itemref idref="ch1"/><itemref idref="ch2"/></spine>
</package>"#,
        ),
        (
            "OPS/nav/toc.xhtml",
            r#"<?xml version="1.0"?>
<html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops"><body>
<nav epub:type="toc"><ol>
  <li><a href="../text/ch1.xhtml#intro">Opening Chapter</a><ol>
    <li><a href="../text/ch1.xhtml#detail">Opening Detail</a></li>
  </ol></li>
  <li><a href="../text/ch2.xhtml#second">Closing Chapter</a></li>
</ol></nav>
</body></html>"#,
        ),
        (
            "OPS/text/ch1.xhtml",
            r#"<html xmlns="http://www.w3.org/1999/xhtml"><body><h1 id="intro">Visible Intro</h1><p>Opening sentence one. Opening sentence two.</p><h2 id="detail">Visible Detail</h2><p>Detail sentence.</p></body></html>"#,
        ),
    ];

    let mut entries = entries.to_vec();
    entries.push((
        "OPS/text/ch2.xhtml",
        r#"<html xmlns="http://www.w3.org/1999/xhtml"><body><h1 id="second">Visible Second</h1><p>Second sentence.</p></body></html>"#,
    ));

    let cursor = Cursor::new(Vec::new());
    let mut writer = ZipWriter::new(cursor);
    let options = SimpleFileOptions::default();
    for (name, content) in entries {
        writer.start_file(name, options).expect("ZIP entry");
        writer
            .write_all(content.as_bytes())
            .expect("ZIP entry content");
    }
    writer.finish().expect("ZIP finish").into_inner()
}

const CONTAINER_XML: &str = r#"<?xml version="1.0"?>
<container xmlns="urn:oasis:names:tc:opendocument:xmlns:container"><rootfiles>
<rootfile full-path="OPS/package.opf" media-type="application/oebps-package+xml"/>
</rootfiles></container>"#;
