use std::io::{Cursor, Write};

use reading_mcp::application::ports::{DocumentRepository, Parser, RetrievedResource};
use reading_mcp::domain::{DocumentSource, MediaType};
use reading_mcp::infrastructure::SqliteDocumentRepository;
use reading_mcp::parsing::{
    ArchiveLimits, EPUB_VALIDATION_REPORT_METADATA_KEY, EPUB_VALIDATION_REPORT_VERSION,
    EpubParser, EpubValidationIntegrity, EpubValidationReport, EpubValidationSeverity,
    validate_epub_document,
};
use tempfile::tempdir;
use zip::write::{SimpleFileOptions, ZipWriter};

#[tokio::test]
async fn clean_epub_persists_valid_zero_degradation_report_with_factual_coverage() {
    let document = EpubParser::new(ArchiveLimits::default())
        .parse(resource(build_clean_epub()))
        .await
        .expect("clean EPUB should parse and validate");

    let report = validation_report(&document);
    assert_eq!(report.schema_version, EPUB_VALIDATION_REPORT_VERSION);
    assert_eq!(report.integrity, EpubValidationIntegrity::Valid);
    assert_eq!(report.error_count, 0);
    assert_eq!(report.degradation_count, 0);

    assert_eq!(report.coverage.package_spine.manifest_items_total, 2);
    assert_eq!(report.coverage.package_spine.spine_items_total, 1);
    assert_eq!(report.coverage.package_spine.spine_items_parsed, 1);
    assert_eq!(report.coverage.navigation.nodes_total, 1);
    assert_eq!(report.coverage.navigation.resolved_fragment, 1);
    assert_eq!(report.coverage.navigation.fragment_targets_resolved, 1);
    assert_eq!(report.coverage.structure.sections_total, 1);
    assert_eq!(report.coverage.structure.sections_epub_nav, 1);
    assert_eq!(report.coverage.blocks.blocks_total, 1);
    assert_eq!(report.coverage.blocks.paragraph_blocks, 1);
    assert_eq!(report.coverage.blocks.blocks_with_exact_paragraph_match, 1);
    assert_eq!(report.coverage.text_units.paragraph_units, 1);
    assert_eq!(report.coverage.text_units.sentence_units, 1);

    assert_eq!(
        document
            .metadata
            .get("epub_validation_integrity")
            .map(String::as_str),
        Some("valid")
    );
    assert_eq!(
        document
            .metadata
            .get("epub_validation_errors")
            .map(String::as_str),
        Some("0")
    );
}

#[tokio::test]
async fn source_gaps_are_degradations_not_integrity_errors() {
    let document = EpubParser::new(ArchiveLimits::default())
        .parse(resource(build_degraded_epub()))
        .await
        .expect("degraded but readable EPUB should parse");
    let report = validation_report(&document);

    assert_eq!(report.integrity, EpubValidationIntegrity::Valid);
    assert_eq!(report.error_count, 0);
    assert!(report.degradation_count >= 4);
    assert_eq!(report.coverage.package_spine.spine_items_total, 2);
    assert_eq!(report.coverage.package_spine.spine_items_parsed, 1);
    assert_eq!(report.coverage.package_spine.spine_items_unsupported_media, 1);
    assert_eq!(report.coverage.navigation.nodes_total, 2);
    assert_eq!(report.coverage.navigation.missing_fragment, 1);
    assert_eq!(report.coverage.navigation.unsupported_resource, 1);
    assert_eq!(report.coverage.structure.applied_navigation_nodes, 1);

    for code in [
        "spine_unsupported_media",
        "navigation_target_missing_fragment",
        "navigation_target_unsupported_resource",
        "navigation_missing_fragment_document_fallback",
    ] {
        assert!(
            report.findings.iter().any(|finding| {
                finding.severity == EpubValidationSeverity::Degradation && finding.code == code
            }),
            "expected degradation {code:?}"
        );
    }
}

#[tokio::test]
async fn validator_detects_tampered_persisted_summary_without_reparsing_source() {
    let mut document = EpubParser::new(ArchiveLimits::default())
        .parse(resource(build_clean_epub()))
        .await
        .expect("clean EPUB should parse");
    document
        .metadata
        .insert("epub_structure_sections".into(), "99".into());

    let report = validate_epub_document(&document);
    assert_eq!(report.integrity, EpubValidationIntegrity::Invalid);
    assert!(report.error_count > 0);
    assert!(report.findings.iter().any(|finding| {
        finding.severity == EpubValidationSeverity::Error
            && finding.code == "summary_count_mismatch"
            && finding.plane == "structure"
    }));
}

#[tokio::test]
async fn validation_report_and_revalidation_survive_sqlite_repository_reopen() {
    let document = EpubParser::new(ArchiveLimits::default())
        .parse(resource(build_clean_epub()))
        .await
        .expect("clean EPUB should parse");
    let expected_report = validation_report(&document);

    let directory = tempdir().expect("temporary directory");
    let path = directory.path().join("state.sqlite");
    let repository = SqliteDocumentRepository::open(&path).expect("repository should open");
    repository
        .save(document.clone())
        .await
        .expect("EPUB Document should persist");
    drop(repository);

    let reopened = SqliteDocumentRepository::open(&path).expect("repository should reopen");
    let restored = reopened
        .get(&document.id)
        .await
        .expect("repository read should succeed")
        .expect("EPUB Document should exist");

    assert_eq!(validation_report(&restored), expected_report);
    assert_eq!(validate_epub_document(&restored), expected_report);
}

fn validation_report(document: &reading_mcp::domain::Document) -> EpubValidationReport {
    serde_json::from_str(
        document
            .metadata
            .get(EPUB_VALIDATION_REPORT_METADATA_KEY)
            .expect("validation report metadata should exist"),
    )
    .expect("validation report JSON should decode")
}

fn resource(bytes: Vec<u8>) -> RetrievedResource {
    RetrievedResource {
        source: DocumentSource("memory:validator.epub".into()),
        final_source: DocumentSource("memory:validator.epub".into()),
        media_type: MediaType("application/epub+zip".into()),
        bytes,
        etag: None,
        last_modified: None,
        metadata: Default::default(),
    }
}

fn build_clean_epub() -> Vec<u8> {
    build_zip(&[
        ("mimetype", "application/epub+zip"),
        ("META-INF/container.xml", container_xml()),
        (
            "OPS/package.opf",
            r#"<?xml version="1.0"?>
<package xmlns="http://www.idpf.org/2007/opf" version="3.0">
  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:title>Validator Clean</dc:title></metadata>
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
<nav epub:type="toc"><ol><li><a href="chapter.xhtml#start">Publisher Chapter</a></li></ol></nav>
</body></html>"#,
        ),
        (
            "OPS/chapter.xhtml",
            r#"<html xmlns="http://www.w3.org/1999/xhtml"><body>
<h1 id="start">Visible Chapter</h1><p id="p1">One sentence.</p>
</body></html>"#,
        ),
    ])
}

fn build_degraded_epub() -> Vec<u8> {
    build_zip(&[
        ("mimetype", "application/epub+zip"),
        ("META-INF/container.xml", container_xml()),
        (
            "OPS/package.opf",
            r#"<?xml version="1.0"?>
<package xmlns="http://www.idpf.org/2007/opf" version="3.0">
  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:title>Validator Degraded</dc:title></metadata>
  <manifest>
    <item id="nav" href="nav.xhtml" media-type="application/xhtml+xml" properties="nav"/>
    <item id="chapter" href="chapter.xhtml" media-type="application/xhtml+xml"/>
    <item id="figure" href="figure.svg" media-type="image/svg+xml"/>
  </manifest>
  <spine><itemref idref="chapter"/><itemref idref="figure" linear="no"/></spine>
</package>"#,
        ),
        (
            "OPS/nav.xhtml",
            r#"<html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops"><body>
<nav epub:type="toc"><ol>
<li><a href="chapter.xhtml#missing">Missing Fragment</a></li>
<li><a href="figure.svg#shape">Figure</a></li>
</ol></nav>
</body></html>"#,
        ),
        (
            "OPS/chapter.xhtml",
            r#"<html xmlns="http://www.w3.org/1999/xhtml"><body>
<h1 id="start">Visible Chapter</h1><p>Readable body.</p>
</body></html>"#,
        ),
        (
            "OPS/figure.svg",
            r#"<svg xmlns="http://www.w3.org/2000/svg"><g id="shape"/></svg>"#,
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
