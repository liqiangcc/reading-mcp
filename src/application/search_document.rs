use std::sync::Arc;

use crate::application::ports::{ApplicationError, DocumentRepository, SearchIndex};
use crate::domain::{DocumentId, DocumentSource, Location, SectionId, TextLocator};

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
    pub hits: Vec<LocatedSearchHit>,
}

pub struct SearchDocumentUseCase {
    search_index: Arc<dyn SearchIndex>,
    repository: Arc<dyn DocumentRepository>,
}

impl SearchDocumentUseCase {
    pub fn new(
        search_index: Arc<dyn SearchIndex>,
        repository: Arc<dyn DocumentRepository>,
    ) -> Self {
        Self {
            search_index,
            repository,
        }
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

        let index_hits = self
            .search_index
            .search(&command.document_id, query, command.limit)
            .await?;
        let document = self
            .repository
            .get(&command.document_id)
            .await?
            .ok_or(ApplicationError::DocumentNotFound)?;

        let mut hits = Vec::with_capacity(index_hits.len());
        for hit in index_hits {
            if hit.source != document.source {
                return Err(ApplicationError::IndexFailed(format!(
                    "search hit source {} does not match canonical document source {}",
                    hit.source.0, document.source.0
                )));
            }
            let section = document.find_section(&hit.section_id).ok_or_else(|| {
                ApplicationError::IndexFailed(format!(
                    "search hit references missing canonical section {}",
                    hit.section_id.0
                ))
            })?;

            // Current InMemory/SQLite SearchIndex rows are paragraph-like retrieval
            // units, but their splitting/location facts are not the canonical
            // Paragraph TextUnit contract. The strongest truthful handoff today is
            // therefore the owning Section locator. Paragraph/Sentence precision is
            // reserved for the later lexical TextUnit index migration.
            hits.push(LocatedSearchHit {
                section_id: hit.section_id,
                title: hit.title,
                source: hit.source,
                snippet: hit.snippet,
                score: hit.score,
                location: hit.location,
                candidate_kind: SearchCandidateKind::Section,
                text_locator: TextLocator::for_section(&document, section),
            });
        }

        Ok(SearchDocumentResult {
            document_id: command.document_id,
            hits,
        })
    }
}
