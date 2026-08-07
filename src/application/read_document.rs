use std::sync::Arc;

use crate::application::ports::{ApplicationError, DocumentRepository};
use crate::application::reading_support::{render_section_tree, truncate_chars};
use crate::domain::{DocumentId, DocumentSource, Location, SectionId};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReadSectionCommand {
    pub document_id: DocumentId,
    pub section_id: SectionId,
    pub max_chars: Option<usize>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReadSectionResult {
    pub document_id: DocumentId,
    pub source: DocumentSource,
    pub section_id: SectionId,
    pub content: String,
    pub location: Location,
    pub truncated: bool,
}

pub struct ReadDocumentUseCase {
    repository: Arc<dyn DocumentRepository>,
}

impl ReadDocumentUseCase {
    pub fn new(repository: Arc<dyn DocumentRepository>) -> Self {
        Self { repository }
    }

    pub async fn execute(
        &self,
        command: ReadSectionCommand,
    ) -> Result<ReadSectionResult, ApplicationError> {
        let document = self
            .repository
            .get(&command.document_id)
            .await?
            .ok_or(ApplicationError::DocumentNotFound)?;
        let document_id = document.id.clone();
        let source = document.source.clone();
        let section = document
            .find_section(&command.section_id)
            .ok_or(ApplicationError::SectionNotFound)?;

        let rendered = render_section_tree(section);
        let (content, truncated) = truncate_chars(rendered, command.max_chars);

        Ok(ReadSectionResult {
            document_id,
            source,
            section_id: section.id.clone(),
            content,
            location: section.location.clone(),
            truncated,
        })
    }
}
