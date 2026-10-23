use std::sync::Arc;

use crate::application::derived_text_units::DerivedTextUnits;
use crate::application::light_sections::SectionTexts;
use crate::application::locator_resolution::{ResolvedLocatorKind, resolve_text_locator_at};
use crate::application::ports::{
    ApplicationError, DocumentRepository, SearchHitKind, SearchIndex, TextUnitIndex,
};
use crate::application::section_view::{DocumentView, carrier_section};
use crate::domain::{
    Document, DocumentId, DocumentSource, Location, NormalizedDocumentHash, SectionId, TextLocator,
};

const MAX_SEARCH_LIMIT: usize = 50;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchDocumentCommand {
    pub document_id: DocumentId,
    pub query: String,
    pub limit: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SearchCandidateKind {
    Section,
    Paragraph,
    Sentence,
}

#[derive(Clone, Debug, PartialEq)]
pub struct LocatedSearchHit {
    pub section_id: SectionId,
    pub title: String,
    pub source: DocumentSource,
    pub snippet: String,
    pub score: f32,
    pub location: Location,
    pub candidate_kind: SearchCandidateKind,
    pub text_locator: TextLocator,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SearchDocumentResult {
    pub document_id: DocumentId,
    pub tokenizer_version: String,
    pub hits: Vec<LocatedSearchHit>,
}

pub struct SearchDocumentUseCase {
    search_index: Arc<dyn SearchIndex>,
    repository: Arc<dyn DocumentRepository>,
    text_unit_index: Option<Arc<dyn TextUnitIndex>>,
}

impl SearchDocumentUseCase {
    pub fn new(
        search_index: Arc<dyn SearchIndex>,
        repository: Arc<dyn DocumentRepository>,
    ) -> Self {
        Self {
            search_index,
            repository,
            text_unit_index: None,
        }
    }

    pub fn with_text_unit_index(mut self, index: Arc<dyn TextUnitIndex>) -> Self {
        self.text_unit_index = Some(index);
        self
    }

    pub async fn execute(
        &self,
        command: SearchDocumentCommand,
    ) -> Result<SearchDocumentResult, ApplicationError> {
        let query = command.query.trim();
        if query.is_empty() {
            return Err(ApplicationError::InvalidRequest(
                "search query must not be empty".into(),
            ));
        }
        if command.limit == 0 || command.limit > MAX_SEARCH_LIMIT {
            return Err(ApplicationError::InvalidRequest(format!(
                "search limit must be between 1 and {MAX_SEARCH_LIMIT}"
            )));
        }

        let loaded = match self.text_unit_index {
            Some(_) => {
                self.repository
                    .get_light_with_hash(&command.document_id)
                    .await?
            }
            None => self.repository.get_with_hash(&command.document_id).await?,
        };
        let (document, normalized_document_hash) =
            loaded.ok_or(ApplicationError::DocumentNotFound)?;
        // The scan walks the tree (a search is a scan), but every section it reports is resolved
        // from one row instead of a walk of that tree.
        let view = match self.text_unit_index {
            Some(_) => {
                DocumentView::load_with_hash(Arc::clone(&self.repository), &command.document_id)
                    .await?
                    .map(|(view, _)| view)
                    .ok_or(ApplicationError::DocumentNotFound)?
            }
            None => DocumentView::from_document(
                Arc::clone(&self.repository),
                Arc::clone(&document),
                &normalized_document_hash,
            ),
        };
        let tokenizer_version = self.search_index.tokenizer_version();
        let units: Arc<dyn TextUnitIndex> = match &self.text_unit_index {
            Some(index) => Arc::clone(index),
            None => Arc::new(DerivedTextUnits::new(Arc::clone(&document))),
        };

        let hits = if self.search_index.supports_precise_lexical_candidates() {
            self.precise_hits(
                &document,
                &view,
                query,
                command.limit,
                &normalized_document_hash,
                units.as_ref(),
            )
            .await?
        } else {
            self.legacy_hits(
                &document,
                &view,
                query,
                command.limit,
                &normalized_document_hash,
            )
            .await?
        };

        Ok(SearchDocumentResult {
            document_id: command.document_id,
            tokenizer_version: tokenizer_version.into(),
            hits,
        })
    }

    async fn precise_hits(
        &self,
        document: &Document,
        view: &DocumentView,
        query: &str,
        limit: usize,
        normalized_document_hash: &NormalizedDocumentHash,
        units: &dyn TextUnitIndex,
    ) -> Result<Vec<LocatedSearchHit>, ApplicationError> {
        let expected_tokenizer_version = self.search_index.tokenizer_version();
        // The scan runs on the document this call already holds: one load per `search_document`.
        let index_hits = match self
            .search_index
            .search_lexical_in(document, normalized_document_hash, query, limit)
            .await
        {
            Ok(hits) => hits,
            Err(ApplicationError::DocumentNotFound) => {
                self.search_index.index(document).await?;
                self.search_index
                    .search_lexical_in(document, normalized_document_hash, query, limit)
                    .await?
            }
            Err(error) => return Err(error),
        };

        let mut texts = match self.text_unit_index {
            Some(_) => SectionTexts::from_repository(document, Arc::clone(&self.repository)),
            None => SectionTexts::from_document(document),
        };
        let mut hits = Vec::with_capacity(index_hits.len());
        for hit in index_hits {
            if hit.source != document.source {
                return Err(ApplicationError::IndexFailed(format!(
                    "search hit source {} does not match canonical document source {}",
                    hit.source.0, document.source.0
                )));
            }
            if hit.tokenizer_version != expected_tokenizer_version {
                return Err(ApplicationError::IndexFailed(format!(
                    "search hit tokenizer version {} does not match runtime {}",
                    hit.tokenizer_version, expected_tokenizer_version
                )));
            }

            let resolved = resolve_text_locator_at(
                document,
                view,
                &hit.text_locator,
                normalized_document_hash,
                units,
                &mut texts,
            )
            .await
            .map_err(|error| {
                ApplicationError::IndexFailed(format!(
                    "search hit carries an invalid canonical locator: {error}"
                ))
            })?;
            if hit.section_id != resolved.locator.owner_section_id {
                return Err(ApplicationError::IndexFailed(format!(
                    "search hit section {} does not match locator owner {}",
                    hit.section_id.0, resolved.locator.owner_section_id.0
                )));
            }

            hits.push(LocatedSearchHit {
                section_id: hit.section_id,
                title: hit.title,
                source: hit.source,
                snippet: hit.snippet,
                score: hit.score,
                location: hit.location,
                candidate_kind: candidate_kind(hit.candidate_kind, resolved.kind)?,
                text_locator: resolved.locator,
            });
        }
        Ok(hits)
    }

    async fn legacy_hits(
        &self,
        document: &Document,
        view: &DocumentView,
        query: &str,
        limit: usize,
        normalized_document_hash: &NormalizedDocumentHash,
    ) -> Result<Vec<LocatedSearchHit>, ApplicationError> {
        let index_hits = self.search_index.search(&document.id, query, limit).await?;
        let mut hits = Vec::with_capacity(index_hits.len());
        for hit in index_hits {
            if hit.source != document.source {
                return Err(ApplicationError::IndexFailed(format!(
                    "legacy search hit source {} does not match canonical document source {}",
                    hit.source.0, document.source.0
                )));
            }
            let row = view.section_by_id(&hit.section_id).await?.ok_or_else(|| {
                ApplicationError::IndexFailed(format!(
                    "legacy search hit references missing canonical section {}",
                    hit.section_id.0
                ))
            })?;
            hits.push(LocatedSearchHit {
                section_id: hit.section_id,
                title: hit.title,
                source: hit.source,
                snippet: hit.snippet,
                score: hit.score,
                location: hit.location,
                candidate_kind: SearchCandidateKind::Section,
                text_locator: TextLocator::for_section_with_hash(
                    document,
                    &carrier_section(&row),
                    normalized_document_hash,
                ),
            });
        }
        Ok(hits)
    }
}

fn candidate_kind(
    indexed: SearchHitKind,
    resolved: ResolvedLocatorKind,
) -> Result<SearchCandidateKind, ApplicationError> {
    match (indexed, resolved) {
        (SearchHitKind::Section, ResolvedLocatorKind::Section) => Ok(SearchCandidateKind::Section),
        (SearchHitKind::Paragraph, ResolvedLocatorKind::Paragraph) => {
            Ok(SearchCandidateKind::Paragraph)
        }
        (SearchHitKind::Sentence, ResolvedLocatorKind::Sentence) => {
            Ok(SearchCandidateKind::Sentence)
        }
        (indexed, resolved) => Err(ApplicationError::IndexFailed(format!(
            "search candidate kind {} does not match resolved locator kind {}",
            indexed.as_str(),
            resolved.as_str()
        ))),
    }
}
