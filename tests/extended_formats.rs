use std::io::{Cursor, Write};

use reading_mcp::application::ports::{Parser, RetrievedResource};
use reading_mcp::domain::{DocumentSource, MediaType};
use reading_mcp::parsing::{ArchiveLimits, ParserRouter};
use zip::write::{SimpleFileOptions, ZipWriter};

#[tokio::test]
async fn epub_docx_and_openapi_share_the_same_normalized_document_contract() {
    let router = ParserRouter::release(100, ArchiveLimits::default());

    let epub = router
        .parse(resource(
            "book.epub",
            "application/epub+zip",
            build_epub(),
        ))
        .await
        .expect("EPUB should parse");
    assert_eq!(epub.title, "Operating Systems EPUB");
    assert!(
        epub.root_sections
            .iter()
            .flat_map(flatten)
            .any(|section| section.content.contains("Orbital memory in EPUB"))
    );
    assert!(epub.root_sections.iter().flat_map(flatten).any(|section| {
        section
            .location
            .native_location
            .as_deref()
            .is_some_and(|value| value.starts_with("epub:"))
    }));

    let docx = router
        .parse(resource(
            "book.docx",
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
            build_docx(),
        ))
        .await
        .expect("DOCX should parse");
    assert_eq!(docx.title, "Operating Systems DOCX");
    assert_eq!(docx.root_sections[0].title, "Virtual Memory");
    assert!(
        docx.root_sections[0]
            .content
            .contains("Orbital memory in DOCX")
    );

    let openapi = router
        .parse(resource(
            "openapi.yaml",
            "application/yaml",
            b"openapi: 3.1.0\ninfo:\n  title: Memory API\npaths:\n  /memory:\n    get:\n      summary: Orbital memory endpoint\n      responses:\n        '200':\n          description: ok\n"
                .to_vec(),
        ))
        .await
        .expect("OpenAPI YAML should parse");
    assert_eq!(openapi.title, "Memory API");
    let path = openapi
        .root_sections
        .iter()
        .find(|section| section.title == "/memory")
        .expect("OpenAPI path should become a section");
    assert_eq!(path.children[0].title, "GET /memory");
    assert!(
        path.children[0]
            .content
            .contains("Orbital memory endpoint")
    );
}

fn resource(name: &str, media_type: &str, bytes: Vec<u8>) -> RetrievedResource {
    RetrievedResource {
        source: DocumentSource(format!("memory:{name}")),
        final_source: DocumentSource(format!("memory:{name}")),
        media_type: MediaType(media_type.into()),
        bytes,
        etag: None,
        last_modified: None,
        metadata: Default::default(),
    }
}

fn flatten(section: &reading_mcp::domain::Section) -> Vec<&reading_mcp::domain::Section> {
    let mut sections = vec![section];
    for child in &section.children {
        sections.extend(flatten(child));
    }
    sections
}

fn build_epub() -> Vec<u8> {
    build_zip(&[
        ("mimetype", "application/epub+zip"),
        (
            "META-INF/container.xml",
            r#"<?xml version="1.0"?>
<container xmlns="urn:oasis:names:tc:opendocument:xmlns:container">
  <rootfiles><rootfile full-path="OPS/package.opf" media-type="application/oebps-package+xml"/></rootfiles>
</container>"#,
        ),
        (
            "OPS/package.opf",
            r#"<?xml version="1.0"?>
<package xmlns="http://www.idpf.org/2007/opf" version="3.0">
  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:title>Operating Systems EPUB</dc:title></metadata>
  <manifest><item id="chapter" href="chapter.xhtml" media-type="application/xhtml+xml"/></manifest>
  <spine><itemref idref="chapter"/></spine>
</package>"#,
        ),
        (
            "OPS/chapter.xhtml",
            r#"<!doctype html><html><head><title>Virtual Memory</title></head><body><main><h1 id="vm">Virtual Memory</h1><p>Orbital memory in EPUB.</p></main></body></html>"#,
        ),
    ])
}

fn build_docx() -> Vec<u8> {
    build_zip(&[
        (
            "word/document.xml",
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>
<w:p><w:pPr><w:pStyle w:val="Heading1"/></w:pPr><w:r><w:t>Virtual Memory</w:t></w:r></w:p>
<w:p><w:r><w:t>Orbital memory in DOCX.</w:t></w:r></w:p>
</w:body></w:document>"#,
        ),
        (
            "docProps/core.xml",
            r#"<?xml version="1.0"?><cp:coreProperties xmlns:cp="http://schemas.openxmlformats.org/package/2006/metadata/core-properties" xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:title>Operating Systems DOCX</dc:title></cp:coreProperties>"#,
        ),
    ])
}

fn build_zip(entries: &[(&str, &str)]) -> Vec<u8> {
    let cursor = Cursor::new(Vec::new());
    let mut writer = ZipWriter::new(cursor);
    let options = SimpleFileOptions::default();
    for (name, content) in entries {
        writer
            .start_file(*name, options)
            .expect("ZIP fixture entry should start");
        writer
            .write_all(content.as_bytes())
            .expect("ZIP fixture entry should write");
    }
    writer
        .finish()
        .expect("ZIP fixture should finish")
        .into_inner()
}
