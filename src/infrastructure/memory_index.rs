use std::collections::HashMap;
use std::sync::RwLock;

use async_trait::async_trait;

use crate::application::ports::{
    ApplicationError, LEXICAL_TOKENIZER_VERSION, LexicalSearchHit, SearchHit, SearchHitKind,
    SearchIndex,
};
use crate::domain::{Document, DocumentId};

use super::lexical::{LexicalCandidate, build_lexical_candidates, score_candidate};

#[derive(Default)]
pub struct InMemorySearchIndex {
    documents: RwLock<HashMap<DocumentId, Vec<LexicalCandidate>>>,
}

#[async_trait]
impl SearchIndex for InMemorySearchIndex {
    fn tokenizer_version(&self) -> &'static str {
        LEXICAL_TOKENIZER_VERSION
    }

    async fn index(&self, document: &Document) -> Result<(), ApplicationError> {
        self.documents
            .write()
            .map_err(|_| ApplicationError::IndexFailed("search index lock poisoned".into()))?
            .insert(document.id.clone(), build_lexical_candidates(document));
        Ok(())
    }

    async fn search(
        &self,
        document_id: &DocumentId,
        query: &str,
        limit: usize,
    ) -> Result<Vec<SearchHit>, ApplicationError> {
        Ok(self
            .search_lexical(document_id, query, limit)
            .await?
            .into_iter()
            .map(|hit| SearchHit {
                section_id: hit.section_id,
                title: hit.title,
                source: hit.source,
                snippet: hit.snippet,
                score: hit.score,
                location: hit.location,
            })
            .collect())
    }

    async fn search_lexical(
        &self,
        document_id: &DocumentId,
        query: &str,
        limit: usize,
    ) -> Result<Vec<LexicalSearchHit>, ApplicationError> {
        if query.trim().is_empty() {
            return Err(ApplicationError::InvalidRequest(
                "search query must not be empty".into(),
            ));
        }
        if limit == 0 {
            return Ok(vec![]);
        }

        let documents = self
            .documents
            .read()
            .map_err(|_| ApplicationError::IndexFailed("search index lock poisoned".into()))?;
        let candidates = documents
            .get(document_id)
            .ok_or(ApplicationError::DocumentNotFound)?;

        let mut scored = candidates
            .iter()
            .filter_map(|candidate| {
                score_candidate(candidate, query).map(|score| LexicalSearchHit {
                    section_id: candidate.section_id.clone(),
                    title: candidate.title.clone(),
                    source: candidate.source.clone(),
                    snippet: candidate.snippet.clone(),
                    score,
                    location: candidate.location.clone(),
                    candidate_kind: candidate.candidate_kind,
                    text_locator: candidate.text_locator.clone(),
                    tokenizer_version: LEXICAL_TOKENIZER_VERSION.into(),
                })
            })
            .collect::<Vec<_>>();

        scored.sort_by(|left, right| {
            right
                .score
                .total_cmp(&left.score)
                .then_with(|| {
                    candidate_kind_order(left.candidate_kind)
                        .cmp(&candidate_kind_order(right.candidate_kind))
                })
                .then_with(|| left.section_id.0.cmp(&right.section_id.0))
                .then_with(|| {
                    left.text_locator
                        .normalized_range
                        .map(|range| range.start())
                        .unwrap_or_default()
                        .cmp(
                            &right
                                .text_locator
                                .normalized_range
                                .map(|range| range.start())
                                .unwrap_or_default(),
                        )
                })
        });
        scored.truncate(limit);
        Ok(scored)
    }
}

const fn candidate_kind_order(kind: SearchHitKind) -> u8 {
    match kind {
        SearchHitKind::Sentence => 0,
        SearchHitKind::Paragraph => 1,
        SearchHitKind::Section => 2,
    }
}
