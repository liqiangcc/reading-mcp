//! The v2 `search` tool: v1 lexical search, answered as sentence handles a caller can read.
//!
//! The ranking is exactly the v1 `search_document` ranking. Each hit is converted to the sentence
//! handle of its resolved locator, so `read(from=hit)` opens the matching sentence directly, and
//! the snippet is bounded so a hit costs a handful of tokens.

use std::sync::Arc;

use crate::application::handle_reading::resolve_document;
use crate::application::ports::{DocumentRepository, TextUnitIndex};
use crate::application::reading_error::ReadingError;
use crate::application::search_document::{
    SearchCandidateKind, SearchDocumentCommand, SearchDocumentUseCase,
};
use crate::application::short_handle::{SectionHandle, parse_handle, sentence_handle};

/// The longest snippet a hit carries, in characters.
pub const MAX_SNIPPET_CHARS: usize = 120;
/// The default and largest `limit` a v2 search accepts.
pub const DEFAULT_SEARCH_LIMIT: usize = 10;
pub const MAX_SEARCH_LIMIT: usize = 50;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchReadingCommand {
    /// A document handle.
    pub doc: String,
    pub query: String,
    pub limit: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchReadingHit {
    pub handle: String,
    pub snippet: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchReadingResult {
    pub total: usize,
    /// The sections named by the hits, in first-hit order: (section handle, title).
    pub sections: Vec<(String, String)>,
    pub hits: Vec<SearchReadingHit>,
}

pub struct SearchReadingUseCase {
    repository: Arc<dyn DocumentRepository>,
    text_unit_index: Arc<dyn TextUnitIndex>,
    search_document: Arc<SearchDocumentUseCase>,
}

impl SearchReadingUseCase {
    pub fn new(
        repository: Arc<dyn DocumentRepository>,
        text_unit_index: Arc<dyn TextUnitIndex>,
        search_document: Arc<SearchDocumentUseCase>,
    ) -> Self {
        Self {
            repository,
            text_unit_index,
            search_document,
        }
    }

    pub async fn execute(
        &self,
        command: SearchReadingCommand,
    ) -> Result<SearchReadingResult, ReadingError> {
        if command.limit == 0 || command.limit > MAX_SEARCH_LIMIT {
            return Err(ReadingError::invalid(
                "limit",
                format!("limit must be between 1 and {MAX_SEARCH_LIMIT}"),
            ));
        }
        let handle = parse_handle(&command.doc).map_err(|error| {
            ReadingError::invalid(
                "doc",
                format!("doc must be a document handle: {}", error.message),
            )
        })?;
        let resolved = resolve_document(
            self.text_unit_index.as_ref(),
            self.repository.as_ref(),
            handle.document(),
        )
        .await?;

        let found = self
            .search_document
            .execute(SearchDocumentCommand {
                document_id: resolved.document.id.clone(),
                query: command.query,
                limit: command.limit,
            })
            .await?;

        let mut sections: Vec<(String, String)> = Vec::new();
        let mut hits = Vec::new();
        for hit in found.hits {
            // Only a sentence candidate becomes a sentence handle. A section or paragraph
            // candidate is not a sentence, and the contract promises sentence handles.
            if hit.candidate_kind != SearchCandidateKind::Sentence {
                continue;
            }
            let Some(paragraph_index) = hit.text_locator.paragraph_index else {
                continue;
            };
            let Some(sentence_index) = hit.text_locator.sentence_index else {
                continue;
            };
            let section_key = self
                .text_unit_index
                .section_key(&resolved.document.id, &hit.section_id)
                .await?
                .ok_or_else(|| {
                    ReadingError::stale_handle(format!(
                        "section {} has no stored section key",
                        hit.section_id.0
                    ))
                })?;
            let section = SectionHandle {
                document: resolved.handle.clone(),
                section_key,
            };
            let section_handle = section.to_string();
            if !sections.iter().any(|(known, _)| *known == section_handle) {
                sections.push((section_handle, hit.title));
            }
            hits.push(SearchReadingHit {
                handle: sentence_handle(&section, paragraph_index, sentence_index),
                snippet: truncate_snippet(&hit.snippet),
            });
        }
        Ok(SearchReadingResult {
            total: hits.len(),
            sections,
            hits,
        })
    }
}

/// The first `MAX_SNIPPET_CHARS` characters of `snippet`, with an ellipsis when it was cut.
fn truncate_snippet(snippet: &str) -> String {
    let mut chars = snippet.chars();
    let head = chars.by_ref().take(MAX_SNIPPET_CHARS).collect::<String>();
    if chars.next().is_some() {
        let mut cut = head;
        cut.pop();
        cut.push('…');
        cut
    } else {
        head
    }
}
