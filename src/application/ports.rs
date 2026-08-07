use std::collections::BTreeMap;

use async_trait::async_trait;
use thiserror::Error;

use crate::domain::{Document, DocumentId, DocumentSource, Location, MediaType, SectionId};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RetrievalOptions {
    pub auth_profile: Option<String>,
    pub force_refresh: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RetrievedResource {
    pub source: DocumentSource,
    pub final_source: DocumentSource,
    pub media_type: MediaType,
    pub bytes: Vec<u8>,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    pub metadata: BTreeMap<String, String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SearchHit {
    pub section_id: SectionId,
    pub snippet: String,
    pub score: f32,
    pub location: Location,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ParsedCacheKey {
    pub final_source: DocumentSource,
    pub raw_sha256: String,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ApplicationError {
    #[error("source blocked: {0}")]
    BlockedSource(String),
    #[error("retrieval failed: {0}")]
    RetrievalFailed(String),
    #[error("parse failed: {0}")]
    ParseFailed(String),
    #[error("document repository failed: {0}")]
    RepositoryFailed(String),
    #[error("cache failed: {0}")]
    CacheFailed(String),
    #[error("search index failed: {0}")]
    IndexFailed(String),
    #[error("document not found")]
    DocumentNotFound,
    #[error("section not found")]
    SectionNotFound,
    #[error("invalid request: {0}")]
    InvalidRequest(String),
}

#[async_trait]
pub trait SourcePolicy: Send + Sync {
    async fn validate(&self, source: &DocumentSource) -> Result<(), ApplicationError>;
}

#[async_trait]
pub trait Retriever: Send + Sync {
    async fn retrieve(
        &self,
        source: &DocumentSource,
        options: &RetrievalOptions,
    ) -> Result<RetrievedResource, ApplicationError>;
}

#[async_trait]
pub trait Parser: Send + Sync {
    async fn parse(&self, resource: RetrievedResource) -> Result<Document, ApplicationError>;
}

#[async_trait]
pub trait RawResourceCache: Send + Sync {
    async fn get(
        &self,
        source: &DocumentSource,
    ) -> Result<Option<RetrievedResource>, ApplicationError>;

    async fn put(
        &self,
        source: &DocumentSource,
        resource: RetrievedResource,
    ) -> Result<(), ApplicationError>;
}

#[async_trait]
pub trait ParsedDocumentCache: Send + Sync {
    async fn get(&self, key: &ParsedCacheKey) -> Result<Option<Document>, ApplicationError>;

    async fn put(&self, key: ParsedCacheKey, document: Document) -> Result<(), ApplicationError>;
}

#[async_trait]
pub trait DocumentRepository: Send + Sync {
    async fn save(&self, document: Document) -> Result<(), ApplicationError>;

    async fn get(&self, id: &DocumentId) -> Result<Option<Document>, ApplicationError>;
}

#[async_trait]
pub trait SearchIndex: Send + Sync {
    async fn index(&self, document: &Document) -> Result<(), ApplicationError>;

    async fn search(
        &self,
        document_id: &DocumentId,
        query: &str,
        limit: usize,
    ) -> Result<Vec<SearchHit>, ApplicationError>;
}
