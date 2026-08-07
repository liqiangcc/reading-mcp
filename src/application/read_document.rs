use std::sync::Arc;

use crate::application::ports::{ApplicationError, DocumentRepository};
use crate::domain::{DocumentId, Location, Section, SectionId};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReadSectionCommand {
    pub document_id: DocumentId,
    pub section_id: SectionId,
    pub max_chars: Option<usize>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReadSectionResult {
    pub document_id: DocumentId,
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
        let section = document
            .find_section(&command.section_id)
            .ok_or(ApplicationError::SectionNotFound)?;

        let rendered = render_section(section);
        let (content, truncated) = truncate_chars(rendered, command.max_chars);

        Ok(ReadSectionResult {
            document_id: document.id,
            section_id: section.id.clone(),
            content,
            location: section.location.clone(),
            truncated,
        })
    }
}

fn render_section(section: &Section) -> String {
    let mut output = String::new();
    render_into(section, &mut output);
    output.trim().to_string()
}

fn render_into(section: &Section, output: &mut String) {
    let heading_level = usize::from(section.level.clamp(1, 6));
    output.push_str(&"#".repeat(heading_level));
    output.push(' ');
    output.push_str(&section.title);
    output.push('\n');

    if !section.content.trim().is_empty() {
        output.push('\n');
        output.push_str(section.content.trim());
        output.push('\n');
    }

    for child in &section.children {
        output.push('\n');
        render_into(child, output);
    }
}

fn truncate_chars(content: String, max_chars: Option<usize>) -> (String, bool) {
    let Some(limit) = max_chars else {
        return (content, false);
    };

    if content.chars().count() <= limit {
        return (content, false);
    }

    (content.chars().take(limit).collect(), true)
}
