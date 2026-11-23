//! V2-4b PR-2, §5: the ported `ocr_private_mcp` helper. It replaces
//! `tests/support/ocr_private_mcp.rs` (deleted in PR-5) on the v2 tools: `open` instead of
//! `open_document`, and `cite` for the normalized identity that the v1 response carried. Everything it asserts is storage-level and unchanged: the publication
//! row, the OCR evidence blob, the runtime package manifest, the parsed-cache telemetry, the
//! corrupted-package boot rejection and the post-startup model-drift rejection.
//!
//! Gated exactly like the v1 helper: `#[ignore = "requires hosted root and verified offline
//! runtime archive"]` and the same `OCR_TEST_VERIFIED_ROOT` / `OCR_TEST_RUNTIME_MANIFEST` env.

use reading_mcp::application::ports::{DocumentRepository, OcrEvidenceStore};
use reading_mcp::domain::{DocumentId, OcrEvidenceBlob};
use reading_mcp::infrastructure::{FileOcrEvidenceStore, SqliteDocumentRepository};
use rmcp::ServiceExt;
use rmcp::transport::TokioChildProcess;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::v2_mcp::{call, identity_of, payload, server_command, tool_error};

#[tokio::test]
#[ignore = "requires hosted root and verified offline runtime archive"]
async fn startup_cache_identity_and_corruption_rejection() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("F07.pdf");
    std::fs::copy("tests/fixtures/scanned_pdf/pdf/F07.pdf", &source).unwrap();
    let state = directory.path().join("state");
    let root = std::path::PathBuf::from(std::env::var("OCR_TEST_VERIFIED_ROOT").unwrap());
    let manifest = std::path::PathBuf::from(std::env::var("OCR_TEST_RUNTIME_MANIFEST").unwrap());
    let alternate_manifest = directory.path().join("alternate-manifest.json");
    let mut alternate: Value = serde_json::from_slice(&std::fs::read(&manifest).unwrap()).unwrap();
    alternate["status"] = json!("synthetic package metadata revision; unchanged runtime files");
    std::fs::write(&alternate_manifest, serde_json::to_vec(&alternate).unwrap()).unwrap();
    let make_command = |revision: &str, manifest: &std::path::Path| {
        let mut command = server_command(directory.path(), &state);
        command
            .env("READING_MCP_TELEMETRY", "true")
            // Host stdlib verifier only; OCR uses the packaged interpreter.
            .env("READING_MCP_PDF_LAYOUT_PYTHON", "/usr/bin/python3")
            .env("READING_MCP_OCR_RUNTIME_ROOT", &root)
            .env("READING_MCP_OCR_RUNTIME_MANIFEST", manifest)
            .env("READING_MCP_OCR_ENABLED", "true")
            .env("READING_MCP_OCR_LANG", "chi_sim")
            .env("READING_MCP_OCR_ENGINE", "/usr/bin/tesseract")
            .env(
                "READING_MCP_OCR_TESSDATA",
                "/usr/share/tesseract-ocr/5/tessdata",
            )
            .env("READING_MCP_OCR_REVISION", revision)
            .kill_on_drop(true);
        command
    };
    let mut previous = None;
    let mut last_document = None;
    for (trial, (revision, manifest, warm)) in [
        ("1", &manifest, false),
        ("1", &manifest, true),
        ("2", &manifest, false),
        ("2", &alternate_manifest, false),
    ]
    .into_iter()
    .enumerate()
    {
        let log = directory.path().join(format!("telemetry-{trial}.jsonl"));
        let started = tokio::time::Instant::now();
        let deadline = started + std::time::Duration::from_secs(if warm { 5 } else { 45 });
        let (transport, _) = TokioChildProcess::builder(make_command(revision, manifest))
            .stderr(std::fs::File::create(&log).unwrap())
            .spawn()
            .unwrap();
        let client = tokio::time::timeout_at(deadline, ().serve(transport))
            .await
            .unwrap()
            .unwrap();
        let opened = tokio::time::timeout_at(
            deadline,
            call(&client, "open", json!({"source": source, "refresh": true})),
        )
        .await
        .unwrap();
        let doc = payload(&opened)["doc"]
            .as_str()
            .expect("doc handle")
            .to_owned();
        let identity = identity_of(&client, &doc).await;
        let normalized_document_hash = identity["normalized_document_hash"].clone();
        client.cancel().await.unwrap();
        if let Some(hash) = &previous {
            assert_eq!(hash == &normalized_document_hash, warm);
        }
        previous = Some(normalized_document_hash);
        let repository = SqliteDocumentRepository::open(state.join("reading-mcp.sqlite")).unwrap();
        let document = repository
            .get(&DocumentId(
                identity["id"].as_str().expect("document id").to_owned(),
            ))
            .await
            .unwrap()
            .unwrap();
        document.validate_ocr_publication().unwrap();
        let evidence_bytes = FileOcrEvidenceStore::new(state.join("ocr-evidence"))
            .get(&document.metadata["ocr_evidence_blob"])
            .await
            .unwrap()
            .unwrap();
        let evidence: OcrEvidenceBlob = serde_json::from_slice(&evidence_bytes).unwrap();
        evidence.validate(1).unwrap();
        let package = evidence.runtime_identity.runtime_package.as_ref().unwrap();
        assert_eq!(
            package.manifest_sha256,
            format!("{:x}", Sha256::digest(std::fs::read(manifest).unwrap()))
        );
        assert_eq!(evidence.runtime_identity.config.operator_revision, revision);
        let events: Vec<Value> = std::fs::read_to_string(log)
            .unwrap()
            .lines()
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect();
        let gets: Vec<_> = events
            .iter()
            .filter(|event| event["event"] == "parsed_cache_get")
            .collect();
        assert_eq!(gets.len(), if warm { 1 } else { 2 });
        assert!(
            gets.iter()
                .all(|event| event["success"] == true && event["hit"] == warm)
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| event["event"] == "parsed_cache_put")
                .count(),
            usize::from(!warm)
        );
        println!(
            "private_runtime_mcp {}",
            json!({"trial":trial,"revision":revision,"cache_hit":warm,
            "startup_and_open_seconds":started.elapsed().as_secs_f64(),"package":package,
            "normalized_document_hash":identity["normalized_document_hash"]})
        );
        last_document = Some(document);
    }
    // Mutate only this disposable hosted reconstruction, restore even on panic.
    // The populated cache must not bypass startup verification of actual bytes.
    struct RestoreFile(std::path::PathBuf, Vec<u8>);
    impl Drop for RestoreFile {
        fn drop(&mut self) {
            std::fs::write(&self.0, &self.1).expect("restore disposable test model");
        }
    }
    let model = root.join("usr/share/tesseract-ocr/5/tessdata/chi_sim.traineddata");
    let restore = RestoreFile(model.clone(), std::fs::read(&model).unwrap());
    let mut changed = restore.1.clone();
    changed[0] ^= 1;
    std::fs::write(&model, changed).unwrap();
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        make_command("2", &alternate_manifest)
            .stdin(std::process::Stdio::null())
            .output(),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(
        !result.status.success(),
        "corrupt package must fail before cached open"
    );
    assert!(String::from_utf8_lossy(&result.stderr).contains("inventory verification failed"));
    drop(restore);
    // A separate fresh revision misses cache after successful startup. Drift
    // introduced only then must be rejected by the actual worker before OCR.
    // Do not claim cache hits rescan an operator-owned immutable install tree.
    let (transport, _) = TokioChildProcess::builder(make_command("3", &alternate_manifest))
        .spawn()
        .unwrap();
    let client = tokio::time::timeout(std::time::Duration::from_secs(10), ().serve(transport))
        .await
        .unwrap()
        .unwrap();
    let restore = RestoreFile(model.clone(), std::fs::read(&model).unwrap());
    let mut changed = restore.1.clone();
    changed[0] ^= 1;
    std::fs::write(&model, changed).unwrap();
    let error = tokio::time::timeout(
        std::time::Duration::from_secs(15),
        call(&client, "open", json!({"source": source, "refresh": true})),
    )
    .await
    .unwrap();
    let error = tool_error(&error);
    assert_eq!(error["error"], "ocr_required", "{error}");
    assert!(
        error.get("retry").is_none(),
        "the drift rejection is not retryable: {error}"
    );
    assert!(!error.to_string().contains("traineddata"));
    client.cancel().await.unwrap();
    drop(restore);
    println!(
        "private runtime MCP rejects post-startup model drift as ocr_required without publication"
    );
    let previous = last_document.unwrap();
    let repository = SqliteDocumentRepository::open(state.join("reading-mcp.sqlite")).unwrap();
    let retained = repository.get(&previous.id).await.unwrap().unwrap();
    assert_eq!(
        retained.normalized_document_hash(),
        previous.normalized_document_hash()
    );
    assert_eq!(retained.root_sections, previous.root_sections);
}
