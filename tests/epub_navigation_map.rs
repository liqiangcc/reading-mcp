use std::io::{Cursor, Write};

use reading_mcp::application::ports::{Parser, RetrievedResource};
use reading_mcp::domain::{DocumentSource, MediaType};
use reading_mcp::parsing::{ArchiveLimits, EpubParser};
use serde_json::Value;
use zip::write::{SimpleFileOptions, ZipWriter};

#[tokio::test]
async fn epub3_nav_map_preserves_hierarchy_resolution_and_feeds_canonical_reconciliation() {
    let document = EpubParser::new(ArchiveLimits::default())
        .parse(resource(build_epub3_nav_fixture()))
        .await
        .expect("EPUB 3 nav fixture should parse");

    assert_eq!(
        document
            .metadata
            .get("epub_package_version")
            .map(String::as_str),
        Some("3.0")
    );
    assert_eq!(
        document
            .metadata
            .get("epub_navigation_provenance")
            .map(String::as_str),
        Some("epub_nav")
    );
    assert_eq!(
        document
            .metadata
            .get("epub_navigation_source_path")
            .map(String::as_str),
        Some("OPS/nav/toc.xhtml")
    );
    assert_eq!(
        document
            .metadata
            .get("epub_navigation_nodes")
            .map(String::as_str),
        Some("3")
    );
    assert_eq!(
        document
            .metadata
            .get("epub_navigation_resolved_nodes")
            .map(String::as_str),
        Some("2")
    );

    let map = navigation_map(&document);
    assert_eq!(map["schema_version"], "epub-navigation-map/v1");
    assert_eq!(map["provenance"], "epub_nav");
    let nodes = map["nodes"].as_array().expect("navigation nodes");
    assert_eq!(nodes.len(), 2);
    assert_eq!(nodes[0]["label"], "Publisher Intro");
    assert_eq!(nodes[0]["depth"], 1);
    assert_eq!(nodes[0]["source_order"], 0);
    assert_eq!(nodes[0]["resolved_entry_path"], "OPS/text/ch1.xhtml");
    assert_eq!(nodes[0]["fragment"], "intro");
    assert_eq!(nodes[0]["resolution_status"], "resolved_fragment");
    let children = nodes[0]["children"]
        .as_array()
        .expect("child navigation nodes");
    assert_eq!(children.len(), 1);
    assert_eq!(children[0]["label"], "Publisher Detail");
    assert_eq!(children[0]["depth"], 2);
    assert_eq!(children[0]["source_order"], 1);
    assert_eq!(children[0]["resolution_status"], "resolved_fragment");
    assert_eq!(nodes[1]["source_order"], 2);
    assert_eq!(nodes[1]["resolution_status"], "missing_fragment");

    // The navigation map remains a separate provenance plane, but this follow-up stage now
    // reconciles proven heading targets into the canonical Section hierarchy.
    assert!(
        document
            .root_sections
            .iter()
            .any(|section| section.title == "Publisher Intro")
    );
    assert!(
        !document
            .root_sections
            .iter()
            .any(|section| section.title == "Visible Intro")
    );
}

#[tokio::test]
async fn malformed_epub3_nav_degrades_to_legacy_ncx_with_explicit_provenance() {
    let document = EpubParser::new(ArchiveLimits::default())
        .parse(resource(build_nav_to_ncx_fallback_fixture()))
        .await
        .expect("malformed EPUB 3 nav should degrade to NCX");

    assert_eq!(
        document
            .metadata
            .get("epub_package_version")
            .map(String::as_str),
        Some("3.0")
    );
    assert_eq!(
        document
            .metadata
            .get("epub_navigation_provenance")
            .map(String::as_str),
        Some("epub_ncx")
    );
    let map = navigation_map(&document);
    assert_eq!(map["provenance"], "epub_ncx");
    assert_eq!(map["source_manifest_id"], "ncx");
    assert_eq!(map["nodes"][0]["label"], "Legacy Chapter");
    assert_eq!(map["nodes"][0]["resolution_status"], "resolved_fragment");
    let diagnostics = map["diagnostics"].as_array().expect("diagnostics");
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic["code"] == "malformed_epub_nav")
    );
}

#[tokio::test]
async fn navigation_resolution_exposes_invalid_missing_and_unsupported_targets() {
    let document = EpubParser::new(ArchiveLimits::default())
        .parse(resource(build_resolution_diagnostics_fixture()))
        .await
        .expect("EPUB with degraded TOC targets should remain readable");
    let map = navigation_map(&document);
    let nodes = map["nodes"].as_array().expect("nodes");
    let statuses = nodes
        .iter()
        .map(|node| node["resolution_status"].as_str().expect("status"))
        .collect::<Vec<_>>();
    assert_eq!(
        statuses,
        vec!["invalid_path", "missing_resource", "unsupported_resource"]
    );
    assert_eq!(
        document
            .metadata
            .get("epub_navigation_resolved_nodes")
            .map(String::as_str),
        Some("0")
    );
}

#[tokio::test]
async fn epub_without_nav_or_ncx_remains_readable_and_records_navigation_degradation() {
    let document = EpubParser::new(ArchiveLimits::default())
        .parse(resource(build_no_navigation_fixture()))
        .await
        .expect("heading/spine fallback EPUB should remain readable");
    assert_eq!(
        document
            .metadata
            .get("epub_navigation_provenance")
            .map(String::as_str),
        Some("none")
    );
    assert_eq!(
        document
            .metadata
            .get("epub_navigation_nodes")
            .map(String::as_str),
        Some("0")
    );
    let map = navigation_map(&document);
    assert!(
        map["diagnostics"]
            .as_array()
            .expect("diagnostics")
            .iter()
            .any(|diagnostic| diagnostic["code"] == "navigation_unavailable")
    );
    assert!(
        document
            .root_sections
            .iter()
            .any(|section| section.title == "Fallback Heading")
    );
}

fn navigation_map(document: &reading_mcp::domain::Document) -> Value {
    serde_json::from_str(
        document
            .metadata
            .get("epub_navigation_map")
            .expect("navigation map metadata"),
    )
    .expect("navigation map JSON")
}

fn resource(bytes: Vec<u8>) -> RetrievedResource {
    RetrievedResource {
        source: DocumentSource("memory:navigation.epub".into()),
        final_source: DocumentSource("memory:navigation.epub".into()),
        media_type: MediaType("application/epub+zip".into()),
        bytes,
        etag: None,
        last_modified: None,
        metadata: Default::default(),
    }
}

fn build_epub3_nav_fixture() -> Vec<u8> {
    build_zip(&[
        ("mimetype", "application/epub+zip"),
        ("META-INF/container.xml", container_xml()),
        (
            "OPS/package.opf",
            r#"<?xml version="1.0"?>
<package xmlns="http://www.idpf.org/2007/opf" version="3.0">
  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:title>Nav Book</dc:title></metadata>
  <manifest>
    <item id="nav" href="nav/toc.xhtml" media-type="application/xhtml+xml" properties="nav scripted"/>
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
  <li><a href="../text/ch1.xhtml#intro">Publisher Intro</a><ol>
    <li><a href="../text/ch1.xhtml#detail">Publisher Detail</a></li>
  </ol></li>
  <li><a href="../text/ch2.xhtml#missing">Publisher Second</a></li>
</ol></nav>
</body></html>"#,
        ),
        (
            "OPS/text/ch1.xhtml",
            r#"<html xmlns="http://www.w3.org/1999/xhtml"><body><h1 id="intro">Visible Intro</h1><p>Opening.</p><h2 id="detail">Visible Detail</h2><p>Detail.</p></body></html>"#,
        ),
        (
            "OPS/text/ch2.xhtml",
            r#"<html xmlns="http://www.w3.org/1999/xhtml"><body><h1 id="second">Visible Second</h1><p>Second.</p></body></html>"#,
        ),
    ])
}

fn build_nav_to_ncx_fallback_fixture() -> Vec<u8> {
    build_zip(&[
        ("mimetype", "application/epub+zip"),
        ("META-INF/container.xml", container_xml()),
        (
            "OPS/package.opf",
            r#"<?xml version="1.0"?>
<package xmlns="http://www.idpf.org/2007/opf" version="3.0">
  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:title>Fallback Book</dc:title></metadata>
  <manifest>
    <item id="nav" href="nav.xhtml" media-type="application/xhtml+xml" properties="nav"/>
    <item id="ncx" href="toc.ncx" media-type="application/x-dtbncx+xml"/>
    <item id="chapter" href="chapter.xhtml" media-type="application/xhtml+xml"/>
  </manifest>
  <spine toc="ncx"><itemref idref="chapter"/></spine>
</package>"#,
        ),
        ("OPS/nav.xhtml", "<html><body><nav"),
        (
            "OPS/toc.ncx",
            r#"<?xml version="1.0"?>
<ncx xmlns="http://www.daisy.org/z3986/2005/ncx/"><navMap>
<navPoint id="n1"><navLabel><text>Legacy Chapter</text></navLabel><content src="chapter.xhtml#start"/></navPoint>
</navMap></ncx>"#,
        ),
        (
            "OPS/chapter.xhtml",
            r#"<html xmlns="http://www.w3.org/1999/xhtml"><body><h1 id="start">Visible Chapter</h1><p>Body.</p></body></html>"#,
        ),
    ])
}

fn build_resolution_diagnostics_fixture() -> Vec<u8> {
    build_zip(&[
        ("mimetype", "application/epub+zip"),
        ("META-INF/container.xml", container_xml()),
        (
            "OPS/package.opf",
            r#"<?xml version="1.0"?>
<package xmlns="http://www.idpf.org/2007/opf" version="3.0">
  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:title>Diagnostics</dc:title></metadata>
  <manifest>
    <item id="nav" href="nav.xhtml" media-type="application/xhtml+xml" properties="nav"/>
    <item id="chapter" href="chapter.xhtml" media-type="application/xhtml+xml"/>
    <item id="svg" href="figure.svg" media-type="image/svg+xml"/>
  </manifest>
  <spine><itemref idref="chapter"/></spine>
</package>"#,
        ),
        (
            "OPS/nav.xhtml",
            r#"<html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops"><body><nav epub:type="toc"><ol>
<li><a href="../../escape.xhtml">Escape</a></li>
<li><a href="missing.xhtml">Missing</a></li>
<li><a href="figure.svg#shape">Figure</a></li>
</ol></nav></body></html>"#,
        ),
        (
            "OPS/chapter.xhtml",
            r#"<html xmlns="http://www.w3.org/1999/xhtml"><body><h1>Readable</h1><p>Body.</p></body></html>"#,
        ),
        (
            "OPS/figure.svg",
            r#"<svg xmlns="http://www.w3.org/2000/svg"><g id="shape"/></svg>"#,
        ),
    ])
}

fn build_no_navigation_fixture() -> Vec<u8> {
    build_zip(&[
        ("mimetype", "application/epub+zip"),
        ("META-INF/container.xml", container_xml()),
        (
            "OPS/package.opf",
            r#"<?xml version="1.0"?>
<package xmlns="http://www.idpf.org/2007/opf" version="3.0">
  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:title>No Nav</dc:title></metadata>
  <manifest><item id="chapter" href="chapter.xhtml" media-type="application/xhtml+xml"/></manifest>
  <spine><itemref idref="chapter"/></spine>
</package>"#,
        ),
        (
            "OPS/chapter.xhtml",
            r#"<html xmlns="http://www.w3.org/1999/xhtml"><body><h1>Fallback Heading</h1><p>Body.</p></body></html>"#,
        ),
    ])
}

fn container_xml() -> &'static str {
    r#"<?xml version="1.0"?>
<container xmlns="urn:oasis:names:tc:opendocument:xmlns:container"><rootfiles>
<rootfile full-path="OPS/package.opf" media-type="application/oebps-package+xml"/>
</rootfiles></container>"#
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
