use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use lopdf::content::{Content, Operation};
use lopdf::{Document as PdfDocument, Object, Stream, dictionary};
use reading_mcp::application::open_document::{OpenDocumentCommand, OpenDocumentUseCase};
use reading_mcp::application::ports::{
    ApplicationError, DocumentRepository, HttpUrlPolicy, LocalSourceAccess, RawResourceCache,
    ResolvedLocalSource, RetrievalOptions, RetrievedResource, Retriever, SourcePolicy,
    SourceViewRenderOptions, SourceViewRenderer,
};
use reading_mcp::application::source_view::{
    GetSourceViewCommand, SourceViewLimits, SourceViewRepresentation, SourceViewUseCase,
};
use reading_mcp::domain::{DocumentSource, MediaType, SectionId, TextLocator};
use reading_mcp::infrastructure::{
    FileRawResourceCache, InMemoryDocumentRepository, InMemoryRawResourceCache,
    InMemoryTextUnitIndex,
};
use reading_mcp::parsing::{
    FileProcessIsolatedPdfSourceViewRenderer, ParserRouter, PdfSourceViewRenderer,
};
use reading_mcp::retrieval::{
    FileRetriever, HttpRetriever, HttpRetrieverConfig, LimitedFileRetriever, LocalFileSourceAccess,
    LocalFileSourcePolicy, RevalidatingHttpRetriever,
};
use reading_mcp::security::PublicHttpAccessPolicy;
use sha2::{Digest, Sha256};
use tempfile::tempdir;

#[tokio::test]
async fn locator_resolves_to_original_pdf_page_and_renders_visual_fixture() {
    let directory = tempdir().expect("temporary directory should be created");
    let path = directory.path().join("fidelity.pdf");
    tokio::fs::write(&path, build_fidelity_pdf())
        .await
        .expect("PDF fixture should be written");

    let repository = Arc::new(InMemoryDocumentRepository::default());
    let index = Arc::new(InMemoryTextUnitIndex::default());
    let opened = OpenDocumentUseCase::with_text_unit_index(
        Arc::new(LocalFileSourcePolicy::allow_roots([directory.path()])),
        Arc::new(FileRetriever),
        Arc::new(ParserRouter::phase4()),
        repository.clone(),
        index.clone(),
        Arc::new(reading_mcp::infrastructure::InMemorySearchIndex::default()),
    )
    .execute(OpenDocumentCommand {
        source: DocumentSource(path.to_string_lossy().into_owned()),
        options: RetrievalOptions::default(),
    })
    .await
    .expect("PDF should open");

    let document = repository
        .get(&opened.document_id)
        .await
        .expect("stored document")
        .expect("the opened document is stored");
    let section_id = SectionId("section://page-2".into());
    let section = document.find_section(&section_id).expect("page-2 section");
    let locator = TextLocator::for_paragraph(
        &document,
        section,
        document
            .paragraph_text_units()
            .units
            .iter()
            .find(|unit| unit.owner_section_id == section_id)
            .expect("a paragraph on page 2"),
    );

    let result = SourceViewUseCase::new(
        repository.clone(),
        Arc::new(FileRetriever),
        Arc::new(PdfSourceViewRenderer),
        SourceViewLimits::default(),
    )
    .execute(GetSourceViewCommand {
        document_id: opened.document_id.clone(),
        target_locator: locator.clone(),
        representation: SourceViewRepresentation::Original,
        dpi: Some(144),
    })
    .await
    .expect("source view should render");

    assert_eq!(result.page_number, 2);
    assert_eq!(result.page_count, 2);
    assert_eq!(result.target_locator, locator);
    assert_eq!(result.view.media_type.0, "image/png");
    assert_eq!(&result.view.bytes[..8], b"\x89PNG\r\n\x1a\n");
    let decoded = image::load_from_memory(&result.view.bytes).expect("result should be PNG");
    assert_eq!(decoded.width(), result.view.width);
    assert_eq!(decoded.height(), result.view.height);
    assert!(
        decoded
            .to_rgba8()
            .pixels()
            .any(|pixel| pixel.0 != [255, 255, 255, 255])
    );
}

#[tokio::test]
async fn source_view_fails_closed_when_source_bytes_change() {
    let directory = tempdir().expect("temporary directory should be created");
    let path = directory.path().join("fidelity.pdf");
    let original = build_fidelity_pdf();
    tokio::fs::write(&path, &original)
        .await
        .expect("PDF fixture should be written");

    let repository = Arc::new(InMemoryDocumentRepository::default());
    let index = Arc::new(InMemoryTextUnitIndex::default());
    let opened = OpenDocumentUseCase::with_text_unit_index(
        Arc::new(LocalFileSourcePolicy::allow_roots([directory.path()])),
        Arc::new(FileRetriever),
        Arc::new(ParserRouter::phase4()),
        repository.clone(),
        index,
        Arc::new(reading_mcp::infrastructure::InMemorySearchIndex::default()),
    )
    .execute(OpenDocumentCommand {
        source: DocumentSource(path.to_string_lossy().into_owned()),
        options: RetrievalOptions::default(),
    })
    .await
    .expect("PDF should open");
    let document = repository
        .get(&opened.document_id)
        .await
        .expect("repository read")
        .expect("document should be persisted");
    let locator =
        reading_mcp::domain::TextLocator::for_section(&document, &document.root_sections[1]);

    let mut changed = original;
    changed.extend_from_slice(b"changed source bytes");
    tokio::fs::write(&path, changed)
        .await
        .expect("changed source should be written");

    let error = SourceViewUseCase::new(
        repository,
        Arc::new(FileRetriever),
        Arc::new(PdfSourceViewRenderer),
        SourceViewLimits::default(),
    )
    .execute(GetSourceViewCommand {
        document_id: opened.document_id,
        target_locator: locator,
        representation: SourceViewRepresentation::Original,
        dpi: None,
    })
    .await
    .expect_err("changed source must not render through an old locator");
    assert!(matches!(
        error,
        reading_mcp::application::ports::ApplicationError::StaleLocator(_)
    ));
}

#[tokio::test]
async fn source_view_enforces_render_dimensions_before_allocating_image() {
    let resource = RetrievedResource {
        source: DocumentSource("memory:fidelity.pdf".into()),
        final_source: DocumentSource("memory:fidelity.pdf".into()),
        media_type: MediaType("application/pdf".into()),
        bytes: build_fidelity_pdf(),
        etag: None,
        last_modified: None,
        metadata: BTreeMap::new(),
    };
    let renderer = PdfSourceViewRenderer;
    let error = SourceViewRenderer::render(
        &renderer,
        resource.bytes,
        resource.media_type,
        1,
        SourceViewRenderOptions {
            dpi: 144,
            max_pages: 2_000,
            max_width: 100,
            max_height: 100,
            max_pixels: 10_000,
            max_image_bytes: 8 * 1024 * 1024,
            max_decoded_stream_bytes: 16 * 1024 * 1024,
        },
    )
    .expect_err("oversized source-view dimensions must be rejected");
    assert_eq!(
        error,
        reading_mcp::application::ports::ApplicationError::SourceViewLimitExceeded(
            reading_mcp::application::ports::SourceViewLimitDetail::new(
                1,
                reading_mcp::application::ports::SourceViewLimitKind::PageDimensions,
                Some(1_190),
                Some(100),
            )
        )
    );
}

/// #272: the source-view local-file resolver refuses exactly what the retrieval
/// chain refuses — the same error value, from the same shared checks.
#[tokio::test]
async fn local_source_access_refuses_what_the_retriever_refuses() {
    let root = tempdir().expect("root directory should be created");
    let outside = tempdir().expect("outside directory should be created");

    let inside = root.path().join("inside.pdf");
    tokio::fs::write(&inside, build_fidelity_pdf())
        .await
        .expect("inside fixture should be written");
    let outside_pdf = outside.path().join("outside.pdf");
    tokio::fs::write(&outside_pdf, build_fidelity_pdf())
        .await
        .expect("outside fixture should be written");

    // Outside the roots: the policy answer today is the BlockedSource the open
    // path reports; the resolver must give the identical error.
    let access = LocalFileSourceAccess::new(
        LocalFileSourcePolicy::allow_roots([root.path()]),
        usize::MAX,
    );
    let outside_source = DocumentSource(outside_pdf.to_string_lossy().into_owned());
    let policy_error = LocalFileSourcePolicy::allow_roots([root.path()])
        .validate(&outside_source)
        .await
        .expect_err("a file outside the roots is refused");
    let resolve_error = access
        .resolve(&outside_source)
        .await
        .expect_err("the resolver refuses the same file");
    assert_eq!(resolve_error, policy_error);

    // A symlink inside the roots that resolves outside them: same refusal.
    let link = root.path().join("link.pdf");
    std::os::unix::fs::symlink(&outside_pdf, &link).expect("symlink should be created");
    let link_source = DocumentSource(link.to_string_lossy().into_owned());
    let policy_error = LocalFileSourcePolicy::allow_roots([root.path()])
        .validate(&link_source)
        .await
        .expect_err("a symlink escaping the roots is refused");
    let resolve_error = access
        .resolve(&link_source)
        .await
        .expect_err("the resolver refuses the same symlink");
    assert_eq!(resolve_error, policy_error);

    // Over the size limit: the retriever's own error, verbatim.
    let small_limit =
        LocalFileSourceAccess::new(LocalFileSourcePolicy::allow_roots([root.path()]), 16);
    let inside_source = DocumentSource(inside.to_string_lossy().into_owned());
    let retriever_error = LimitedFileRetriever::new(16)
        .retrieve(&inside_source, &RetrievalOptions::default())
        .await
        .expect_err("the retriever refuses an oversized file");
    let resolve_error = small_limit
        .resolve(&inside_source)
        .await
        .expect_err("the resolver refuses the same oversized file");
    assert_eq!(resolve_error, retriever_error);

    // And an allowed file resolves to its canonical path and `file://` source.
    let access = LocalFileSourceAccess::new(
        LocalFileSourcePolicy::allow_roots([root.path()]),
        usize::MAX,
    );
    let resolved = access
        .resolve(&inside_source)
        .await
        .expect("an allowed file resolves");
    assert_eq!(
        resolved.final_source,
        DocumentSource(format!(
            "file://{}",
            inside.canonicalize().unwrap().to_string_lossy()
        ))
    );
    let resolved = resolved.path;
    assert_eq!(
        resolved,
        inside.canonicalize().expect("canonical inside path")
    );
}

/// #272: `source_view` on a local PDF goes through the file worker against the
/// file itself; rewriting the bytes under the same path stays fail-closed.
#[tokio::test]
async fn source_view_local_file_fails_closed_when_source_bytes_change() {
    let directory = tempdir().expect("temporary directory should be created");
    let path = directory.path().join("fidelity.pdf");
    let original = build_fidelity_pdf();
    tokio::fs::write(&path, &original)
        .await
        .expect("PDF fixture should be written");

    let repository = Arc::new(InMemoryDocumentRepository::default());
    let index = Arc::new(InMemoryTextUnitIndex::default());
    let opened = OpenDocumentUseCase::with_text_unit_index(
        Arc::new(LocalFileSourcePolicy::allow_roots([directory.path()])),
        Arc::new(FileRetriever),
        Arc::new(ParserRouter::phase4()),
        repository.clone(),
        index,
        Arc::new(reading_mcp::infrastructure::InMemorySearchIndex::default()),
    )
    .execute(OpenDocumentCommand {
        source: DocumentSource(path.to_string_lossy().into_owned()),
        options: RetrievalOptions::default(),
    })
    .await
    .expect("PDF should open");
    let document = repository
        .get(&opened.document_id)
        .await
        .expect("repository read")
        .expect("document should be persisted");
    let locator =
        reading_mcp::domain::TextLocator::for_section(&document, &document.root_sections[1]);

    let mut changed = original;
    changed.extend_from_slice(b"changed source bytes");
    tokio::fs::write(&path, changed)
        .await
        .expect("changed source should be written");

    let renderer = FileProcessIsolatedPdfSourceViewRenderer::with_executable(
        std::path::PathBuf::from(env!("CARGO_BIN_EXE_reading-mcp")),
        std::time::Duration::from_secs(30),
    );
    let error = SourceViewUseCase::new(
        repository,
        Arc::new(FileRetriever),
        Arc::new(renderer),
        SourceViewLimits::default(),
    )
    .with_local_source_access(Arc::new(LocalFileSourceAccess::new(
        LocalFileSourcePolicy::allow_roots([directory.path()]),
        usize::MAX,
    )))
    .execute(GetSourceViewCommand {
        document_id: opened.document_id,
        target_locator: locator,
        representation: SourceViewRepresentation::Original,
        dpi: None,
    })
    .await
    .expect_err("changed source must not render through an old locator");
    assert!(matches!(error, ApplicationError::StaleLocator(_)));
}

/// #272: a worker that saw different bytes than the expected `content_hash`
/// reports `source_sha256` and stops; the parent answers `StaleLocator` and
/// returns no PNG.
#[test]
fn isolated_worker_hash_mismatch_answers_stale_locator() {
    let directory = tempdir().expect("temporary directory should be created");
    let path = directory.path().join("fidelity.pdf");
    std::fs::write(&path, build_fidelity_pdf()).expect("PDF fixture should be written");

    let renderer = FileProcessIsolatedPdfSourceViewRenderer::with_executable(
        std::path::PathBuf::from(env!("CARGO_BIN_EXE_reading-mcp")),
        std::time::Duration::from_secs(30),
    );
    let error = renderer
        .render_local_file(
            &path,
            "sha256:0000000000000000000000000000000000000000000000000000000000000000",
            MediaType("application/pdf".into()),
            1,
            SourceViewRenderOptions {
                dpi: 72,
                max_pages: 2_000,
                max_width: 2_400,
                max_height: 3_200,
                max_pixels: 4_000_000,
                max_image_bytes: 8 * 1024 * 1024,
                max_decoded_stream_bytes: 16 * 1024 * 1024,
            },
        )
        .expect_err("a hash-mismatched file must not render");
    assert!(
        matches!(error, ApplicationError::StaleLocator(_)),
        "hash mismatch stays stale: {error:?}"
    );
}

/// #272: a worker that reports no `source_sha256` proves nothing about identity;
/// the missing field is a worker failure, never a pass.
#[cfg(unix)]
#[test]
fn isolated_renderer_fails_a_worker_that_omits_source_sha256() {
    use std::os::unix::fs::PermissionsExt;

    let directory = tempfile::tempdir().expect("worker directory should be created");
    let worker = directory.path().join("no-hash-worker.sh");
    // Worker argv: flag input output metadata … — write metadata without the hash.
    std::fs::write(
        &worker,
        "#!/bin/sh\nprintf '{\"width\":1,\"height\":1,\"page_count\":1,\"image_bytes\":1}' > \"$4\"\nprintf x > \"$3\"\n",
    )
    .expect("worker script should be written");
    let mut permissions = std::fs::metadata(&worker)
        .expect("worker metadata should be available")
        .permissions();
    permissions.set_mode(0o700);
    std::fs::set_permissions(&worker, permissions).expect("worker should be executable");

    let renderer = FileProcessIsolatedPdfSourceViewRenderer::with_executable(
        worker,
        std::time::Duration::from_secs(5),
    );
    let error = renderer
        .render(
            build_fidelity_pdf(),
            MediaType("application/pdf".into()),
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
        .expect_err("metadata without source_sha256 must not pass");
    assert!(
        matches!(error, ApplicationError::SourceViewFailed(_)),
        "missing source_sha256 is a worker failure: {error:?}"
    );
}

/// #272: a URL source keeps the buffered path — the retriever's bytes staged —
/// and renders exactly as before, even with the local-file resolver configured.
#[tokio::test]
async fn source_view_url_source_still_renders_through_the_staged_path() {
    let (endpoint, server) = spawn_pdf_http_fixture().await;
    let source = DocumentSource(format!("http://example.test:{}/doc.pdf", endpoint.port()));

    let retriever: Arc<dyn Retriever> = Arc::new(HttpRetriever::new(
        Arc::new(TestHttpPolicy { endpoint }),
        HttpRetrieverConfig::default(),
    ));
    let repository = Arc::new(InMemoryDocumentRepository::default());
    let index = Arc::new(InMemoryTextUnitIndex::default());
    let opened = OpenDocumentUseCase::with_text_unit_index(
        Arc::new(AllowAllSourcePolicy),
        retriever.clone(),
        Arc::new(ParserRouter::phase4()),
        repository.clone(),
        index,
        Arc::new(reading_mcp::infrastructure::InMemorySearchIndex::default()),
    )
    .execute(OpenDocumentCommand {
        source: source.clone(),
        options: RetrievalOptions::default(),
    })
    .await
    .expect("PDF should open over the fixture endpoint");
    let document = repository
        .get(&opened.document_id)
        .await
        .expect("repository read")
        .expect("document should be persisted");
    let locator =
        reading_mcp::domain::TextLocator::for_section(&document, &document.root_sections[0]);

    // The resolver fails loudly if a URL source ever reaches it.
    let renderer = FileProcessIsolatedPdfSourceViewRenderer::with_executable(
        std::path::PathBuf::from(env!("CARGO_BIN_EXE_reading-mcp")),
        std::time::Duration::from_secs(30),
    );
    let result = SourceViewUseCase::new(
        repository,
        retriever,
        Arc::new(renderer),
        SourceViewLimits::default(),
    )
    .with_local_source_access(Arc::new(RefusingLocalSourceAccess))
    .execute(GetSourceViewCommand {
        document_id: opened.document_id,
        target_locator: locator,
        representation: SourceViewRepresentation::Original,
        dpi: Some(72),
    })
    .await
    .expect("a URL source renders through the buffered path");

    assert_eq!(result.view.media_type.0, "image/png");
    assert_eq!(&result.view.bytes[..8], b"\x89PNG\r\n\x1a\n");
    server.abort();

    // The same bytes as a local file must produce the same PNG through the
    // mapped path that the URL source produced through the staged path.
    let directory = tempdir().expect("local root should be created");
    let path = directory.path().join("fidelity.pdf");
    tokio::fs::write(&path, build_fidelity_pdf())
        .await
        .expect("PDF fixture should be written");
    let local_repository = Arc::new(InMemoryDocumentRepository::default());
    let local_opened = OpenDocumentUseCase::with_text_unit_index(
        Arc::new(LocalFileSourcePolicy::allow_roots([directory.path()])),
        Arc::new(FileRetriever),
        Arc::new(ParserRouter::phase4()),
        local_repository.clone(),
        Arc::new(InMemoryTextUnitIndex::default()),
        Arc::new(reading_mcp::infrastructure::InMemorySearchIndex::default()),
    )
    .execute(OpenDocumentCommand {
        source: DocumentSource(path.to_string_lossy().into_owned()),
        options: RetrievalOptions::default(),
    })
    .await
    .expect("PDF should open as a local file");
    let local_document = local_repository
        .get(&local_opened.document_id)
        .await
        .expect("repository read")
        .expect("document should be persisted");
    let local_locator = reading_mcp::domain::TextLocator::for_section(
        &local_document,
        &local_document.root_sections[0],
    );
    let local = SourceViewUseCase::new(
        local_repository,
        Arc::new(FileRetriever),
        Arc::new(FileProcessIsolatedPdfSourceViewRenderer::with_executable(
            std::path::PathBuf::from(env!("CARGO_BIN_EXE_reading-mcp")),
            std::time::Duration::from_secs(30),
        )),
        SourceViewLimits::default(),
    )
    .with_local_source_access(Arc::new(LocalFileSourceAccess::new(
        LocalFileSourcePolicy::allow_roots([directory.path()]),
        usize::MAX,
    )))
    .execute(GetSourceViewCommand {
        document_id: local_opened.document_id,
        target_locator: local_locator,
        representation: SourceViewRepresentation::Original,
        dpi: Some(72),
    })
    .await
    .expect("a local source renders through the mapped path");

    assert_eq!(
        local.view.bytes, result.view.bytes,
        "the mapped local path must render the same PNG the staged URL path produced"
    );
}

struct AllowAllSourcePolicy;

#[async_trait::async_trait]
impl SourcePolicy for AllowAllSourcePolicy {
    async fn validate(&self, _source: &DocumentSource) -> Result<(), ApplicationError> {
        Ok(())
    }
}

struct RefusingLocalSourceAccess;

#[async_trait::async_trait]
impl LocalSourceAccess for RefusingLocalSourceAccess {
    async fn resolve(
        &self,
        source: &DocumentSource,
    ) -> Result<ResolvedLocalSource, ApplicationError> {
        Err(ApplicationError::SourceViewFailed(format!(
            "the local-file resolver must never see {source:?}"
        )))
    }
}

struct TestHttpPolicy {
    endpoint: std::net::SocketAddr,
}

#[async_trait::async_trait]
impl reading_mcp::security::HttpAccessPolicy for TestHttpPolicy {
    fn parse_and_validate(&self, source: &DocumentSource) -> Result<url::Url, ApplicationError> {
        let url = url::Url::parse(&source.0)
            .map_err(|error| ApplicationError::InvalidRequest(error.to_string()))?;
        self.validate_url(&url)?;
        Ok(url)
    }

    fn validate_url(&self, url: &url::Url) -> Result<(), ApplicationError> {
        if url.scheme() != "http" {
            return Err(ApplicationError::BlockedSource(
                "test policy only allows HTTP".into(),
            ));
        }
        Ok(())
    }

    async fn resolve_public_endpoint(
        &self,
        url: &url::Url,
    ) -> Result<std::net::SocketAddr, ApplicationError> {
        self.validate_url(url)?;
        Ok(std::net::SocketAddr::new(
            self.endpoint.ip(),
            url.port().unwrap_or(self.endpoint.port()),
        ))
    }
}

/// The `spawn_http_fixture` pattern of the HTTP retrieval tests, serving the
/// PDF fixture bytes so the URL-source path is exercised end to end.
async fn spawn_pdf_http_fixture() -> (std::net::SocketAddr, tokio::task::JoinHandle<()>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("fixture listener should bind");
    let address = listener.local_addr().expect("fixture address should exist");
    let body = build_fidelity_pdf();
    let handle = tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                break;
            };
            let body = body.clone();
            tokio::spawn(async move {
                let mut buffer = vec![0u8; 4096];
                let Ok(_read) = socket.read(&mut buffer).await else {
                    return;
                };
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/pdf\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let mut response = response.into_bytes();
                response.extend_from_slice(&body);
                let _ = socket.write_all(&response).await;
            });
        }
    });
    (address, handle)
}

fn build_fidelity_pdf() -> Vec<u8> {
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
    let mut page_ids = Vec::new();

    for operations in [
        vec![
            Operation::new("BT", vec![]),
            Operation::new("Tf", vec!["F1".into(), 18.into()]),
            Operation::new("Td", vec![72.into(), 720.into()]),
            Operation::new("Tj", vec![Object::string_literal("LEFT COLUMN")]),
            Operation::new("ET", vec![]),
            Operation::new("BT", vec![]),
            Operation::new("Tf", vec!["F1".into(), 18.into()]),
            Operation::new("Td", vec![320.into(), 720.into()]),
            Operation::new("Tj", vec![Object::string_literal("RIGHT COLUMN")]),
            Operation::new("ET", vec![]),
        ],
        vec![
            Operation::new("BT", vec![]),
            Operation::new("Tf", vec!["F1".into(), 18.into()]),
            Operation::new("Td", vec![72.into(), 720.into()]),
            Operation::new("Tj", vec![Object::string_literal("Equation: E = mc^2")]),
            Operation::new("ET", vec![]),
            Operation::new("BT", vec![]),
            Operation::new("Tf", vec!["F1".into(), 14.into()]),
            Operation::new("Td", vec![100.into(), 440.into()]),
            Operation::new("Tj", vec![Object::string_literal("Figure 1")]),
            Operation::new("ET", vec![]),
            Operation::new("RG", vec![0.into(), 0.into(), 0.into()]),
            Operation::new("re", vec![72.into(), 500.into(), 180.into(), 120.into()]),
            Operation::new("S", vec![]),
            Operation::new("re", vec![300.into(), 500.into(), 220.into(), 120.into()]),
            Operation::new("S", vec![]),
            Operation::new("m", vec![410.into(), 500.into()]),
            Operation::new("l", vec![410.into(), 620.into()]),
            Operation::new("m", vec![300.into(), 560.into()]),
            Operation::new("l", vec![520.into(), 560.into()]),
            Operation::new("S", vec![]),
            Operation::new("BT", vec![]),
            Operation::new("Tf", vec!["F1".into(), 14.into()]),
            Operation::new("Td", vec![330.into(), 590.into()]),
            Operation::new("Tj", vec![Object::string_literal("TABLE 1")]),
            Operation::new("ET", vec![]),
        ],
    ] {
        let content = Content { operations };
        let content_id = document.add_object(Stream::new(
            dictionary! {},
            content.encode().expect("fixture content should encode"),
        ));
        page_ids.push(document.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "Contents" => content_id,
        }));
    }

    document.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => page_ids.iter().copied().map(Object::Reference).collect::<Vec<_>>(),
            "Count" => page_ids.len() as i64,
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

// ---------------------------------------------------------------------------
// #274: a URL source with a usable raw-cache entry renders its stored body —
// the worker maps the cache file and checks its sha256 against `content_hash`
// — without contacting the origin. Anything else falls back to today's
// buffered path, revalidation and errors included.
// ---------------------------------------------------------------------------

/// A request-counting fixture serving the fidelity PDF with an ETag: a
/// matching `If-None-Match` answers `304`, anything else the full body, so a
/// fallback through `RevalidatingHttpRetriever` is visible in `requests`.
struct PdfHttpFixture {
    endpoint: std::net::SocketAddr,
    requests: Arc<AtomicUsize>,
    server: tokio::task::JoinHandle<()>,
}

async fn spawn_counting_pdf_fixture() -> PdfHttpFixture {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("fixture listener should bind");
    let endpoint = listener.local_addr().expect("fixture address should exist");
    let requests = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&requests);
    let server = tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                break;
            };
            counted.fetch_add(1, Ordering::Relaxed);
            tokio::spawn(async move {
                let body = build_fidelity_pdf();
                let mut buffer = vec![0u8; 8192];
                let Ok(read) = socket.read(&mut buffer).await else {
                    return;
                };
                let request = String::from_utf8_lossy(&buffer[..read]).to_lowercase();
                let not_modified = request
                    .split("\r\n")
                    .any(|line| line == "if-none-match: \"pdf-v1\"");
                let response = if not_modified {
                    "HTTP/1.1 304 Not Modified\r\nETag: \"pdf-v1\"\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                        .as_bytes()
                        .to_vec()
                } else {
                    let mut response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/pdf\r\nETag: \"pdf-v1\"\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    )
                    .into_bytes();
                    response.extend_from_slice(&body);
                    response
                };
                let _ = socket.write_all(&response).await;
            });
        }
    });
    PdfHttpFixture {
        endpoint,
        requests,
        server,
    }
}

/// The result of opening the fixture PDF through the real revalidating
/// retriever chain: the stored document, a locator on its first page, and the
/// pieces `source_view` wiring and the assertions need.
struct UrlSourceView {
    source: DocumentSource,
    retriever: Arc<dyn Retriever>,
    policy: Arc<TestHttpPolicy>,
    repository: Arc<InMemoryDocumentRepository>,
    document_id: reading_mcp::domain::DocumentId,
    locator: TextLocator,
}

async fn open_url_pdf(
    fixture: &PdfHttpFixture,
    raw_cache: Arc<dyn RawResourceCache>,
) -> UrlSourceView {
    let source = DocumentSource(format!(
        "http://example.test:{}/doc.pdf",
        fixture.endpoint.port()
    ));
    let policy = Arc::new(TestHttpPolicy {
        endpoint: fixture.endpoint,
    });
    let http = Arc::new(HttpRetriever::new(
        policy.clone(),
        HttpRetrieverConfig::default(),
    ));
    let retriever: Arc<dyn Retriever> = Arc::new(RevalidatingHttpRetriever::new(http, raw_cache));
    let repository = Arc::new(InMemoryDocumentRepository::default());
    let opened = OpenDocumentUseCase::with_text_unit_index(
        Arc::new(AllowAllSourcePolicy),
        retriever.clone(),
        Arc::new(ParserRouter::phase4()),
        repository.clone(),
        Arc::new(InMemoryTextUnitIndex::default()),
        Arc::new(reading_mcp::infrastructure::InMemorySearchIndex::default()),
    )
    .execute(OpenDocumentCommand {
        source: source.clone(),
        options: RetrievalOptions::default(),
    })
    .await
    .expect("PDF should open over the fixture endpoint");
    let document = repository
        .get(&opened.document_id)
        .await
        .expect("repository read")
        .expect("document should be persisted");
    let locator =
        reading_mcp::domain::TextLocator::for_section(&document, &document.root_sections[0]);
    UrlSourceView {
        source,
        retriever,
        policy,
        repository,
        document_id: opened.document_id,
        locator,
    }
}

/// The production `source_view` wiring for a URL source: the process-isolated
/// renderer, the retriever chain that opened the document, its raw cache, its
/// URL-level HTTP policy, and `max_document_bytes`.
fn url_source_view(
    opened: &UrlSourceView,
    raw_cache: Arc<dyn RawResourceCache>,
    url_policy: Arc<dyn HttpUrlPolicy>,
    max_document_bytes: usize,
) -> SourceViewUseCase {
    SourceViewUseCase::new(
        opened.repository.clone(),
        opened.retriever.clone(),
        Arc::new(FileProcessIsolatedPdfSourceViewRenderer::with_executable(
            std::path::PathBuf::from(env!("CARGO_BIN_EXE_reading-mcp")),
            std::time::Duration::from_secs(30),
        )),
        SourceViewLimits::default(),
    )
    .with_url_source_cache(raw_cache, url_policy, max_document_bytes)
}

fn test_render_options() -> SourceViewRenderOptions {
    SourceViewRenderOptions {
        dpi: 72,
        max_pages: 2_000,
        max_width: 2_400,
        max_height: 3_200,
        max_pixels: 4_000_000,
        max_image_bytes: 8 * 1024 * 1024,
        max_decoded_stream_bytes: 16 * 1024 * 1024,
    }
}

fn render_fixture_bytes_as_local_file() -> reading_mcp::application::ports::RenderedSourceView {
    let directory = tempdir().expect("local root should be created");
    let path = directory.path().join("fidelity.pdf");
    let bytes = build_fidelity_pdf();
    std::fs::write(&path, &bytes).expect("PDF fixture should be written");
    let renderer = FileProcessIsolatedPdfSourceViewRenderer::with_executable(
        std::path::PathBuf::from(env!("CARGO_BIN_EXE_reading-mcp")),
        std::time::Duration::from_secs(30),
    );
    renderer
        .render_local_file(
            &path,
            &format!("sha256:{:x}", Sha256::digest(&bytes)),
            MediaType("application/pdf".into()),
            1,
            test_render_options(),
        )
        .expect("the same bytes render as a local file")
}

fn raw_cache_body(cache_dir: &std::path::Path) -> std::path::PathBuf {
    std::fs::read_dir(cache_dir.join("raw"))
        .expect("raw cache directory should exist")
        .flatten()
        .map(|entry| entry.path())
        .find(|path| path.extension().is_some_and(|ext| ext == "bin"))
        .expect("the raw cache holds one body file")
}

fn pdf_resource(source: &DocumentSource, bytes: Vec<u8>) -> RetrievedResource {
    RetrievedResource {
        source: source.clone(),
        final_source: source.clone(),
        media_type: MediaType("application/pdf".into()),
        bytes,
        etag: Some("\"pdf-v1\"".into()),
        last_modified: None,
        metadata: BTreeMap::new(),
    }
}

fn url_source_view_command(opened: &UrlSourceView) -> GetSourceViewCommand {
    GetSourceViewCommand {
        document_id: opened.document_id.clone(),
        target_locator: opened.locator.clone(),
        representation: SourceViewRepresentation::Original,
        dpi: Some(72),
    }
}

/// #274: a URL source with a usable raw-cache entry renders its stored body —
/// the worker maps the cache file and checks its sha256 — and the origin is
/// never contacted: 0 requests during the call and a PNG byte-identical to the
/// same bytes rendered as a local file.
#[tokio::test]
async fn source_view_url_source_renders_the_cached_body_without_a_request() {
    let fixture = spawn_counting_pdf_fixture().await;
    let cache_dir = tempdir().expect("cache directory should be created");
    let raw_cache = Arc::new(FileRawResourceCache::new(cache_dir.path()));
    let opened = open_url_pdf(&fixture, raw_cache.clone()).await;
    assert_eq!(
        fixture.requests.load(Ordering::Relaxed),
        1,
        "open fetched the document once"
    );

    let before = fixture.requests.load(Ordering::Relaxed);
    let result = url_source_view(&opened, raw_cache, opened.policy.clone(), usize::MAX)
        .execute(url_source_view_command(&opened))
        .await
        .expect("the cached body renders without a network round trip");

    assert_eq!(
        fixture.requests.load(Ordering::Relaxed) - before,
        0,
        "the cached path never contacts the origin"
    );
    assert_eq!(&result.view.bytes[..8], b"\x89PNG\r\n\x1a\n");
    assert_eq!(
        render_fixture_bytes_as_local_file().bytes,
        result.view.bytes,
        "the cached-body render is byte-identical to the mapped local render"
    );
    fixture.server.abort();
}

/// #274: the origin going away changes nothing while the cache entry is
/// usable — the render needs no network at all.
#[tokio::test]
async fn source_view_url_source_renders_the_cached_body_with_the_origin_gone() {
    let fixture = spawn_counting_pdf_fixture().await;
    let cache_dir = tempdir().expect("cache directory should be created");
    let raw_cache = Arc::new(FileRawResourceCache::new(cache_dir.path()));
    let opened = open_url_pdf(&fixture, raw_cache.clone()).await;
    fixture.server.abort();

    let result = url_source_view(&opened, raw_cache, opened.policy.clone(), usize::MAX)
        .execute(url_source_view_command(&opened))
        .await
        .expect("the cached body renders with the origin gone");
    assert_eq!(&result.view.bytes[..8], b"\x89PNG\r\n\x1a\n");
}

/// #274: a body rewritten under the same key fails closed — the worker's
/// sha256 check against `content_hash` answers `stale_handle` and never falls
/// back to the network.
#[tokio::test]
async fn source_view_url_source_with_rewritten_cache_body_answers_stale_handle() {
    let fixture = spawn_counting_pdf_fixture().await;
    let cache_dir = tempdir().expect("cache directory should be created");
    let raw_cache = Arc::new(FileRawResourceCache::new(cache_dir.path()));
    let opened = open_url_pdf(&fixture, raw_cache.clone()).await;

    let mut changed = build_fidelity_pdf();
    changed.extend_from_slice(b"rewritten cache body");
    std::fs::write(raw_cache_body(cache_dir.path()), changed)
        .expect("cache body should be rewritten under the same key");

    let before = fixture.requests.load(Ordering::Relaxed);
    let error = url_source_view(&opened, raw_cache, opened.policy.clone(), usize::MAX)
        .execute(url_source_view_command(&opened))
        .await
        .expect_err("a rewritten body must not render");
    assert!(
        matches!(error, ApplicationError::StaleLocator(_)),
        "the worker hash mismatch answers stale: {error:?}"
    );
    assert_eq!(
        fixture.requests.load(Ordering::Relaxed) - before,
        0,
        "a hash mismatch never falls back to the network"
    );
    fixture.server.abort();
}

/// #274: with the cache files deleted the call takes today's buffered path —
/// the miss fetches through the retriever and renders as before.
#[tokio::test]
async fn source_view_url_source_without_a_cache_entry_falls_back() {
    let fixture = spawn_counting_pdf_fixture().await;
    let cache_dir = tempdir().expect("cache directory should be created");
    let raw_cache = Arc::new(FileRawResourceCache::new(cache_dir.path()));
    let opened = open_url_pdf(&fixture, raw_cache.clone()).await;

    for entry in std::fs::read_dir(cache_dir.path().join("raw"))
        .expect("raw cache directory should exist")
        .flatten()
    {
        std::fs::remove_file(entry.path()).expect("cache file should be removed");
    }

    let before = fixture.requests.load(Ordering::Relaxed);
    let result = url_source_view(&opened, raw_cache, opened.policy.clone(), usize::MAX)
        .execute(url_source_view_command(&opened))
        .await
        .expect("the fallback renders through the buffered path");
    assert_eq!(&result.view.bytes[..8], b"\x89PNG\r\n\x1a\n");
    assert_eq!(
        fixture.requests.load(Ordering::Relaxed) - before,
        1,
        "the cache miss fetched through the retriever"
    );
    fixture.server.abort();
}

/// #274: an entry whose `final_source` names another URL is not this
/// document's body — a redirected URL cached under its pre-redirect key — and
/// the call falls back to today's path, which answers its own stale answer.
#[tokio::test]
async fn source_view_url_source_with_a_different_final_source_falls_back() {
    let fixture = spawn_counting_pdf_fixture().await;
    let cache_dir = tempdir().expect("cache directory should be created");
    let raw_cache = Arc::new(FileRawResourceCache::new(cache_dir.path()));
    let opened = open_url_pdf(&fixture, raw_cache.clone()).await;

    let mut foreign = pdf_resource(&opened.source, build_fidelity_pdf());
    foreign.final_source = DocumentSource("http://other.test/different.pdf".into());
    raw_cache
        .put(&opened.source, foreign)
        .await
        .expect("foreign cache entry should be written");

    let before = fixture.requests.load(Ordering::Relaxed);
    let error = url_source_view(&opened, raw_cache, opened.policy.clone(), usize::MAX)
        .execute(url_source_view_command(&opened))
        .await
        .expect_err("a foreign final_source answers stale on the buffered path");
    assert!(
        matches!(error, ApplicationError::StaleLocator(_)),
        "today's path gives its own stale answer: {error:?}"
    );
    assert_eq!(
        fixture.requests.load(Ordering::Relaxed) - before,
        1,
        "the fallback revalidated through the retriever"
    );
    fixture.server.abort();
}

/// #274: a non-PDF body is not a `source_view` source — the call falls back
/// to today's buffered path and reports the same refusal it gives there.
#[tokio::test]
async fn source_view_url_source_with_a_non_pdf_cache_entry_falls_back() {
    let fixture = spawn_counting_pdf_fixture().await;
    let cache_dir = tempdir().expect("cache directory should be created");
    let raw_cache = Arc::new(FileRawResourceCache::new(cache_dir.path()));
    let opened = open_url_pdf(&fixture, raw_cache.clone()).await;

    let mut html = pdf_resource(&opened.source, build_fidelity_pdf());
    html.media_type = MediaType("text/html".into());
    raw_cache
        .put(&opened.source, html)
        .await
        .expect("non-PDF cache entry should be written");

    let before = fixture.requests.load(Ordering::Relaxed);
    let error = url_source_view(&opened, raw_cache, opened.policy.clone(), usize::MAX)
        .execute(url_source_view_command(&opened))
        .await
        .expect_err("a non-PDF media type fails on the buffered path");
    assert!(
        matches!(error, ApplicationError::SourceViewFailed(_)),
        "today's buffered path gives its own refusal: {error:?}"
    );
    assert_eq!(
        fixture.requests.load(Ordering::Relaxed) - before,
        1,
        "the fallback revalidated through the retriever"
    );
    fixture.server.abort();
}

/// #274: a cached body over `READING_MCP_MAX_DOCUMENT_BYTES` is refused with
/// `BudgetedRetriever`'s own message — no request, no worker.
#[tokio::test]
async fn source_view_url_source_with_body_over_the_document_limit_is_refused() {
    let fixture = spawn_counting_pdf_fixture().await;
    let cache_dir = tempdir().expect("cache directory should be created");
    let raw_cache = Arc::new(FileRawResourceCache::new(cache_dir.path()));
    let opened = open_url_pdf(&fixture, raw_cache.clone()).await;

    let before = fixture.requests.load(Ordering::Relaxed);
    let error = url_source_view(&opened, raw_cache, opened.policy.clone(), 16)
        .execute(url_source_view_command(&opened))
        .await
        .expect_err("a body over the limit is refused");
    assert_eq!(
        error,
        ApplicationError::ResourceLimitExceeded(format!(
            "document is {} bytes; limit is 16 bytes",
            build_fidelity_pdf().len()
        )),
        "the same message BudgetedRetriever answers"
    );
    assert_eq!(
        fixture.requests.load(Ordering::Relaxed) - before,
        0,
        "the size refusal never contacts the origin"
    );
    fixture.server.abort();
}

/// #274: a URL the HTTP policy refuses at the URL level — `http://` with
/// `allow_http` off — fails with the same error the retrieval path gives, with
/// no request and no DNS. `max_document_bytes` sits below the cached body
/// length on purpose: the policy check runs before the cache is touched, so a
/// refused URL answers `BlockedSource`, never the size limit.
#[tokio::test]
async fn source_view_url_source_refused_by_the_url_policy() {
    let fixture = spawn_counting_pdf_fixture().await;
    let cache_dir = tempdir().expect("cache directory should be created");
    let raw_cache = Arc::new(FileRawResourceCache::new(cache_dir.path()));
    let opened = open_url_pdf(&fixture, raw_cache.clone()).await;

    let before = fixture.requests.load(Ordering::Relaxed);
    let https_only: Arc<dyn HttpUrlPolicy> = Arc::new(PublicHttpAccessPolicy::https_only());
    let error = url_source_view(&opened, raw_cache, https_only, 16)
        .execute(url_source_view_command(&opened))
        .await
        .expect_err("a URL the policy refuses fails before rendering");
    assert_eq!(
        error,
        ApplicationError::BlockedSource("plain HTTP is disabled by policy".into()),
        "the same refusal the retrieval path gives"
    );
    assert_eq!(
        fixture.requests.load(Ordering::Relaxed) - before,
        0,
        "the policy refusal never contacts the origin"
    );
    fixture.server.abort();
}

/// #274: an in-memory raw cache locates nothing — the call falls back to
/// today's buffered path, revalidation included.
#[tokio::test]
async fn source_view_url_source_with_an_in_memory_cache_falls_back() {
    let fixture = spawn_counting_pdf_fixture().await;
    let raw_cache = Arc::new(InMemoryRawResourceCache::default());
    let opened = open_url_pdf(&fixture, raw_cache.clone()).await;

    let before = fixture.requests.load(Ordering::Relaxed);
    let result = url_source_view(&opened, raw_cache, opened.policy.clone(), usize::MAX)
        .execute(url_source_view_command(&opened))
        .await
        .expect("the fallback renders through the buffered path");
    assert_eq!(&result.view.bytes[..8], b"\x89PNG\r\n\x1a\n");
    assert_eq!(
        fixture.requests.load(Ordering::Relaxed) - before,
        1,
        "the fallback revalidated through the retriever"
    );
    fixture.server.abort();
}

/// #274: `FileRawResourceCache::put` never truncates or rewrites a body in
/// place — a reader holding the old file keeps its inode and bytes while the
/// path already serves the new ones, and no temp files are left behind.
#[tokio::test]
async fn file_raw_resource_cache_put_replaces_the_body_atomically() {
    let cache_dir = tempdir().expect("cache directory should be created");
    let cache = FileRawResourceCache::new(cache_dir.path());
    let source = DocumentSource("https://example.test/doc.pdf".into());

    cache
        .put(&source, pdf_resource(&source, b"first body".to_vec()))
        .await
        .expect("first put should succeed");
    let body_path = raw_cache_body(cache_dir.path());
    let mut old_body = std::fs::File::open(&body_path).expect("old body should open");

    cache
        .put(
            &source,
            pdf_resource(&source, b"second, longer body".to_vec()),
        )
        .await
        .expect("second put should succeed");

    use std::io::Read;
    let mut old_bytes = Vec::new();
    old_body
        .read_to_end(&mut old_bytes)
        .expect("the open handle still reads");
    assert_eq!(old_bytes, b"first body", "the old inode keeps its bytes");
    assert_eq!(
        std::fs::read(&body_path).expect("the body path reads"),
        b"second, longer body",
        "the path serves the new bytes"
    );
    let cached = cache
        .get(&source)
        .await
        .expect("cache get should succeed")
        .expect("the entry exists");
    assert_eq!(cached.bytes, b"second, longer body");

    let names: Vec<_> = std::fs::read_dir(cache_dir.path().join("raw"))
        .expect("raw cache directory should exist")
        .flatten()
        .map(|entry| entry.file_name())
        .collect();
    assert_eq!(names.len(), 2, "no temp files are left behind: {names:?}");
}
