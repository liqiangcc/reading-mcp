use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use serde_json::json;

use crate::application::get_text_units::{EffectiveTextUnitKind, TextUnitDirection};
use crate::application::ports::{
    ApplicationError, BodyOrderWindow, DocumentHeader, DocumentRepository, LexicalSearchHit,
    OwnedStreamUnit, ParsedCacheKey, ParsedDocumentCache, Parser, RawResourceCache,
    RetrievalOptions, RetrievedResource, Retriever, SearchHit, SearchIndex, SectionRow,
    SectionTextUnits, StoredStreamItem, TextUnitIndex, TextUnitStreamKey, TextUnitStreamSummary,
};
use crate::domain::{
    Document, DocumentId, DocumentSegmentation, DocumentSource, NormalizedDocumentHash,
    ParagraphTextUnitSet, SectionId, SentenceTextUnitSet, TextUnit,
};

/// The process's current and peak resident set size, in KiB, from `/proc/self/status`.
///
/// Linux only, and only reached from the observed wrappers, which are installed only when
/// telemetry is on, so it costs nothing when telemetry is off. Any read or parse failure
/// yields `None`, and the fields are then simply left out of the event.
#[cfg(target_os = "linux")]
fn memory_counters() -> Option<(u64, u64)> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let mut rss_kib = None;
    let mut hwm_kib = None;
    for line in status.lines() {
        if let Some(value) = line.strip_prefix("VmRSS:") {
            rss_kib = parse_status_kib(value);
        } else if let Some(value) = line.strip_prefix("VmHWM:") {
            hwm_kib = parse_status_kib(value);
        }
    }
    Some((rss_kib?, hwm_kib?))
}

#[cfg(target_os = "linux")]
fn parse_status_kib(value: &str) -> Option<u64> {
    value.split_whitespace().next()?.parse().ok()
}

#[cfg(not(target_os = "linux"))]
fn memory_counters() -> Option<(u64, u64)> {
    None
}

fn emit(value: serde_json::Value) {
    let mut value = value;
    if let Some((rss_kib, hwm_kib)) = memory_counters()
        && let Some(object) = value.as_object_mut()
    {
        object.insert("rss_kib".into(), json!(rss_kib));
        object.insert("hwm_kib".into(), json!(hwm_kib));
    }
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

    fn emit_parse_event(
        started: Instant,
        bytes: usize,
        media_type: &str,
        result: Result<&Document, &ApplicationError>,
    ) {
        match result {
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
    }
}

#[async_trait]
impl Parser for ObservedParser {
    async fn parse(&self, resource: RetrievedResource) -> Result<Document, ApplicationError> {
        let started = Instant::now();
        let bytes = resource.bytes.len();
        let media_type = resource.media_type.0.clone();
        let result = self.inner.parse(resource).await;
        Self::emit_parse_event(started, bytes, &media_type, result.as_ref());
        result
    }

    async fn parse_with_segmentation(
        &self,
        resource: RetrievedResource,
    ) -> Result<(Document, Option<Arc<DocumentSegmentation>>), ApplicationError> {
        let started = Instant::now();
        let bytes = resource.bytes.len();
        let media_type = resource.media_type.0.clone();
        let result = self.inner.parse_with_segmentation(resource).await;
        Self::emit_parse_event(
            started,
            bytes,
            &media_type,
            result.as_ref().map(|(document, _)| document),
        );
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

    /// Forwarded, not defaulted: a no-op here would silently disable the warm in the runtime, as a
    /// no-op `get_with_hash` did for `ObservedDocumentRepository` before #207.
    fn prewarm(&self, document_id: &DocumentId, normalized_document_hash: &NormalizedDocumentHash) {
        self.inner.prewarm(document_id, normalized_document_hash);
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

    async fn index_with_segmentation(
        &self,
        document: Arc<Document>,
        normalized_document_hash: NormalizedDocumentHash,
        segmentation: Arc<DocumentSegmentation>,
    ) -> Result<(), ApplicationError> {
        let started = Instant::now();
        let result = self
            .inner
            .index_with_segmentation(
                Arc::clone(&document),
                normalized_document_hash,
                Arc::clone(&segmentation),
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

    async fn search_lexical_in(
        &self,
        document: &Document,
        normalized_document_hash: &NormalizedDocumentHash,
        query: &str,
        limit: usize,
    ) -> Result<Vec<LexicalSearchHit>, ApplicationError> {
        let started = Instant::now();
        let result = self
            .inner
            .search_lexical_in(document, normalized_document_hash, query, limit)
            .await;
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

    async fn get_light_with_hash(
        &self,
        id: &DocumentId,
    ) -> Result<Option<(Arc<Document>, NormalizedDocumentHash)>, ApplicationError> {
        let started = Instant::now();
        let result = self.inner.get_light_with_hash(id).await;
        emit(json!({
            "event": "repository_get",
            "duration_ms": started.elapsed().as_millis(),
            "hit": result.as_ref().is_ok_and(|value| value.is_some()),
            "success": result.is_ok()
        }));
        result
    }

    async fn get_rebuilt_with_hash(
        &self,
        id: &DocumentId,
        with_text: bool,
    ) -> Result<Option<(Arc<Document>, NormalizedDocumentHash)>, ApplicationError> {
        let started = Instant::now();
        let result = self.inner.get_rebuilt_with_hash(id, with_text).await;
        emit(json!({
            "event": "repository_get",
            "duration_ms": started.elapsed().as_millis(),
            "hit": result.as_ref().is_ok_and(|value| value.is_some()),
            "success": result.is_ok()
        }));
        result
    }

    async fn load_metadata(
        &self,
        id: &DocumentId,
        key: &str,
    ) -> Result<Option<String>, ApplicationError> {
        self.inner.load_metadata(id, key).await
    }

    async fn load_section_text(
        &self,
        id: &DocumentId,
        section_id: &SectionId,
    ) -> Result<Option<String>, ApplicationError> {
        self.inner.load_section_text(id, section_id).await
    }

    async fn load_section_char_len(
        &self,
        id: &DocumentId,
        section_id: &SectionId,
    ) -> Result<Option<usize>, ApplicationError> {
        self.inner.load_section_char_len(id, section_id).await
    }

    async fn body_order(
        &self,
        id: &DocumentId,
    ) -> Result<Option<Result<Vec<SectionId>, ApplicationError>>, ApplicationError> {
        self.inner.body_order(id).await
    }

    async fn body_order_keys(
        &self,
        id: &DocumentId,
    ) -> Result<Option<Result<Vec<i64>, ApplicationError>>, ApplicationError> {
        self.inner.body_order_keys(id).await
    }

    // The section-row calls are forwarded, never left to their trait defaults: a default here would
    // load the whole document inside the wrapper and silently disable the fast path (#207).
    async fn document_header(
        &self,
        id: &DocumentId,
    ) -> Result<Option<DocumentHeader>, ApplicationError> {
        self.inner.document_header(id).await
    }

    async fn section_by_id(
        &self,
        id: &DocumentId,
        section_id: &SectionId,
    ) -> Result<Option<SectionRow>, ApplicationError> {
        self.inner.section_by_id(id, section_id).await
    }

    async fn section_by_key(
        &self,
        id: &DocumentId,
        section_key: i64,
    ) -> Result<Option<SectionRow>, ApplicationError> {
        self.inner.section_by_key(id, section_key).await
    }

    async fn sections_in_key_range(
        &self,
        id: &DocumentId,
        from_key: i64,
        to_key_inclusive: i64,
        limit: usize,
    ) -> Result<Vec<SectionRow>, ApplicationError> {
        self.inner
            .sections_in_key_range(id, from_key, to_key_inclusive, limit)
            .await
    }

    async fn section_children(
        &self,
        id: &DocumentId,
        parent_key: Option<i64>,
        limit: usize,
    ) -> Result<Vec<SectionRow>, ApplicationError> {
        self.inner.section_children(id, parent_key, limit).await
    }

    async fn section_ancestors(
        &self,
        id: &DocumentId,
        section_key: i64,
    ) -> Result<Vec<SectionRow>, ApplicationError> {
        self.inner.section_ancestors(id, section_key).await
    }

    async fn body_order_window(
        &self,
        id: &DocumentId,
        section_key: i64,
        before: usize,
        after: usize,
    ) -> Result<Option<BodyOrderWindow>, ApplicationError> {
        self.inner
            .body_order_window(id, section_key, before, after)
            .await
    }

    async fn body_order_position(
        &self,
        id: &DocumentId,
        section_key: i64,
    ) -> Result<Option<usize>, ApplicationError> {
        self.inner.body_order_position(id, section_key).await
    }

    async fn body_order_at(
        &self,
        id: &DocumentId,
        ord: usize,
    ) -> Result<Option<i64>, ApplicationError> {
        self.inner.body_order_at(id, ord).await
    }

    async fn body_order_len(&self, id: &DocumentId) -> Result<Option<usize>, ApplicationError> {
        self.inner.body_order_len(id).await
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

    async fn replace_document_segmentation(
        &self,
        document: &Arc<Document>,
        segmentation: Arc<DocumentSegmentation>,
    ) -> Result<(), ApplicationError> {
        let started = Instant::now();
        let result = self
            .inner
            .replace_document_segmentation(document, segmentation)
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

    async fn document_key(
        &self,
        document_id: &DocumentId,
    ) -> Result<Option<i64>, ApplicationError> {
        let started = Instant::now();
        let result = self.inner.document_key(document_id).await;
        emit_text_unit_read(started, "document_key", result.is_ok());
        result
    }

    async fn document_id_for_key(
        &self,
        doc_key: i64,
    ) -> Result<Option<DocumentId>, ApplicationError> {
        let started = Instant::now();
        let result = self.inner.document_id_for_key(doc_key).await;
        emit_text_unit_read(started, "document_id_for_key", result.is_ok());
        result
    }

    async fn section_key(
        &self,
        document_id: &DocumentId,
        section_id: &SectionId,
    ) -> Result<Option<i64>, ApplicationError> {
        let started = Instant::now();
        let result = self.inner.section_key(document_id, section_id).await;
        emit_text_unit_read(started, "section_key", result.is_ok());
        result
    }

    async fn section_id_for_key(
        &self,
        document_id: &DocumentId,
        section_key: i64,
    ) -> Result<Option<SectionId>, ApplicationError> {
        let started = Instant::now();
        let result = self
            .inner
            .section_id_for_key(document_id, section_key)
            .await;
        emit_text_unit_read(started, "section_id_for_key", result.is_ok());
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
        ApplicationError::OcrRequired(_) => "ocr_required",
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

/// The phases of one cache-miss decode of `document_json`, measured inside
/// `SqliteDocumentRepository::get_with_hash`. Milliseconds are floating point: a phase of a fast
/// document is well below 1 ms and must not round to zero.
#[doc(hidden)]
#[derive(Clone, Copy, Debug, Default)]
pub struct DocumentDecodePhases {
    /// The length of the `document_json` string, i.e. the bytes the row carries.
    pub json_bytes: usize,
    /// The `SELECT document_json, normalized_document_hash` row read.
    pub row_read_ms: f64,
    /// `serde_json::from_str::<StoredDocument>`.
    pub parse_ms: f64,
    /// `StoredDocument::into_document`, which for a chunked document loads every section's text.
    pub section_text_load_ms: f64,
    /// `Document::validate_ocr_publication`.
    pub validate_ms: f64,
    /// `Document::normalized_document_hash`, only when the stored hash column was empty.
    pub hash_ms: Option<f64>,
}

/// Emits the phase breakdown of one full document load as a `repository_get` event, plus the byte
/// size of every `metadata` entry of the decoded document. Only a runtime with `READING_MCP_TELEMETRY`
/// on emits it.
#[doc(hidden)]
pub fn emit_document_decode_miss(document: &Document, phases: DocumentDecodePhases) {
    let mut metadata_key_bytes = document
        .metadata
        .iter()
        .map(|(key, value)| (key.clone(), key.len() + value.len()))
        .collect::<Vec<_>>();
    metadata_key_bytes.sort_by_key(|entry| std::cmp::Reverse(entry.1));
    emit(json!({
        "event": "repository_get",
        "stage": "decode",
        "document_id": document.id.0,
        "json_bytes": phases.json_bytes,
        "row_read_ms": phases.row_read_ms,
        "parse_ms": phases.parse_ms,
        "section_text_load_ms": phases.section_text_load_ms,
        "validate_ms": phases.validate_ms,
        "hash_ms": phases.hash_ms,
        "sections": document.section_count(),
        "metadata_key_bytes": metadata_key_bytes
            .into_iter()
            .map(|(key, bytes)| json!({ "key": key, "bytes": bytes }))
            .collect::<Vec<_>>(),
    }));
}

/// Emits one `scan_prewarm` event: how many section texts the background warm of the scan text cache
/// stored and how long it took. Only a runtime with `READING_MCP_TELEMETRY` on emits it.
#[doc(hidden)]
pub fn emit_scan_prewarm(document_id: &DocumentId, sections: usize, duration_ms: u64, ok: bool) {
    emit(json!({
        "event": "scan_prewarm",
        "document_id": document_id.0,
        "sections": sections,
        "duration_ms": duration_ms,
        "success": ok,
    }));
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
