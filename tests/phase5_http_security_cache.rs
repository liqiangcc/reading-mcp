use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use reading_mcp::application::ports::{
    ApplicationError, Parser, RetrievalOptions, RetrievedResource, Retriever,
};
use reading_mcp::domain::{Document, DocumentSource, MediaType};
use reading_mcp::infrastructure::{
    CachingParser, CachingRetriever, InMemoryParsedDocumentCache, InMemoryRawResourceCache,
};
use reading_mcp::parsing::ParserRouter;
use reading_mcp::retrieval::{HttpRetriever, HttpRetrieverConfig};
use reading_mcp::security::HttpAccessPolicy;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use url::Url;

#[tokio::test]
async fn http_retriever_follows_validated_redirects_and_preserves_metadata() {
    let (endpoint, server) = spawn_http_fixture().await;
    let policy = Arc::new(TestHttpPolicy { endpoint });
    let retriever = HttpRetriever::new(policy, HttpRetrieverConfig::default());

    let resource = retriever
        .retrieve(
            &DocumentSource(format!("http://example.test:{}/start", endpoint.port())),
            &RetrievalOptions::default(),
        )
        .await
        .expect("safe redirect chain should be retrieved");

    assert_eq!(resource.media_type.0, "text/markdown");
    assert_eq!(resource.etag.as_deref(), Some("\"fixture-v1\""));
    assert_eq!(
        resource.last_modified.as_deref(),
        Some("Fri, 07 Aug 2026 00:00:00 GMT")
    );
    assert!(resource.final_source.0.ends_with("/doc.md"));
    assert_eq!(
        resource
            .metadata
            .get("http_redirect_count")
            .map(String::as_str),
        Some("1")
    );
    assert!(String::from_utf8_lossy(&resource.bytes).contains("Virtual Memory"));

    server.abort();
}

#[tokio::test]
async fn http_retriever_validates_every_redirect_target_before_connecting() {
    let (endpoint, server) = spawn_http_fixture().await;
    let policy = Arc::new(TestHttpPolicy { endpoint });
    let retriever = HttpRetriever::new(policy, HttpRetrieverConfig::default());

    let error = retriever
        .retrieve(
            &DocumentSource(format!("http://example.test:{}/blocked", endpoint.port())),
            &RetrievalOptions::default(),
        )
        .await
        .expect_err("redirect to blocked host must be rejected");

    assert!(matches!(error, ApplicationError::BlockedSource(_)));
    server.abort();
}

#[tokio::test]
async fn http_retriever_enforces_response_size_limit() {
    let (endpoint, server) = spawn_http_fixture().await;
    let policy = Arc::new(TestHttpPolicy { endpoint });
    let config = HttpRetrieverConfig {
        max_response_bytes: 8,
        ..HttpRetrieverConfig::default()
    };
    let retriever = HttpRetriever::new(policy, config);

    let error = retriever
        .retrieve(
            &DocumentSource(format!("http://example.test:{}/large", endpoint.port())),
            &RetrievalOptions::default(),
        )
        .await
        .expect_err("oversized response must be rejected");

    assert!(error.to_string().contains("exceeds 8 bytes"));
    server.abort();
}

#[tokio::test]
async fn raw_and_parsed_caches_have_independent_lifecycles() {
    let retrieve_calls = Arc::new(AtomicUsize::new(0));
    let parse_calls = Arc::new(AtomicUsize::new(0));
    let source = DocumentSource("memory:cached.md".into());

    let retriever: Arc<dyn Retriever> = Arc::new(CachingRetriever::new(
        Arc::new(CountingRetriever {
            calls: retrieve_calls.clone(),
        }),
        Arc::new(InMemoryRawResourceCache::default()),
    ));
    let parser: Arc<dyn Parser> = Arc::new(CachingParser::new(
        Arc::new(CountingParser {
            calls: parse_calls.clone(),
            inner: ParserRouter::phase4(),
        }),
        Arc::new(InMemoryParsedDocumentCache::default()),
    ));

    let options = RetrievalOptions::default();
    let first = retriever
        .retrieve(&source, &options)
        .await
        .expect("first retrieval should succeed");
    parser
        .parse(first)
        .await
        .expect("first parse should succeed");

    let second = retriever
        .retrieve(&source, &options)
        .await
        .expect("second retrieval should use raw cache");
    parser
        .parse(second)
        .await
        .expect("second parse should use parsed cache");

    assert_eq!(retrieve_calls.load(Ordering::SeqCst), 1);
    assert_eq!(parse_calls.load(Ordering::SeqCst), 1);

    let refreshed = retriever
        .retrieve(
            &source,
            &RetrievalOptions {
                force_refresh: true,
                ..RetrievalOptions::default()
            },
        )
        .await
        .expect("forced refresh should call the underlying retriever");
    parser
        .parse(refreshed)
        .await
        .expect("unchanged refreshed bytes should still reuse parsed cache");

    assert_eq!(retrieve_calls.load(Ordering::SeqCst), 2);
    assert_eq!(parse_calls.load(Ordering::SeqCst), 1);
}

struct TestHttpPolicy {
    endpoint: SocketAddr,
}

#[async_trait]
impl HttpAccessPolicy for TestHttpPolicy {
    fn parse_and_validate(&self, source: &DocumentSource) -> Result<Url, ApplicationError> {
        let url = Url::parse(&source.0)
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
        if url.host_str() == Some("blocked.test") {
            return Err(ApplicationError::BlockedSource(
                "blocked redirect target".into(),
            ));
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

struct CountingRetriever {
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl Retriever for CountingRetriever {
    async fn retrieve(
        &self,
        source: &DocumentSource,
        _options: &RetrievalOptions,
    ) -> Result<RetrievedResource, ApplicationError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(RetrievedResource {
            source: source.clone(),
            final_source: source.clone(),
            media_type: MediaType("text/markdown".into()),
            bytes: b"# Cached\n\nVirtual memory remains unchanged.\n".to_vec(),
            etag: Some("fixture".into()),
            last_modified: None,
            metadata: Default::default(),
        })
    }
}

struct CountingParser {
    calls: Arc<AtomicUsize>,
    inner: ParserRouter,
}

#[async_trait]
impl Parser for CountingParser {
    async fn parse(&self, resource: RetrievedResource) -> Result<Document, ApplicationError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.inner.parse(resource).await
    }
}

async fn spawn_http_fixture() -> (SocketAddr, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("fixture listener should bind");
    let address = listener.local_addr().expect("fixture address should exist");
    let handle = tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                break;
            };
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
                    .unwrap_or("/");

                let response = match path {
                    "/start" => {
                        "HTTP/1.1 302 Found\r\nLocation: /doc.md\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_string()
                    }
                    "/blocked" => {
                        "HTTP/1.1 302 Found\r\nLocation: http://blocked.test/private\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_string()
                    }
                    "/doc.md" => {
                        let body = "# Operating Systems\n\n## Virtual Memory\n\nPage tables map memory.\n";
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: text/markdown\r\nContent-Length: {}\r\nETag: \"fixture-v1\"\r\nLast-Modified: Fri, 07 Aug 2026 00:00:00 GMT\r\nConnection: close\r\n\r\n{body}",
                            body.len()
                        )
                    }
                    "/large" => {
                        let body = "0123456789abcdef";
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                            body.len()
                        )
                    }
                    _ => "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_string(),
                };

                let _ = socket.write_all(response.as_bytes()).await;
                let _ = socket.shutdown().await;
            });
        }
    });

    (address, handle)
}
