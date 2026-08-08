use std::collections::HashMap;
use std::sync::RwLock;

use async_trait::async_trait;

use crate::application::ports::{ApplicationError, DocumentRepository};
use crate::domain::{Document, DocumentId};

#[derive(Default)]
pub struct InMemoryDocumentRepository {
    documents: RwLock<HashMap<DocumentId, Document>>,
}

#[async_trait]
impl DocumentRepository for InMemoryDocumentRepository {
    async fn save(&self, document: Document) -> Result<(), ApplicationError> {
        self.documents
            .write()
            .map_err(|_| ApplicationError::RepositoryFailed("repository lock poisoned".into()))?
            .insert(document.id.clone(), document);
        Ok(())
    }

    async fn get(&self, id: &DocumentId) -> Result<Option<Document>, ApplicationError> {
        Ok(self
            .documents
            .read()
            .map_err(|_| ApplicationError::RepositoryFailed("repository lock poisoned".into()))?
            .get(id)
            .cloned())
    }
}
