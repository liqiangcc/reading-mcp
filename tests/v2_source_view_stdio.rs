//! V2-4b PR-2, §5: ports `tests/source_view_stdio.rs` to the v2 `source_view` tool.
//!
//! The v1 test resolved a sentence locator through `get_text_units` and asked `get_source_view` for
//! the original page, then asserted the response's audit metadata. v2 `source_view` takes any
//! handle and answers `{at, page, pages}` plus the PNG image block; the document identity the v1
//! audit fields carried is asserted through `cite` on the same handle, which returns
//! `content_hash`, `normalized_document_hash` and its version. `source_binding_version` is a v1
//! response field with no v2 name (§2.1 item 3). The renderer-deadline test never touched a tool
//! and is kept verbatim: it is transport-agnostic.

#[path = "support/v2_mcp.rs"]
mod v2_mcp;

use std::time::Duration;

use lopdf::content::{Content, Operation};
use lopdf::{Document as PdfDocument, Object, Stream, dictionary};
use reading_mcp::application::ports::{
    ApplicationError, SourceViewLimitKind, SourceViewRenderOptions, SourceViewRenderer,
};
use reading_mcp::domain::{MediaType, NORMALIZED_DOCUMENT_HASH_VERSION};
use reading_mcp::parsing::FileProcessIsolatedPdfSourceViewRenderer;
use serde_json::json;

use v2_mcp::{
    call, cite_one, flat_text, identity_of, locator_of, payload, start_client,
    start_client_with_env, tool_error, v2_open, v2_read,
};

#[tokio::test]
async fn v2_stdio_source_view_returns_page_metadata_and_png_image_block() {
    let root = tempfile::tempdir().expect("source root should be created");
    let path = root.path().join("fidelity.pdf");
    tokio::fs::write(&path, build_pdf())
        .await
        .expect("PDF fixture should be written");

    let client = start_client(root.path(), std::path::Path::new("memory")).await;

    let opened = v2_open(&client, &path).await;
    assert_eq!(opened["format"], "pdf", "{opened}");
    let doc = opened["doc"].as_str().expect("doc handle").to_owned();

    // The v1 sentence locator of the first page: `read` at the document handle, then `cite` it.
    let sentence = payload(&v2_read(&client, &doc, json!({})).await);
    assert_eq!(flat_text(&sentence), "SOURCE VIEW SENTENCE.", "{sentence}");
    let sentence = sentence["at"]
        .as_str()
        .expect("the first sentence's own handle")
        .to_owned();
    let locator = locator_of(&client, &sentence).await;
    assert_eq!(locator["owner_section_id"], "section://page-1");

    let result = call(&client, "source_view", json!({ "at": sentence })).await;
    assert!(
        result.is_error != Some(true),
        "source view should render through stdio MCP: {result:?}"
    );
    let wire = serde_json::to_value(&result).expect("tool result should serialize");
    let content = wire["content"]
        .as_array()
        .expect("tool result content should be an array");
    assert!(
        content
            .iter()
            .any(|block| block["type"] == "image" && block["mimeType"] == "image/png"),
        "the page renders as a PNG image block: {wire}"
    );
    assert!(
        result.structured_content.is_none(),
        "v2 source_view never sets structuredContent"
    );

    // The v1 `GetSourceViewResponse` fields: page number and count, then the document identity.
    let metadata = payload(&result);
    assert_eq!(metadata["at"], sentence);
    assert_eq!(metadata["page"], 1, "{metadata}");
    assert_eq!(metadata["pages"], 1, "{metadata}");

    let identity = identity_of(&client, &sentence).await;
    assert!(
        identity["content_hash"]
            .as_str()
            .expect("content hash")
            .starts_with("sha256:"),
        "{identity}"
    );
    assert!(
        identity["normalized_document_hash"]
            .as_str()
            .expect("normalized document hash")
            .starts_with("sha256:"),
        "{identity}"
    );
    assert_eq!(
        identity["ndh"], NORMALIZED_DOCUMENT_HASH_VERSION,
        "the v1 normalized_document_hash_version"
    );
    assert_eq!(
        cite_one(&client, &doc).await["doc"]["content_hash"],
        identity["content_hash"],
        "the page is bound to the same document identity"
    );

    client
        .cancel()
        .await
        .expect("MCP process should close cleanly");
}

/// #264: a page whose decoded image stream is above the configured source-view
/// limit answers `too_large` with a hint naming the page and both numbers, and
/// the same handle renders when the limit is raised. The fixture's 9-megapixel
/// image also trips the default `max_pixels`, so the deployment profile's
/// 16M-pixel budget is set like the OCR tests do.
#[tokio::test]
async fn v2_stdio_source_view_reports_decoded_stream_limit_and_renders_above_it() {
    let root = tempfile::tempdir().expect("source root should be created");
    let path = root.path().join("big_stream.pdf");
    tokio::fs::write(
        &path,
        std::fs::read("tests/fixtures/source_view/big_stream.pdf").unwrap(),
    )
    .await
    .expect("fixture should be copied");

    for stream_limit in [16 * 1024 * 1024_u64, 64 * 1024 * 1024] {
        let state = root.path().join(format!("state-{stream_limit}"));
        let stream_limit = stream_limit.to_string();
        let env = [
            ("READING_MCP_SOURCE_VIEW_MAX_PIXELS", "16000000"),
            (
                "READING_MCP_SOURCE_VIEW_MAX_DECODED_STREAM_BYTES",
                stream_limit.as_str(),
            ),
        ];
        let client = start_client_with_env(root.path(), &state, &env).await;

        let opened = v2_open(&client, &path).await;
        let doc = opened["doc"].as_str().expect("doc handle").to_owned();
        let first = payload(&v2_read(&client, &doc, json!({})).await);
        let sentence = first["at"].as_str().expect("sentence handle").to_owned();

        let result = call(&client, "source_view", json!({ "at": sentence })).await;
        if stream_limit == "16777216" {
            let error = tool_error(&result);
            assert_eq!(error["error"], "too_large", "{error}");
            let hint = error["hint"].as_str().expect("a too_large hint");
            assert!(hint.contains("第 1 页"), "{hint}");
            assert!(hint.contains("27,000,000"), "{hint}");
            assert!(hint.contains("16,777,216"), "{hint}");
            assert!(hint.contains("read"), "{hint}");
            assert!(
                error.get("retry").is_none(),
                "a too_large answer is not retryable: {error}"
            );
        } else {
            assert!(
                result.is_error != Some(true),
                "the same page renders above its decoded size: {result:?}"
            );
            let wire = serde_json::to_value(&result).expect("tool result should serialize");
            assert!(
                wire["content"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|block| block["type"] == "image" && block["mimeType"] == "image/png"),
                "the page renders as a PNG image block: {wire}"
            );
            assert_eq!(payload(&result)["page"], 1);
        }
        client
            .cancel()
            .await
            .expect("MCP process should close cleanly");
    }
}

/// The PyMuPDF worker reports the same rejection through the same error record;
/// gated on the layout-python environment like the OCR tests, since CI's normal
/// job has no PyMuPDF.
#[tokio::test]
#[ignore = "requires READING_MCP_PDF_LAYOUT_PYTHON with PyMuPDF 1.28.2"]
async fn v2_stdio_pymupdf_source_view_reports_decoded_stream_limit() {
    let python = std::env::var("READING_MCP_PDF_LAYOUT_PYTHON")
        .expect("READING_MCP_PDF_LAYOUT_PYTHON names the PyMuPDF interpreter");
    let root = tempfile::tempdir().expect("source root should be created");
    let path = root.path().join("big_stream.pdf");
    tokio::fs::write(
        &path,
        std::fs::read("tests/fixtures/source_view/big_stream.pdf").unwrap(),
    )
    .await
    .expect("fixture should be copied");

    for stream_limit in ["16777216", "67108864"] {
        let state = root.path().join(format!("state-pymupdf-{stream_limit}"));
        let env = [
            ("READING_MCP_SOURCE_VIEW_MAX_PIXELS", "16000000"),
            (
                "READING_MCP_SOURCE_VIEW_MAX_DECODED_STREAM_BYTES",
                stream_limit,
            ),
            ("READING_MCP_PDF_LAYOUT_PYTHON", python.as_str()),
        ];
        let client = start_client_with_env(root.path(), &state, &env).await;

        let opened = v2_open(&client, &path).await;
        let doc = opened["doc"].as_str().expect("doc handle").to_owned();
        let first = payload(&v2_read(&client, &doc, json!({})).await);
        let sentence = first["at"].as_str().expect("sentence handle").to_owned();

        let result = call(&client, "source_view", json!({ "at": sentence })).await;
        if stream_limit == "16777216" {
            let error = tool_error(&result);
            assert_eq!(error["error"], "too_large", "{error}");
            let hint = error["hint"].as_str().expect("a too_large hint");
            assert!(hint.contains("第 1 页"), "{hint}");
            assert!(hint.contains("27,000,000"), "{hint}");
            assert!(hint.contains("16,777,216"), "{hint}");
        } else {
            assert!(
                result.is_error != Some(true),
                "the same page renders above its decoded size: {result:?}"
            );
        }
        client
            .cancel()
            .await
            .expect("MCP process should close cleanly");
    }
}

/// A corrupt input is not a limit rejection: the isolated worker cannot open
/// it as a PDF and the parent reports `SourceViewFailed`, which v2 maps to
/// `internal`. Exercised at the worker boundary because a corrupt file never
/// produces a document handle to call `source_view` with.
#[test]
fn isolated_worker_reports_a_corrupt_pdf_as_source_view_failure() {
    let renderer = FileProcessIsolatedPdfSourceViewRenderer::with_executable(
        std::path::PathBuf::from(env!("CARGO_BIN_EXE_reading-mcp")),
        Duration::from_secs(30),
    );
    let error = renderer
        .render(
            b"not a pdf at all".to_vec(),
            MediaType("application/pdf".into()),
            1,
            SourceViewRenderOptions {
                dpi: 72,
                max_pages: 2_000,
                max_width: 2_400,
                max_height: 3_200,
                max_pixels: 4_000_000,
                max_image_bytes: 8 * 1024 * 1024,
                max_decoded_stream_bytes: 64 * 1024 * 1024,
            },
        )
        .expect_err("a corrupt PDF must not render");
    assert!(
        matches!(error, ApplicationError::SourceViewFailed(_)),
        "corrupt input stays SourceViewFailed: {error:?}"
    );
}

#[cfg(unix)]
#[test]
fn isolated_renderer_terminates_a_worker_that_exceeds_the_deadline() {
    use std::os::unix::fs::PermissionsExt;

    let directory = tempfile::tempdir().expect("worker directory should be created");
    let worker = directory.path().join("sleep-worker.sh");
    std::fs::write(&worker, "#!/bin/sh\nsleep 5\n").expect("worker script should be written");
    let mut permissions = std::fs::metadata(&worker)
        .expect("worker metadata should be available")
        .permissions();
    permissions.set_mode(0o700);
    std::fs::set_permissions(&worker, permissions).expect("worker should be executable");

    let renderer = FileProcessIsolatedPdfSourceViewRenderer::with_executable(
        worker,
        Duration::from_millis(25),
    );
    let error = renderer
        .render(
            build_pdf(),
            reading_mcp::domain::MediaType("application/pdf".into()),
            1,
            SourceViewRenderOptions {
                dpi: 72,
                max_pages: 10,
                max_width: 1_000,
                max_height: 1_000,
                max_pixels: 1_000_000,
                max_image_bytes: 1024 * 1024,
                max_decoded_stream_bytes: 1024 * 1024,
            },
        )
        .expect_err("deadline must terminate the worker process");
    let ApplicationError::SourceViewLimitExceeded(detail) = error else {
        panic!("a worker deadline is a source-view limit rejection: {error:?}");
    };
    assert_eq!(detail.page, 1);
    assert_eq!(detail.kind, SourceViewLimitKind::Timeout);
    assert_eq!(detail.limit, Some(25));
}

fn build_pdf() -> Vec<u8> {
    let mut document = PdfDocument::with_version("1.5");
    let pages_id = document.new_object_id();
    let font_id = document.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => "Helvetica",
    });
    let resources_id = document.add_object(dictionary! {
        "Font" => dictionary! { "F1" => font_id },
    });
    let content = Content {
        operations: vec![
            Operation::new("BT", vec![]),
            Operation::new("Tf", vec!["F1".into(), 14.into()]),
            Operation::new("Td", vec![72.into(), 720.into()]),
            Operation::new("Tj", vec![Object::string_literal("SOURCE VIEW SENTENCE.")]),
            Operation::new("ET", vec![]),
        ],
    };
    let content_id = document.add_object(Stream::new(
        dictionary! {},
        content.encode().expect("fixture content should encode"),
    ));
    let page_id = document.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => pages_id,
        "Contents" => content_id,
    });
    document.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => vec![Object::Reference(page_id)],
            "Count" => 1,
            "Resources" => resources_id,
            "MediaBox" => vec![0.into(), 0.into(), 595.into(), 842.into()],
        }),
    );
    let catalog_id = document.add_object(dictionary! {
        "Type" => "Catalog",
        "Pages" => pages_id,
    });
    document.trailer.set("Root", catalog_id);

    let mut bytes = Vec::new();
    document
        .save_to(&mut bytes)
        .expect("fixture PDF should serialize");
    bytes
}
