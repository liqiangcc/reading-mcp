use std::sync::Arc;

use crate::application::ports::{
    ApplicationError, DocumentReliabilityInspector, DocumentRepository, Parser, RetrievalOptions,
    Retriever, SearchIndex, SourcePolicy, TextUnitIndex,
};
use crate::application::reading_profile::{
    ReadingProfile, ReliabilitySummary, build_reading_profile,
};
use crate::domain::{
    ContentHash, DocumentId, DocumentSource, MediaType, NORMALIZATION_VERSION,
    NORMALIZED_DOCUMENT_HASH_VERSION, NORMALIZED_TEXT_COORDINATE_SPACE, NormalizedDocumentHash,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenDocumentCommand {
    pub source: DocumentSource,
    pub options: RetrievalOptions,
}

#[derive(Clone, Debug, PartialEq, Eq)]
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
        }
    }

    pub fn with_reliability_inspector(
        mut self,
        reliability_inspector: Arc<dyn DocumentReliabilityInspector>,
    ) -> Self {
        self.reliability_inspector = Some(reliability_inspector);
        self
    }

    pub async fn execute(
        &self,
        command: OpenDocumentCommand,
    ) -> Result<OpenDocumentResult, ApplicationError> {
        self.source_policy.validate(&command.source).await?;

        let resource = self
            .retriever
            .retrieve(&command.source, &command.options)
            .await?;

        let document = self.parser.parse(resource).await?;
        let paragraph_units = document.try_paragraph_text_units().map_err(|error| {
            ApplicationError::TextUnitIndexFailed(format!(
                "cannot build current Paragraph coverage from persisted block evidence: {error}"
            ))
        })?;
        let sentence_units = document.try_sentence_text_units().map_err(|error| {
            ApplicationError::TextUnitIndexFailed(format!(
                "cannot build current Sentence coverage from persisted block evidence: {error}"
            ))
        })?;
        let reliability = match &self.reliability_inspector {
            Some(inspector) => inspector.inspect(&document)?,
            None => ReliabilitySummary::not_applicable(),
        };
        let reading_profile = build_reading_profile(
            &document,
            &paragraph_units,
            &sentence_units,
            reliability,
            self.search_index.supports_precise_lexical_candidates(),
        )?;
        let result = OpenDocumentResult {
            document_id: document.id.clone(),
            source: document.source.clone(),
            title: document.title.clone(),
            media_type: document.media_type.clone(),
            content_hash: document.content_hash.clone(),
            normalized_document_hash: document.normalized_document_hash(),
            normalized_document_hash_version: NORMALIZED_DOCUMENT_HASH_VERSION.into(),
            normalization_version: NORMALIZATION_VERSION.into(),
            normalized_text_coordinate_space: NORMALIZED_TEXT_COORDINATE_SPACE.into(),
            section_count: document.section_count(),
            reading_profile,
        };

        self.repository.save(document.clone()).await?;
        if let Some(index) = &self.text_unit_index {
            index
                .replace_document(&document.id, &paragraph_units.units)
                .await?;
        }
        self.search_index.index(&document).await?;

        Ok(result)
    }
}
