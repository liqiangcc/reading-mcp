use std::sync::Arc;

use crate::application::ports::{
    ApplicationError, DocumentRepository, Parser, RetrievalOptions, Retriever, SearchIndex,
    SourcePolicy,
};
use crate::domain::{ContentHash, DocumentId, DocumentSource, MediaType};

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
    pub section_count: usize,
}

pub struct OpenDocumentUseCase {
    source_policy: Arc<dyn SourcePolicy>,
    retriever: Arc<dyn Retriever>,
    parser: Arc<dyn Parser>,
    repository: Arc<dyn DocumentRepository>,
    search_index: Arc<dyn SearchIndex>,
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
            search_index,
        }
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
        let result = OpenDocumentResult {
            document_id: document.id.clone(),
            source: document.source.clone(),
            title: document.title.clone(),
            media_type: document.media_type.clone(),
            content_hash: document.content_hash.clone(),
            section_count: document.section_count(),
        };

        self.repository.save(document.clone()).await?;
        self.search_index.index(&document).await?;

        Ok(result)
    }
}
