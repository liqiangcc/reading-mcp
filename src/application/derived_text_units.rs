use std::sync::{Arc, OnceLock};

use async_trait::async_trait;

use crate::application::ports::{ApplicationError, SectionTextUnits, TextUnitIndex};
use crate::domain::{
    Document, DocumentId, ParagraphTextUnitSet, SectionId, SentenceTextUnitSet, TextUnit,
};

/// A read-only TextUnitIndex over one decoded document: derives the unit sets once (lazily) with
/// `document.try_text_unit_sets()`. Use cases built without a stored text-unit index (tests) use it; the runtime
/// always passes the stored SQLite index, so production never re-segments a document on read.
pub struct DerivedTextUnits {
    document: Arc<Document>,
    sets: OnceLock<Result<(ParagraphTextUnitSet, SentenceTextUnitSet), String>>,
}

impl DerivedTextUnits {
    pub fn new(document: Arc<Document>) -> Self {
        Self {
            document,
            sets: OnceLock::new(),
        }
    }

    fn sets(&self) -> Result<&(ParagraphTextUnitSet, SentenceTextUnitSet), ApplicationError> {
        match self.sets.get_or_init(|| {
            self.document
                .try_text_unit_sets()
                .map_err(|error| error.to_string())
        }) {
            Ok(sets) => Ok(sets),
            Err(error) => Err(derived_error(error)),
        }
    }
}

fn derived_error(error: &str) -> ApplicationError {
    ApplicationError::TextUnitIndexFailed(format!(
        "cannot read text units from persisted block evidence: {error}"
    ))
}

fn read_only() -> ApplicationError {
    ApplicationError::InvalidRequest("derived text units are read-only".into())
}

#[async_trait]
impl TextUnitIndex for DerivedTextUnits {
    async fn replace_document(
        &self,
        _document_id: &DocumentId,
        _units: &[TextUnit],
    ) -> Result<(), ApplicationError> {
        Err(read_only())
    }

    async fn list_document(
        &self,
        _document_id: &DocumentId,
    ) -> Result<Vec<TextUnit>, ApplicationError> {
        Ok(self.sets()?.0.units.clone())
    }

    async fn replace_document_sets(
        &self,
        _document: &Arc<Document>,
        _paragraphs: Arc<ParagraphTextUnitSet>,
        _sentences: Arc<SentenceTextUnitSet>,
    ) -> Result<(), ApplicationError> {
        Err(read_only())
    }

    async fn section_units(
        &self,
        _document_id: &DocumentId,
        section_id: &SectionId,
    ) -> Result<Option<SectionTextUnits>, ApplicationError> {
        let (paragraphs, sentences) = self.sets()?;
        Ok(Some(SectionTextUnits::from_sets(
            paragraphs, sentences, section_id,
        )))
    }
}
