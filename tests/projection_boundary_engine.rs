//! Issue #91 canonical projection report exporter.
//!
//! Exercises the *current production* PDF path — `LayoutPdfParser` assembled
//! exactly as the release runtime does when no OCR identity is configured
//! (`router.pdf = LayoutPdfParser` inside CachingParser/BudgetedParser) — with
//! local OCR disabled, plus the production `EpubParser` control.  Emits a JSON
//! report consumed by `scripts/projection_quality.py`; the scorer owns all
//! gold comparisons so this file only records canonical facts.
//!
//! Hosted-only: requires the pinned layout Python (READING_MCP_PDF_LAYOUT_PYTHON).

use reading_mcp::application::ports::{
    DocumentReliabilityInspector, DocumentRepository, Parser, RetrievedResource,
};
use reading_mcp::application::read_document::{ReadDocumentUseCase, ReadExactTargetCommand};
use reading_mcp::domain::{
    Document, DocumentSource, MediaType, NormalizedTextRange, ParagraphTextUnitSet,
    SentenceEligibility, SentenceTextUnitSet, TextLocator, TextUnitId,
};
use reading_mcp::infrastructure::{ResourceBudget, SqliteDocumentRepository};
use reading_mcp::parsing::{
    ArchiveLimits, EpubParser, LayoutPdfParser, PersistedDocumentReliabilityInspector,
};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{path::PathBuf, sync::Arc};

const PDF_CASES: &[&str] = &[
    "P01-single-column",
    "P02-double-column",
    "P03-cross-page",
    "P04-dehyphenation",
    "P05-whitespace-loss",
    "P06-token-stress",
    "P07-front-matter",
    "P08-preserved-hyphen",
];
const EPUB_CASES: &[&str] = &["P09-epub-control"];
const DETERMINISM_RUNS: usize = 3;

fn resource(case: &str, media: MediaType, bytes: Vec<u8>) -> RetrievedResource {
    let source = DocumentSource(format!(
        "https://fixtures.invalid/projection-quality/{case}"
    ));
    RetrievedResource {
        source: source.clone(),
        final_source: source,
        media_type: media,
        bytes,
        etag: None,
        last_modified: None,
        metadata: Default::default(),
    }
}

/// Deterministic fingerprint over canonical paragraph/sentence unit facts
/// (identity, normalized ranges, text). `TextUnit`/`SentenceTextUnit` are not
/// `Serialize`; this digest gives the scorer cross-run unit equality evidence
/// without reimplementing any projection logic.
fn units_fingerprint(paragraphs: &ParagraphTextUnitSet, sentences: &SentenceTextUnitSet) -> String {
    let mut hasher = Sha256::new();
    let mut feed = |id: &TextUnitId, range: NormalizedTextRange, text: &str| {
        hasher.update(id.as_ref().as_bytes());
        hasher.update(u64::try_from(range.start()).unwrap().to_be_bytes());
        hasher.update(u64::try_from(range.end()).unwrap().to_be_bytes());
        hasher.update(text.as_bytes());
    };
    for unit in &paragraphs.units {
        feed(&unit.id, unit.normalized_range, &unit.text);
    }
    for unit in &sentences.units {
        feed(&unit.id, unit.normalized_range, &unit.text);
    }
    format!("sha256:{:x}", hasher.finalize())
}

async fn units_report(
    case: &str,
    document: &Document,
    directory: &std::path::Path,
) -> serde_json::Value {
    let paragraphs = document.try_paragraph_text_units().unwrap();
    let sentences = document.try_sentence_text_units().unwrap();
    let reliability = PersistedDocumentReliabilityInspector
        .inspect(document)
        .unwrap();
    let degradation_codes: Vec<_> = reliability
        .evidence
        .iter()
        .flat_map(|evidence| evidence.degradation_codes.iter().cloned())
        .collect();
    assert!(
        !paragraphs.units.is_empty(),
        "{case}: canonical parse produced no paragraphs"
    );
    let database = directory.join(format!("{case}.sqlite"));
    {
        let repository = SqliteDocumentRepository::open(&database).unwrap();
        repository.save(document.clone()).await.unwrap();
    }
    let repository = Arc::new(SqliteDocumentRepository::open(&database).unwrap());
    let restored = repository.get(&document.id).await.unwrap().unwrap();
    let reader = ReadDocumentUseCase::new(repository);
    let mut exact_reads = 0usize;
    let mut equal_reads = 0usize;
    let units: Vec<_> = paragraphs
        .units
        .iter()
        .map(|paragraph| {
            let section = document.find_section(&paragraph.owner_section_id).unwrap();
            let coverage = sentences
                .coverage
                .iter()
                .find(|c| c.paragraph_id == paragraph.id)
                .unwrap();
            let page = restored
                .original_source_target_for_range(
                    &paragraph.owner_section_id,
                    paragraph.normalized_range,
                )
                .unwrap()
                .map(|target| serde_json::to_value(target).unwrap());
            let boundaries: Vec<_> = sentences
                .units
                .iter()
                .filter(|s| s.parent_paragraph_id == paragraph.id)
                .map(|s| {
                    json!({"text": s.text,
                        "start": s.normalized_range.start() - paragraph.normalized_range.start(),
                        "end": s.normalized_range.end() - paragraph.normalized_range.start()})
                })
                .collect();
            let locator = TextLocator::for_paragraph(document, section, paragraph);
            let mut targets = vec![(locator, paragraph.text.as_str())];
            targets.extend(
                sentences
                    .units
                    .iter()
                    .filter(|s| s.parent_paragraph_id == paragraph.id)
                    .map(|s| {
                        (
                            TextLocator::for_sentence(document, section, s),
                            s.text.as_str(),
                        )
                    }),
            );
            // Exact reads are awaited by the caller; serde_json values cannot
            // hold futures, so perform them in a nested async block.
            let reads = async {
                let mut total = 0usize;
                let mut equal = 0usize;
                for (locator, text) in targets {
                    let result = reader
                        .read_exact(ReadExactTargetCommand {
                            document_id: document.id.clone(),
                            target_locator: locator.clone(),
                            max_chars: Some(8192),
                        })
                        .await
                        .unwrap();
                    total += 1;
                    if result.complete && result.content == text {
                        equal += 1;
                    }
                }
                (total, equal)
            };
            (paragraph, coverage, page, boundaries, reads)
        })
        .collect();
    // Resolve the per-paragraph async read blocks in order.
    let mut unit_json = Vec::new();
    for (paragraph, coverage, page, boundaries, reads) in units {
        let (total, equal) = reads.await;
        exact_reads += total;
        equal_reads += equal;
        unit_json.push(json!({
            "section": document.find_section(&paragraph.owner_section_id).unwrap().title,
            "text": paragraph.text,
            "source_order": paragraph.source_order,
            "eligible": coverage.eligibility == SentenceEligibility::Eligible,
            "content_class": coverage.content_class.as_str(),
            "exact_read": total == equal,
            "page": page,
            "sentences": boundaries,
        }));
    }
    json!({
        "content_hash": document.content_hash.0,
        "normalized_hash": document.normalized_document_hash().0,
        "degradation_codes": degradation_codes,
        "paragraphs": unit_json,
        "exact_reads": {"total": exact_reads, "equal": equal_reads},
    })
}

#[tokio::test]
#[ignore = "requires pinned hosted layout dependencies"]
async fn export_projection_quality_canonical_report() {
    let directory = tempfile::tempdir().unwrap();
    let python: PathBuf = std::env::var("READING_MCP_PDF_LAYOUT_PYTHON")
        .expect("pinned layout python")
        .into();
    let mut report = serde_json::Map::new();
    for case in PDF_CASES {
        // Production-equivalent assembly with OCR disabled: LayoutPdfParser
        // without any OCR config/identity is exactly the release native path.
        let parser = LayoutPdfParser::new(python.clone(), ResourceBudget::default());
        let bytes = std::fs::read(format!("tests/projection_quality/pdf/{case}.pdf")).unwrap();
        let source = format!("https://fixtures.invalid/projection-quality/{case}.pdf");
        let mut normalized_hashes = Vec::new();
        let mut unit_fingerprints = Vec::new();
        let mut parsed_document = None;
        for _ in 0..DETERMINISM_RUNS {
            let document = parser
                .parse(resource(
                    case,
                    MediaType("application/pdf".into()),
                    bytes.clone(),
                ))
                .await
                .unwrap_or_else(|error| panic!("{case}: canonical parse failed: {error:?}"));
            normalized_hashes.push(document.normalized_document_hash().0.clone());
            unit_fingerprints.push(units_fingerprint(
                &document.try_paragraph_text_units().unwrap(),
                &document.try_sentence_text_units().unwrap(),
            ));
            parsed_document = Some(document);
        }
        assert_eq!(
            normalized_hashes
                .iter()
                .collect::<std::collections::BTreeSet<_>>()
                .len(),
            1,
            "{case}: three canonical parses disagree on normalized hash"
        );
        assert_eq!(
            unit_fingerprints
                .iter()
                .collect::<std::collections::BTreeSet<_>>()
                .len(),
            1,
            "{case}: three canonical parses disagree on unit/range facts"
        );
        let document = parsed_document.unwrap();
        let mut entry = units_report(case, &document, directory.path()).await;
        entry["document_source"] = json!(source);
        entry["normalized_hashes"] = json!(normalized_hashes);
        entry["unit_fingerprints"] = json!(unit_fingerprints);
        // The #91 gate must observe the non-OCR path explicitly.
        let codes: Vec<_> = entry["degradation_codes"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|v| v.as_str())
            .collect();
        assert!(
            codes.contains(&"pdf_local_ocr_not_applied"),
            "{case}: local OCR must not have run"
        );
        report.insert(case.to_string(), entry);
    }
    for case in EPUB_CASES {
        let parser = EpubParser::new(ArchiveLimits::default());
        let bytes = std::fs::read(format!("tests/projection_quality/epub/{case}.epub")).unwrap();
        let source = format!("https://fixtures.invalid/projection-quality/{case}.epub");
        let mut normalized_hashes = Vec::new();
        let mut unit_fingerprints = Vec::new();
        let mut parsed_document = None;
        for _ in 0..DETERMINISM_RUNS {
            let document = parser
                .parse(resource(
                    case,
                    MediaType("application/epub+zip".into()),
                    bytes.clone(),
                ))
                .await
                .unwrap_or_else(|error| panic!("{case}: epub parse failed: {error:?}"));
            normalized_hashes.push(document.normalized_document_hash().0.clone());
            unit_fingerprints.push(units_fingerprint(
                &document.try_paragraph_text_units().unwrap(),
                &document.try_sentence_text_units().unwrap(),
            ));
            parsed_document = Some(document);
        }
        assert_eq!(
            normalized_hashes
                .iter()
                .collect::<std::collections::BTreeSet<_>>()
                .len(),
            1,
            "{case}: epub parses disagree"
        );
        assert_eq!(
            unit_fingerprints
                .iter()
                .collect::<std::collections::BTreeSet<_>>()
                .len(),
            1,
            "{case}: epub unit facts disagree"
        );
        let document = parsed_document.unwrap();
        let mut entry = units_report(case, &document, directory.path()).await;
        entry["document_source"] = json!(source);
        entry["normalized_hashes"] = json!(normalized_hashes);
        entry["unit_fingerprints"] = json!(unit_fingerprints);
        report.insert(case.to_string(), entry);
    }
    let output = PathBuf::from(
        std::env::var("READING_MCP_PROJECTION_REPORT").expect("READING_MCP_PROJECTION_REPORT"),
    );
    std::fs::create_dir_all(output.parent().unwrap()).unwrap();
    let encoded = serde_json::to_string_pretty(&json!({
        "schema": "projection-quality-report/v1",
        "cases": report,
    }))
    .unwrap();
    std::fs::write(&output, &encoded).unwrap();
    println!("{encoded}");
}
