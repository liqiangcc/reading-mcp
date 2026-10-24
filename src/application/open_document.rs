use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::time::{Instant, timeout_at};

use crate::application::ports::{
    ApplicationError, DocumentReliabilityInspector, DocumentRepository, OpenResultStore, Parser,
    RetrievalOptions, RetrievedResource, Retriever, SearchIndex, SourcePolicy, TextUnitIndex,
};
use crate::application::reading_profile::{
    READING_PROFILE_SCHEMA_VERSION, ReadingProfile, ReliabilitySummary, build_reading_profile,
};
use crate::application::text_unit_stream::{TEXT_UNIT_STORE_VERSION, TEXT_UNIT_STREAM_VERSION};
use crate::domain::{
    ContentHash, DocumentId, DocumentSource, MediaType, NORMALIZATION_VERSION,
    NORMALIZED_DOCUMENT_HASH_VERSION, NORMALIZED_TEXT_COORDINATE_SPACE, NormalizedDocumentHash,
    TEXT_SEGMENTATION_VERSION,
};

/// Version of the stored open result. Bump it when the result shape or its meaning changes.
const OPEN_RESULT_VERSION: &str = "open-result/v1";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenDocumentCommand {
    pub source: DocumentSource,
    pub options: RetrievalOptions,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenDocumentResult {
    pub document_id: DocumentId,
    pub source: DocumentSource,
    pub title: String,
    pub media_type: MediaType,
    pub content_hash: ContentHash,
    pub normalized_document_hash: NormalizedDocumentHash,
    pub normalized_document_hash_version: String,
    pub normalization_version: String,
    pub normalized_text_coordinate_space: String,
    pub section_count: usize,
    pub reading_profile: ReadingProfile,
}

pub struct OpenDocumentUseCase {
    source_policy: Arc<dyn SourcePolicy>,
    retriever: Arc<dyn Retriever>,
    parser: Arc<dyn Parser>,
    repository: Arc<dyn DocumentRepository>,
    text_unit_index: Option<Arc<dyn TextUnitIndex>>,
    search_index: Arc<dyn SearchIndex>,
    reliability_inspector: Option<Arc<dyn DocumentReliabilityInspector>>,
    ocr_enabled: bool,
    /// The store of open results and the ingest fingerprint (OCR and layout backend identity) they are keyed by.
    open_results: Option<(Arc<dyn OpenResultStore>, String)>,
}

impl OpenDocumentUseCase {
    pub fn new(
        source_policy: Arc<dyn SourcePolicy>,
        retriever: Arc<dyn Retriever>,
        parser: Arc<dyn Parser>,
        repository: Arc<dyn DocumentRepository>,
        search_index: Arc<dyn SearchIndex>,
    ) -> Self {
        Self {
            source_policy,
            retriever,
            parser,
            repository,
            text_unit_index: None,
            search_index,
            reliability_inspector: None,
            ocr_enabled: false,
            open_results: None,
        }
    }

    pub fn with_text_unit_index(
        source_policy: Arc<dyn SourcePolicy>,
        retriever: Arc<dyn Retriever>,
        parser: Arc<dyn Parser>,
        repository: Arc<dyn DocumentRepository>,
        text_unit_index: Arc<dyn TextUnitIndex>,
        search_index: Arc<dyn SearchIndex>,
    ) -> Self {
        Self {
            source_policy,
            retriever,
            parser,
            repository,
            text_unit_index: Some(text_unit_index),
            search_index,
            reliability_inspector: None,
            ocr_enabled: false,
            open_results: None,
        }
    }

    pub fn with_reliability_inspector(
        mut self,
        reliability_inspector: Arc<dyn DocumentReliabilityInspector>,
    ) -> Self {
        self.reliability_inspector = Some(reliability_inspector);
        self
    }

    /// Returns the stored result when the same bytes were opened under the same fingerprint, and stores each new
    /// result. `ingest_fingerprint` must change whenever the parser or its configuration changes.
    /// `force_refresh` skips the lookup, re-ingests and replaces the stored result.
    pub fn with_open_result_store(
        mut self,
        store: Arc<dyn OpenResultStore>,
        ingest_fingerprint: String,
    ) -> Self {
        self.open_results = Some((store, ingest_fingerprint));
        self
    }

    /// The single source of the open-result key fingerprint, so the hit and miss paths cannot drift.
    fn open_result_fingerprint(&self, ingest_fingerprint: &str) -> String {
        format!(
            "{OPEN_RESULT_VERSION}|{}|{NORMALIZATION_VERSION}|{NORMALIZED_DOCUMENT_HASH_VERSION}|{TEXT_SEGMENTATION_VERSION}|{READING_PROFILE_SCHEMA_VERSION}|{}|index={}|precise={}|reliability={}|{ingest_fingerprint}|{TEXT_UNIT_STREAM_VERSION}|{TEXT_UNIT_STORE_VERSION}",
            env!("CARGO_PKG_VERSION"),
            self.search_index.tokenizer_version(),
            self.search_index.index_version(),
            self.search_index.supports_precise_lexical_candidates(),
            self.reliability_inspector.is_some(),
        )
    }

    pub async fn execute(
        &self,
        command: OpenDocumentCommand,
    ) -> Result<OpenDocumentResult, ApplicationError> {
        if self.ocr_enabled {
            let known_ocr_pdf = std::sync::atomic::AtomicBool::new(false);
            let deadline = Instant::now() + Duration::from_secs(90);
            timeout_at(
                deadline,
                self.execute_inner(command, Some(deadline), Some(&known_ocr_pdf)),
            )
            .await
            .map_err(|_| {
                if known_ocr_pdf.load(std::sync::atomic::Ordering::Relaxed) {
                    ApplicationError::OcrTimeout
                } else {
                    ApplicationError::ResourceLimitExceeded(
                        "OCR whole-open exceeded 90 second deadline".into(),
                    )
                }
            })?
        } else {
            self.execute_inner(command, None, None).await
        }
    }

    pub fn with_ocr_budget(mut self, enabled: bool) -> Self {
        self.ocr_enabled = enabled;
        self
    }

    async fn execute_inner(
        &self,
        command: OpenDocumentCommand,
        whole_deadline: Option<Instant>,
        known_ocr_pdf: Option<&std::sync::atomic::AtomicBool>,
    ) -> Result<OpenDocumentResult, ApplicationError> {
        self.source_policy.validate(&command.source).await?;

        let resource = self
            .retriever
            .retrieve(&command.source, &command.options)
            .await?;

        if !command.options.force_refresh
            && let Some((store, ingest_fingerprint)) = &self.open_results
        {
            let content_hash = ContentHash::of_bytes(&resource.bytes);
            let id = DocumentId::derive(&resource.final_source, &content_hash);
            let fingerprint = self.open_result_fingerprint(ingest_fingerprint);
            if let Some(result) = store.get(&id, &fingerprint).await? {
                // A hit after a process restart is exactly the case where the in-memory scan text
                // cache is empty, so the bounded warm runs here too. It runs after the result exists,
                // never delays it, and stops at its own budget.
                self.search_index
                    .prewarm(&result.document_id, &result.normalized_document_hash);
                return Ok(result);
            }
        }

        let is_pdf = resource.media_type.is_pdf();
        if self.ocr_enabled && is_pdf {
            if let Some(known) = known_ocr_pdf {
                known.store(true, std::sync::atomic::Ordering::Relaxed);
            }
            let deadline = Instant::now() + Duration::from_secs(60);
            let deadline = whole_deadline.map_or(deadline, |whole| whole.min(deadline));
            timeout_at(deadline, self.ingest(resource, Some(deadline)))
                .await
                .map_err(|_| ApplicationError::OcrTimeout)?
        } else {
            self.ingest(resource, None).await
        }
    }

    async fn ingest(
        &self,
        resource: RetrievedResource,
        deadline: Option<Instant>,
    ) -> Result<OpenDocumentResult, ApplicationError> {
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            return Err(ApplicationError::OcrTimeout);
        }

        let (document, parsed_segmentation) = self.parser.parse_with_segmentation(resource).await?;
        document
            .validate_ocr_publication()
            .map_err(ApplicationError::ParseFailed)?;
        // A parser that had to segment the document already (EPUB validation does) hands that
        // segmentation back, so the open never builds the same one twice.
        let segmentation = match parsed_segmentation {
            Some(segmentation) => segmentation,
            None => Arc::new(document.try_segmentation().map_err(|error| {
                ApplicationError::TextUnitIndexFailed(format!(
                    "cannot build current Paragraph coverage from persisted block evidence: {error}"
                ))
            })?),
        };
        let normalized_document_hash = document.normalized_document_hash();
        let reliability = match &self.reliability_inspector {
            Some(inspector) => inspector.inspect_with_segmentation(&document, &segmentation)?,
            None => ReliabilitySummary::not_applicable(),
        };
        let reading_profile = build_reading_profile(
            &document,
            &segmentation,
            reliability,
            self.search_index.supports_precise_lexical_candidates(),
        )?;
        let result = OpenDocumentResult {
            document_id: document.id.clone(),
            source: document.source.clone(),
            title: document.title.clone(),
            media_type: document.media_type.clone(),
            content_hash: document.content_hash.clone(),
            normalized_document_hash: normalized_document_hash.clone(),
            normalized_document_hash_version: NORMALIZED_DOCUMENT_HASH_VERSION.into(),
            normalization_version: NORMALIZATION_VERSION.into(),
            normalized_text_coordinate_space: NORMALIZED_TEXT_COORDINATE_SPACE.into(),
            section_count: document.section_count(),
            reading_profile,
        };

        // CPU-only projection/profile work may not yield to the timeout. Never
        // start publication after its deadline even in that case.
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            return Err(ApplicationError::OcrTimeout);
        }
        let document = Arc::new(document);
        // Drop any stored result before re-indexing, so a crash part way through can never leave a row that
        // claims the document is complete.
        if let Some((store, _)) = &self.open_results {
            store.remove(&document.id).await?;
        }
        // The segmentation goes first: `save_shared` leaves the section text out of
        // `document_json` only when `section_text_v3` already holds the text of this very document
        // version, and reads it back from there on load.
        if let Some(index) = &self.text_unit_index {
            index
                .replace_document_segmentation(&document, Arc::clone(&segmentation))
                .await?;
        }
        self.repository.save_shared(Arc::clone(&document)).await?;
        self.search_index
            .index_with_segmentation(
                Arc::clone(&document),
                normalized_document_hash,
                segmentation,
            )
            .await?;

        if let Some((store, ingest_fingerprint)) = &self.open_results {
            let fingerprint = self.open_result_fingerprint(ingest_fingerprint);
            store.put(&result, &fingerprint).await?;
        }
        // The result is complete, so the bounded warm may start: it runs in the background, never
        // touches this response, yields to a foreground request and stops at its time budget.
        self.search_index
            .prewarm(&result.document_id, &result.normalized_document_hash);
        Ok(result)
    }
}
