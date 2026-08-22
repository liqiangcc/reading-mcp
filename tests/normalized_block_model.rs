use std::io::{Cursor, Write};

use reading_mcp::application::ports::{DocumentRepository, Parser, RetrievedResource};
use reading_mcp::domain::{
    DocumentSource, MediaType, NORMALIZED_BLOCK_MODEL_VERSION, NormalizedBlockKind,
    NormalizedTextRange,
};
use reading_mcp::infrastructure::SqliteDocumentRepository;
use reading_mcp::parsing::{ArchiveLimits, EpubParser, HtmlParser};
use tempfile::tempdir;
use zip::write::{SimpleFileOptions, ZipWriter};

#[tokio::test]
async fn html_native_blocks_are_exact_section_content_slices() {
    let document = HtmlParser
        .parse(html_resource(
            r#"<html><body>
<h1 id="chapter">Chapter</h1>
<p id="p1">First <b>paragraph</b>.</p>
<blockquote id="quote">Quoted text.</blockquote>
<ul><li id="item">Item text.</li></ul>
<pre id="code">let x = 1;
line2</pre>
<table id="table"><tr><td>A</td><td>B</td></tr></table>
</body></html>"#,
        ))
        .await
        .expect("HTML should parse");

    let map = document
        .normalized_block_map()
        .expect("block metadata should be valid")
        .expect("HTML should persist a block map");
    assert_eq!(map.schema_version, NORMALIZED_BLOCK_MODEL_VERSION);
    assert_eq!(map.blocks.len(), 5);
    assert_eq!(
        map.blocks
            .iter()
            .map(|block| block.kind)
            .collect::<Vec<_>>(),
        vec![
            NormalizedBlockKind::Paragraph,
            NormalizedBlockKind::BlockQuote,
            NormalizedBlockKind::ListItem,
            NormalizedBlockKind::Preformatted,
            NormalizedBlockKind::Table,
        ]
    );

    let owner = document
        .find_section(&map.blocks[0].owner_section_id)
        .expect("block owner should exist");
    assert_eq!(owner.title, "Chapter");
    assert_eq!(
        owner.content,
        "First paragraph.\n\nQuoted text.\n\nItem text.\n\nlet x = 1;\nline2\n\nA B"
    );

    let expected = [
        "First paragraph.",
        "Quoted text.",
        "Item text.",
        "let x = 1;\nline2",
        "A B",
    ];
    for (source_order, (block, expected_text)) in map.blocks.iter().zip(expected).enumerate() {
        assert_eq!(block.source_order, source_order);
        assert_eq!(block.block_index, source_order + 1);
        assert_eq!(
            owner
                .normalized_text_slice(block.normalized_range)
                .expect("block range must resolve"),
            expected_text
        );
    }
    assert_eq!(map.blocks[0].native_anchor.as_deref(), Some("p1"));
    assert_eq!(map.blocks[0].native_location.as_deref(), Some("html:#p1"));
}

#[tokio::test]
async fn headingless_html_blocks_keep_contiguous_source_order() {
    let document = HtmlParser
        .parse(html_resource(
            "<html><body><p>First.</p><p>Second.</p></body></html>",
        ))
        .await
        .expect("headingless HTML should parse");
    let map = document
        .normalized_block_map()
        .expect("block map should validate")
        .expect("block map should exist");
    assert_eq!(map.blocks.len(), 2);
    assert_eq!(map.blocks[0].source_order, 0);
    assert_eq!(map.blocks[1].source_order, 1);
}

#[tokio::test]
async fn normalized_block_map_round_trips_through_sqlite_document_repository() {
    let document = HtmlParser
        .parse(html_resource(
            "<html><body><h1>Chapter</h1><p id='p'>Body.</p><pre>code();</pre></body></html>",
        ))
        .await
        .expect("HTML should parse");
    let expected = document
        .normalized_block_map()
        .expect("block map should validate")
        .expect("block map should exist");

    let directory = tempdir().expect("temp directory");
    let path = directory.path().join("state.sqlite");
    let repository = SqliteDocumentRepository::open(&path).expect("repository should open");
    repository
        .save(document.clone())
        .await
        .expect("document should persist");
    drop(repository);

    let reopened = SqliteDocumentRepository::open(&path).expect("repository should reopen");
    let restored = reopened
        .get(&document.id)
        .await
        .expect("repository read should succeed")
        .expect("document should exist");
    assert_eq!(
        restored
            .normalized_block_map()
            .expect("restored block map should validate")
            .expect("restored block map should exist"),
        expected
    );
}

#[tokio::test]
async fn epub_blocks_are_remapped_to_reconciled_section_identity_and_native_location() {
    let document = EpubParser::new(ArchiveLimits::default())
        .parse(epub_resource(build_epub_fixture()))
        .await
        .expect("EPUB should parse");
    let map = document
        .normalized_block_map()
        .expect("EPUB block map should validate")
        .expect("EPUB block map should exist");
    assert_eq!(map.blocks.len(), 2);

    let first = &map.blocks[0];
    assert!(first.owner_section_id.0.starts_with("section://epub-1/"));
    let owner = document
        .find_section(&first.owner_section_id)
        .expect("remapped block owner should exist");
    assert_eq!(owner.title, "Publisher Chapter");
    assert_eq!(
        owner
            .normalized_text_slice(first.normalized_range)
            .expect("range should resolve"),
        "Opening paragraph."
    );
    assert_eq!(first.native_anchor.as_deref(), Some("p1"));
    assert_eq!(
        first.native_location.as_deref(),
        Some("epub:OPS/chapter.xhtml#p1")
    );
    assert_eq!(map.blocks[1].kind, NormalizedBlockKind::Preformatted);
}

#[tokio::test]
async fn block_map_is_persisted_but_does_not_change_current_hash_or_text_unit_identity() {
    let document = HtmlParser
        .parse(html_resource(
            "<html><body><h1>Chapter</h1><p>One sentence.</p></body></html>",
        ))
        .await
        .expect("HTML should parse");
    let hash = document.normalized_document_hash();
    let paragraph_ids = document
        .paragraph_text_units()
        .units
        .into_iter()
        .map(|unit| unit.id)
        .collect::<Vec<_>>();

    let mut without_blocks = document.clone();
    without_blocks.metadata.remove("normalized_block_map");
    without_blocks
        .metadata
        .remove("normalized_block_map_version");
    without_blocks.metadata.remove("normalized_blocks");

    assert_eq!(without_blocks.normalized_document_hash(), hash);
    assert_eq!(
        without_blocks
            .paragraph_text_units()
            .units
            .into_iter()
            .map(|unit| unit.id)
            .collect::<Vec<_>>(),
        paragraph_ids
    );
}

#[tokio::test]
async fn normalized_block_validator_rejects_overlap_and_bad_source_order() {
    let document = HtmlParser
        .parse(html_resource(
            "<html><body><h1>Chapter</h1><p>First.</p><p>Second.</p></body></html>",
        ))
        .await
        .expect("HTML should parse");
    let mut map = document
        .normalized_block_map()
        .expect("block map should validate")
        .expect("block map should exist");

    map.blocks[1].source_order = 9;
    assert!(document.validate_normalized_block_map(&map).is_err());

    let mut overlap = document
        .normalized_block_map()
        .expect("block map should validate")
        .expect("block map should exist");
    let first = overlap.blocks[0].normalized_range;
    overlap.blocks[1].normalized_range =
        NormalizedTextRange::new(first.end() - 1, overlap.blocks[1].normalized_range.end())
            .expect("range should be ordered");
    assert!(document.validate_normalized_block_map(&overlap).is_err());
}

fn html_resource(source: &str) -> RetrievedResource {
    RetrievedResource {
        source: DocumentSource("memory:block.html".into()),
        final_source: DocumentSource("memory:block.html".into()),
        media_type: MediaType("text/html".into()),
        bytes: source.as_bytes().to_vec(),
        etag: None,
        last_modified: None,
        metadata: Default::default(),
    }
}

fn epub_resource(bytes: Vec<u8>) -> RetrievedResource {
    RetrievedResource {
        source: DocumentSource("memory:block.epub".into()),
        final_source: DocumentSource("memory:block.epub".into()),
        media_type: MediaType("application/epub+zip".into()),
        bytes,
        etag: None,
        last_modified: None,
        metadata: Default::default(),
    }
}

fn build_epub_fixture() -> Vec<u8> {
    build_zip(&[
        ("mimetype", "application/epub+zip"),
        (
            "META-INF/container.xml",
            r#"<?xml version="1.0"?>
<container xmlns="urn:oasis:names:tc:opendocument:xmlns:container"><rootfiles>
<rootfile full-path="OPS/package.opf" media-type="application/oebps-package+xml"/>
</rootfiles></container>"#,
        ),
        (
            "OPS/package.opf",
            r#"<?xml version="1.0"?>
<package xmlns="http://www.idpf.org/2007/opf" version="3.0">
  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:title>Block Book</dc:title></metadata>
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
<nav epub:type="toc"><ol><li><a href="chapter.xhtml#chapter">Publisher Chapter</a></li></ol></nav>
</body></html>"#,
        ),
        (
            "OPS/chapter.xhtml",
            r#"<html xmlns="http://www.w3.org/1999/xhtml"><body>
<h1 id="chapter">Visible Chapter</h1>
<p id="p1">Opening paragraph.</p>
<pre>let x = 1;</pre>
</body></html>"#,
        ),
    ])
}

fn build_zip(entries: &[(&str, &str)]) -> Vec<u8> {
    let cursor = Cursor::new(Vec::new());
    let mut writer = ZipWriter::new(cursor);
    let options = SimpleFileOptions::default();
    for (name, content) in entries {
        writer.start_file(*name, options).expect("ZIP entry");
        writer.write_all(content.as_bytes()).expect("ZIP content");
    }
    writer.finish().expect("ZIP finish").into_inner()
}
