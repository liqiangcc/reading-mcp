use reading_mcp::application::ports::{
    DocumentRepository, OcrEvidenceStore, Parser, RetrievedResource,
};
use reading_mcp::domain::{
    DocumentSource, MediaType, OcrDerivation, OcrEvidenceBlob, OcrRuntimeIdentity,
};
use reading_mcp::infrastructure::{FileOcrEvidenceStore, ResourceBudget, SqliteDocumentRepository};
use reading_mcp::parsing::LayoutPdfParser;
use sha2::{Digest, Sha256};
use std::sync::Arc;

#[tokio::test]
#[ignore = "requires hosted root and verified offline runtime archive"]
async fn archived_runtime_runs_rust_parser_and_persists_exact_f07() {
    let root = std::path::PathBuf::from(std::env::var("OCR_TEST_VERIFIED_ROOT").unwrap());
    let report: serde_json::Value = serde_json::from_slice(
        &std::fs::read(std::env::var("OCR_TEST_ARCHIVE_REPORT").unwrap()).unwrap(),
    )
    .unwrap();
    let identity: OcrRuntimeIdentity = serde_json::from_value(report["identity"].clone()).unwrap();
    assert_eq!(
        OcrRuntimeIdentity::build(identity.config.clone(), identity.dependencies.clone()).unwrap(),
        identity
    );
    let expected: Vec<String> =
        serde_json::from_value(report["canonical_paragraphs"].clone()).unwrap();
    assert_eq!(expected.len(), 4);
    // This path only exists inside the verified RootDirectory on the runner.
    let python = std::path::PathBuf::from("/opt/ocr-python/bin/python");
    assert!(
        !python.exists(),
        "test must not accidentally use a host interpreter"
    );
    let directory = tempfile::tempdir().unwrap();
    let store = Arc::new(FileOcrEvidenceStore::new(directory.path().join("evidence")));
    let parser = LayoutPdfParser::new(python, ResourceBudget::default())
        .with_ocr_runtime_root(root)
        .with_ocr_config(identity.config.clone())
        .with_ocr_identity(identity.clone())
        .with_evidence_store(store.clone());
    let bytes = std::fs::read("tests/fixtures/scanned_pdf/pdf/F07.pdf").unwrap();
    let raw_hash = format!("{:x}", Sha256::digest(&bytes));
    let source = DocumentSource("file:///frozen/F07.pdf".into());
    let document = tokio::time::timeout(
        std::time::Duration::from_secs(60),
        parser.parse(RetrievedResource {
            source: source.clone(),
            final_source: source,
            media_type: MediaType("application/pdf".into()),
            bytes,
            etag: None,
            last_modified: None,
            metadata: Default::default(),
        }),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(document.content_hash.0, format!("sha256:{raw_hash}"));
    let actual: Vec<_> = document
        .try_paragraph_text_units()
        .unwrap()
        .units
        .into_iter()
        .map(|u| u.text)
        .collect();
    assert_eq!(actual, expected);
    let derivation: OcrDerivation =
        serde_json::from_str(&document.metadata["ocr_derivation"]).unwrap();
    derivation.validate_against(&identity, &raw_hash).unwrap();
    document.validate_ocr_publication().unwrap();
    let payload = store
        .get(&document.metadata["ocr_evidence_blob"])
        .await
        .unwrap()
        .unwrap();
    let evidence: OcrEvidenceBlob = serde_json::from_slice(&payload).unwrap();
    evidence.validate(1).unwrap();
    assert_eq!(evidence.runtime_identity, identity);
    assert_eq!(evidence.pages[0].attempts.len(), 2);
    assert_eq!(evidence.pages[0].selection.len(), 4);
    let database = directory.path().join("documents.sqlite");
    {
        let repository = SqliteDocumentRepository::open(&database).unwrap();
        repository.save(document.clone()).await.unwrap();
    }
    let repository = SqliteDocumentRepository::open(&database).unwrap();
    let restored = repository.get(&document.id).await.unwrap().unwrap();
    restored.validate_ocr_publication().unwrap();
    assert_eq!(
        restored.normalized_document_hash(),
        document.normalized_document_hash()
    );
    assert_eq!(
        restored.original_source_binding_map().unwrap(),
        document.original_source_binding_map().unwrap()
    );
    println!(
        "verified archived runtime: Rust F07 parse, four exact paragraphs, typed evidence and SQLite reopen passed"
    );
}
