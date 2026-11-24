use std::collections::BTreeMap;
use std::sync::Arc;

use lopdf::content::{Content, Operation};
use lopdf::{Document as PdfDocument, Object, Stream, dictionary};
use reading_mcp::application::open_document::{OpenDocumentCommand, OpenDocumentUseCase};
use reading_mcp::application::ports::{
    ApplicationError, DocumentRepository, LocalSourceAccess, ResolvedLocalSource, RetrievalOptions,
    RetrievedResource, Retriever, SourcePolicy, SourceViewRenderOptions, SourceViewRenderer,
};
use reading_mcp::application::source_view::{
    GetSourceViewCommand, SourceViewLimits, SourceViewRepresentation, SourceViewUseCase,
};
use reading_mcp::domain::{DocumentSource, MediaType, SectionId, TextLocator};
use reading_mcp::infrastructure::{InMemoryDocumentRepository, InMemoryTextUnitIndex};
use reading_mcp::parsing::{
    FileProcessIsolatedPdfSourceViewRenderer, ParserRouter, PdfSourceViewRenderer,
};
use reading_mcp::retrieval::{
    FileRetriever, HttpRetriever, HttpRetrieverConfig, LimitedFileRetriever, LocalFileSourceAccess,
    LocalFileSourcePolicy,
};
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
