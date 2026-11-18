//! PDF-4b: the v2 `ocr_required` hint says what the source lacks and what a reader can do.
//!
//! The numbers in the hint are the parser's own evidence, so the test reads them back from
//! `OcrRequiredDetail` instead of hard-coding a guess. `F01.pdf` is deliberately absent: it is a
//! native text-layer fixture (448 characters, no raster), so it opens normally.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use reading_mcp::application::open_document::OpenDocumentUseCase;
use reading_mcp::application::open_reading::{OpenReadingCommand, OpenReadingUseCase};
use reading_mcp::application::ports::{
    ApplicationError, OcrRequiredDetail, Parser, RetrievedResource,
};
use reading_mcp::domain::{DocumentSource, MediaType};
use reading_mcp::infrastructure::{
    InMemoryDocumentRepository, InMemorySearchIndex, InMemoryTextUnitIndex, SqliteOpenResultStore,
};
use reading_mcp::mcp::v2_contracts::{ErrorView, retry_for};
use reading_mcp::parsing::{ArchiveLimits, ParserRouter, PdfParser};
use reading_mcp::retrieval::{FileRetriever, LocalFileSourcePolicy};
use tempfile::tempdir;

/// Image-only fixtures: `F02` has no text at all, `F05` is the four-page scan whose only text is a
/// page footer, the shape of the `naturebp.pdf` a reader reported.
const IMAGE_ONLY_FIXTURES: [&str; 2] = ["F02.pdf", "F05.pdf"];

fn fixture_path(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/scanned_pdf/pdf")
        .join(name)
}

/// The evidence the default PDF backend measures for this fixture, read from the parser itself.
async fn measured_detail(name: &str) -> OcrRequiredDetail {
    let path = fixture_path(name);
    let source = DocumentSource(format!("file://{}", path.display()));
    let resource = RetrievedResource {
        source: source.clone(),
        final_source: source,
        media_type: MediaType("application/pdf".into()),
        bytes: std::fs::read(&path).expect("fixture bytes"),
        etag: None,
        last_modified: None,
        metadata: Default::default(),
    };
    match PdfParser
        .parse(resource)
        .await
        .expect_err("an image-only fixture must not publish a document")
    {
        ApplicationError::OcrRequired(detail) => detail,
        other => panic!("{name} must answer ocr_required, got {other}"),
    }
}

fn v2_body(error: &reading_mcp::application::reading_error::ReadingError) -> String {
    serde_json::to_string(&ErrorView::new(error, retry_for(error)))
        .expect("the v2 error serializes")
}

#[tokio::test]
async fn an_image_only_pdf_answers_ocr_required_with_its_own_counts_and_the_same_answer_on_reopen()
{
    for name in IMAGE_ONLY_FIXTURES {
        let detail = measured_detail(name).await;
        let pages = detail
            .pages
            .expect("the default backend measures the page count");
        let characters = detail
            .characters
            .expect("the default backend measures the character count");
        assert!(pages > 0, "{name}: a measured page count is never zero");

        let directory = tempdir().expect("temp directory");
        let path = directory.path().join(name);
        std::fs::copy(fixture_path(name), &path).expect("fixture copied");
        let store = Arc::new(
            SqliteOpenResultStore::open(directory.path().join("state.sqlite"))
                .expect("open result store"),
        );
        let repository = Arc::new(InMemoryDocumentRepository::default());
        let open_document = Arc::new(
            OpenDocumentUseCase::new(
                Arc::new(LocalFileSourcePolicy::allow_roots([directory.path()])),
                Arc::new(FileRetriever),
                Arc::new(ParserRouter::release(1_000, ArchiveLimits::default())),
                Arc::clone(&repository),
                Arc::new(InMemorySearchIndex::default()),
            )
            .with_open_result_store(store, "pdf4b-test-fingerprint".into()),
        );
        let open = OpenReadingUseCase::new(
            open_document,
            repository,
            Arc::new(InMemoryTextUnitIndex::default()),
        );
        let command = || OpenReadingCommand {
            source: DocumentSource(path.to_string_lossy().into_owned()),
            refresh: false,
        };

        let first = open
            .execute(command())
            .await
            .expect_err("an image-only PDF must not open");
        assert_eq!(first.code.as_str(), "ocr_required");
        assert_eq!(
            (first.pages, first.characters),
            (Some(pages), Some(characters)),
            "{name}: the v2 error keeps the parser's evidence"
        );
        let first_body = v2_body(&first);
        assert!(
            first_body.contains(&format!("（{pages} 页只取到 {characters} 个字符）")),
            "{name}: {first_body}"
        );
        assert!(
            first_body.contains("离线 OCR 后的 PDF 或文本"),
            "{name}: {first_body}"
        );
        assert!(
            first_body.contains("在支持 OCR 的服务上重新 open"),
            "{name}: {first_body}"
        );
        assert!(!first_body.contains("\"retry\""), "{name}: {first_body}");

        // v1 sends the parser's own message, unchanged by PDF-4b.
        assert_eq!(first.detail, detail.to_string(), "{name}");

        // Nothing is stored for a failed open, so a reopen re-measures the same bytes and answers
        // the identical v2 error; no stored detail and no schema change are involved.
        let second = open
            .execute(command())
            .await
            .expect_err("a reopen must report the same failure, not flap to ok");
        assert_eq!(second, first, "{name}: a reopen answers the same error");
        assert_eq!(
            v2_body(&second),
            first_body,
            "{name}: identical bytes on reopen"
        );
    }
}
