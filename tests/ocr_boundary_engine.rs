use reading_mcp::application::cite_reading::CiteReadingUseCase;
use reading_mcp::application::get_text_units::RequestedTextUnitKind;
use reading_mcp::application::ports::{
    DocumentReliabilityInspector, DocumentRepository, Parser, RetrievedResource,
};
use reading_mcp::application::read_reading::{ReadReadingCommand, ReadReadingUseCase};
use reading_mcp::application::short_handle::{
    DocumentHandle, SectionHandle, handle_checksum, paragraph_handle, sentence_handle,
};
use reading_mcp::application::text_unit_store::store_text_units;
use reading_mcp::domain::{
    Document, DocumentSource, MediaType, OcrConfig, OriginalSourceTarget, SentenceEligibility,
    TextLocator,
};
use reading_mcp::infrastructure::{
    FileOcrEvidenceStore, InMemoryTextUnitIndex, ResourceBudget, SqliteDocumentRepository,
    build_ocr_runtime_identity,
};
use reading_mcp::parsing::{LayoutPdfParser, PersistedDocumentReliabilityInspector};
use serde_json::json;
use std::{path::PathBuf, sync::Arc, time::Duration};

#[tokio::test]
#[ignore = "requires pinned hosted OCR dependencies"]
async fn export_real_canonical_boundaries_without_gold_input() {
    let directory = tempfile::tempdir().unwrap();
    let store = Arc::new(FileOcrEvidenceStore::new(directory.path().join("evidence")));
    let mut report = serde_json::Map::new();
    let mut failed = false;
    for case in [
        "F01", "F02", "F03", "F04-form", "F04-flat", "F05", "F06", "F07", "F08", "F12", "F13",
        "F14",
    ] {
        let config = OcrConfig {
            enabled: true,
            engine_path: "/usr/bin/tesseract".into(),
            tessdata_path: "/usr/share/tesseract-ocr/5/tessdata".into(),
            languages: match case {
                "F07" => vec!["chi_sim".into()],
                "F08" => vec!["eng".into(), "chi_sim".into()],
                _ => vec!["eng".into()],
            },
            operator_revision: "1".into(),
            dpi: 300,
            oem: 1,
            psm: 3,
            detector_version: "pdf-layout/v1".into(),
            protocol_version: "pdf-layout/v1".into(),
        };
        let identity = build_ocr_runtime_identity(config.clone()).unwrap();
        let parser = LayoutPdfParser::new(
            std::env::var("READING_MCP_PDF_LAYOUT_PYTHON")
                .unwrap()
                .into(),
            ResourceBudget::default(),
        )
        .with_ocr_config(config)
        .with_ocr_identity(identity)
        .with_evidence_store(store.clone());
        let source = DocumentSource(format!("file:///frozen/{case}.pdf"));
        let resource = RetrievedResource {
            source: source.clone(),
            final_source: source,
            media_type: MediaType("application/pdf".into()),
            bytes: std::fs::read(format!("tests/fixtures/scanned_pdf/pdf/{case}.pdf")).unwrap(),
            etag: None,
            last_modified: None,
            metadata: Default::default(),
        };
        let document =
            match tokio::time::timeout(Duration::from_secs(60), parser.parse(resource)).await {
                Ok(Ok(document)) => document,
                error => {
                    report.insert(case.into(), json!({"error":format!("{error:?}")}));
                    failed = true;
                    continue;
                }
            };
        let paragraphs = document.try_paragraph_text_units().unwrap();
        let reliability = PersistedDocumentReliabilityInspector
            .inspect(&document)
            .unwrap();
        let expected_code = if matches!(case, "F01" | "F03" | "F04-form" | "F04-flat") {
            "pdf_local_ocr_not_applied"
        } else {
            "pdf_local_ocr_unverified"
        };
        assert!(
            reliability.evidence.iter().any(|evidence| evidence
                .degradation_codes
                .iter()
                .any(|code| code == expected_code)),
            "{case}"
        );
        let mut invalid = document.clone();
        invalid.metadata.insert("ocr_derivation".into(), "{".into());
        assert!(
            PersistedDocumentReliabilityInspector
                .inspect(&invalid)
                .is_err()
        );
        let sentences = document.try_sentence_text_units().unwrap();
        assert!(!paragraphs.units.is_empty());
        assert!(!sentences.units.is_empty());
        let database = directory.path().join(format!("{case}.sqlite"));
        {
            let repository = SqliteDocumentRepository::open(&database).unwrap();
            repository.save(document.clone()).await.unwrap();
        }
        let repository = Arc::new(SqliteDocumentRepository::open(&database).unwrap());
        let restored = repository.get(&document.id).await.unwrap().unwrap();
        assert_eq!(restored.try_paragraph_text_units().unwrap(), paragraphs);
        assert_eq!(restored.try_sentence_text_units().unwrap(), sentences);
        let units_index: Arc<InMemoryTextUnitIndex> = Arc::new(InMemoryTextUnitIndex::default());
        store_text_units(units_index.as_ref(), &restored)
            .await
            .expect("text units should store");
        let units: Arc<dyn reading_mcp::application::ports::TextUnitIndex> = units_index;
        let mut exact_reads = 0;
        for (index, paragraph) in paragraphs.units.iter().enumerate() {
            // Frozen F05 has two paragraphs on each of pages 1, 2 and 3;
            // page 4 is blank. Every other fixture here has one original page.
            // This is an acceptance expectation, never an input to OCR.
            let expected_page = if case == "F05" {
                (index / 2 + 1) as u32
            } else {
                1
            };
            let section = document.find_section(&paragraph.owner_section_id).unwrap();
            let mut targets = vec![(
                TextLocator::for_paragraph(&document, section, paragraph),
                paragraph.text.as_str(),
            )];
            targets.extend(
                sentences
                    .units
                    .iter()
                    .filter(|sentence| sentence.parent_paragraph_id == paragraph.id)
                    .map(|sentence| {
                        (
                            TextLocator::for_sentence(&document, section, sentence),
                            sentence.text.as_str(),
                        )
                    }),
            );
            for (locator, text) in targets {
                assert_eq!(
                    restored
                        .original_source_target_for_range(
                            &locator.owner_section_id,
                            locator.normalized_range.unwrap()
                        )
                        .unwrap(),
                    Some(OriginalSourceTarget::Page {
                        page_number: expected_page
                    }),
                    "{case}"
                );
                let from = handle_for_locator(&units, &restored, &locator).await;
                let result = read_handle(repository.clone(), units.clone(), &from).await;
                assert_eq!(result.items[0].text, text, "{case}");
                let cite = CiteReadingUseCase::new(repository.clone(), units.clone())
                    .execute(&[result.items[0].handle.clone()])
                    .await
                    .expect("cite");
                assert_eq!(cite.items[0].locator, Some(locator.clone()), "{case}");
                exact_reads += 1;
            }
        }
        assert_eq!(exact_reads, paragraphs.units.len() + sentences.units.len());
        let units: Vec<_> = paragraphs
            .units
            .iter()
            .map(|p| {
                let coverage = sentences
                    .coverage
                    .iter()
                    .find(|c| c.paragraph_id == p.id)
                    .unwrap();
                let boundaries: Vec<_> = sentences
                    .units
                    .iter()
                    .filter(|s| s.parent_paragraph_id == p.id)
                    .map(|s| {
                        json!({"text":s.text,
                    "start":s.normalized_range.start() - p.normalized_range.start(),
                    "end":s.normalized_range.end() - p.normalized_range.start()})
                    })
                    .collect();
                json!({"text":p.text, "source_order":p.source_order,
                "eligible":coverage.eligibility == SentenceEligibility::Eligible,
                "content_class":coverage.content_class.as_str(), "sentences":boundaries})
            })
            .collect();
        report.insert(
            case.into(),
            json!({"content_hash":document.content_hash.0,
            "normalized_hash":document.normalized_document_hash().0, "paragraphs":units,
            "sqlite_reopen_exact_reads":exact_reads, "original_page_binding_checks":exact_reads}),
        );
    }
    let output = PathBuf::from(std::env::var("READING_MCP_OCR_BOUNDARY_REPORT").unwrap());
    std::fs::create_dir_all(output.parent().unwrap()).unwrap();
    let encoded = serde_json::to_string_pretty(&report).unwrap();
    std::fs::write(output, &encoded).unwrap();
    println!("{encoded}");
    assert!(
        !failed,
        "one or more real canonical exports failed; see public report"
    );
}

// A `TextLocator` resolves to its v2 read handle; reading from the handle returns the unit's text
// and `cite` on the returned item resolves the same locator back.
async fn handle_for_locator(
    units: &Arc<dyn reading_mcp::application::ports::TextUnitIndex>,
    document: &Document,
    locator: &TextLocator,
) -> String {
    let section = SectionHandle {
        document: DocumentHandle {
            doc_key: units
                .document_key(&document.id)
                .await
                .expect("document key")
                .expect("stored document"),
            checksum: handle_checksum(
                &document.id,
                &document.normalized_document_hash(),
                &document.media_type,
            ),
        },
        section_key: units
            .section_key(&document.id, &locator.owner_section_id)
            .await
            .expect("section key")
            .expect("stored section"),
    };
    match (locator.paragraph_index, locator.sentence_index) {
        (Some(paragraph), Some(sentence)) => sentence_handle(&section, paragraph, sentence),
        (Some(paragraph), None) => paragraph_handle(&section, paragraph),
        _ => section.to_string(),
    }
}

async fn read_handle(
    repository: Arc<dyn reading_mcp::application::ports::DocumentRepository>,
    units: Arc<dyn reading_mcp::application::ports::TextUnitIndex>,
    from: &str,
) -> reading_mcp::application::read_reading::ReadReadingResult {
    ReadReadingUseCase::new(repository, units)
        .execute(ReadReadingCommand {
            from: from.to_string(),
            unit: RequestedTextUnitKind::Sentence,
            max_chars: Some(8192),
            direction: None,
            back: 0,
        })
        .await
        .expect("read")
}
