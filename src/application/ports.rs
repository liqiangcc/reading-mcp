use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use thiserror::Error;

use crate::domain::{
    Document, DocumentId, DocumentSegmentation, DocumentSource, Location, MediaType,
    NormalizedDocumentHash, ParagraphContentClass, ParagraphSectionCoverage, ParagraphTextUnitSet,
    SectionId, SentenceParagraphCoverage, SentenceTextUnit, SentenceTextUnitSet,
    TEXT_SEGMENTATION_VERSION, TextLocator, TextUnit,
};

use super::get_text_units::{
    EffectiveTextUnitKind, RequestedTextUnitKind, TextUnitCoveragePolicy, TextUnitDirection,
    TextUnitEnumerationCoverage,
};
use super::open_document::OpenDocumentResult;
use super::reading_profile::ReliabilitySummary;
use super::text_unit_stream::{SectionStreamPlan, StreamUnit, plan_section_stream};

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

#[async_trait]
pub trait OcrEvidenceStore: Send + Sync {
    async fn put_immutable(&self, identity: &str, bytes: &[u8])
    -> Result<String, ApplicationError>;
    async fn get(&self, digest: &str) -> Result<Option<Vec<u8>>, ApplicationError>;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceViewRenderOptions {
    pub dpi: u32,
    pub max_pages: usize,
    pub max_width: u32,
    pub max_height: u32,
    pub max_pixels: u64,
    pub max_image_bytes: usize,
    pub max_decoded_stream_bytes: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RenderedSourceView {
    pub media_type: MediaType,
    pub bytes: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub page_count: usize,
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
    pub ocr_fingerprint: String,
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum ApplicationError {
    #[error("source blocked: {0}")]
    BlockedSource(String),
    #[error("retrieval failed: {0}")]
    RetrievalFailed(String),
    #[error("parse failed: {0}")]
    ParseFailed(String),
    #[error("unsupported text encoding: {0}")]
    UnsupportedTextEncoding(String),
    #[error("local OCR worker failed; no parsed document was published")]
    OcrFailed,
    #[error("local OCR dependencies are unavailable or changed; operator repair is required")]
    OcrUnavailable,
    #[error(
        "source contains image-only pages requiring inspection; enable local OCR or provide a usable text layer"
    )]
    OcrRequired,
    #[error("local OCR found no supported prose projection; inspect the original source")]
    OcrNoSupportedProjection,
    #[error("local OCR is busy; retry explicitly later")]
    OcrBusy,
    #[error("local OCR exceeded its shared time budget; retry explicitly later")]
    OcrTimeout,
    #[error("local OCR input or output exceeds the configured resource limit")]
    OcrResourceLimit,
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
    #[error("stale structure resolution: {0}")]
    StaleStructure(String),
    #[error("stale persisted document: {0}")]
    StaleDocument(String),
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
    #[error("source view failed: {0}")]
    SourceViewFailed(String),
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

    /// Parses `resource` and returns the text segmentation the parser had to build anyway, so a
    /// caller that needs the segmentation never builds the same one twice. The default parses
    /// without a segmentation, and every parser whose parse path has no segmentation returns
    /// `None`; the caller then segments the returned document itself.
    async fn parse_with_segmentation(
        &self,
        resource: RetrievedResource,
    ) -> Result<(Document, Option<Arc<DocumentSegmentation>>), ApplicationError> {
        Ok((self.parse(resource).await?, None))
    }
}

pub trait SourceViewRenderer: Send + Sync {
    fn render(
        &self,
        bytes: Vec<u8>,
        media_type: MediaType,
        page: u32,
        options: SourceViewRenderOptions,
    ) -> Result<RenderedSourceView, ApplicationError>;
}

pub trait DocumentReliabilityInspector: Send + Sync {
    fn inspect(&self, document: &Document) -> Result<ReliabilitySummary, ApplicationError>;

    /// Inspects `document` reusing the segmentation the caller already built from it. The default
    /// ignores it.
    fn inspect_with_segmentation(
        &self,
        document: &Document,
        _segmentation: &DocumentSegmentation,
    ) -> Result<ReliabilitySummary, ApplicationError> {
        self.inspect(document)
    }
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

/// Stores the result of opening a document, keyed by its id and an open-result fingerprint.
#[async_trait]
pub trait OpenResultStore: Send + Sync {
    /// The stored result for `id`, only if it was stored under exactly `fingerprint`.
    /// A row that no longer deserializes is a miss (Ok(None)), not an error.
    async fn get(
        &self,
        id: &DocumentId,
        fingerprint: &str,
    ) -> Result<Option<OpenDocumentResult>, ApplicationError>;

    async fn put(
        &self,
        result: &OpenDocumentResult,
        fingerprint: &str,
    ) -> Result<(), ApplicationError>;

    async fn remove(&self, id: &DocumentId) -> Result<(), ApplicationError>;
}

#[async_trait]
pub trait DocumentRepository: Send + Sync {
    async fn save(&self, document: Document) -> Result<(), ApplicationError>;

    /// Saves a document the caller keeps using, without copying it when the repository can share it.
    async fn save_shared(&self, document: Arc<Document>) -> Result<(), ApplicationError> {
        self.save(Arc::unwrap_or_clone(document)).await
    }

    async fn get(&self, id: &DocumentId) -> Result<Option<Arc<Document>>, ApplicationError>;

    /// The document and its normalized document hash. The default computes the hash (O(document));
    /// stores override it to return a hash saved when the document was stored.
    async fn get_with_hash(
        &self,
        id: &DocumentId,
    ) -> Result<Option<(Arc<Document>, NormalizedDocumentHash)>, ApplicationError> {
        Ok(self.get(id).await?.map(|document| {
            let hash = document.normalized_document_hash();
            (document, hash)
        }))
    }
}

/// The persisted paragraph and sentence units of one section, in source order, with their coverage records.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SectionTextUnits {
    pub paragraphs: Vec<TextUnit>,
    /// `None` when the document has no paragraph coverage record for this section.
    pub paragraph_coverage: Option<ParagraphSectionCoverage>,
    pub sentences: Vec<SentenceTextUnit>,
    pub sentence_coverage: Vec<SentenceParagraphCoverage>,
}

impl SectionTextUnits {
    /// The same section slice that the read paths compute from full unit sets.
    pub fn from_sets(
        paragraphs: &ParagraphTextUnitSet,
        sentences: &SentenceTextUnitSet,
        section_id: &SectionId,
    ) -> Self {
        Self {
            paragraphs: paragraphs
                .units
                .iter()
                .filter(|unit| unit.owner_section_id == *section_id)
                .cloned()
                .collect(),
            paragraph_coverage: paragraphs
                .coverage
                .iter()
                .find(|coverage| coverage.owner_section_id == *section_id)
                .cloned(),
            sentences: sentences
                .units
                .iter()
                .filter(|unit| unit.owner_section_id == *section_id)
                .cloned()
                .collect(),
            sentence_coverage: sentences
                .coverage
                .iter()
                .filter(|coverage| coverage.owner_section_id == *section_id)
                .cloned()
                .collect(),
        }
    }
}

/// An owned unit of a section stream, decoded from a stored stream row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OwnedStreamUnit {
    Paragraph(TextUnit),
    Sentence(SentenceTextUnit),
}

/// One stored stream item: its absolute position in the chosen policy's stream and the referenced unit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredStreamItem {
    pub position: usize,
    pub unit: OwnedStreamUnit,
    pub content_class: ParagraphContentClass,
    pub degradation: Option<String>,
    pub char_count: usize,
}

/// The size and coverage of one section stream under one policy.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextUnitStreamSummary {
    pub total_items: usize,
    pub coverage: TextUnitEnumerationCoverage,
}

/// Which declared stream: one section's stream for one requested kind and coverage policy.
#[derive(Clone, Copy, Debug)]
pub struct TextUnitStreamKey<'a> {
    pub document_id: &'a DocumentId,
    pub section_id: &'a SectionId,
    pub kind: RequestedTextUnitKind,
    pub policy: TextUnitCoveragePolicy,
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

    /// Persists both unit sets of a document. The document supplies the section texts and the
    /// document-level hashes the store keeps once.
    async fn replace_document_sets(
        &self,
        document: &Arc<Document>,
        paragraphs: Arc<ParagraphTextUnitSet>,
        sentences: Arc<SentenceTextUnitSet>,
    ) -> Result<(), ApplicationError>;

    /// Persists a document's segmentation without building whole-document unit sets. The default
    /// materializes the units and calls `replace_document_sets`.
    async fn replace_document_segmentation(
        &self,
        document: &Arc<Document>,
        segmentation: Arc<DocumentSegmentation>,
    ) -> Result<(), ApplicationError> {
        let normalized_document_hash = document.normalized_document_hash();
        let (paragraphs, sentences) = segmentation.materialize(document, &normalized_document_hash);
        self.replace_document_sets(document, Arc::new(paragraphs), Arc::new(sentences))
            .await
    }

    /// Units of one section as persisted by `replace_document_sets`, or `None` when this document's units are not
    /// stored for the current `TEXT_SEGMENTATION_VERSION`.
    async fn section_units(
        &self,
        document_id: &DocumentId,
        section_id: &SectionId,
    ) -> Result<Option<SectionTextUnits>, ApplicationError>;

    /// One stored unit of a section by its 1-based indices: a paragraph (`sentence_index` None) or a sentence.
    /// Err(StaleDocument) when the document's units are not stored; Ok(None) when stored but no such unit.
    async fn section_unit(
        &self,
        document_id: &DocumentId,
        section_id: &SectionId,
        kind: EffectiveTextUnitKind,
        paragraph_index: usize,
        sentence_index: Option<usize>,
    ) -> Result<Option<OwnedStreamUnit>, ApplicationError> {
        let Some(units) = self.section_units(document_id, section_id).await? else {
            return Err(stale_text_units(document_id));
        };
        Ok(match kind {
            EffectiveTextUnitKind::Paragraph => units
                .paragraphs
                .into_iter()
                .find(|unit| unit.paragraph_index == paragraph_index)
                .map(OwnedStreamUnit::Paragraph),
            EffectiveTextUnitKind::Sentence => units
                .sentences
                .into_iter()
                .find(|unit| {
                    unit.paragraph_index == paragraph_index
                        && Some(unit.sentence_index) == sentence_index
                })
                .map(OwnedStreamUnit::Sentence),
        })
    }

    /// `None` when the section's stream is not stored (open the document again).
    async fn section_stream_summary(
        &self,
        key: TextUnitStreamKey<'_>,
    ) -> Result<Option<TextUnitStreamSummary>, ApplicationError> {
        let TextUnitStreamKey {
            document_id,
            section_id,
            kind,
            policy,
        } = key;
        let Some(units) = self.section_units(document_id, section_id).await? else {
            return Ok(None);
        };
        let plan = plan_units(&units, section_id, kind)?;
        Ok(Some(TextUnitStreamSummary {
            total_items: match policy {
                TextUnitCoveragePolicy::PreserveSource => plan.entries.len(),
                TextUnitCoveragePolicy::EligibleOnly => {
                    plan.entries.iter().filter(|entry| entry.eligible).count()
                }
            },
            coverage: match policy {
                TextUnitCoveragePolicy::PreserveSource => plan.preserve_source,
                TextUnitCoveragePolicy::EligibleOnly => plan.eligible_only,
            },
        }))
    }

    /// Up to `limit` stream items. Forward: positions `position..` ascending. Backward: positions `..position`,
    /// returned in ascending order (the last `limit` items before `position`).
    async fn section_stream_items(
        &self,
        key: TextUnitStreamKey<'_>,
        direction: TextUnitDirection,
        position: usize,
        limit: usize,
    ) -> Result<Vec<StoredStreamItem>, ApplicationError> {
        let TextUnitStreamKey {
            document_id,
            section_id,
            kind,
            policy,
        } = key;
        let Some(units) = self.section_units(document_id, section_id).await? else {
            return Err(stale_text_units(document_id));
        };
        let plan = plan_units(&units, section_id, kind)?;
        // Number the chosen policy's stream densely, so PreserveSource and EligibleOnly positions are
        // both 0..total-1.
        let mut stream = Vec::new();
        let mut eligible_position = 0usize;
        for (preserve_position, entry) in plan.entries.iter().enumerate() {
            let stream_position = match policy {
                TextUnitCoveragePolicy::PreserveSource => preserve_position,
                TextUnitCoveragePolicy::EligibleOnly => {
                    if !entry.eligible {
                        continue;
                    }
                    let stream_position = eligible_position;
                    eligible_position += 1;
                    stream_position
                }
            };
            stream.push((stream_position, entry));
        }
        let total = stream.len();
        let (start, end) = match direction {
            TextUnitDirection::Forward => (position.min(total), (position + limit).min(total)),
            TextUnitDirection::Backward => {
                let end = position.min(total);
                (end.saturating_sub(limit), end)
            }
        };
        Ok(stream[start..end]
            .iter()
            .map(|(stream_position, entry)| StoredStreamItem {
                position: *stream_position,
                unit: owned_unit(entry.unit),
                content_class: entry.content_class,
                degradation: entry.degradation.map(str::to_owned),
                char_count: entry.char_count,
            })
            .collect())
    }

    /// The stream position of the unit with this kind and indices, if it is in the stream.
    async fn section_stream_position(
        &self,
        key: TextUnitStreamKey<'_>,
        unit_kind: EffectiveTextUnitKind,
        paragraph_index: usize,
        sentence_index: Option<usize>,
    ) -> Result<Option<usize>, ApplicationError> {
        let TextUnitStreamKey {
            document_id,
            section_id,
            kind,
            policy,
        } = key;
        let Some(units) = self.section_units(document_id, section_id).await? else {
            return Ok(None);
        };
        let plan = plan_units(&units, section_id, kind)?;
        let mut eligible_position = 0usize;
        for (preserve_position, entry) in plan.entries.iter().enumerate() {
            if policy == TextUnitCoveragePolicy::EligibleOnly && !entry.eligible {
                continue;
            }
            let position = match policy {
                TextUnitCoveragePolicy::PreserveSource => preserve_position,
                TextUnitCoveragePolicy::EligibleOnly => {
                    let position = eligible_position;
                    eligible_position += 1;
                    position
                }
            };
            if unit_matches(entry.unit, unit_kind, paragraph_index, sentence_index) {
                return Ok(Some(position));
            }
        }
        Ok(None)
    }
}

/// Plans a section stream from one section's persisted units.
fn plan_units<'a>(
    units: &'a SectionTextUnits,
    section_id: &SectionId,
    kind: RequestedTextUnitKind,
) -> Result<SectionStreamPlan<'a>, ApplicationError> {
    let paragraphs = units.paragraphs.iter().collect::<Vec<_>>();
    let sentences = units.sentences.iter().collect::<Vec<_>>();
    let sentence_coverage = units.sentence_coverage.iter().collect::<Vec<_>>();
    plan_section_stream(
        section_id,
        &paragraphs,
        units.paragraph_coverage.as_ref(),
        &sentences,
        &sentence_coverage,
        kind,
    )
}

fn owned_unit(unit: StreamUnit<'_>) -> OwnedStreamUnit {
    match unit {
        StreamUnit::Paragraph(unit) => OwnedStreamUnit::Paragraph(unit.clone()),
        StreamUnit::Sentence(unit) => OwnedStreamUnit::Sentence(unit.clone()),
    }
}

fn unit_matches(
    unit: StreamUnit<'_>,
    kind: EffectiveTextUnitKind,
    paragraph_index: usize,
    sentence_index: Option<usize>,
) -> bool {
    if unit.paragraph_index() != paragraph_index {
        return false;
    }
    match (kind, unit) {
        (EffectiveTextUnitKind::Paragraph, StreamUnit::Paragraph(_)) => sentence_index.is_none(),
        (EffectiveTextUnitKind::Sentence, StreamUnit::Sentence(unit)) => {
            Some(unit.sentence_index) == sentence_index
        }
        _ => false,
    }
}

fn stale_text_units(document_id: &DocumentId) -> ApplicationError {
    ApplicationError::StaleDocument(format!(
        "text units of document {} are not stored for {TEXT_SEGMENTATION_VERSION}; open the document again",
        document_id.0
    ))
}

#[async_trait]
pub trait SearchIndex: Send + Sync {
    fn supports_precise_lexical_candidates(&self) -> bool {
        self.tokenizer_version() != LEGACY_SEARCH_TOKENIZER_VERSION
    }

    fn tokenizer_version(&self) -> &'static str {
        LEGACY_SEARCH_TOKENIZER_VERSION
    }

    fn index_version(&self) -> &'static str {
        "none"
    }

    async fn index(&self, document: &Document) -> Result<(), ApplicationError>;

    /// Indexes `document` using the segmentation the caller already built from it, so the index does
    /// not segment it again. The default ignores the segmentation.
    async fn index_with_segmentation(
        &self,
        document: Arc<Document>,
        _normalized_document_hash: NormalizedDocumentHash,
        _segmentation: Arc<DocumentSegmentation>,
    ) -> Result<(), ApplicationError> {
        self.index(&document).await
    }

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
