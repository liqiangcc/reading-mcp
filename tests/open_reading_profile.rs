use std::io::{Cursor, Write};

use reading_mcp::application::ports::{
    DocumentReliabilityInspector, Parser, RetrievedResource,
};
use reading_mcp::application::reading_profile::{
    READING_PROFILE_SCHEMA_VERSION, ReadingCapabilityAvailability, ReliabilityIntegrity,
    ReliabilitySummary, build_reading_profile,
};
use reading_mcp::domain::{DocumentSource, MediaType, TEXT_SEGMENTATION_VERSION};
use reading_mcp::parsing::{
    ArchiveLimits, EPUB_VALIDATION_REPORT_METADATA_KEY, EpubParser, HtmlParser,
    PersistedDocumentReliabilityInspector,
};
use zip::write::{SimpleFileOptions, ZipWriter};

#[tokio::test]
async fn canonical_profile_preserves_native_fallback_and_coarse_evidence() {
    let document = HtmlParser
        .parse(resource(
            "memory:profile.html",
            "text/html",
            br#"<!doctype html><html><body><main>
<h1 id="profile">Profile</h1>
<p>Native sentence.</p>
<blockquote><p>Quoted one.</p><p>Quoted two.</p></blockquote>
<pre>let value = 1;</pre>
<div>Fallback tail.</div>
</main></body></html>"#
                .to_vec(),
        ))
        .await
        .expect("HTML profile fixture should parse");

    let paragraphs = document
        .try_paragraph_text_units()
        .expect("Paragraph coverage should materialize");
    let sentences = document
        .try_sentence_text_units()
        .expect("Sentence coverage should materialize");
    let profile = build_reading_profile(
        &document,
        &paragraphs,
        &sentences,
        ReliabilitySummary::not_applicable(),
        true,
    )
    .expect("canonical profile should satisfy coverage invariants");

    assert_eq!(profile.schema_version, READING_PROFILE_SCHEMA_VERSION);
    assert_eq!(
        profile.capabilities.paragraph_enumeration.availability,
        ReadingCapabilityAvailability::Available
    );
    assert_eq!(
        profile.capabilities.paragraph_enumeration.segmentation_version,
        TEXT_SEGMENTATION_VERSION
    );
    assert!(profile.capabilities.lexical_search.precise_candidates);

    let coverage = profile.canonical_text_coverage;
    assert_eq!(
        coverage.paragraph_chars + coverage.paragraph_separator_chars,
        coverage.owner_chars
    );
    assert_eq!(
        coverage.sentence_eligible_paragraphs + coverage.coarse_paragraphs,
        coverage.paragraph_count
    );
    assert_eq!(
        coverage.sentence_chars
            + coverage.sentence_separator_chars
            + coverage.sentence_coarse_only_chars,
        coverage.paragraph_chars
    );
    assert!(coverage.native_paragraph_chars > 0);
    assert!(coverage.native_structural_container_chars > 0);
    assert!(coverage.native_non_prose_chars > 0);
    assert!(coverage.fallback_chars > 0);
    assert!(coverage.coarse_paragraphs >= 2);
    assert!(coverage.sentence_coarse_only_chars > 0);
    assert!(
        profile
            .capabilities
            .sentence_first_enumeration
            .source_preserving_coarse_regions
    );
}

#[tokio::test]
async fn clean_epub_projects_validator_and_publication_denominator() {
    let document = EpubParser::new(ArchiveLimits::default())
        .parse(resource(
            "memory:clean.epub",
            "application/epub+zip",
            build_clean_epub(),
        ))
        .await
        .expect("clean EPUB should parse");

    let summary = PersistedDocumentReliabilityInspector
        .inspect(&document)
        .expect("persisted EPUB evidence should project");

    assert_eq!(summary.evidence.len(), 1);
    assert_eq!(summary.evidence[0].kind, "epub_structure_validator");
    assert_eq!(summary.evidence[0].integrity, ReliabilityIntegrity::Valid);
    assert_eq!(summary.evidence[0].degradation_count, 0);
    assert!(summary.evidence[0].degradation_codes.is_empty());

    let publication = summary
        .publication_coverage
        .expect("EPUB should expose publication coverage");
    assert_eq!(publication.source_units_total, 1);
    assert_eq!(publication.source_units_represented, 1);
    assert_eq!(publication.source_units_missing, 0);
    assert_eq!(publication.source_units_unsupported, 0);

    let provenance = summary
        .structure_provenance
        .expect("EPUB should expose structure provenance");
    assert_eq!(provenance.native_navigation_sections, 1);
    assert_eq!(provenance.legacy_navigation_sections, 0);

    let navigation = summary
        .navigation_resolution
        .expect("EPUB should expose navigation resolution");
    assert_eq!(navigation.targets_total, 1);
    assert_eq!(navigation.targets_resolved, 1);
    assert_eq!(navigation.targets_unresolved_or_unsupported, 0);
}

#[tokio::test]
async fn degraded_epub_keeps_canonical_readability_and_exposes_source_gap() {
    let document = EpubParser::new(ArchiveLimits::default())
        .parse(resource(
            "memory:degraded.epub",
            "application/epub+zip",
            build_degraded_epub(),
        ))
        .await
        .expect("degraded but readable EPUB should parse");

    let paragraphs = document
        .try_paragraph_text_units()
        .expect("canonical Paragraph coverage should remain readable");
    let sentences = document
        .try_sentence_text_units()
        .expect("canonical Sentence coverage should remain readable");
    let reliability = PersistedDocumentReliabilityInspector
        .inspect(&document)
        .expect("degraded persisted facts should still project");
    let profile = build_reading_profile(&document, &paragraphs, &sentences, reliability, true)
        .expect("degraded source should still have a truthful open profile");

    assert_eq!(
        profile.canonical_text_coverage.paragraph_chars
            + profile.canonical_text_coverage.paragraph_separator_chars,
        profile.canonical_text_coverage.owner_chars
    );

    let publication = profile
        .reliability
        .publication_coverage
        .expect("source publication denominator must remain visible");
    assert_eq!(publication.source_units_total, 2);
    assert_eq!(publication.source_units_represented, 1);
    assert_eq!(publication.source_units_unsupported, 1);

    let evidence = &profile.reliability.evidence[0];
    assert!(evidence.degradation_count >= 4);
    for code in [
        "spine_unsupported_media",
        "navigation_target_missing_fragment",
        "navigation_target_unsupported_resource",
        "navigation_missing_fragment_document_fallback",
    ] {
        assert!(
            evidence.degradation_codes.iter().any(|actual| actual == code),
            "expected degradation code {code:?}"
        );
    }
}

#[tokio::test]
async fn missing_required_epub_reliability_evidence_fails_closed() {
    let mut document = EpubParser::new(ArchiveLimits::default())
        .parse(resource(
            "memory:tampered.epub",
            "application/epub+zip",
            build_clean_epub(),
        ))
        .await
        .expect("clean EPUB should parse before tampering");
    document.metadata.remove(EPUB_VALIDATION_REPORT_METADATA_KEY);

    let error = PersistedDocumentReliabilityInspector
        .inspect(&document)
        .expect_err("required EPUB reliability evidence must not become not_applicable");
    assert!(error.to_string().contains("missing required reliability evidence"));
}

fn resource(source: &str, media_type: &str, bytes: Vec<u8>) -> RetrievedResource {
    RetrievedResource {
        source: DocumentSource(source.into()),
        final_source: DocumentSource(source.into()),
        media_type: MediaType(media_type.into()),
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
  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:title>Profile Clean</dc:title></metadata>
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
<h1 id="start">Visible Chapter</h1><p>One sentence.</p>
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
  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:title>Profile Degraded</dc:title></metadata>
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
