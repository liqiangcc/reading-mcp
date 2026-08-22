use std::collections::BTreeMap;

use async_trait::async_trait;
use thiserror::Error;

use crate::domain::{
    Document, DocumentId, DocumentSource, Location, MediaType, SectionId, TextLocator, TextUnit,
};

use super::reading_profile::ReliabilitySummary;

pub const LEXICAL_TOKENIZER_VERSION: &str = "lexical-tokenizer/v1";
pub const LEGACY_SEARCH_TOKENIZER_VERSION: &str = "legacy-search-tokenizer/unversioned";

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
    pub title: String,
    pub source: DocumentSource,
    pub snippet: String,
    pub score: f32,
    pub location: Location,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SearchHitKind {
    Section,
    Paragraph,
    Sentence,
}

impl SearchHitKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Section => "section",
            Self::Paragraph => "paragraph",
            Self::Sentence => "sentence",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct LexicalSearchHit {
    pub section_id: SectionId,
    pub title: String,
    pub source: DocumentSource,
    pub snippet: String,
    pub score: f32,
    pub location: Location,
    pub candidate_kind: SearchHitKind,
    pub text_locator: TextLocator,
    pub tokenizer_version: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ParsedCacheKey {
    pub final_source: DocumentSource,
    pub raw_sha256: String,
    pub normalization_version: String,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ApplicationError {
    #[error("source blocked: {0}")]
    BlockedSource(String),
    #[error("retrieval failed: {0}")]
    RetrievalFailed(String),
    #[error("parse failed: {0}")]
    ParseFailed(String),
    #[error("resource limit exceeded: {0}")]
    ResourceLimitExceeded(String),
    #[error("authentication profile failed: {0}")]
    AuthenticationFailed(String),
    #[error("document repository failed: {0}")]
    RepositoryFailed(String),
    #[error("cache failed: {0}")]
    CacheFailed(String),
    #[error("search index failed: {0}")]
    IndexFailed(String),
    #[error("text unit index failed: {0}")]
    TextUnitIndexFailed(String),
    #[error("document not found")]
    DocumentNotFound,
    #[error("section not found")]
    SectionNotFound,
    #[error("invalid source locator: {0}")]
    InvalidLocator(String),
    #[error("stale source locator: {0}")]
    StaleLocator(String),
    #[error("invalid read cursor: {0}")]
    InvalidCursor(String),
    #[error("stale read cursor: {0}")]
    StaleCursor(String),
    #[error("read cursor target mismatch: {0}")]
    CursorTargetMismatch(String),
    #[error("read cursor encoding failed: {0}")]
    CursorEncodingFailed(String),
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

pub trait DocumentReliabilityInspector: Send + Sync {
    fn inspect(&self, document: &Document) -> Result<ReliabilitySummary, ApplicationError>;
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
pub trait TextUnitIndex: Send + Sync {
    async fn replace_document(
        &self,
        document_id: &DocumentId,
        units: &[TextUnit],
    ) -> Result<(), ApplicationError>;

    async fn list_document(
        &self,
        document_id: &DocumentId,
    ) -> Result<Vec<TextUnit>, ApplicationError>;
}

#[async_trait]
pub trait SearchIndex: Send + Sync {
    fn supports_precise_lexical_candidates(&self) -> bool {
        self.tokenizer_version() != LEGACY_SEARCH_TOKENIZER_VERSION
    }

    fn tokenizer_version(&self) -> &'static str {
        LEGACY_SEARCH_TOKENIZER_VERSION
    }

    async fn index(&self, document: &Document) -> Result<(), ApplicationError>;

    async fn search(
        &self,
        document_id: &DocumentId,
        query: &str,
        limit: usize,
    ) -> Result<Vec<SearchHit>, ApplicationError>;

    async fn search_lexical(
        &self,
        _document_id: &DocumentId,
        _query: &str,
        _limit: usize,
    ) -> Result<Vec<LexicalSearchHit>, ApplicationError> {
        Err(ApplicationError::IndexFailed(
            "search adapter does not implement canonical lexical candidates".into(),
        ))
    }
}
