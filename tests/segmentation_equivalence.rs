//! Ingest from compact boundaries must store exactly what ingest from unit sets stores.

use std::collections::BTreeMap;
use std::io::{Cursor, Write};
use std::sync::Arc;

use lopdf::content::{Content, Operation};
use lopdf::{Document as PdfDocument, Object, Stream, dictionary, text_string};
use reading_mcp::application::ports::{Parser, RetrievedResource, TextUnitIndex};
use reading_mcp::domain::{Document, DocumentSource, MediaType};
use reading_mcp::infrastructure::SqliteTextUnitIndex;
use reading_mcp::parsing::{ArchiveLimits, ParserRouter};
use zip::write::{SimpleFileOptions, ZipWriter};

fn resource(name: &str, media_type: &str, bytes: Vec<u8>) -> RetrievedResource {
    RetrievedResource {
        source: DocumentSource(format!("memory:{name}")),
        final_source: DocumentSource(format!("memory:{name}")),
        media_type: MediaType(media_type.into()),
        bytes,
        etag: None,
        last_modified: None,
        metadata: BTreeMap::new(),
    }
}

fn build_zip(entries: &[(&str, &str)]) -> Vec<u8> {
    let cursor = Cursor::new(Vec::new());
    let mut writer = ZipWriter::new(cursor);
    let options = SimpleFileOptions::default();
    for (name, content) in entries {
        writer.start_file(*name, options).expect("ZIP entry");
        writer
            .write_all(content.as_bytes())
            .expect("ZIP entry content");
    }
    writer.finish().expect("ZIP finish").into_inner()
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
  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:title>Segmentation EPUB</dc:title></metadata>
  <manifest>
    <item id="nav" href="nav.xhtml" media-type="application/xhtml+xml" properties="nav"/>
    <item id="chapter" href="chapter.xhtml" media-type="application/xhtml+xml"/>
  </manifest>
  <spine><itemref idref="chapter"/></spine>
</package>"#,
        ),
        (
            "OPS/nav.xhtml",
            r#"<html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops"><body>
<nav epub:type="toc"><ol><li><a href="chapter.xhtml#vm">Virtual Memory</a></li></ol></nav>
</body></html>"#,
        ),
        (
            "OPS/chapter.xhtml",
            r#"<html xmlns="http://www.w3.org/1999/xhtml"><body><main><h1 id="vm">Virtual Memory</h1><p>Orbital memory in EPUB.</p><p>A second paragraph. With two sentences.</p><blockquote><p>Quoted thought.</p></blockquote><pre>code line one. code line two.</pre></main></body></html>"#,
        ),
    ])
}

fn build_pdf_bytes(page_texts: &[&str]) -> Vec<u8> {
    let mut document = PdfDocument::with_version("1.5");
    let pages_id = document.new_object_id();
    let font_id = document.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => "Helvetica",
    });
    let resources_id = document.add_object(dictionary! {
        "Font" => dictionary! {
            "F1" => font_id,
        },
    });

    let mut page_ids = Vec::new();
    for text in page_texts {
        let content = Content {
            operations: vec![
                Operation::new("BT", vec![]),
                Operation::new("Tf", vec!["F1".into(), 12.into()]),
                Operation::new("Td", vec![72.into(), 720.into()]),
                Operation::new("Tj", vec![Object::string_literal(*text)]),
                Operation::new("ET", vec![]),
            ],
        };
        let content_id = document.add_object(Stream::new(
            dictionary! {},
            content.encode().expect("fixture content should encode"),
        ));
        let page_id = document.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "Contents" => content_id,
        });
        page_ids.push(page_id);
    }

    let kids = page_ids
        .iter()
        .copied()
        .map(Object::Reference)
        .collect::<Vec<_>>();
    document.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => kids,
            "Count" => page_ids.len() as i64,
            "Resources" => resources_id,
            "MediaBox" => vec![0.into(), 0.into(), 595.into(), 842.into()],
        }),
    );

    let catalog_id = document.add_object(dictionary! {
        "Type" => "Catalog",
        "Pages" => pages_id,
    });
    let info_id = document.add_object(dictionary! {
        "Title" => text_string("Segmentation"),
    });
    document.trailer.set("Root", catalog_id);
    document.trailer.set("Info", info_id);
    let mut bytes = Vec::new();
    document
        .save_to(&mut bytes)
        .expect("PDF fixture should serialize");
    bytes
}

async fn fixtures() -> Vec<(&'static str, Document)> {
    let router = ParserRouter::release(100, ArchiveLimits::default());
    let mut documents = Vec::new();

    let plain = router
        .parse(resource(
            "plain.txt",
            "text/plain",
            "One. Two. Three.\n\nSecond paragraph here."
                .as_bytes()
                .to_vec(),
        ))
        .await
        .expect("plain parses");
    documents.push(("plain", plain));

    let markdown = router
        .parse(resource(
            "book.md",
            "text/markdown",
            r#"# Virtual Memory

Address spaces give each process an isolated memory view.

## Page tables

- First item with one sentence.
- Second item. With two sentences.

> A quoted thought. It spans sentences.

```rust
fn main() { println!("Hi."); }
```

| Column A | Column B |
|----------|----------|
| one      | two      |
"#
            .as_bytes()
            .to_vec(),
        ))
        .await
        .expect("markdown parses");
    documents.push(("markdown", markdown));

    let html = router
        .parse(resource(
            "page.html",
            "text/html",
            r#"<!doctype html><html><head><title>Memory</title></head><body><h1>Virtual Memory</h1><p>Orbital memory in HTML.</p><ul><li>List one.</li><li>List two. More text.</li></ul><blockquote>Quote text. Second sentence.</blockquote><pre><code>code block text</code></pre><table><tr><th>A</th><th>B</th></tr><tr><td>1</td><td>2</td></tr></table></body></html>"#.as_bytes().to_vec(),
        ))
        .await
        .expect("html parses");
    documents.push(("html", html));

    let epub = router
        .parse(resource("book.epub", "application/epub+zip", build_epub()))
        .await
        .expect("epub parses");
    documents.push(("epub", epub));

    let pdf = router
        .parse(resource(
            "book.pdf",
            "application/pdf",
            build_pdf_bytes(&[
                "Address spaces give each process an isolated memory view.",
                "Page table entries map virtual pages to physical frames.",
            ]),
        ))
        .await
        .expect("pdf parses");
    documents.push(("pdf", pdf));

    documents
}

fn all_section_ids(document: &Document) -> Vec<reading_mcp::domain::SectionId> {
    fn collect(
        section: &reading_mcp::domain::Section,
        out: &mut Vec<reading_mcp::domain::SectionId>,
    ) {
        out.push(section.id.clone());
        for child in &section.children {
            collect(child, out);
        }
    }
    let mut sections = Vec::new();
    for root in &document.root_sections {
        collect(root, &mut sections);
    }
    sections
}

#[tokio::test]
async fn materialized_units_equal_the_unit_sets_for_every_fixture() {
    for (label, document) in fixtures().await {
        let segmentation = document
            .try_segmentation()
            .expect("segmentation should decode");
        let hash = document.normalized_document_hash();
        let materialized = segmentation.materialize(&document, &hash);
        let rebuilt = document
            .try_text_unit_sets()
            .expect("unit sets should materialize");

        assert_eq!(
            materialized, rebuilt,
            "fixture {label} materialized units differ from the unit sets"
        );
    }
}

#[tokio::test]
async fn stored_layouts_and_units_from_boundaries_equal_the_unit_path() {
    for (label, document) in fixtures().await {
        let document = Arc::new(document);
        let segmentation = Arc::new(
            document
                .try_segmentation()
                .expect("segmentation should decode"),
        );
        let hash = document.normalized_document_hash();
        let (paragraphs, sentences) = segmentation.materialize(&document, &hash);

        let directory = tempfile::tempdir().expect("temporary directory");
        let units = SqliteTextUnitIndex::open(directory.path().join("units.sqlite"))
            .expect("unit index should open");
        let boundaries = SqliteTextUnitIndex::open(directory.path().join("boundaries.sqlite"))
            .expect("boundary index should open");

        units
            .replace_document_sets(
                &document,
                Arc::new(paragraphs.clone()),
                Arc::new(sentences.clone()),
            )
            .await
            .expect("unit sets should persist");
        boundaries
            .replace_document_segmentation(&document, Arc::clone(&segmentation))
            .await
            .expect("boundaries should persist");

        assert_eq!(
            units
                .stored_section_layouts(&document.id)
                .await
                .expect("unit layouts"),
            boundaries
                .stored_section_layouts(&document.id)
                .await
                .expect("boundary layouts"),
            "fixture {label} stored different section layouts"
        );

        for section_id in all_section_ids(&document) {
            assert_eq!(
                units
                    .section_units(&document.id, &section_id)
                    .await
                    .expect("unit section"),
                boundaries
                    .section_units(&document.id, &section_id)
                    .await
                    .expect("boundary section"),
                "fixture {label} section {} differs",
                section_id.0
            );
        }
    }
}
