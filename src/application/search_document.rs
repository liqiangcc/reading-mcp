use std::sync::Arc;

use crate::application::locator_resolution::{ResolvedLocatorKind, resolve_text_locator};
use crate::application::ports::{ApplicationError, DocumentRepository, SearchHitKind, SearchIndex};
use crate::domain::{Document, DocumentId, DocumentSource, Location, SectionId, TextLocator};

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

        let document = self
            .repository
            .get(&command.document_id)
            .await?
            .ok_or(ApplicationError::DocumentNotFound)?;
        let tokenizer_version = self.search_index.tokenizer_version();

        let hits = if self.search_index.supports_precise_lexical_candidates() {
            self.precise_hits(&document, query, command.limit).await?
        } else {
            self.legacy_hits(&document, query, command.limit).await?
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
        query: &str,
        limit: usize,
    ) -> Result<Vec<LocatedSearchHit>, ApplicationError> {
        let expected_tokenizer_version = self.search_index.tokenizer_version();
        let index_hits = match self
            .search_index
            .search_lexical(&document.id, query, limit)
            .await
        {
            Ok(hits) => hits,
            Err(ApplicationError::DocumentNotFound) => {
                self.search_index.index(document).await?;
                self.search_index
                    .search_lexical(&document.id, query, limit)
                    .await?
            }
            Err(error) => return Err(error),
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

            let resolved = resolve_text_locator(document, &hit.text_locator).map_err(|error| {
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
        query: &str,
        limit: usize,
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
            let section = document.find_section(&hit.section_id).ok_or_else(|| {
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
                text_locator: TextLocator::for_section(document, section),
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
