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

#[path = "support/ocr_private_mcp.rs"]
mod private_mcp;

#[tokio::test]
#[ignore = "requires hosted root and verified offline runtime archive"]
async fn archived_runtime_runs_rust_parser_and_persists_exact_f07() {
    let root = std::path::PathBuf::from(std::env::var("OCR_TEST_VERIFIED_ROOT").unwrap());
    let report: serde_json::Value = serde_json::from_slice(
        &std::fs::read(std::env::var("OCR_TEST_ARCHIVE_REPORT").unwrap()).unwrap(),
    )
    .unwrap();
    let baseline: OcrRuntimeIdentity = serde_json::from_value(report["identity"].clone()).unwrap();
    let manifest = std::path::PathBuf::from(std::env::var("OCR_TEST_RUNTIME_MANIFEST").unwrap());
    let identity = reading_mcp::parsing::inspect_private_ocr_runtime(
        baseline.config,
        std::path::Path::new("/usr/bin/python3"),
        &root,
        &manifest,
    )
    .unwrap();
    assert!(identity.runtime_package.is_some());
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
        .with_ocr_runtime_package(root.clone(), manifest.clone())
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
    assert_eq!(evidence.visual_attempts.len(), 1);
    assert_eq!(evidence.visual_attempts[0].page, 1);
    assert!(
        evidence.visual_attempts[0]
            .attempts
            .iter()
            .flat_map(|attempt| attempt.res.boxes.iter())
            .any(|item| item.label == "text")
    );

    let f11_source = DocumentSource("file:///frozen/F11.pdf".into());
    let f11 = parser
        .parse(RetrievedResource {
            source: f11_source.clone(),
            final_source: f11_source,
            media_type: MediaType("application/pdf".into()),
            bytes: std::fs::read("tests/fixtures/scanned_pdf/pdf/F11.pdf").unwrap(),
            etag: None,
            last_modified: None,
            metadata: Default::default(),
        })
        .await
        .unwrap();
    let f11_units = f11.try_paragraph_text_units().unwrap().units;
    assert!(!f11_units.is_empty());
    let prose_paragraphs = f11
        .normalized_block_map()
        .unwrap()
        .unwrap()
        .blocks
        .iter()
        .filter(|block| block.kind == reading_mcp::domain::NormalizedBlockKind::Paragraph)
        .count();
    assert_eq!(prose_paragraphs, 6);
    let f11_payload = store
        .get(f11.metadata.get("ocr_evidence_blob").unwrap())
        .await
        .unwrap()
        .unwrap();
    let f11_evidence: OcrEvidenceBlob = serde_json::from_slice(&f11_payload).unwrap();
    f11_evidence.validate(1).unwrap();
    assert_eq!(f11_evidence.visual_attempts.len(), 1);
    let labels: Vec<_> = f11_evidence.visual_attempts[0]
        .attempts
        .iter()
        .flat_map(|attempt| attempt.res.boxes.iter().map(|item| item.label.as_str()))
        .collect();
    assert!(labels.contains(&"image") && labels.contains(&"formula"));
    let visual_classes: Vec<_> = f11_evidence.visual_attempts[0]
        .projection
        .regions
        .iter()
        .map(|region| region.label.as_str())
        .collect();
    assert!(visual_classes.contains(&"image") && visual_classes.contains(&"formula"));
    let limited = LayoutPdfParser::new(
        "/opt/ocr-python/bin/python".into(),
        ResourceBudget {
            max_normalized_chars: 1,
            ..ResourceBudget::default()
        },
    )
    .with_ocr_runtime_package(root, manifest)
    .with_ocr_config(identity.config.clone())
    .with_ocr_identity(identity)
    .with_evidence_store(store.clone());
    let error = tokio::time::timeout(
        std::time::Duration::from_secs(60),
        limited.parse(RetrievedResource {
            source: document.source.clone(),
            final_source: document.source.clone(),
            media_type: MediaType("application/pdf".into()),
            bytes: std::fs::read("tests/fixtures/scanned_pdf/pdf/F07.pdf").unwrap(),
            etag: None,
            last_modified: None,
            metadata: Default::default(),
        }),
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(
        error,
        reading_mcp::application::ports::ApplicationError::OcrResourceLimit
    );
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

/// Resumable OCR end to end on the real private runtime: a deliberately small
/// invocation budget interrupts F05 (four scanned pages) with a typed
/// OCR_TIMEOUT after durable per-page progress; fresh parser+store instances
/// (process-restart equivalent) then complete only the remaining pages. The
/// `ocr_resume` document metadata proves checkpointed pages replayed without
/// re-running the engine, and the result is a normal canonical document.
#[tokio::test]
#[ignore = "requires hosted root and verified offline runtime archive"]
async fn archived_runtime_resumes_interrupted_f05_without_repeating_pages() {
    use reading_mcp::application::ports::ApplicationError;
    use reading_mcp::infrastructure::{FileOcrCheckpointStore, MetaIdentity, checkpoint_key};
    use reading_mcp::parsing::PDF_LAYOUT_CACHE_NAMESPACE;
    use std::collections::BTreeSet;
    use std::time::Duration;

    let root = std::path::PathBuf::from(std::env::var("OCR_TEST_VERIFIED_ROOT").unwrap());
    let report: serde_json::Value = serde_json::from_slice(
        &std::fs::read(std::env::var("OCR_TEST_ARCHIVE_REPORT").unwrap()).unwrap(),
    )
    .unwrap();
    let baseline: OcrRuntimeIdentity = serde_json::from_value(report["identity"].clone()).unwrap();
    let manifest = std::path::PathBuf::from(std::env::var("OCR_TEST_RUNTIME_MANIFEST").unwrap());
    let identity = reading_mcp::parsing::inspect_private_ocr_runtime(
        baseline.config,
        std::path::Path::new("/usr/bin/python3"),
        &root,
        &manifest,
    )
    .unwrap();
    let python = std::path::PathBuf::from("/opt/ocr-python/bin/python");
    let directory = tempfile::tempdir().unwrap();
    let evidence = Arc::new(FileOcrEvidenceStore::new(directory.path().join("evidence")));
    let checkpoint_dir = directory.path().join("checkpoints");
    let bytes = std::fs::read("tests/fixtures/scanned_pdf/pdf/F05.pdf").unwrap();
    let raw_hash = format!("{:x}", Sha256::digest(&bytes));
    let meta = MetaIdentity {
        original_sha256: raw_hash.clone(),
        runtime_identity_sha256: identity.sha256.clone(),
        layout_namespace: PDF_LAYOUT_CACHE_NAMESPACE.into(),
    };
    let key = checkpoint_key(
        &meta.original_sha256,
        &meta.runtime_identity_sha256,
        &meta.layout_namespace,
    );
    let resource = || RetrievedResource {
        source: DocumentSource("file:///frozen/F05.pdf".into()),
        final_source: DocumentSource("file:///frozen/F05.pdf".into()),
        media_type: MediaType("application/pdf".into()),
        bytes: bytes.clone(),
        etag: None,
        last_modified: None,
        metadata: Default::default(),
    };
    // Calibrate against the real single-shot cost on this runner. The
    // accumulation bound starts below the total and widens until at least
    // one page persists; the cap stays below the full cost so a capture
    // attempt cannot normally finish the whole document unresumed.
    let probe = LayoutPdfParser::new(python.clone(), ResourceBudget::default())
        .with_ocr_runtime_package(root.clone(), manifest.clone())
        .with_ocr_config(identity.config.clone())
        .with_ocr_identity(identity.clone())
        .with_evidence_store(evidence.clone());
    let started = std::time::Instant::now();
    tokio::time::timeout(Duration::from_secs(300), probe.parse(resource()))
        .await
        .expect("baseline parse exceeded the bound")
        .unwrap_or_else(|error| panic!("baseline F05 parse failed: {error:?}"));
    let single_shot = started.elapsed().as_secs_f64();
    let mut accumulation = (single_shot * 0.35).clamp(2.0, 60.0);
    let mut stored = BTreeSet::new();
    let mut document = None;
    for _ in 0..10u32 {
        let seconds = if stored.is_empty() {
            accumulation
        } else {
            240.0
        };
        // A fresh store and parser per attempt is the process-restart case.
        let store = Arc::new(FileOcrCheckpointStore::new(&checkpoint_dir));
        let parser = LayoutPdfParser::new(python.clone(), ResourceBudget::default())
            .with_ocr_runtime_package(root.clone(), manifest.clone())
            .with_ocr_config(identity.config.clone())
            .with_ocr_identity(identity.clone())
            .with_evidence_store(evidence.clone())
            .with_checkpoint_store(store.clone())
            .with_ocr_invocation_budget(Duration::from_secs_f64(seconds));
        match tokio::time::timeout(Duration::from_secs(300), parser.parse(resource()))
            .await
            .expect("attempt exceeded the hard per-invocation bound")
        {
            Ok(done) => {
                document = Some(done);
                break;
            }
            Err(ApplicationError::OcrTimeout) => {
                let load = store.load(&key, &meta).await.unwrap();
                let now: BTreeSet<u32> = load.pages.keys().copied().collect();
                assert!(stored.is_subset(&now), "durable page progress regressed");
                assert!(
                    seconds <= accumulation || now.len() > stored.len(),
                    "a finishing attempt must leave at least one new completed page"
                );
                if now.is_empty() {
                    // Widen asymptotically toward the single-shot cost so the
                    // bound eventually lands inside the partial window
                    // (past the first page, before the last) whatever its
                    // position, without reaching the full-document cost.
                    accumulation += (single_shot - accumulation) * 0.5;
                }
                if !now.is_empty() {
                    // Source/runtime identity drift must fail closed.
                    let drifted = MetaIdentity {
                        original_sha256: "0".repeat(64),
                        ..meta.clone()
                    };
                    assert!(
                        store.load(&key, &drifted).await.unwrap().pages.is_empty(),
                        "identity drift leaked a resumable checkpoint"
                    );
                }
                stored = now;
            }
            Err(other) => panic!("non-retryable failure during resume: {other:?}"),
        }
    }
    let document = document.expect("resumable OCR did not converge on F05");
    assert!(
        !stored.is_empty(),
        "F05 converged without a persisted partial checkpoint: resume was not exercised"
    );
    let resume: serde_json::Value = serde_json::from_str(
        document
            .metadata
            .get("ocr_resume")
            .expect("resume metadata missing on a resumed document"),
    )
    .unwrap();
    let pages = |name: &str| -> BTreeSet<u32> {
        resume[name]
            .as_array()
            .unwrap()
            .iter()
            .map(|value| u32::try_from(value.as_u64().unwrap()).unwrap())
            .collect()
    };
    let (resumed, computed) = (pages("resumed"), pages("computed"));
    assert!(!resumed.is_empty(), "the resume path was never exercised");
    assert_eq!(
        resumed, stored,
        "previously completed pages must replay instead of re-running the engine"
    );
    assert!(resumed.is_disjoint(&computed), "a page was computed twice");
    assert_eq!(document.content_hash.0, format!("sha256:{raw_hash}"));
    let derivation: OcrDerivation =
        serde_json::from_str(&document.metadata["ocr_derivation"]).unwrap();
    derivation.validate_against(&identity, &raw_hash).unwrap();
    document.validate_ocr_publication().unwrap();
    let page_count: u32 = document.metadata["pdf_pages"].parse().unwrap();
    let payload = evidence
        .get(&document.metadata["ocr_evidence_blob"])
        .await
        .unwrap()
        .unwrap();
    let blob: OcrEvidenceBlob = serde_json::from_slice(&payload).unwrap();
    blob.validate(page_count).unwrap();
    // Success consumed the checkpoint: the store directory is gone.
    let store = FileOcrCheckpointStore::new(&checkpoint_dir);
    assert!(store.load(&key, &meta).await.unwrap().pages.is_empty());
    // No worker unit survives a timeout, a resume, or the final success.
    let units = std::process::Command::new("systemctl")
        .args(["list-units", "--all", "--no-legend", "reading-mcp-ocr-*"])
        .output()
        .unwrap();
    assert!(
        !String::from_utf8_lossy(&units.stdout)
            .lines()
            .any(|line| line.contains(" active ")),
        "an OCR unit survived the interrupted/resumed runs"
    );
    println!(
        "verified resumable OCR: F05 interrupted, resumed {resumed:?}, computed {computed:?}, canonical document published"
    );
}
