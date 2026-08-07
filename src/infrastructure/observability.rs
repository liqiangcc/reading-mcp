use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use serde_json::json;

use crate::application::ports::{
    ApplicationError, Parser, RetrievalOptions, RetrievedResource, Retriever, SearchHit,
    SearchIndex,
};
use crate::domain::{Document, DocumentId, DocumentSource};

fn emit(value: serde_json::Value) {
    eprintln!("{value}");
}

pub struct ObservedRetriever {
    inner: Arc<dyn Retriever>,
}

impl ObservedRetriever {
    pub fn new(inner: Arc<dyn Retriever>) -> Self {
        Self { inner }
    }
}

#[async_trait]
impl Retriever for ObservedRetriever {
    async fn retrieve(
        &self,
        source: &DocumentSource,
        options: &RetrievalOptions,
    ) -> Result<RetrievedResource, ApplicationError> {
        let started = Instant::now();
        let result = self.inner.retrieve(source, options).await;
        match &result {
            Ok(resource) => emit(json!({
                "event": "retrieve",
                "duration_ms": started.elapsed().as_millis(),
                "bytes": resource.bytes.len(),
                "media_type": resource.media_type.0,
                "force_refresh": options.force_refresh,
                "success": true
            })),
            Err(error) => emit(json!({
                "event": "retrieve",
                "duration_ms": started.elapsed().as_millis(),
                "force_refresh": options.force_refresh,
                "error_class": error_class(error),
                "success": false
            })),
        }
        result
    }
}

pub struct ObservedParser {
    inner: Arc<dyn Parser>,
}

impl ObservedParser {
    pub fn new(inner: Arc<dyn Parser>) -> Self {
        Self { inner }
    }
}

#[async_trait]
impl Parser for ObservedParser {
    async fn parse(&self, resource: RetrievedResource) -> Result<Document, ApplicationError> {
        let started = Instant::now();
        let bytes = resource.bytes.len();
        let media_type = resource.media_type.0.clone();
        let result = self.inner.parse(resource).await;
        match &result {
            Ok(document) => emit(json!({
                "event": "parse",
                "duration_ms": started.elapsed().as_millis(),
                "bytes": bytes,
                "media_type": media_type,
                "sections": document.section_count(),
                "pdf_pages": document.metadata.get("pdf_page_count"),
                "success": true
            })),
            Err(error) => emit(json!({
                "event": "parse",
                "duration_ms": started.elapsed().as_millis(),
                "bytes": bytes,
                "media_type": media_type,
                "error_class": error_class(error),
                "success": false
            })),
        }
        result
    }
}

pub struct ObservedSearchIndex {
    inner: Arc<dyn SearchIndex>,
}

impl ObservedSearchIndex {
    pub fn new(inner: Arc<dyn SearchIndex>) -> Self {
        Self { inner }
    }
}

#[async_trait]
impl SearchIndex for ObservedSearchIndex {
    async fn index(&self, document: &Document) -> Result<(), ApplicationError> {
        let started = Instant::now();
        let result = self.inner.index(document).await;
        emit(json!({
            "event": "index",
            "duration_ms": started.elapsed().as_millis(),
            "sections": document.section_count(),
            "success": result.is_ok()
        }));
        result
    }

    async fn search(
        &self,
        document_id: &DocumentId,
        query: &str,
        limit: usize,
    ) -> Result<Vec<SearchHit>, ApplicationError> {
        let started = Instant::now();
        let result = self.inner.search(document_id, query, limit).await;
        emit(json!({
            "event": "search",
            "duration_ms": started.elapsed().as_millis(),
            "query_chars": query.chars().count(),
            "limit": limit,
            "hits": result.as_ref().map(Vec::len).unwrap_or_default(),
            "success": result.is_ok()
        }));
        result
    }
}

fn error_class(error: &ApplicationError) -> &'static str {
    match error {
        ApplicationError::BlockedSource(_) => "blocked_source",
        ApplicationError::RetrievalFailed(_) => "retrieval_failed",
        ApplicationError::ParseFailed(_) => "parse_failed",
        ApplicationError::ResourceLimitExceeded(_) => "resource_limit",
        ApplicationError::AuthenticationFailed(_) => "authentication_failed",
        ApplicationError::RepositoryFailed(_) => "repository_failed",
        ApplicationError::CacheFailed(_) => "cache_failed",
        ApplicationError::IndexFailed(_) => "index_failed",
        ApplicationError::DocumentNotFound => "document_not_found",
        ApplicationError::SectionNotFound => "section_not_found",
        ApplicationError::InvalidRequest(_) => "invalid_request",
    }
}
