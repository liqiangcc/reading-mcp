use reading_mcp::application::ports::OcrEvidenceStore;
use reading_mcp::application::ports::{Parser, RetrievedResource};
use reading_mcp::domain::{DocumentSource, MediaType, OcrConfig};
use reading_mcp::infrastructure::{FileOcrEvidenceStore, build_ocr_runtime_identity};
use reading_mcp::parsing::LayoutPdfParser;
use sha2::Digest;
use std::sync::Arc;
use tempfile::tempdir;

#[tokio::test]
#[ignore = "requires pinned hosted OCR dependencies"]
async fn real_f07_ocr_publishes_typed_evidence_and_page_bindings() {
    let bytes = std::fs::read("tests/fixtures/scanned_pdf/pdf/F07.pdf").unwrap();
    let source = DocumentSource("file:///frozen/F07.pdf".into());
    let resource = RetrievedResource {
        source: source.clone(),
        final_source: source,
        media_type: MediaType("application/pdf".into()),
        bytes: bytes.clone(),
        etag: None,
        last_modified: None,
        metadata: Default::default(),
    };
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
    let python = std::env::var("READING_MCP_PDF_LAYOUT_PYTHON").unwrap();
    let directory = tempdir().unwrap();
    let store = Arc::new(FileOcrEvidenceStore::new(directory.path().join("evidence")));
    let parser = LayoutPdfParser::new(
        python.into(),
        reading_mcp::infrastructure::ResourceBudget::default(),
    )
    .with_ocr_config(config)
    .with_ocr_identity(identity.clone())
    .with_evidence_store(store.clone());
    let document = parser.parse(resource).await.unwrap();
    let units = document.try_paragraph_text_units().unwrap();
    assert!(units.len() > 1);
    assert_eq!(
        document.content_hash.0,
        format!("sha256:{:x}", sha2::Sha256::digest(&bytes))
    );
    let derivation: reading_mcp::domain::OcrDerivation =
        serde_json::from_str(document.metadata.get("ocr_derivation").unwrap()).unwrap();
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
    assert!(!blob.is_empty());
    let map = document.original_source_binding_map().unwrap().unwrap();
    assert!(!map.bindings.is_empty());
    assert!(map.bindings.iter().all(|b| matches!(
        b.target,
        reading_mcp::domain::OriginalSourceTarget::Page { page_number: 1 }
    )));
}
