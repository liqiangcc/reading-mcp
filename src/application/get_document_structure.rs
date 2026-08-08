use std::sync::Arc;

use crate::application::ports::{ApplicationError, DocumentRepository};
use crate::domain::{DocumentId, Location, Section, SectionId};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SectionOutline {
    pub section_id: SectionId,
    pub parent_id: Option<SectionId>,
    pub title: String,
    pub level: u8,
    pub location: Location,
    pub children: Vec<SectionOutline>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DocumentStructureResult {
    pub document_id: DocumentId,
    pub title: String,
    pub sections: Vec<SectionOutline>,
}

pub struct GetDocumentStructureUseCase {
    repository: Arc<dyn DocumentRepository>,
}

impl GetDocumentStructureUseCase {
    pub fn new(repository: Arc<dyn DocumentRepository>) -> Self {
        Self { repository }
    }

    pub async fn execute(
        &self,
        document_id: DocumentId,
        max_depth: Option<u8>,
    ) -> Result<DocumentStructureResult, ApplicationError> {
        let document = self
            .repository
            .get(&document_id)
            .await?
            .ok_or(ApplicationError::DocumentNotFound)?;

        Ok(DocumentStructureResult {
            document_id: document.id.clone(),
            title: document.title,
            sections: document
                .root_sections
                .iter()
                .map(|section| outline(section, max_depth, 1))
                .collect(),
        })
    }
}

fn outline(section: &Section, max_depth: Option<u8>, depth: u8) -> SectionOutline {
    let include_children = max_depth.is_none_or(|limit| depth < limit);

    SectionOutline {
        section_id: section.id.clone(),
        parent_id: section.parent_id.clone(),
        title: section.title.clone(),
        level: section.level,
        location: section.location.clone(),
        children: if include_children {
            section
                .children
                .iter()
                .map(|child| outline(child, max_depth, depth.saturating_add(1)))
                .collect()
        } else {
            vec![]
        },
    }
}
