use std::sync::Arc;

use crate::application::ports::{ApplicationError, SearchHit, SearchIndex};
use crate::domain::DocumentId;

const MAX_SEARCH_LIMIT: usize = 50;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchDocumentCommand {
    pub document_id: DocumentId,
    pub query: String,
    pub limit: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SearchDocumentResult {
    pub document_id: DocumentId,
    pub hits: Vec<SearchHit>,
}

pub struct SearchDocumentUseCase {
    search_index: Arc<dyn SearchIndex>,
}

impl SearchDocumentUseCase {
    pub fn new(search_index: Arc<dyn SearchIndex>) -> Self {
        Self { search_index }
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

        let hits = self
            .search_index
            .search(&command.document_id, query, command.limit)
            .await?;

        Ok(SearchDocumentResult {
            document_id: command.document_id,
            hits,
        })
    }
}
