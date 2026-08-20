use std::sync::Arc;

use crate::application::ports::{ApplicationError, DocumentRepository};
use crate::domain::{DocumentId, Location, Section, SectionId};

const MAX_STRUCTURE_NODES: usize = 2_000;

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

        let visible_nodes = document
            .root_sections
            .iter()
            .map(|section| count_visible_nodes(section, max_depth, 1))
            .sum::<usize>();
        if visible_nodes > MAX_STRUCTURE_NODES {
            return Err(ApplicationError::ResourceLimitExceeded(format!(
                "document structure would return {visible_nodes} nodes; server limit is {MAX_STRUCTURE_NODES}; request a smaller max_depth or use search_document to locate a section"
            )));
        }

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

fn count_visible_nodes(section: &Section, max_depth: Option<u8>, depth: u8) -> usize {
    let include_children = max_depth.is_none_or(|limit| depth < limit);
    1 + if include_children {
        section
            .children
            .iter()
            .map(|child| count_visible_nodes(child, max_depth, depth.saturating_add(1)))
            .sum::<usize>()
    } else {
        0
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
