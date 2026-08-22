use std::io::{Cursor, Write};

use reading_mcp::application::ports::{Parser, RetrievedResource};
use reading_mcp::domain::{DocumentSource, MediaType};
use reading_mcp::parsing::{ArchiveLimits, EpubParser};
use serde_json::Value;
use zip::write::{SimpleFileOptions, ZipWriter};

#[tokio::test]
async fn publisher_navigation_hierarchy_overrides_heading_parentage_without_copying_text() {
    let document = parse(build_hierarchical_nav_fixture()).await;

    assert_eq!(document.root_sections.len(), 1);
    let chapter = &document.root_sections[0];
    assert_eq!(chapter.title, "Publisher Chapter");
    assert_eq!(chapter.content, "Chapter body.");
    assert_eq!(chapter.location.anchor.as_deref(), Some("chapter"));
    assert_eq!(chapter.children.len(), 1);

    let appendix = &chapter.children[0];
    assert_eq!(appendix.title, "Publisher Appendix");
    assert_eq!(appendix.content, "Appendix body.");
    assert_eq!(appendix.location.anchor.as_deref(), Some("appendix"));
    assert_eq!(appendix.parent_id.as_ref(), Some(&chapter.id));
    assert_eq!(chapter.level, 1);
    assert_eq!(appendix.level, 2);
    assert_eq!(
        appendix.location.section_path,
        vec!["Publisher Chapter", "Publisher Appendix"]
    );

    let structure = structure_map(&document);
    assert_eq!(
        structure["schema_version"],
        "epub-structure-reconciliation/v1"
    );
    assert_eq!(structure["applied_navigation_nodes"], 2);
    let sections = structure["sections"].as_array().expect("section facts");
    assert_eq!(sections.len(), 2);
    assert!(
        sections
            .iter()
            .all(|section| section["provenance"] == "epub_nav")
    );
}

#[tokio::test]
async fn canonical_order_remains_spine_order_even_when_navigation_order_conflicts() {
    let document = parse(build_reversed_nav_fixture()).await;

    assert_eq!(document.root_sections.len(), 2);
    assert_eq!(document.root_sections[0].title, "Publisher First");
    assert_eq!(document.root_sections[1].title, "Publisher Second");

    let structure = structure_map(&document);
    let diagnostics = structure["diagnostics"].as_array().expect("diagnostics");
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic["code"] == "navigation_order_conflicts_spine_order")
    );

    let spine = structure["spine"].as_array().expect("spine facts");
    assert_eq!(spine.len(), 2);
    assert_eq!(spine[0]["spine_index"], 1);
    assert_eq!(spine[0]["linear"], true);
    assert_eq!(spine[0]["parse_status"], "parsed");
    assert_eq!(spine[1]["spine_index"], 2);
    assert_eq!(spine[1]["linear"], false);
    assert_eq!(spine[1]["parse_status"], "parsed");
    assert_eq!(structure["linear_spine_items"], 1);
    assert_eq!(structure["non_linear_spine_items"], 1);

    let second_fact = structure["sections"]
        .as_array()
        .expect("section facts")
        .iter()
        .find(|section| section["spine_index"] == 2)
        .expect("second spine Section fact");
    assert_eq!(second_fact["linear"], false);
}

#[tokio::test]
async fn non_heading_fragment_does_not_fabricate_a_canonical_section_boundary() {
    let document = parse(build_non_heading_fragment_fixture()).await;

    assert_eq!(document.root_sections.len(), 1);
    assert_eq!(document.root_sections[0].title, "Visible Heading");

    let structure = structure_map(&document);
    assert_eq!(structure["applied_navigation_nodes"], 0);
    assert_eq!(structure["sections"][0]["provenance"], "xhtml_heading");
    assert!(
        structure["diagnostics"]
            .as_array()
            .expect("diagnostics")
            .iter()
            .any(|diagnostic| diagnostic["code"] == "navigation_fragment_not_section_boundary")
    );
}

#[tokio::test]
async fn headingless_content_remains_addressable_with_spine_item_fallback_provenance() {
    let document = parse(build_headingless_fixture()).await;

    assert_eq!(document.root_sections.len(), 1);
    assert_eq!(document.root_sections[0].content, "Headingless body.");
    let structure = structure_map(&document);
    assert_eq!(structure["applied_navigation_nodes"], 0);
    assert_eq!(structure["sections"][0]["provenance"], "spine_item");
}

#[tokio::test]
async fn ncx_navigation_can_supply_canonical_title_without_being_relabelled_epub_nav() {
    let document = parse(build_ncx_fixture()).await;

    assert_eq!(document.root_sections[0].title, "NCX Chapter");
    let structure = structure_map(&document);
    assert_eq!(structure["navigation_provenance"], "epub_ncx");
    assert_eq!(structure["sections"][0]["provenance"], "epub_ncx");
}

async fn parse(bytes: Vec<u8>) -> reading_mcp::domain::Document {
    EpubParser::new(ArchiveLimits::default())
        .parse(RetrievedResource {
            source: DocumentSource("memory:structure.epub".into()),
            final_source: DocumentSource("memory:structure.epub".into()),
            media_type: MediaType("application/epub+zip".into()),
            bytes,
            etag: None,
            last_modified: None,
            metadata: Default::default(),
        })
        .await
        .expect("EPUB fixture should parse")
}

fn structure_map(document: &reading_mcp::domain::Document) -> Value {
    serde_json::from_str(
        document
            .metadata
            .get("epub_structure_map")
            .expect("structure map metadata"),
    )
    .expect("structure map JSON")
}

fn build_hierarchical_nav_fixture() -> Vec<u8> {
    build_zip(vec![
        ("mimetype", "application/epub+zip".into()),
        ("META-INF/container.xml", container_xml().into()),
        (
            "OPS/package.opf",
            package(
                r#"<item id="nav" href="nav.xhtml" media-type="application/xhtml+xml" properties="nav"/>
<item id="chapter" href="chapter.xhtml" media-type="application/xhtml+xml"/>
<item id="appendix" href="appendix.xhtml" media-type="application/xhtml+xml"/>"#,
                r#"<itemref idref="chapter"/><itemref idref="appendix"/>"#,
                None,
            ),
        ),
        (
            "OPS/nav.xhtml",
            r#"<html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops"><body>
<nav epub:type="toc"><ol><li><a href="chapter.xhtml#chapter">Publisher Chapter</a><ol>
<li><a href="appendix.xhtml#appendix">Publisher Appendix</a></li>
</ol></li></ol></nav></body></html>"#
                .into(),
        ),
        (
            "OPS/chapter.xhtml",
            r#"<html xmlns="http://www.w3.org/1999/xhtml"><body><h1 id="chapter">Visible Chapter</h1><p>Chapter body.</p></body></html>"#
                .into(),
        ),
        (
            "OPS/appendix.xhtml",
            r#"<html xmlns="http://www.w3.org/1999/xhtml"><body><h1 id="appendix">Visible Appendix</h1><p>Appendix body.</p></body></html>"#
                .into(),
        ),
    ])
}

fn build_reversed_nav_fixture() -> Vec<u8> {
    build_zip(vec![
        ("mimetype", "application/epub+zip".into()),
        ("META-INF/container.xml", container_xml().into()),
        (
            "OPS/package.opf",
            package(
                r#"<item id="nav" href="nav.xhtml" media-type="application/xhtml+xml" properties="nav"/>
<item id="first" href="first.xhtml" media-type="application/xhtml+xml"/>
<item id="second" href="second.xhtml" media-type="application/xhtml+xml"/>"#,
                r#"<itemref idref="first"/><itemref idref="second" linear="no"/>"#,
                None,
            ),
        ),
        (
            "OPS/nav.xhtml",
            r#"<html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops"><body>
<nav epub:type="toc"><ol>
<li><a href="second.xhtml#second">Publisher Second</a></li>
<li><a href="first.xhtml#first">Publisher First</a></li>
</ol></nav></body></html>"#
                .into(),
        ),
        (
            "OPS/first.xhtml",
            r#"<html xmlns="http://www.w3.org/1999/xhtml"><body><h1 id="first">Visible First</h1><p>First.</p></body></html>"#
                .into(),
        ),
        (
            "OPS/second.xhtml",
            r#"<html xmlns="http://www.w3.org/1999/xhtml"><body><h1 id="second">Visible Second</h1><p>Second.</p></body></html>"#
                .into(),
        ),
    ])
}

fn build_non_heading_fragment_fixture() -> Vec<u8> {
    build_zip(vec![
        ("mimetype", "application/epub+zip".into()),
        ("META-INF/container.xml", container_xml().into()),
        (
            "OPS/package.opf",
            package(
                r#"<item id="nav" href="nav.xhtml" media-type="application/xhtml+xml" properties="nav"/>
<item id="chapter" href="chapter.xhtml" media-type="application/xhtml+xml"/>"#,
                r#"<itemref idref="chapter"/>"#,
                None,
            ),
        ),
        (
            "OPS/nav.xhtml",
            r#"<html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops"><body><nav epub:type="toc"><ol>
<li><a href="chapter.xhtml#paragraph-target">Publisher Paragraph Target</a></li>
</ol></nav></body></html>"#
                .into(),
        ),
        (
            "OPS/chapter.xhtml",
            r#"<html xmlns="http://www.w3.org/1999/xhtml"><body><h1 id="heading">Visible Heading</h1><p id="paragraph-target">Body.</p></body></html>"#
                .into(),
        ),
    ])
}

fn build_headingless_fixture() -> Vec<u8> {
    build_zip(vec![
        ("mimetype", "application/epub+zip".into()),
        ("META-INF/container.xml", container_xml().into()),
        (
            "OPS/package.opf",
            package(
                r#"<item id="chapter" href="chapter.xhtml" media-type="application/xhtml+xml"/>"#,
                r#"<itemref idref="chapter"/>"#,
                None,
            ),
        ),
        (
            "OPS/chapter.xhtml",
            r#"<html xmlns="http://www.w3.org/1999/xhtml"><head><title>Headingless</title></head><body><p>Headingless body.</p></body></html>"#
                .into(),
        ),
    ])
}

fn build_ncx_fixture() -> Vec<u8> {
    build_zip(vec![
        ("mimetype", "application/epub+zip".into()),
        ("META-INF/container.xml", container_xml().into()),
        (
            "OPS/package.opf",
            package(
                r#"<item id="ncx" href="toc.ncx" media-type="application/x-dtbncx+xml"/>
<item id="chapter" href="chapter.xhtml" media-type="application/xhtml+xml"/>"#,
                r#"<itemref idref="chapter"/>"#,
                Some("ncx"),
            ),
        ),
        (
            "OPS/toc.ncx",
            r#"<ncx xmlns="http://www.daisy.org/z3986/2005/ncx/"><navMap>
<navPoint id="n1"><navLabel><text>NCX Chapter</text></navLabel><content src="chapter.xhtml#chapter"/></navPoint>
</navMap></ncx>"#
                .into(),
        ),
        (
            "OPS/chapter.xhtml",
            r#"<html xmlns="http://www.w3.org/1999/xhtml"><body><h1 id="chapter">Visible Chapter</h1><p>Body.</p></body></html>"#
                .into(),
        ),
    ])
}

fn package(manifest: &str, spine_items: &str, toc: Option<&str>) -> String {
    let toc = toc
        .map(|value| format!(" toc=\"{value}\""))
        .unwrap_or_default();
    format!(
        r#"<?xml version="1.0"?>
<package xmlns="http://www.idpf.org/2007/opf" version="3.0">
<metadata xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:title>Structure Book</dc:title></metadata>
<manifest>{manifest}</manifest><spine{toc}>{spine_items}</spine></package>"#
    )
}

fn container_xml() -> &'static str {
    r#"<?xml version="1.0"?>
<container xmlns="urn:oasis:names:tc:opendocument:xmlns:container"><rootfiles>
<rootfile full-path="OPS/package.opf" media-type="application/oebps-package+xml"/>
</rootfiles></container>"#
}

fn build_zip(entries: Vec<(&str, String)>) -> Vec<u8> {
    let cursor = Cursor::new(Vec::new());
    let mut writer = ZipWriter::new(cursor);
    let options = SimpleFileOptions::default();
    for (name, content) in entries {
        writer.start_file(name, options).expect("ZIP entry");
        writer
            .write_all(content.as_bytes())
            .expect("ZIP content");
    }
    writer.finish().expect("ZIP finish").into_inner()
}
