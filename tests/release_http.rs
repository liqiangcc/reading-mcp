use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use reading_mcp::application::ports::{
    ApplicationError, RetrievalOptions, Retriever, RawResourceCache,
};
use reading_mcp::domain::DocumentSource;
use reading_mcp::infrastructure::InMemoryRawResourceCache;
use reading_mcp::retrieval::{
    CredentialProvider, HttpRetriever, HttpRetrieverConfig, RevalidatingHttpRetriever,
};
use reading_mcp::security::HttpAccessPolicy;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use url::Url;

#[tokio::test]
async fn http_cache_uses_conditional_revalidation_and_reuses_304_body() {
    let state = Arc::new(FixtureState::default());
    let (endpoint, server) = spawn_fixture(state.clone()).await;
    let policy = Arc::new(TestHttpPolicy { endpoint });
    let http = Arc::new(HttpRetriever::new(policy, HttpRetrieverConfig::default()));
    let cache: Arc<dyn RawResourceCache> = Arc::new(InMemoryRawResourceCache::default());
    let retriever = RevalidatingHttpRetriever::new(http, cache);
    let source = DocumentSource(format!(
        "http://example.test:{}/conditional.md",
        endpoint.port()
    ));

    let first = retriever
        .retrieve(&source, &RetrievalOptions::default())
        .await
        .expect("first request should download the document");
    let second = retriever
        .retrieve(&source, &RetrievalOptions::default())
        .await
        .expect("second request should revalidate and reuse cached body");

    assert_eq!(first.bytes, second.bytes);
    assert_eq!(first.etag.as_deref(), Some("\"fixture-v1\""));
    assert_eq!(state.conditional_requests.load(Ordering::SeqCst), 2);
    assert_eq!(state.not_modified_responses.load(Ordering::SeqCst), 1);
    server.abort();
}

#[tokio::test]
async fn auth_profile_is_injected_only_for_allowed_host_and_not_leaked_on_redirect() {
    let state = Arc::new(FixtureState::default());
    let (endpoint, server) = spawn_fixture(state.clone()).await;
    let policy = Arc::new(TestHttpPolicy { endpoint });
    let http = HttpRetriever::with_credentials(
        policy,
        HttpRetrieverConfig::default(),
        Arc::new(TestCredentialProvider),
    );
    let options = RetrievalOptions {
        auth_profile: Some("company-docs".into()),
        force_refresh: false,
    };

    let resource = http
        .retrieve(
            &DocumentSource(format!("http://example.test:{}/auth.md", endpoint.port())),
            &options,
        )
        .await
        .expect("allowed auth profile should inject bearer token");
    assert!(String::from_utf8_lossy(&resource.bytes).contains("Authenticated"));
    assert_eq!(state.authorized_requests.load(Ordering::SeqCst), 1);

    let error = http
        .retrieve(
            &DocumentSource(format!(
                "http://example.test:{}/auth-redirect",
                endpoint.port()
            )),
            &options,
        )
        .await
        .expect_err("auth profile must be re-evaluated for redirect host");
    assert!(matches!(error, ApplicationError::AuthenticationFailed(_)));
    assert_eq!(state.other_host_requests.load(Ordering::SeqCst), 0);
    server.abort();
}

#[derive(Default)]
struct FixtureState {
    conditional_requests: AtomicUsize,
    not_modified_responses: AtomicUsize,
    authorized_requests: AtomicUsize,
    other_host_requests: AtomicUsize,
}

struct TestCredentialProvider;

impl CredentialProvider for TestCredentialProvider {
    fn bearer_token(&self, profile: &str, url: &Url) -> Result<String, ApplicationError> {
        if profile != "company-docs" {
            return Err(ApplicationError::AuthenticationFailed(
                "unexpected test profile".into(),
            ));
        }
        if url.host_str() != Some("example.test") {
            return Err(ApplicationError::AuthenticationFailed(
                "profile is not allowed for redirect host".into(),
            ));
        }
        Ok("test-secret".into())
    }
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
        match url.host_str() {
            Some("example.test" | "other.test") => Ok(()),
            _ => Err(ApplicationError::BlockedSource(
                "unexpected test host".into(),
            )),
        }
    }

    async fn resolve_public_endpoint(&self, url: &Url) -> Result<SocketAddr, ApplicationError> {
        self.validate_url(url)?;
        Ok(SocketAddr::new(
            self.endpoint.ip(),
            url.port().unwrap_or(self.endpoint.port()),
        ))
    }
}

async fn spawn_fixture(
    state: Arc<FixtureState>,
) -> (SocketAddr, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("fixture listener should bind");
    let address = listener.local_addr().expect("fixture address should exist");
    let handle = tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                break;
            };
            let state = state.clone();
            tokio::spawn(async move {
                let mut buffer = vec![0u8; 8192];
                let Ok(read) = socket.read(&mut buffer).await else {
                    return;
                };
                let request = String::from_utf8_lossy(&buffer[..read]);
                let first_line = request.lines().next().unwrap_or_default();
                let path = first_line.split_whitespace().nth(1).unwrap_or("/");
                let host = request
                    .lines()
                    .find_map(|line| line.strip_prefix("host: ").or_else(|| line.strip_prefix("Host: ")))
                    .unwrap_or_default();

                let response = match path {
                    "/conditional.md" => {
                        state.conditional_requests.fetch_add(1, Ordering::SeqCst);
                        if request.contains("If-None-Match: \"fixture-v1\"")
                            || request.contains("if-none-match: \"fixture-v1\"")
                        {
                            state
                                .not_modified_responses
                                .fetch_add(1, Ordering::SeqCst);
                            "HTTP/1.1 304 Not Modified\r\nETag: \"fixture-v1\"\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_string()
                        } else {
                            let body = "# Conditional\n\nCached document body.\n";
                            format!(
                                "HTTP/1.1 200 OK\r\nContent-Type: text/markdown\r\nETag: \"fixture-v1\"\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                                body.len()
                            )
                        }
                    }
                    "/auth.md" => {
                        if host.starts_with("other.test") {
                            state.other_host_requests.fetch_add(1, Ordering::SeqCst);
                        }
                        if request.contains("Authorization: Bearer test-secret")
                            || request.contains("authorization: Bearer test-secret")
                        {
                            state.authorized_requests.fetch_add(1, Ordering::SeqCst);
                            let body = "# Authenticated\n\nAuthenticated document.\n";
                            format!(
                                "HTTP/1.1 200 OK\r\nContent-Type: text/markdown\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                                body.len()
                            )
                        } else {
                            "HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_string()
                        }
                    }
                    "/auth-redirect" => format!(
                        "HTTP/1.1 302 Found\r\nLocation: http://other.test:{}/auth.md\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                        address.port()
                    ),
                    _ => "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_string(),
                };

                let _ = socket.write_all(response.as_bytes()).await;
                let _ = socket.shutdown().await;
            });
        }
    });
    (address, handle)
}
