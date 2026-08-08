use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use reqwest::StatusCode;
use reqwest::header::{
    CONTENT_LENGTH, CONTENT_TYPE, ETAG, IF_MODIFIED_SINCE, IF_NONE_MATCH, LAST_MODIFIED, LOCATION,
};
use reqwest::redirect::Policy;
use tokio::sync::Semaphore;
use url::Url;

use crate::application::ports::{ApplicationError, RetrievalOptions, RetrievedResource, Retriever};
use crate::domain::{DocumentSource, MediaType};
use crate::security::HttpAccessPolicy;

use super::auth::{CredentialProvider, NoCredentialProvider};

#[derive(Clone, Debug)]
pub struct HttpRetrieverConfig {
    pub max_redirects: usize,
    pub max_response_bytes: usize,
    pub request_timeout: Duration,
    pub connect_timeout: Duration,
    pub max_concurrency: usize,
    pub user_agent: String,
}

impl Default for HttpRetrieverConfig {
    fn default() -> Self {
        Self {
            max_redirects: 5,
            max_response_bytes: 16 * 1024 * 1024,
            request_timeout: Duration::from_secs(20),
            connect_timeout: Duration::from_secs(8),
            max_concurrency: 8,
            user_agent: "reading-mcp/0.1".into(),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HttpValidators {
    pub etag: Option<String>,
    pub last_modified: Option<String>,
}

pub enum HttpRetrievalOutcome {
    Resource(RetrievedResource),
    NotModified,
}

pub struct HttpRetriever {
    access_policy: Arc<dyn HttpAccessPolicy>,
    credentials: Arc<dyn CredentialProvider>,
    config: HttpRetrieverConfig,
    permits: Semaphore,
}

impl HttpRetriever {
    pub fn new(access_policy: Arc<dyn HttpAccessPolicy>, config: HttpRetrieverConfig) -> Self {
        Self::with_credentials(access_policy, config, Arc::new(NoCredentialProvider))
    }

    pub fn with_credentials(
        access_policy: Arc<dyn HttpAccessPolicy>,
        config: HttpRetrieverConfig,
        credentials: Arc<dyn CredentialProvider>,
    ) -> Self {
        let max_concurrency = config.max_concurrency.max(1);
        Self {
            access_policy,
            credentials,
            config,
            permits: Semaphore::new(max_concurrency),
        }
    }

    pub async fn retrieve_conditional(
        &self,
        source: &DocumentSource,
        options: &RetrievalOptions,
        validators: &HttpValidators,
    ) -> Result<HttpRetrievalOutcome, ApplicationError> {
        self.retrieve_internal(source, options, Some(validators))
            .await
    }

    async fn fetch_once(
        &self,
        url: &Url,
        auth_profile: Option<&str>,
        validators: Option<&HttpValidators>,
    ) -> Result<reqwest::Response, ApplicationError> {
        let endpoint = self.access_policy.resolve_public_endpoint(url).await?;
        let host = url
            .host_str()
            .ok_or_else(|| ApplicationError::InvalidRequest("URL must contain a host".into()))?;

        let mut builder = reqwest::Client::builder()
            .no_proxy()
            .redirect(Policy::none())
            .connect_timeout(self.config.connect_timeout)
            .timeout(self.config.request_timeout)
            .user_agent(&self.config.user_agent);

        if host.parse::<std::net::IpAddr>().is_err() {
            builder = builder.resolve(host, endpoint);
        }

        let client = builder.build().map_err(|error| {
            ApplicationError::RetrievalFailed(format!("failed to build HTTP client: {error}"))
        })?;
        let mut request = client.get(url.clone());

        if let Some(profile) = auth_profile {
            let token = self.credentials.bearer_token(profile, url)?;
            request = request.bearer_auth(token);
        }
        if let Some(validators) = validators {
            if let Some(etag) = &validators.etag {
                request = request.header(IF_NONE_MATCH, etag);
            }
            if let Some(last_modified) = &validators.last_modified {
                request = request.header(IF_MODIFIED_SINCE, last_modified);
            }
        }

        request.send().await.map_err(|error| {
            ApplicationError::RetrievalFailed(format!("HTTP request failed for {url}: {error}"))
        })
    }

    async fn retrieve_internal(
        &self,
        source: &DocumentSource,
        options: &RetrievalOptions,
        validators: Option<&HttpValidators>,
    ) -> Result<HttpRetrievalOutcome, ApplicationError> {
        let _permit = self.permits.acquire().await.map_err(|_| {
            ApplicationError::RetrievalFailed("HTTP concurrency limiter is closed".into())
        })?;

        let mut current = self.access_policy.parse_and_validate(source)?;
        let mut redirect_count = 0usize;

        loop {
            self.access_policy.validate_url(&current)?;
            let request_validators = (redirect_count == 0).then_some(validators).flatten();
            let response = self
                .fetch_once(
                    &current,
                    options.auth_profile.as_deref(),
                    request_validators,
                )
                .await?;
            let status = response.status();

            if status == StatusCode::NOT_MODIFIED {
                if validators.is_some() {
                    return Ok(HttpRetrievalOutcome::NotModified);
                }
                return Err(ApplicationError::RetrievalFailed(format!(
                    "unexpected HTTP 304 returned for {current}"
                )));
            }

            if status.is_redirection() {
                if redirect_count >= self.config.max_redirects {
                    return Err(ApplicationError::RetrievalFailed(format!(
                        "redirect limit exceeded at {current}"
                    )));
                }

                let location = response
                    .headers()
                    .get(LOCATION)
                    .and_then(|value| value.to_str().ok())
                    .ok_or_else(|| {
                        ApplicationError::RetrievalFailed(format!(
                            "redirect response from {current} has no valid Location header"
                        ))
                    })?;
                let next = current.join(location).map_err(|error| {
                    ApplicationError::RetrievalFailed(format!(
                        "invalid redirect target from {current}: {error}"
                    ))
                })?;
                self.access_policy.validate_url(&next)?;
                current = next;
                redirect_count += 1;
                continue;
            }

            if !status.is_success() {
                return Err(ApplicationError::RetrievalFailed(format!(
                    "HTTP {status} returned for {current}"
                )));
            }

            if let Some(length) = response.content_length()
                && length > self.config.max_response_bytes as u64
            {
                return Err(ApplicationError::ResourceLimitExceeded(format!(
                    "response from {current} exceeds {} bytes",
                    self.config.max_response_bytes
                )));
            }

            let media_type = response_media_type(&response, &current)?;
            let etag = header_string(&response, ETAG);
            let last_modified = header_string(&response, LAST_MODIFIED);
            let declared_length = header_string(&response, CONTENT_LENGTH);
            let mut metadata = BTreeMap::new();
            metadata.insert("http_status".into(), status.as_u16().to_string());
            metadata.insert("http_redirect_count".into(), redirect_count.to_string());
            metadata.insert("http_content_type".into(), media_type.0.clone());
            if let Some(length) = declared_length {
                metadata.insert("http_content_length".into(), length);
            }

            let mut response = response;
            let mut bytes = Vec::new();
            while let Some(chunk) = response.chunk().await.map_err(|error| {
                ApplicationError::RetrievalFailed(format!(
                    "failed reading response body from {current}: {error}"
                ))
            })? {
                if bytes.len().saturating_add(chunk.len()) > self.config.max_response_bytes {
                    return Err(ApplicationError::ResourceLimitExceeded(format!(
                        "response body from {current} exceeds {} bytes after decompression",
                        self.config.max_response_bytes
                    )));
                }
                bytes.extend_from_slice(&chunk);
            }

            return Ok(HttpRetrievalOutcome::Resource(RetrievedResource {
                source: source.clone(),
                final_source: DocumentSource(current.to_string()),
                media_type,
                bytes,
                etag,
                last_modified,
                metadata,
            }));
        }
    }
}

#[async_trait]
impl Retriever for HttpRetriever {
    async fn retrieve(
        &self,
        source: &DocumentSource,
        options: &RetrievalOptions,
    ) -> Result<RetrievedResource, ApplicationError> {
        match self.retrieve_internal(source, options, None).await? {
            HttpRetrievalOutcome::Resource(resource) => Ok(resource),
            HttpRetrievalOutcome::NotModified => Err(ApplicationError::RetrievalFailed(
                "unexpected not-modified result without cache validators".into(),
            )),
        }
    }
}

fn response_media_type(
    response: &reqwest::Response,
    url: &Url,
) -> Result<MediaType, ApplicationError> {
    if let Some(value) = response.headers().get(CONTENT_TYPE)
        && let Ok(value) = value.to_str()
    {
        let media_type = value
            .split(';')
            .next()
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase();
        if allowed_media_type(&media_type) {
            return Ok(MediaType(value.to_string()));
        }
        return Err(ApplicationError::RetrievalFailed(format!(
            "unsupported HTTP content type: {value}"
        )));
    }

    let inferred = media_type_from_url(url).ok_or_else(|| {
        ApplicationError::RetrievalFailed(
            "HTTP response has no supported Content-Type and URL extension is unknown".into(),
        )
    })?;
    Ok(MediaType(inferred.into()))
}

fn allowed_media_type(value: &str) -> bool {
    matches!(
        value,
        "text/plain"
            | "text/markdown"
            | "text/x-markdown"
            | "text/html"
            | "application/xhtml+xml"
            | "application/pdf"
            | "application/epub+zip"
            | "application/vnd.openxmlformats-officedocument.wordprocessingml.document"
            | "application/json"
            | "application/yaml"
            | "application/x-yaml"
            | "text/yaml"
            | "application/vnd.oai.openapi+json"
            | "application/vnd.oai.openapi+yaml"
    )
}

fn media_type_from_url(url: &Url) -> Option<&'static str> {
    let path = url.path().to_ascii_lowercase();
    if path.ends_with(".md") || path.ends_with(".markdown") {
        Some("text/markdown")
    } else if path.ends_with(".txt") || path.ends_with(".text") {
        Some("text/plain")
    } else if path.ends_with(".html") || path.ends_with(".htm") {
        Some("text/html")
    } else if path.ends_with(".pdf") {
        Some("application/pdf")
    } else if path.ends_with(".epub") {
        Some("application/epub+zip")
    } else if path.ends_with(".docx") {
        Some("application/vnd.openxmlformats-officedocument.wordprocessingml.document")
    } else if path.ends_with(".json") {
        Some("application/json")
    } else if path.ends_with(".yaml") || path.ends_with(".yml") {
        Some("application/yaml")
    } else {
        None
    }
}

fn header_string(
    response: &reqwest::Response,
    name: reqwest::header::HeaderName,
) -> Option<String> {
    response
        .headers()
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string)
}
