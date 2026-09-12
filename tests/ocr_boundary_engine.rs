use reading_mcp::application::ports::{DocumentRepository, Parser, RetrievedResource};
use reading_mcp::application::read_document::{ReadDocumentUseCase, ReadExactTargetCommand};
use reading_mcp::domain::{
    DocumentSource, MediaType, OcrConfig, OriginalSourceTarget, SentenceEligibility, TextLocator,
};
use reading_mcp::infrastructure::{
    FileOcrEvidenceStore, ResourceBudget, SqliteDocumentRepository, build_ocr_runtime_identity,
};
use reading_mcp::parsing::LayoutPdfParser;
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
        let reader = ReadDocumentUseCase::new(repository);
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
                let result = reader
                    .read_exact(ReadExactTargetCommand {
                        document_id: document.id.clone(),
                        target_locator: locator.clone(),
                        max_chars: Some(8192),
                    })
                    .await
                    .unwrap();
                assert!(result.complete, "{case}");
                assert_eq!(result.content, text, "{case}");
                assert_eq!(result.resolved_target_locator, locator, "{case}");
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
