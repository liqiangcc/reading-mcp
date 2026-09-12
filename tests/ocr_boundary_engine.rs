use reading_mcp::application::ports::{Parser, RetrievedResource};
use reading_mcp::domain::{DocumentSource, MediaType, OcrConfig, SentenceEligibility};
use reading_mcp::infrastructure::{
    FileOcrEvidenceStore, ResourceBudget, build_ocr_runtime_identity,
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
            "normalized_hash":document.normalized_document_hash().0, "paragraphs":units}),
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
