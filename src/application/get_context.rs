use std::sync::Arc;

use crate::application::ports::{ApplicationError, DocumentRepository};
use crate::application::reading_support::{
    flatten_sections, render_section_shallow, truncate_chars,
};
use crate::domain::{DocumentId, DocumentSource, Location, SectionId};

const MAX_CONTEXT_WINDOW: usize = 20;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GetContextCommand {
    pub document_id: DocumentId,
    pub section_id: SectionId,
    pub before: usize,
    pub after: usize,
    pub max_chars: Option<usize>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GetContextResult {
    pub document_id: DocumentId,
    pub source: DocumentSource,
    pub owner_section_id: SectionId,
    pub content: String,
    pub location: Location,
    pub truncated: bool,
}

pub struct GetContextUseCase {
    repository: Arc<dyn DocumentRepository>,
}

impl GetContextUseCase {
    pub fn new(repository: Arc<dyn DocumentRepository>) -> Self {
        Self { repository }
    }

    pub async fn execute(
        &self,
        command: GetContextCommand,
    ) -> Result<GetContextResult, ApplicationError> {
        if command.before > MAX_CONTEXT_WINDOW || command.after > MAX_CONTEXT_WINDOW {
            return Err(ApplicationError::InvalidRequest(format!(
                "context window must not exceed {MAX_CONTEXT_WINDOW} sections on either side"
            )));
        }

        let document = self
            .repository
            .get(&command.document_id)
            .await?
            .ok_or(ApplicationError::DocumentNotFound)?;
        let document_id = document.id.clone();
        let source = document.source.clone();

        let mut sections = Vec::new();
        flatten_sections(&document.root_sections, &mut sections);
        let owner_index = sections
            .iter()
            .position(|section| section.id == command.section_id)
            .ok_or(ApplicationError::SectionNotFound)?;
        let owner = sections[owner_index];

        let start = owner_index.saturating_sub(command.before);
        let end = (owner_index + command.after + 1).min(sections.len());
        let rendered = sections[start..end]
            .iter()
            .map(|section| render_section_shallow(section))
            .collect::<Vec<_>>()
            .join("\n\n");
        let (content, truncated) = truncate_chars(rendered, command.max_chars);

        Ok(GetContextResult {
            document_id,
            source,
            owner_section_id: owner.id.clone(),
            content,
            location: owner.location.clone(),
            truncated,
        })
    }
}
