use async_trait::async_trait;

use crate::application::ports::{ApplicationError, LexicalSearchHit, SearchHit, SearchIndex};
use crate::domain::{Document, DocumentId};

#[derive(Default)]
pub struct NoopSearchIndex;

#[async_trait]
impl SearchIndex for NoopSearchIndex {
    async fn index(&self, _document: &Document) -> Result<(), ApplicationError> {
        Ok(())
    }

    async fn search(
        &self,
        _document_id: &DocumentId,
        _query: &str,
        _limit: usize,
    ) -> Result<Vec<SearchHit>, ApplicationError> {
        Ok(vec![])
    }

    async fn search_lexical(
        &self,
        _document_id: &DocumentId,
        _query: &str,
        _limit: usize,
    ) -> Result<Vec<LexicalSearchHit>, ApplicationError> {
        Ok(vec![])
    }
}
