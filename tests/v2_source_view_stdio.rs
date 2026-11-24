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

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use lopdf::content::{Content, Operation};
use lopdf::{Document as PdfDocument, Object, Stream, dictionary};
use reading_mcp::application::open_document::{OpenDocumentCommand, OpenDocumentUseCase};
use reading_mcp::application::ports::{
    ApplicationError, RawResourceCache, RenderedSourceView, RetrievalOptions, RetrievedResource,
    Retriever, SourcePolicy, SourceViewLimitDetail, SourceViewLimitKind, SourceViewRenderOptions,
    SourceViewRenderer, TextUnitIndex,
};
use reading_mcp::application::short_handle::handle_checksum;
use reading_mcp::domain::{DocumentSource, MediaType, NORMALIZED_DOCUMENT_HASH_VERSION};
use reading_mcp::infrastructure::{
    FileRawResourceCache, SqliteDocumentRepository, SqliteOpenResultStore, SqliteSearchIndex,
    SqliteTextUnitIndex,
};
use reading_mcp::parsing::{FileProcessIsolatedPdfSourceViewRenderer, ParserRouter};
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

/// #272: a local PDF whose bytes are rewritten under the same path after `open`
/// answers `stale_handle` — the worker hashes the file itself and the parent
/// fails closed, the same as the buffered path did.
#[tokio::test]
async fn v2_stdio_source_view_on_rewritten_local_pdf_answers_stale_handle() {
    let root = tempfile::tempdir().expect("source root should be created");
    let path = root.path().join("fidelity.pdf");
    let original = build_pdf();
    tokio::fs::write(&path, &original)
        .await
        .expect("PDF fixture should be written");

    let client = start_client(root.path(), std::path::Path::new("memory")).await;
    let opened = v2_open(&client, &path).await;
    let doc = opened["doc"].as_str().expect("doc handle").to_owned();
    let first = payload(&v2_read(&client, &doc, json!({})).await);
    let sentence = first["at"].as_str().expect("sentence handle").to_owned();

    let mut changed = original;
    changed.extend_from_slice(b"changed source bytes");
    tokio::fs::write(&path, changed)
        .await
        .expect("changed source should be written");

    let result = call(&client, "source_view", json!({ "at": sentence })).await;
    let error = tool_error(&result);
    assert_eq!(error["error"], "stale_handle", "{error}");

    client
        .cancel()
        .await
        .expect("MCP process should close cleanly");
}

/// #274: a URL source whose raw-cache body was rewritten under the same key
/// answers `stale_handle` through the real server — the worker maps the cache
/// file, hashes it, and the parent fails closed instead of falling back to the
/// network. The stdio binary refuses loopback endpoints, so the state dir is
/// seeded in-process: the document through `OpenDocumentUseCase` on the real
/// sqlite stores, its raw-cache entry through `FileRawResourceCache::put`,
/// then the body file rewritten. The document handle is `d<doc_key>~<checksum>`
/// — `doc_key` from the seeded index, the checksum through `handle_checksum`.
#[tokio::test]
async fn v2_stdio_source_view_on_rewritten_url_cache_body_answers_stale_handle() {
    let root = tempfile::tempdir().expect("source root should be created");
    let state = tempfile::tempdir().expect("state directory should be created");
    let database = state.path().join("reading-mcp.sqlite");

    let source = DocumentSource("https://example.test/doc.pdf".into());
    let mut bytes = build_pdf();
    let resource = RetrievedResource {
        source: source.clone(),
        final_source: source.clone(),
        media_type: MediaType("application/pdf".into()),
        bytes: bytes.clone(),
        etag: Some("\"pdf-v1\"".into()),
        last_modified: None,
        metadata: BTreeMap::new(),
    };
    let repository =
        Arc::new(SqliteDocumentRepository::open(&database).expect("repository should open"));
    let index = Arc::new(SqliteTextUnitIndex::open(&database).expect("index should open"));
    let open_results =
        Arc::new(SqliteOpenResultStore::open(&database).expect("open result store should open"));
    let search = Arc::new(SqliteSearchIndex::new(repository.clone(), index.clone()));
    // The store's startup prune deletes every document no open result names, so the seed must
    // leave the `open_results_v1` row a real open leaves.
    let opened = OpenDocumentUseCase::with_text_unit_index(
        Arc::new(SeededSourcePolicy),
        Arc::new(SeededRetriever(resource.clone())),
        Arc::new(ParserRouter::phase4()),
        repository.clone(),
        index.clone(),
        search,
    )
    .with_open_result_store(open_results, "seeded|pdf-layout=off|pdf-text=test".into())
    .execute(OpenDocumentCommand {
        source: source.clone(),
        options: RetrievalOptions::default(),
    })
    .await
    .expect("the seed document should open");

    let cache = FileRawResourceCache::new(state.path().join("cache"));
    cache
        .put(&source, resource)
        .await
        .expect("seed cache entry should be written");
    let body = std::fs::read_dir(state.path().join("cache").join("raw"))
        .expect("raw cache directory should exist")
        .flatten()
        .map(|entry| entry.path())
        .find(|path| path.extension().is_some_and(|ext| ext == "bin"))
        .expect("the seeded body file");
    bytes.extend_from_slice(b"rewritten cache body");
    std::fs::write(&body, &bytes).expect("cache body should be rewritten");

    let doc_key = index
        .document_key(&opened.document_id)
        .await
        .expect("document key should read")
        .expect("the seeded document has a key");
    let handle = format!(
        "d{doc_key}~{}",
        handle_checksum(
            &opened.document_id,
            &opened.normalized_document_hash,
            &MediaType("application/pdf".into()),
        )
    );

    let client = start_client(root.path(), state.path()).await;
    let result = call(&client, "source_view", json!({ "at": handle })).await;
    let error = tool_error(&result);
    assert_eq!(error["error"], "stale_handle", "{error}");

    client
        .cancel()
        .await
        .expect("MCP process should close cleanly");
}

/// A source policy that accepts the seeded URL — the stdio binary's own policy
/// is what `source_view` consults, not the seeding path's.
struct SeededSourcePolicy;

#[async_trait::async_trait]
impl SourcePolicy for SeededSourcePolicy {
    async fn validate(&self, _source: &DocumentSource) -> Result<(), ApplicationError> {
        Ok(())
    }
}

/// A retriever that serves the seeded URL resource — the same role the
/// counting fixture plays in the in-process tests.
struct SeededRetriever(RetrievedResource);

#[async_trait::async_trait]
impl Retriever for SeededRetriever {
    async fn retrieve(
        &self,
        _source: &DocumentSource,
        _options: &RetrievalOptions,
    ) -> Result<RetrievedResource, ApplicationError> {
        Ok(self.0.clone())
    }
}

/// #272: a local path replaced by a symlink that resolves outside the allowed
/// roots is refused the way the retrieval policy refuses it — `blocked_source`.
#[cfg(unix)]
#[tokio::test]
async fn v2_stdio_source_view_on_root_escaping_symlink_answers_blocked_source() {
    let root = tempfile::tempdir().expect("source root should be created");
    let outside = tempfile::tempdir().expect("outside directory should be created");
    let path = root.path().join("fidelity.pdf");
    let outside_pdf = outside.path().join("outside.pdf");
    tokio::fs::write(&path, build_pdf())
        .await
        .expect("PDF fixture should be written");
    tokio::fs::write(&outside_pdf, build_pdf())
        .await
        .expect("outside fixture should be written");

    let client = start_client(root.path(), std::path::Path::new("memory")).await;
    let opened = v2_open(&client, &path).await;
    let doc = opened["doc"].as_str().expect("doc handle").to_owned();
    let first = payload(&v2_read(&client, &doc, json!({})).await);
    let sentence = first["at"].as_str().expect("sentence handle").to_owned();

    tokio::fs::remove_file(&path)
        .await
        .expect("original file should be removed");
    std::os::unix::fs::symlink(&outside_pdf, &path).expect("symlink should be created");

    let result = call(&client, "source_view", json!({ "at": sentence })).await;
    let error = tool_error(&result);
    assert_eq!(error["error"], "blocked_source", "{error}");

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

/// #268: a drawn embedded image whose `Width × Height` exceeds `max_pixels`
/// answers `too_large` instead of rendering without the image: the hint names
/// the page, the measured count and the configured limit, and the same handle
/// renders as a PNG block once the limit covers the image.
#[tokio::test]
async fn v2_stdio_source_view_reports_embedded_image_pixel_limit_and_renders_above_it() {
    let root = tempfile::tempdir().expect("source root should be created");
    let path = root.path().join("big_pixels.pdf");
    tokio::fs::write(
        &path,
        std::fs::read("tests/fixtures/source_view/big_pixels.pdf").unwrap(),
    )
    .await
    .expect("fixture should be copied");

    // The default pixel limit (4,000,000) rejects the drawn 5,000,000-pixel
    // image; no env override is set for this leg.
    let client = start_client(root.path(), &root.path().join("state-default")).await;
    let opened = v2_open(&client, &path).await;
    let doc = opened["doc"].as_str().expect("doc handle").to_owned();
    let first = payload(&v2_read(&client, &doc, json!({})).await);
    let sentence = first["at"].as_str().expect("sentence handle").to_owned();
    let result = call(&client, "source_view", json!({ "at": sentence })).await;
    let error = tool_error(&result);
    assert_eq!(error["error"], "too_large", "{error}");
    let hint = error["hint"].as_str().expect("a too_large hint");
    assert!(hint.contains("第 1 页"), "{hint}");
    assert!(hint.contains("5,000,000"), "{hint}");
    assert!(hint.contains("4,000,000"), "{hint}");
    assert!(hint.contains("read"), "{hint}");
    assert!(
        error.get("retry").is_none(),
        "a too_large answer is not retryable: {error}"
    );
    client
        .cancel()
        .await
        .expect("MCP process should close cleanly");

    // The deployment profile's 16M-pixel budget covers the image, so the page
    // renders as a PNG image block on page 1.
    let client = start_client_with_env(
        root.path(),
        &root.path().join("state-16mp"),
        &[("READING_MCP_SOURCE_VIEW_MAX_PIXELS", "16000000")],
    )
    .await;
    let opened = v2_open(&client, &path).await;
    let doc = opened["doc"].as_str().expect("doc handle").to_owned();
    let first = payload(&v2_read(&client, &doc, json!({})).await);
    let sentence = first["at"].as_str().expect("sentence handle").to_owned();
    let result = call(&client, "source_view", json!({ "at": sentence })).await;
    assert!(
        result.is_error != Some(true),
        "the same page renders above the image's pixel count: {result:?}"
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
    client
        .cancel()
        .await
        .expect("MCP process should close cleanly");
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

// #268: renderer-level coverage of the drawn-image pixel check, through the
// production file-worker path so the assertion exercises the same code the
// parent process drives. Default options except `max_pixels`.

fn source_view_options(max_pixels: u64) -> SourceViewRenderOptions {
    SourceViewRenderOptions {
        dpi: 144,
        max_pages: 2_000,
        max_width: 2_400,
        max_height: 3_200,
        max_pixels,
        max_image_bytes: 8 * 1024 * 1024,
        max_decoded_stream_bytes: 16 * 1024 * 1024,
    }
}

fn render_source_view(
    bytes: Vec<u8>,
    max_pixels: u64,
) -> Result<RenderedSourceView, ApplicationError> {
    FileProcessIsolatedPdfSourceViewRenderer::with_executable(
        std::path::PathBuf::from(env!("CARGO_BIN_EXE_reading-mcp")),
        Duration::from_secs(30),
    )
    .render(
        bytes,
        MediaType("application/pdf".into()),
        1,
        source_view_options(max_pixels),
    )
}

fn render_source_view_fixture(
    fixture: &str,
    max_pixels: u64,
) -> Result<RenderedSourceView, ApplicationError> {
    render_source_view(
        std::fs::read(format!("tests/fixtures/source_view/{fixture}.pdf")).unwrap(),
        max_pixels,
    )
}

fn embedded_image_limit(error: &ApplicationError) -> SourceViewLimitDetail {
    let ApplicationError::SourceViewLimitExceeded(detail) = error else {
        panic!("expected a source-view limit rejection: {error:?}");
    };
    *detail
}

/// A drawn DeviceGray 2,500×2,000 image is over the default limit; the first
/// oversized measurement and the limit are reported.
#[test]
fn source_view_rejects_a_drawn_image_over_the_pixel_limit() {
    let error = render_source_view_fixture("big_pixels", 4_000_000)
        .expect_err("a drawn 5,000,000-pixel image must reject");
    assert_eq!(
        embedded_image_limit(&error),
        SourceViewLimitDetail::new(
            1,
            SourceViewLimitKind::EmbeddedImagePixels,
            Some(5_000_000),
            Some(4_000_000),
        )
    );
}

/// The comparison is strictly `>`, matching `pdf-syntax`: one pixel under the
/// drawn count still rejects, while the limit itself renders.
#[test]
fn source_view_rejects_one_pixel_below_the_image() {
    let error = render_source_view_fixture("big_pixels", 4_999_999)
        .expect_err("5,000,000 pixels is over a 4,999,999 limit");
    assert_eq!(
        embedded_image_limit(&error).kind,
        SourceViewLimitKind::EmbeddedImagePixels
    );
}

/// At exactly `max_pixels` the page renders and the image is visibly drawn:
/// the fixture's rectangle is gray level 40, so most of its pixels are dark —
/// before this fix that area was blank.
#[test]
fn source_view_renders_an_image_at_exactly_the_pixel_limit() {
    let rendered = render_source_view_fixture("big_pixels", 5_000_000)
        .expect("5,000,000 pixels equals the limit and renders");
    let decoded = image::load_from_memory(&rendered.bytes)
        .expect("result should be a PNG")
        .to_rgba8();
    // The 2500×2000 image is letterboxed into its (72, 360)-(300, 588) pt
    // rectangle: the drawn area keeps the 1.25 aspect, so it covers
    // (72, 382.8)-(300, 565.2) pt of the 595×842 pt page — inset a pixel to
    // keep anti-aliased edges out of the count.
    let x0 = (72.0 / 595.0 * rendered.width as f32) as u32 + 2;
    let x1 = (300.0 / 595.0 * rendered.width as f32) as u32 - 2;
    let y0 = (382.8 / 842.0 * rendered.height as f32) as u32 + 2;
    let y1 = (565.2 / 842.0 * rendered.height as f32) as u32 - 2;
    let mut dark = 0_u64;
    let mut total = 0_u64;
    for y in y0..y1 {
        for x in x0..x1 {
            let pixel = decoded.get_pixel(x, y).0;
            total += 1;
            if pixel[..3].iter().all(|channel| *channel < 80) {
                dark += 1;
            }
        }
    }
    assert!(
        dark * 10 > total * 9,
        "the drawn image's rectangle should be mostly dark, {dark}/{total}"
    );
}

/// The same image drawn only inside a nested form XObject is found by the
/// `Do` walk and rejects with the same numbers.
#[test]
fn source_view_rejects_an_image_drawn_inside_a_form_xobject() {
    let error = render_source_view_fixture("big_pixels_form", 4_000_000)
        .expect_err("a drawn 5,000,000-pixel image in a form must reject");
    assert_eq!(
        embedded_image_limit(&error),
        SourceViewLimitDetail::new(
            1,
            SourceViewLimitKind::EmbeddedImagePixels,
            Some(5_000_000),
            Some(4_000_000),
        )
    );
}

/// An image the page's resources name but its content never draws must not
/// reject: the walk follows `Do` operators, not the resource dictionary.
#[test]
fn source_view_renders_a_page_that_lists_but_never_draws_the_image() {
    render_source_view_fixture("big_pixels_unused", 4_000_000)
        .expect("an undrawn image stays invisible, as before");
}

/// `/SMask` image data decodes through the same pixel-limited path, so an
/// oversized soft mask rejects the page that draws the masked image.
#[test]
fn source_view_rejects_an_oversized_smask_image() {
    let error = render_source_view_fixture("big_smask", 4_000_000)
        .expect_err("a drawn image whose /SMask is 5,000,000 pixels must reject");
    assert_eq!(
        embedded_image_limit(&error),
        SourceViewLimitDetail::new(
            1,
            SourceViewLimitKind::EmbeddedImagePixels,
            Some(5_000_000),
            Some(4_000_000),
        )
    );
}

/// `pdf-interpret` decodes an `/SMask` stream through `ImageXObject::new`
/// without checking its `/Subtype`, so an oversized soft mask rejects even
/// when it carries none.
#[test]
fn source_view_rejects_an_oversized_smask_without_a_subtype() {
    let error = render_source_view(
        build_masked_image_pdf(
            Some((
                dictionary! {
                    "W" => 2500,
                    "H" => 2000,
                    "ColorSpace" => "DeviceGray",
                    "BitsPerComponent" => 8,
                },
                vec![0u8; 100],
            )),
            None,
        ),
        4_000_000,
    )
    .expect_err("a 5,000,000-pixel /SMask with no /Subtype must reject");
    assert_eq!(
        embedded_image_limit(&error),
        SourceViewLimitDetail::new(
            1,
            SourceViewLimitKind::EmbeddedImagePixels,
            Some(5_000_000),
            Some(4_000_000),
        )
    );
}

/// The interpreter reaches a `/Mask` stream only when there is no `/SMask`
/// stream, so an oversized `/Mask` shadowed by an in-limit `/SMask` never
/// rejects — a page cannot fail on a mask the interpreter does not decode.
#[test]
fn source_view_ignores_an_oversized_mask_shadowed_by_smask() {
    render_source_view(
        build_masked_image_pdf(
            Some((
                dictionary! {
                    "Type" => "XObject",
                    "Subtype" => "Image",
                    "W" => 10,
                    "H" => 10,
                    "ColorSpace" => "DeviceGray",
                    "BitsPerComponent" => 8,
                },
                vec![0u8; 100],
            )),
            Some((
                dictionary! {
                    "Type" => "XObject",
                    "Subtype" => "Image",
                    "W" => 2500,
                    "H" => 2000,
                    "ColorSpace" => "DeviceGray",
                    "BitsPerComponent" => 8,
                },
                vec![0u8; 100],
            )),
        ),
        4_000_000,
    )
    .expect("an /SMask stream shadows /Mask, so the page renders");
}

/// A corrupt image is not a limit rejection: the stream is invalid for its
/// filter, so the renderer keeps today's behaviour and renders without it.
#[test]
fn source_view_renders_a_corrupt_image_the_same_as_before() {
    render_source_view_fixture("corrupt_image", 4_000_000)
        .expect("a corrupt image drops silently, as on main");
}

/// A form XObject that names itself in its own resources and draws itself
/// terminates the `Do` walk (the visited set breaks the cycle). The page then
/// fails inside the interpreter the same way it does on `main` — the worker
/// reports no limit record, so the parent sees a plain worker failure.
#[test]
fn source_view_form_cycle_terminates_the_walk() {
    let error = render_source_view(build_form_cycle_pdf(), 4_000_000)
        .expect_err("the self-drawing form fails during rendering, as on main");
    assert!(
        matches!(error, ApplicationError::SourceViewFailed(_)),
        "a cyclic form is a worker failure, not a limit rejection: {error:?}"
    );
}

/// One page whose only content draws `/Fm0`, a form XObject that lists itself
/// in its own `/Resources` and draws itself.
fn build_form_cycle_pdf() -> Vec<u8> {
    let mut document = PdfDocument::with_version("1.5");
    let pages_id = document.new_object_id();
    let form_id = document.new_object_id();
    let resources_id = document.add_object(dictionary! {
        "XObject" => dictionary! { "Fm0" => form_id },
    });
    document.objects.insert(
        form_id,
        Object::Stream(Stream::new(
            dictionary! {
                "Type" => "XObject",
                "Subtype" => "Form",
                "BBox" => vec![0.into(), 0.into(), 100.into(), 100.into()],
                "Resources" => dictionary! {
                    "XObject" => dictionary! { "Fm0" => form_id },
                },
            },
            b"/Fm0 Do\n".to_vec(),
        )),
    );
    let content_id = document.add_object(Stream::new(dictionary! {}, b"/Fm0 Do\n".to_vec()));
    let page_id = document.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => pages_id,
        "Contents" => content_id,
        "Resources" => resources_id,
    });
    document.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => vec![Object::Reference(page_id)],
            "Count" => 1,
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

/// One page that draws `/Im0`, a 10×10 DeviceGray image whose dict carries the
/// given `/SMask` and `/Mask` streams.
fn build_masked_image_pdf(
    smask: Option<(lopdf::Dictionary, Vec<u8>)>,
    mask: Option<(lopdf::Dictionary, Vec<u8>)>,
) -> Vec<u8> {
    let mut document = PdfDocument::with_version("1.5");
    let pages_id = document.new_object_id();
    let mut image_dict = dictionary! {
        "Type" => "XObject",
        "Subtype" => "Image",
        "W" => 10,
        "H" => 10,
        "ColorSpace" => "DeviceGray",
        "BitsPerComponent" => 8,
    };
    for (key, entry) in [("SMask", smask), ("Mask", mask)] {
        let Some((dict, bytes)) = entry else {
            continue;
        };
        let mask_id = document.add_object(Stream::new(dict, bytes));
        image_dict.set(key, mask_id);
    }
    let image_id = document.add_object(Stream::new(image_dict, vec![0u8; 100]));
    let resources_id = document.add_object(dictionary! {
        "XObject" => dictionary! { "Im0" => image_id },
    });
    let content = Content {
        operations: vec![
            Operation::new("q", vec![]),
            Operation::new(
                "cm",
                vec![
                    100.into(),
                    0.into(),
                    0.into(),
                    100.into(),
                    72.into(),
                    360.into(),
                ],
            ),
            Operation::new("Do", vec!["Im0".into()]),
            Operation::new("Q", vec![]),
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
        "Resources" => resources_id,
    });
    document.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => vec![Object::Reference(page_id)],
            "Count" => 1,
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
