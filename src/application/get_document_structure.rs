use std::sync::Arc;

use crate::application::ports::{ApplicationError, DocumentRepository};
use crate::domain::{DocumentId, Location, Section, SectionId};

const MAX_STRUCTURE_RESPONSE_NODES: usize = 1_000;

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
    pub truncated: bool,
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

        let mut budget = OutlineBudget::new(MAX_STRUCTURE_RESPONSE_NODES);
        let sections = outline_sections(&document.root_sections, max_depth, 1, &mut budget);

        Ok(DocumentStructureResult {
            document_id: document.id.clone(),
            title: document.title,
            sections,
            truncated: budget.truncated,
        })
    }
}

struct OutlineBudget {
    remaining: usize,
    truncated: bool,
}

impl OutlineBudget {
    fn new(limit: usize) -> Self {
        Self {
            remaining: limit,
            truncated: false,
        }
    }

    fn take(&mut self) -> bool {
        if self.remaining == 0 {
            self.truncated = true;
            return false;
        }
        self.remaining -= 1;
        true
    }
}

fn outline_sections(
    sections: &[Section],
    max_depth: Option<u8>,
    depth: u8,
    budget: &mut OutlineBudget,
) -> Vec<SectionOutline> {
    let mut output = Vec::new();
    for section in sections {
        let Some(section) = outline(section, max_depth, depth, budget) else {
            break;
        };
        output.push(section);
    }
    output
}

fn outline(
    section: &Section,
    max_depth: Option<u8>,
    depth: u8,
    budget: &mut OutlineBudget,
) -> Option<SectionOutline> {
    if !budget.take() {
        return None;
    }

    let include_children = max_depth.is_none_or(|limit| depth < limit);

    Some(SectionOutline {
        section_id: section.id.clone(),
        parent_id: section.parent_id.clone(),
        title: section.title.clone(),
        level: section.level,
        location: section.location.clone(),
        children: if include_children {
            outline_sections(
                &section.children,
                max_depth,
                depth.saturating_add(1),
                budget,
            )
        } else {
            vec![]
        },
    })
}
