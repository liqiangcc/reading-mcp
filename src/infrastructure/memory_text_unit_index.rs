use std::collections::HashMap;
use std::sync::RwLock;

use async_trait::async_trait;

use crate::application::ports::{ApplicationError, TextUnitIndex};
use crate::domain::{DocumentId, TextUnit};

#[derive(Default)]
pub struct InMemoryTextUnitIndex {
    documents: RwLock<HashMap<DocumentId, Vec<TextUnit>>>,
}

#[async_trait]
impl TextUnitIndex for InMemoryTextUnitIndex {
    async fn replace_document(
        &self,
        document_id: &DocumentId,
        units: &[TextUnit],
    ) -> Result<(), ApplicationError> {
        self.documents
            .write()
            .map_err(|_| {
                ApplicationError::TextUnitIndexFailed("text unit index lock poisoned".into())
            })?
            .insert(document_id.clone(), units.to_vec());
        Ok(())
    }

    async fn list_document(
        &self,
        document_id: &DocumentId,
    ) -> Result<Vec<TextUnit>, ApplicationError> {
        Ok(self
            .documents
            .read()
            .map_err(|_| {
                ApplicationError::TextUnitIndexFailed("text unit index lock poisoned".into())
            })?
            .get(document_id)
            .cloned()
            .unwrap_or_default())
    }
}
