//! The section text one reader call needs, for the readers that hold only the light document.
//!
//! A light `Document` carries no section `content`, so a reader that renders or slices a section asks
//! the repository for exactly that section (`DocumentRepository::load_section_text`) and for exactly
//! that section's length (`DocumentRepository::load_section_char_len`). Every request is made once per
//! call and kept here, so a reader's diff stays mechanical and its cost stays O(the text it renders).
//!
//! A reader that loaded the full document — the path taken when the use case was built without a
//! stored text-unit index — reads the same text from that document, so both paths run the same
//! rendering code and produce the same bytes.

use std::collections::HashMap;
use std::sync::Arc;

use crate::application::ports::{ApplicationError, DocumentRepository};
use crate::domain::{Document, DocumentId, SectionId};

/// The section text and section lengths one reader call has asked for.
pub(crate) struct SectionTexts<'a> {
    document: &'a Document,
    /// `None` when `document` carries its own section text.
    repository: Option<Arc<dyn DocumentRepository>>,
    char_lens: HashMap<SectionId, usize>,
}

impl<'a> SectionTexts<'a> {
    /// Section text from `document` itself.
    pub(crate) fn from_document(document: &'a Document) -> Self {
        Self {
            document,
            repository: None,
            char_lens: HashMap::new(),
        }
    }

    /// Section text from `repository`, one section per call.
    pub(crate) fn from_repository(
        document: &'a Document,
        repository: Arc<dyn DocumentRepository>,
    ) -> Self {
        Self {
            document,
            repository: Some(repository),
            char_lens: HashMap::new(),
        }
    }

    /// The number of Unicode scalars of one section's normalized text.
    pub(crate) async fn char_len(
        &mut self,
        section_id: &SectionId,
    ) -> Result<usize, ApplicationError> {
        if let Some(len) = self.char_lens.get(section_id) {
            return Ok(*len);
        }
        let len = match &self.repository {
            None => self
                .document
                .find_section(section_id)
                .map(|section| section.normalized_text_len()),
            Some(repository) => {
                repository
                    .load_section_char_len(&self.document.id, section_id)
                    .await?
            }
        };
        let len = len.ok_or_else(|| missing_text(&self.document.id, section_id))?;
        self.char_lens.insert(section_id.clone(), len);
        Ok(len)
    }
}

/// A section of the document whose text the repository cannot produce is a broken store, not an
/// empty section: fail loudly instead of rendering nothing.
fn missing_text(document_id: &DocumentId, section_id: &SectionId) -> ApplicationError {
    ApplicationError::RepositoryFailed(format!(
        "section {} of document {} has no stored text",
        section_id.0, document_id.0
    ))
}
