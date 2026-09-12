//! Hosted privileged integration only; never run on the production host.
use reading_mcp::{
    application::ports::{OcrEvidenceStore, Parser, RetrievedResource},
    domain::{DocumentSource, MediaType, OcrConfig, OcrDerivation, OcrEvidenceBlob},
    infrastructure::{FileOcrEvidenceStore, ResourceBudget, build_ocr_runtime_identity},
    parsing::LayoutPdfParser,
};
use std::sync::Arc;

#[tokio::test]
#[ignore = "requires pinned hosted OCR dependencies and systemd root"]
async fn real_f07_uses_systemd_boundary_and_publishes_verified_evidence() {
    let config = OcrConfig {
        enabled: true,
        engine_path: "/usr/bin/tesseract".into(),
        tessdata_path: "/usr/share/tesseract-ocr/5/tessdata".into(),
        languages: vec!["chi_sim".into()],
        operator_revision: "1".into(),
        dpi: 300,
        oem: 1,
        psm: 3,
        detector_version: "pdf-layout/v1".into(),
        protocol_version: "pdf-layout/v1".into(),
    };
    let identity = build_ocr_runtime_identity(config.clone()).unwrap();
    let directory = tempfile::tempdir().unwrap();
    let store = Arc::new(FileOcrEvidenceStore::new(directory.path().join("evidence")));
    let parser = LayoutPdfParser::new(
        std::env::var("READING_MCP_PDF_LAYOUT_PYTHON")
            .unwrap()
            .into(),
        ResourceBudget::default(),
    )
    .with_ocr_config(config)
    .with_ocr_identity(identity.clone())
    .with_evidence_store(store.clone())
    .with_systemd_ocr_sandbox();
    let source = DocumentSource("file:///frozen/F07.pdf".into());
    let document = tokio::time::timeout(
        std::time::Duration::from_secs(60),
        parser.parse(RetrievedResource {
            source: source.clone(),
            final_source: source,
            media_type: MediaType("application/pdf".into()),
            bytes: std::fs::read("tests/fixtures/scanned_pdf/pdf/F07.pdf").unwrap(),
            etag: None,
            last_modified: None,
            metadata: Default::default(),
        }),
    )
    .await
    .unwrap()
    .unwrap();
    document.validate_ocr_publication().unwrap();
    assert_eq!(document.try_paragraph_text_units().unwrap().units.len(), 4);
    let derivation = OcrDerivation::from_metadata(&document.metadata)
        .unwrap()
        .unwrap();
    derivation
        .validate_against(
            &identity,
            document.content_hash.0.trim_start_matches("sha256:"),
        )
        .unwrap();
    let blob = store
        .get(document.metadata.get("ocr_evidence_blob").unwrap())
        .await
        .unwrap()
        .unwrap();
    let evidence: OcrEvidenceBlob = serde_json::from_slice(&blob).unwrap();
    evidence.validate(1).unwrap();
    assert_eq!(evidence.runtime_identity, identity);
    assert_eq!(evidence.pages[0].attempts.len(), 2);
}
