//! PDF-3: opening a paper's landing-page link reads the paper, not the abstract page.
//!
//! These tests run the real `HttpRetriever` (redirects, content-type checks, size limits) against
//! the local HTTP fixture server pattern of `tests/phase5_http_security_cache.rs`, with the
//! landing-page resolver in front of it.

use std::net::SocketAddr;
use std::sync::Arc;

use async_trait::async_trait;
use lopdf::content::{Content, Operation};
use lopdf::{Document as PdfDocument, Object, Stream, dictionary, text_string};
use reading_mcp::application::get_document_structure::GetDocumentStructureUseCase;
use reading_mcp::application::open_document::{OpenDocumentCommand, OpenDocumentUseCase};
use reading_mcp::application::ports::{
    ApplicationError, Parser, RetrievalOptions, Retriever, SourcePolicy,
};
use reading_mcp::application::read_document::{ReadDocumentUseCase, ReadSectionCommand};
use reading_mcp::domain::{DocumentId, DocumentSource, SectionId};
use reading_mcp::infrastructure::{InMemoryDocumentRepository, InMemorySearchIndex};
use reading_mcp::parsing::ParserRouter;
use reading_mcp::retrieval::{
    HttpRetriever, HttpRetrieverConfig, LANDING_REQUESTED_SOURCE, LANDING_RESOLUTION_FAILED,
    LANDING_RULE, LandingPageConfig, LandingPageRetriever, RULE_CITATION_PDF_URL,
};
use reading_mcp::security::HttpAccessPolicy;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use url::Url;

const PDF_SENTENCE: &str = "Landing page resolution reads the paper body.";

struct LocalTestPolicy {
    endpoint: SocketAddr,
    refused_hosts: Vec<&'static str>,
}

#[async_trait]
impl HttpAccessPolicy for LocalTestPolicy {
    fn parse_and_validate(&self, source: &DocumentSource) -> Result<Url, ApplicationError> {
        let url = Url::parse(source.0.trim())
            .map_err(|error| ApplicationError::InvalidRequest(error.to_string()))?;
        self.validate_url(&url)?;
        Ok(url)
    }

    fn validate_url(&self, url: &Url) -> Result<(), ApplicationError> {
        if url.scheme() != "http" {
            return Err(ApplicationError::BlockedSource(
                "test policy only allows HTTP".into(),
            ));
        }
        if let Some(host) = url.host_str()
            && self.refused_hosts.contains(&host)
        {
            return Err(ApplicationError::BlockedSource(format!(
                "refused host {host}"
            )));
        }
        Ok(())
    }

    async fn resolve_public_endpoint(&self, url: &Url) -> Result<SocketAddr, ApplicationError> {
        self.validate_url(url)?;
        Ok(SocketAddr::new(
            self.endpoint.ip(),
            url.port().unwrap_or(self.endpoint.port()),
        ))
    }
}

#[async_trait]
impl SourcePolicy for LocalTestPolicy {
    async fn validate(&self, source: &DocumentSource) -> Result<(), ApplicationError> {
        self.parse_and_validate(source).map(|_| ())
    }
}

fn resolver(
    endpoint: SocketAddr,
    config: LandingPageConfig,
) -> (Arc<LocalTestPolicy>, LandingPageRetriever) {
    let policy = Arc::new(LocalTestPolicy {
        endpoint,
        refused_hosts: vec!["blocked.test"],
    });
    let retriever: Arc<dyn Retriever> = Arc::new(HttpRetriever::new(
        policy.clone(),
        HttpRetrieverConfig::default(),
    ));
    (
        policy.clone(),
        LandingPageRetriever::new(retriever, policy, config),
    )
}

fn enabled() -> LandingPageConfig {
    LandingPageConfig {
        enabled: true,
        allow_http: true,
    }
}

async fn open(
    policy: Arc<LocalTestPolicy>,
    retriever: Arc<dyn Retriever>,
    source: &str,
) -> (
    Arc<InMemoryDocumentRepository>,
    reading_mcp::application::open_document::OpenDocumentResult,
) {
    let repository = Arc::new(InMemoryDocumentRepository::default());
    let index = Arc::new(InMemorySearchIndex::default());
    let parser: Arc<dyn Parser> = Arc::new(ParserRouter::phase4());
    let opened = OpenDocumentUseCase::new(policy, retriever, parser, repository.clone(), index)
        .execute(OpenDocumentCommand {
            source: DocumentSource(source.into()),
            options: RetrievalOptions::default(),
        })
        .await
        .expect("the source should open");
    (repository, opened)
}

async fn body_text(
    repository: &Arc<InMemoryDocumentRepository>,
    document_id: &DocumentId,
) -> String {
    let structure = GetDocumentStructureUseCase::new(repository.clone())
        .execute(document_id.clone(), None)
        .await
        .expect("the opened document should have a structure");
    let root = structure
        .sections
        .first()
        .expect("the document should have a section");
    ReadDocumentUseCase::new(repository.clone())
        .execute(ReadSectionCommand {
            document_id: document_id.clone(),
            section_id: SectionId(root.section_id.0.clone()),
            max_chars: None,
        })
        .await
        .expect("the root section should read")
        .content
}

#[tokio::test]
async fn a_landing_page_with_citation_pdf_url_opens_the_paper() {
    let (endpoint, server) = spawn_fixture().await;
    let (policy, resolver) = resolver(endpoint, enabled());
    let source = format!("http://example.test:{}/article", endpoint.port());

    let resource = resolver
        .retrieve(
            &DocumentSource(source.clone()),
            &RetrievalOptions::default(),
        )
        .await
        .expect("the landing page should resolve");
    assert_eq!(resource.media_type.essence(), "application/pdf");
    assert_eq!(
        resource.final_source.0,
        format!("http://example.test:{}/paper.pdf", endpoint.port())
    );
    assert_eq!(resource.source.0, source);
    assert_eq!(
        resource.metadata.get(LANDING_RULE).map(String::as_str),
        Some(RULE_CITATION_PDF_URL)
    );
    assert_eq!(
        resource
            .metadata
            .get(LANDING_REQUESTED_SOURCE)
            .map(String::as_str),
        Some(source.as_str())
    );
    assert!(!resource.metadata.contains_key(LANDING_RESOLUTION_FAILED));

    let (repository, opened) = open(
        policy,
        Arc::new(resolver),
        &format!("http://example.test:{}/article", endpoint.port()),
    )
    .await;
    assert_eq!(opened.media_type.0, "application/pdf");
    assert_eq!(opened.title, "Operating Systems");
    let text = body_text(&repository, &opened.document_id).await;
    assert!(text.contains(PDF_SENTENCE), "opened text was {text:?}");

    server.abort();
}

#[tokio::test]
async fn the_landing_page_and_the_direct_pdf_link_are_the_same_document() {
    let (endpoint, server) = spawn_fixture().await;
    let (policy, resolver) = resolver(endpoint, enabled());
    let retriever: Arc<dyn Retriever> = Arc::new(resolver);

    let (_, landing) = open(
        policy.clone(),
        retriever.clone(),
        &format!("http://example.test:{}/article", endpoint.port()),
    )
    .await;
    let (_, direct) = open(
        policy,
        retriever,
        &format!("http://example.test:{}/paper.pdf", endpoint.port()),
    )
    .await;

    assert_eq!(landing.document_id, direct.document_id);
    assert_eq!(landing.content_hash, direct.content_hash);
    assert_eq!(landing.media_type.0, "application/pdf");
    assert_eq!(direct.media_type.0, "application/pdf");

    server.abort();
}

#[tokio::test]
async fn a_target_that_is_not_a_pdf_keeps_the_landing_page() {
    let (endpoint, server) = spawn_fixture().await;
    let (policy, resolver) = resolver(endpoint, enabled());
    let source = format!("http://example.test:{}/article-html", endpoint.port());

    let resource = resolver
        .retrieve(
            &DocumentSource(source.clone()),
            &RetrievalOptions::default(),
        )
        .await
        .expect("the landing page should be returned");
    assert_eq!(resource.media_type.essence(), "text/html");
    assert_eq!(resource.final_source.0, source);
    assert_eq!(
        resource
            .metadata
            .get(LANDING_RESOLUTION_FAILED)
            .map(String::as_str),
        Some("not_pdf")
    );
    assert!(!resource.metadata.contains_key(LANDING_RULE));

    let (repository, opened) = open(policy, Arc::new(resolver), &source).await;
    assert_eq!(opened.media_type.essence(), "text/html");
    let text = body_text(&repository, &opened.document_id).await;
    assert!(
        text.contains("Abstract only page"),
        "opened text was {text:?}"
    );

    server.abort();
}

#[tokio::test]
async fn a_target_the_policy_refuses_keeps_the_landing_page() {
    let (endpoint, server) = spawn_fixture().await;
    let (policy, resolver) = resolver(endpoint, enabled());
    let source = format!("http://example.test:{}/article-refused", endpoint.port());

    let resource = resolver
        .retrieve(
            &DocumentSource(source.clone()),
            &RetrievalOptions::default(),
        )
        .await
        .expect("the landing page should be returned");
    assert_eq!(resource.media_type.essence(), "text/html");
    assert_eq!(
        resource
            .metadata
            .get(LANDING_RESOLUTION_FAILED)
            .map(String::as_str),
        Some("refused_by_policy")
    );

    let (_, opened) = open(policy, Arc::new(resolver), &source).await;
    assert_eq!(opened.media_type.essence(), "text/html");

    server.abort();
}

#[tokio::test]
async fn a_target_that_changes_the_scheme_keeps_the_landing_page() {
    let (endpoint, server) = spawn_fixture().await;
    let (_, resolver) = resolver(endpoint, enabled());
    let source = format!("http://example.test:{}/article-scheme", endpoint.port());

    let resource = resolver
        .retrieve(
            &DocumentSource(source.clone()),
            &RetrievalOptions::default(),
        )
        .await
        .expect("the landing page should be returned");
    assert_eq!(resource.final_source.0, source);
    assert_eq!(
        resource
            .metadata
            .get(LANDING_RESOLUTION_FAILED)
            .map(String::as_str),
        Some("refused_by_policy"),
        "an https target must never be fetched for an http landing page"
    );

    server.abort();
}

#[tokio::test]
async fn a_target_that_is_itself_a_landing_page_is_not_followed() {
    let (endpoint, server) = spawn_fixture().await;
    let (policy, resolver) = resolver(endpoint, enabled());
    let source = format!("http://example.test:{}/article-nested", endpoint.port());

    let resource = resolver
        .retrieve(
            &DocumentSource(source.clone()),
            &RetrievalOptions::default(),
        )
        .await
        .expect("the landing page should be returned");
    assert_eq!(
        resource
            .metadata
            .get(LANDING_RESOLUTION_FAILED)
            .map(String::as_str),
        Some("not_pdf"),
        "resolution depth is one"
    );

    let (repository, opened) = open(policy, Arc::new(resolver), &source).await;
    assert_eq!(opened.media_type.essence(), "text/html");
    let text = body_text(&repository, &opened.document_id).await;
    assert!(
        text.contains("Outer abstract page"),
        "opened text was {text:?}"
    );

    server.abort();
}

#[tokio::test]
async fn a_redirect_chain_that_ends_on_a_landing_page_resolves() {
    let (endpoint, server) = spawn_fixture().await;
    let (policy, resolver) = resolver(endpoint, enabled());
    let source = format!("http://example.test:{}/doi/10.1", endpoint.port());

    let resource = resolver
        .retrieve(
            &DocumentSource(source.clone()),
            &RetrievalOptions::default(),
        )
        .await
        .expect("the redirect chain should resolve");
    assert_eq!(resource.media_type.essence(), "application/pdf");
    assert_eq!(
        resource.metadata.get(LANDING_RULE).map(String::as_str),
        Some(RULE_CITATION_PDF_URL)
    );

    let (repository, opened) = open(policy, Arc::new(resolver), &source).await;
    let text = body_text(&repository, &opened.document_id).await;
    assert!(text.contains(PDF_SENTENCE), "opened text was {text:?}");

    server.abort();
}

#[tokio::test]
async fn the_disabled_knob_opens_the_landing_page_as_today() {
    let (endpoint, server) = spawn_fixture().await;
    let source = format!("http://example.test:{}/article", endpoint.port());

    let (disabled_policy, disabled) = resolver(
        endpoint,
        LandingPageConfig {
            enabled: false,
            allow_http: true,
        },
    );
    let resource = disabled
        .retrieve(
            &DocumentSource(source.clone()),
            &RetrievalOptions::default(),
        )
        .await
        .expect("the landing page should be returned");
    assert_eq!(resource.media_type.essence(), "text/html");
    assert!(
        !resource
            .metadata
            .keys()
            .any(|key| key.starts_with("landing_")),
        "the knob off adds no landing metadata"
    );
    let (_, disabled_open) = open(disabled_policy, Arc::new(disabled), &source).await;

    // The same open without any resolver in the stack: the pre-PDF-3 behaviour.
    let policy = Arc::new(LocalTestPolicy {
        endpoint,
        refused_hosts: vec![],
    });
    let plain: Arc<dyn Retriever> = Arc::new(HttpRetriever::new(
        policy.clone(),
        HttpRetrieverConfig::default(),
    ));
    let (_, plain_open) = open(policy, plain, &source).await;

    assert_eq!(disabled_open.media_type.essence(), "text/html");
    assert_eq!(disabled_open.document_id, plain_open.document_id);
    assert_eq!(disabled_open.title, plain_open.title);
    assert_eq!(disabled_open.section_count, plain_open.section_count);
    assert_eq!(
        disabled_open.content_hash, plain_open.content_hash,
        "the knob off must read exactly the bytes read today"
    );

    server.abort();
}

#[tokio::test]
async fn an_html_page_without_the_meta_tag_reports_no_meta() {
    let (endpoint, server) = spawn_fixture().await;
    let (_, resolver) = resolver(endpoint, enabled());
    let resource = resolver
        .retrieve(
            &DocumentSource(format!("http://example.test:{}/abstract", endpoint.port())),
            &RetrievalOptions::default(),
        )
        .await
        .expect("the landing page should be returned");
    assert_eq!(resource.media_type.essence(), "text/html");
    assert_eq!(
        resource
            .metadata
            .get(LANDING_RESOLUTION_FAILED)
            .map(String::as_str),
        Some("no_meta")
    );

    server.abort();
}

#[tokio::test]
async fn a_target_that_cannot_be_fetched_keeps_the_landing_page() {
    let (endpoint, server) = spawn_fixture().await;
    let (_, resolver) = resolver(endpoint, enabled());
    let resource = resolver
        .retrieve(
            &DocumentSource(format!(
                "http://example.test:{}/article-missing",
                endpoint.port()
            )),
            &RetrievalOptions::default(),
        )
        .await
        .expect("the landing page should be returned");
    assert_eq!(resource.media_type.essence(), "text/html");
    assert_eq!(
        resource
            .metadata
            .get(LANDING_RESOLUTION_FAILED)
            .map(String::as_str),
        Some("fetch_failed")
    );

    server.abort();
}

// ------------------------------------------------------------------------------------------------
// Fixtures
// ------------------------------------------------------------------------------------------------

fn pdf_bytes() -> Vec<u8> {
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
            Operation::new("Tf", vec!["F1".into(), 12.into()]),
            Operation::new("Td", vec![72.into(), 720.into()]),
            Operation::new("Tj", vec![Object::string_literal(PDF_SENTENCE)]),
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
    let info_id = document.add_object(dictionary! {
        "Title" => text_string("Operating Systems"),
    });
    document.trailer.set("Root", catalog_id);
    document.trailer.set("Info", info_id);

    let mut bytes = Vec::new();
    document
        .save_to(&mut bytes)
        .expect("fixture PDF should serialize");
    bytes
}

fn landing_page(meta: &str, body: &str) -> String {
    format!(
        "<!doctype html><html><head><title>Abstract</title>{meta}</head><body><p>{body}</p></body></html>"
    )
}

async fn spawn_fixture() -> (SocketAddr, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("fixture listener should bind");
    let address = listener.local_addr().expect("fixture address should exist");
    let pdf = pdf_bytes();
    let handle = tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                break;
            };
            let pdf = pdf.clone();
            tokio::spawn(async move {
                let mut buffer = vec![0u8; 4096];
                let Ok(read) = socket.read(&mut buffer).await else {
                    return;
                };
                let request = String::from_utf8_lossy(&buffer[..read]);
                let path = request
                    .lines()
                    .next()
                    .and_then(|line| line.split_whitespace().nth(1))
                    .unwrap_or("/")
                    .to_string();

                let response: Vec<u8> = match path.as_str() {
                    "/article" => text_response(
                        "text/html; charset=utf-8",
                        &landing_page(
                            r#"<meta name="citation_pdf_url" content="/paper.pdf">"#,
                            "Abstract only page.",
                        ),
                    ),
                    "/article-html" => text_response(
                        "text/html",
                        &landing_page(
                            r#"<meta name="citation_pdf_url" content="/target-html">"#,
                            "Abstract only page.",
                        ),
                    ),
                    "/target-html" => text_response(
                        "text/html",
                        "<html><body><p>Target page body.</p></body></html>",
                    ),
                    "/article-refused" => text_response(
                        "text/html",
                        &landing_page(
                            r#"<meta name="citation_pdf_url" content="http://blocked.test/paper.pdf">"#,
                            "Abstract only page.",
                        ),
                    ),
                    "/article-scheme" => text_response(
                        "text/html",
                        &landing_page(
                            r#"<meta name="citation_pdf_url" content="https://example.test/paper.pdf">"#,
                            "Abstract only page.",
                        ),
                    ),
                    "/article-nested" => text_response(
                        "text/html",
                        &landing_page(
                            r#"<meta name="citation_pdf_url" content="/intermediate">"#,
                            "Outer abstract page.",
                        ),
                    ),
                    "/intermediate" => text_response(
                        "text/html",
                        &landing_page(
                            r#"<meta name="citation_pdf_url" content="/paper.pdf">"#,
                            "Intermediate abstract page.",
                        ),
                    ),
                    "/article-missing" => text_response(
                        "text/html",
                        &landing_page(
                            r#"<meta name="citation_pdf_url" content="/missing.pdf">"#,
                            "Abstract only page.",
                        ),
                    ),
                    "/abstract" => text_response(
                        "text/html",
                        "<html><body><p>Abstract without a PDF link.</p></body></html>",
                    ),
                    "/doi/10.1" => {
                        b"HTTP/1.1 302 Found\r\nLocation: /article\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                            .to_vec()
                    }
                    "/paper.pdf" => binary_response("application/pdf", &pdf),
                    _ => {
                        b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                            .to_vec()
                    }
                };

                let _ = socket.write_all(&response).await;
                let _ = socket.shutdown().await;
            });
        }
    });

    (address, handle)
}

fn text_response(content_type: &str, body: &str) -> Vec<u8> {
    binary_response(content_type, body.as_bytes())
}

fn binary_response(content_type: &str, body: &[u8]) -> Vec<u8> {
    let mut response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )
    .into_bytes();
    response.extend_from_slice(body);
    response
}
