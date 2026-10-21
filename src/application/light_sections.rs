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
use crate::application::reading_support::SectionContents;
use crate::domain::{Document, DocumentId, Section, SectionId};

/// The section text and section lengths one reader call has asked for.
pub(crate) struct SectionTexts<'a> {
    document: &'a Document,
    /// `None` when `document` carries its own section text.
    repository: Option<Arc<dyn DocumentRepository>>,
    texts: HashMap<SectionId, String>,
    char_lens: HashMap<SectionId, usize>,
}

impl<'a> SectionTexts<'a> {
    /// Section text from `document` itself.
    pub(crate) fn from_document(document: &'a Document) -> Self {
        Self {
            document,
            repository: None,
            texts: HashMap::new(),
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
            texts: HashMap::new(),
            char_lens: HashMap::new(),
        }
    }

    /// Loads one section's text into the cache, unless it is already there.
    async fn load(&mut self, section_id: &SectionId) -> Result<(), ApplicationError> {
        if self.texts.contains_key(section_id) {
            return Ok(());
        }
        let text = match self.repository {
            None => self
                .document
                .find_section(section_id)
                .map(|section| section.content.clone()),
            Some(repository) => {
                repository
                    .load_section_text(&self.document.id, section_id)
                    .await?
            }
        };
        let text = text.ok_or_else(|| missing_text(&self.document.id, section_id))?;
        self.texts.insert(section_id.clone(), text);
        Ok(())
    }

    /// The normalized text of one section.
    pub(crate) async fn text(
        &mut self,
        section_id: &SectionId,
    ) -> Result<String, ApplicationError> {
        self.load(section_id).await?;
        self.texts
            .get(section_id)
            .cloned()
            .ok_or_else(|| missing_text(&self.document.id, section_id))
    }

    /// The number of Unicode scalars of one section's normalized text.
    pub(crate) async fn char_len(
        &mut self,
        section_id: &SectionId,
    ) -> Result<usize, ApplicationError> {
        if let Some(len) = self.char_lens.get(section_id) {
            return Ok(*len);
        }
        let len = match self.repository {
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

    /// Loads the text of `section` and of every section below it, in document order.
    pub(crate) async fn fill_subtree(&mut self, section: &Section) -> Result<(), ApplicationError> {
        if self.repository.is_none() {
            return Ok(());
        }
        for section_id in subtree_ids(section) {
            self.load(&section_id).await?;
        }
        Ok(())
    }

    /// The texts loaded so far, as the renderers read them.
    pub(crate) fn contents(&self) -> SectionContents<'_> {
        match self.repository {
            None => SectionContents::Inline,
            Some(_) => SectionContents::Loaded(&self.texts),
        }
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

/// `section` and every section below it, in document order.
fn subtree_ids(section: &Section) -> Vec<SectionId> {
    fn collect(section: &Section, output: &mut Vec<SectionId>) {
        output.push(section.id.clone());
        for child in &section.children {
            collect(child, output);
        }
    }
    let mut output = Vec::new();
    collect(section, &mut output);
    output
}
