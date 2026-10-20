use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use serde_json::json;

use crate::application::get_text_units::{EffectiveTextUnitKind, TextUnitDirection};
use crate::application::ports::{
    ApplicationError, DocumentRepository, LexicalSearchHit, OwnedStreamUnit, ParsedCacheKey,
    ParsedDocumentCache, Parser, RawResourceCache, RetrievalOptions, RetrievedResource, Retriever,
    SearchHit, SearchIndex, SectionTextUnits, StoredStreamItem, TextUnitIndex, TextUnitStreamKey,
    TextUnitStreamSummary,
};
use crate::domain::{
    Document, DocumentId, DocumentSource, NormalizedDocumentHash, ParagraphTextUnitSet, SectionId,
    SentenceTextUnitSet, TextUnit,
};

fn emit(value: serde_json::Value) {
    eprintln!("{value}");
}

pub struct ObservedRawResourceCache {
    inner: Arc<dyn RawResourceCache>,
}

impl ObservedRawResourceCache {
    pub fn new(inner: Arc<dyn RawResourceCache>) -> Self {
        Self { inner }
    }
}

#[async_trait]
impl RawResourceCache for ObservedRawResourceCache {
    async fn get(
        &self,
        source: &DocumentSource,
    ) -> Result<Option<RetrievedResource>, ApplicationError> {
        let started = Instant::now();
        let result = self.inner.get(source).await;
        emit(json!({
            "event": "raw_cache_get",
            "duration_ms": started.elapsed().as_millis(),
            "hit": result.as_ref().is_ok_and(|value| value.is_some()),
            "success": result.is_ok()
        }));
        result
    }

    async fn put(
        &self,
        source: &DocumentSource,
        resource: RetrievedResource,
    ) -> Result<(), ApplicationError> {
        let started = Instant::now();
        let result = self.inner.put(source, resource).await;
        emit(json!({
            "event": "raw_cache_put",
            "duration_ms": started.elapsed().as_millis(),
            "success": result.is_ok()
        }));
        result
    }
}

pub struct ObservedParsedDocumentCache {
    inner: Arc<dyn ParsedDocumentCache>,
}

impl ObservedParsedDocumentCache {
    pub fn new(inner: Arc<dyn ParsedDocumentCache>) -> Self {
        Self { inner }
    }
}

#[async_trait]
impl ParsedDocumentCache for ObservedParsedDocumentCache {
    async fn get(&self, key: &ParsedCacheKey) -> Result<Option<Document>, ApplicationError> {
        let started = Instant::now();
        let result = self.inner.get(key).await;
        emit(json!({
            "event": "parsed_cache_get",
            "duration_ms": started.elapsed().as_millis(),
            "hit": result.as_ref().is_ok_and(|value| value.is_some()),
            "success": result.is_ok()
        }));
        result
    }

    async fn put(&self, key: ParsedCacheKey, document: Document) -> Result<(), ApplicationError> {
        let started = Instant::now();
        let result = self.inner.put(key, document).await;
        emit(json!({
            "event": "parsed_cache_put",
            "duration_ms": started.elapsed().as_millis(),
            "success": result.is_ok()
        }));
        result
    }
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
    fn tokenizer_version(&self) -> &'static str {
        self.inner.tokenizer_version()
    }

    fn index_version(&self) -> &'static str {
        self.inner.index_version()
    }

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

    async fn index_with_units(
        &self,
        document: Arc<Document>,
        paragraphs: Arc<ParagraphTextUnitSet>,
        sentences: Arc<SentenceTextUnitSet>,
    ) -> Result<(), ApplicationError> {
        let started = Instant::now();
        let result = self
            .inner
            .index_with_units(
                Arc::clone(&document),
                Arc::clone(&paragraphs),
                Arc::clone(&sentences),
            )
            .await;
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
        emit_search_event(
            started,
            query,
            limit,
            result.as_ref().map(Vec::len).unwrap_or_default(),
            result.is_ok(),
        );
        result
    }

    async fn search_lexical(
        &self,
        document_id: &DocumentId,
        query: &str,
        limit: usize,
    ) -> Result<Vec<LexicalSearchHit>, ApplicationError> {
        let started = Instant::now();
        let result = self.inner.search_lexical(document_id, query, limit).await;
        emit_search_event(
            started,
            query,
            limit,
            result.as_ref().map(Vec::len).unwrap_or_default(),
            result.is_ok(),
        );
        result
    }
}

pub struct ObservedDocumentRepository {
    inner: Arc<dyn DocumentRepository>,
}

impl ObservedDocumentRepository {
    pub fn new(inner: Arc<dyn DocumentRepository>) -> Self {
        Self { inner }
    }
}

#[async_trait]
impl DocumentRepository for ObservedDocumentRepository {
    async fn save(&self, document: Document) -> Result<(), ApplicationError> {
        let started = Instant::now();
        let sections = document.section_count();
        let result = self.inner.save(document).await;
        emit(json!({
            "event": "repository_save",
            "duration_ms": started.elapsed().as_millis(),
            "sections": sections,
            "success": result.is_ok()
        }));
        result
    }

    async fn save_shared(&self, document: Arc<Document>) -> Result<(), ApplicationError> {
        let started = Instant::now();
        let sections = document.section_count();
        let result = self.inner.save_shared(document).await;
        emit(json!({
            "event": "repository_save",
            "duration_ms": started.elapsed().as_millis(),
            "sections": sections,
            "success": result.is_ok()
        }));
        result
    }

    async fn get(&self, id: &DocumentId) -> Result<Option<Arc<Document>>, ApplicationError> {
        let started = Instant::now();
        let result = self.inner.get(id).await;
        emit(json!({
            "event": "repository_get",
            "duration_ms": started.elapsed().as_millis(),
            "hit": result.as_ref().is_ok_and(|value| value.is_some()),
            "success": result.is_ok()
        }));
        result
    }

    async fn get_with_hash(
        &self,
        id: &DocumentId,
    ) -> Result<Option<(Arc<Document>, NormalizedDocumentHash)>, ApplicationError> {
        let started = Instant::now();
        let result = self.inner.get_with_hash(id).await;
        emit(json!({
            "event": "repository_get",
            "duration_ms": started.elapsed().as_millis(),
            "hit": result.as_ref().is_ok_and(|value| value.is_some()),
            "success": result.is_ok()
        }));
        result
    }
}

pub struct ObservedTextUnitIndex {
    inner: Arc<dyn TextUnitIndex>,
}

impl ObservedTextUnitIndex {
    pub fn new(inner: Arc<dyn TextUnitIndex>) -> Self {
        Self { inner }
    }
}

fn emit_text_unit_write(started: Instant, success: bool) {
    emit(json!({
        "event": "text_unit_sets_write",
        "duration_ms": started.elapsed().as_millis(),
        "success": success
    }));
}

fn emit_text_unit_read(started: Instant, method: &'static str, success: bool) {
    emit(json!({
        "event": "text_unit_read",
        "duration_ms": started.elapsed().as_millis(),
        "method": method,
        "success": success
    }));
}

#[async_trait]
impl TextUnitIndex for ObservedTextUnitIndex {
    async fn replace_document(
        &self,
        document_id: &DocumentId,
        units: &[TextUnit],
    ) -> Result<(), ApplicationError> {
        let started = Instant::now();
        let result = self.inner.replace_document(document_id, units).await;
        emit_text_unit_write(started, result.is_ok());
        result
    }

    async fn list_document(
        &self,
        document_id: &DocumentId,
    ) -> Result<Vec<TextUnit>, ApplicationError> {
        let started = Instant::now();
        let result = self.inner.list_document(document_id).await;
        emit_text_unit_read(started, "list_document", result.is_ok());
        result
    }

    async fn replace_document_sets(
        &self,
        document: &Arc<Document>,
        paragraphs: Arc<ParagraphTextUnitSet>,
        sentences: Arc<SentenceTextUnitSet>,
    ) -> Result<(), ApplicationError> {
        let started = Instant::now();
        let result = self
            .inner
            .replace_document_sets(document, paragraphs, sentences)
            .await;
        emit_text_unit_write(started, result.is_ok());
        result
    }

    async fn section_units(
        &self,
        document_id: &DocumentId,
        section_id: &SectionId,
    ) -> Result<Option<SectionTextUnits>, ApplicationError> {
        let started = Instant::now();
        let result = self.inner.section_units(document_id, section_id).await;
        emit_text_unit_read(started, "section_units", result.is_ok());
        result
    }

    async fn section_unit(
        &self,
        document_id: &DocumentId,
        section_id: &SectionId,
        kind: EffectiveTextUnitKind,
        paragraph_index: usize,
        sentence_index: Option<usize>,
    ) -> Result<Option<OwnedStreamUnit>, ApplicationError> {
        let started = Instant::now();
        let result = self
            .inner
            .section_unit(
                document_id,
                section_id,
                kind,
                paragraph_index,
                sentence_index,
            )
            .await;
        emit_text_unit_read(started, "section_unit", result.is_ok());
        result
    }

    async fn section_stream_summary(
        &self,
        key: TextUnitStreamKey<'_>,
    ) -> Result<Option<TextUnitStreamSummary>, ApplicationError> {
        let started = Instant::now();
        let result = self.inner.section_stream_summary(key).await;
        emit_text_unit_read(started, "section_stream_summary", result.is_ok());
        result
    }

    async fn section_stream_items(
        &self,
        key: TextUnitStreamKey<'_>,
        direction: TextUnitDirection,
        position: usize,
        limit: usize,
    ) -> Result<Vec<StoredStreamItem>, ApplicationError> {
        let started = Instant::now();
        let result = self
            .inner
            .section_stream_items(key, direction, position, limit)
            .await;
        emit_text_unit_read(started, "section_stream_items", result.is_ok());
        result
    }

    async fn section_stream_position(
        &self,
        key: TextUnitStreamKey<'_>,
        unit_kind: EffectiveTextUnitKind,
        paragraph_index: usize,
        sentence_index: Option<usize>,
    ) -> Result<Option<usize>, ApplicationError> {
        let started = Instant::now();
        let result = self
            .inner
            .section_stream_position(key, unit_kind, paragraph_index, sentence_index)
            .await;
        emit_text_unit_read(started, "section_stream_position", result.is_ok());
        result
    }
}

fn emit_search_event(started: Instant, query: &str, limit: usize, hits: usize, success: bool) {
    emit(json!({
        "event": "search",
        "duration_ms": started.elapsed().as_millis(),
        "query_chars": query.chars().count(),
        "limit": limit,
        "hits": hits,
        "success": success
    }));
}

fn error_class(error: &ApplicationError) -> &'static str {
    match error {
        ApplicationError::BlockedSource(_) => "blocked_source",
        ApplicationError::RetrievalFailed(_) => "retrieval_failed",
        ApplicationError::ParseFailed(_) => "parse_failed",
        ApplicationError::UnsupportedTextEncoding(_) => "unsupported_text_encoding",
        ApplicationError::OcrFailed => "ocr_failed",
        ApplicationError::OcrUnavailable => "ocr_unavailable",
        ApplicationError::OcrRequired => "ocr_required",
        ApplicationError::OcrBusy => "ocr_busy",
        ApplicationError::OcrTimeout => "ocr_timeout",
        ApplicationError::OcrResourceLimit => "ocr_resource_limit",
        ApplicationError::OcrNoSupportedProjection => "ocr_no_supported_projection",
        ApplicationError::SourceViewFailed(_) => "source_view_failed",
        ApplicationError::ResourceLimitExceeded(_) => "resource_limit",
        ApplicationError::AuthenticationFailed(_) => "authentication_failed",
        ApplicationError::RepositoryFailed(_) => "repository_failed",
        ApplicationError::CacheFailed(_) => "cache_failed",
        ApplicationError::IndexFailed(_) => "index_failed",
        ApplicationError::TextUnitIndexFailed(_) => "text_unit_index_failed",
        ApplicationError::DocumentNotFound => "document_not_found",
        ApplicationError::SectionNotFound => "section_not_found",
        ApplicationError::InvalidLocator(_) => "invalid_locator",
        ApplicationError::StaleLocator(_) => "stale_locator",
        ApplicationError::StaleStructure(_) => "stale_structure",
        ApplicationError::StaleDocument(_) => "stale_document",
        ApplicationError::InvalidCursor(_) => "invalid_cursor",
        ApplicationError::StaleCursor(_) => "stale_cursor",
        ApplicationError::CursorTargetMismatch(_) => "cursor_target_mismatch",
        ApplicationError::CursorEncodingFailed(_) => "cursor_encoding_failed",
        ApplicationError::InvalidRequest(_) => "invalid_request",
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use crate::application::get_text_units::{RequestedTextUnitKind, TextUnitCoveragePolicy};

    use super::*;

    #[derive(Default)]
    struct FakeTextUnitIndex {
        summaries: AtomicUsize,
        items: AtomicUsize,
        positions: AtomicUsize,
        units: AtomicUsize,
    }

    #[async_trait]
    impl TextUnitIndex for FakeTextUnitIndex {
        async fn replace_document(
            &self,
            _document_id: &DocumentId,
            _units: &[TextUnit],
        ) -> Result<(), ApplicationError> {
            Ok(())
        }

        async fn list_document(
            &self,
            _document_id: &DocumentId,
        ) -> Result<Vec<TextUnit>, ApplicationError> {
            Ok(Vec::new())
        }

        async fn replace_document_sets(
            &self,
            _document: &Arc<Document>,
            _paragraphs: Arc<ParagraphTextUnitSet>,
            _sentences: Arc<SentenceTextUnitSet>,
        ) -> Result<(), ApplicationError> {
            Ok(())
        }

        async fn section_units(
            &self,
            _document_id: &DocumentId,
            _section_id: &SectionId,
        ) -> Result<Option<SectionTextUnits>, ApplicationError> {
            panic!("ObservedTextUnitIndex must forward reads, never fall back to section_units");
        }

        async fn section_unit(
            &self,
            _document_id: &DocumentId,
            _section_id: &SectionId,
            _kind: EffectiveTextUnitKind,
            _paragraph_index: usize,
            _sentence_index: Option<usize>,
        ) -> Result<Option<OwnedStreamUnit>, ApplicationError> {
            self.units.fetch_add(1, Ordering::SeqCst);
            Ok(None)
        }

        async fn section_stream_summary(
            &self,
            _key: TextUnitStreamKey<'_>,
        ) -> Result<Option<TextUnitStreamSummary>, ApplicationError> {
            self.summaries.fetch_add(1, Ordering::SeqCst);
            Ok(None)
        }

        async fn section_stream_items(
            &self,
            _key: TextUnitStreamKey<'_>,
            _direction: TextUnitDirection,
            _position: usize,
            _limit: usize,
        ) -> Result<Vec<StoredStreamItem>, ApplicationError> {
            self.items.fetch_add(1, Ordering::SeqCst);
            Ok(Vec::new())
        }

        async fn section_stream_position(
            &self,
            _key: TextUnitStreamKey<'_>,
            _unit_kind: EffectiveTextUnitKind,
            _paragraph_index: usize,
            _sentence_index: Option<usize>,
        ) -> Result<Option<usize>, ApplicationError> {
            self.positions.fetch_add(1, Ordering::SeqCst);
            Ok(None)
        }
    }

    #[tokio::test]
    async fn observed_text_unit_index_forwards_every_read_to_the_inner_overrides() {
        let fake = Arc::new(FakeTextUnitIndex::default());
        let observed = ObservedTextUnitIndex::new(fake.clone());
        let document_id = DocumentId("doc".into());
        let section_id = SectionId("section".into());
        let key = TextUnitStreamKey {
            document_id: &document_id,
            section_id: &section_id,
            kind: RequestedTextUnitKind::Paragraph,
            policy: TextUnitCoveragePolicy::PreserveSource,
        };

        observed.section_stream_summary(key).await.unwrap();
        observed
            .section_stream_items(key, TextUnitDirection::Forward, 0, 10)
            .await
            .unwrap();
        observed
            .section_stream_position(key, EffectiveTextUnitKind::Paragraph, 1, None)
            .await
            .unwrap();
        observed
            .section_unit(
                &document_id,
                &section_id,
                EffectiveTextUnitKind::Paragraph,
                1,
                None,
            )
            .await
            .unwrap();

        assert_eq!(fake.summaries.load(Ordering::SeqCst), 1);
        assert_eq!(fake.items.load(Ordering::SeqCst), 1);
        assert_eq!(fake.positions.load(Ordering::SeqCst), 1);
        assert_eq!(fake.units.load(Ordering::SeqCst), 1);
    }
}
