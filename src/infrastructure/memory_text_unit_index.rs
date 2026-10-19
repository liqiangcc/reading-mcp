use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use async_trait::async_trait;

use crate::application::ports::{ApplicationError, SectionTextUnits, TextUnitIndex};
use crate::domain::{DocumentId, ParagraphTextUnitSet, SectionId, SentenceTextUnitSet, TextUnit};

type DocumentSets = (Arc<ParagraphTextUnitSet>, Arc<SentenceTextUnitSet>);

#[derive(Default)]
pub struct InMemoryTextUnitIndex {
    documents: RwLock<HashMap<DocumentId, Vec<TextUnit>>>,
    sets: RwLock<HashMap<DocumentId, DocumentSets>>,
}

#[async_trait]
impl TextUnitIndex for InMemoryTextUnitIndex {
    async fn replace_document(
        &self,
        document_id: &DocumentId,
        units: &[TextUnit],
    ) -> Result<(), ApplicationError> {
        self.documents
            .write()
            .map_err(|_| {
                ApplicationError::TextUnitIndexFailed("text unit index lock poisoned".into())
            })?
            .insert(document_id.clone(), units.to_vec());
        Ok(())
    }

    async fn list_document(
        &self,
        document_id: &DocumentId,
    ) -> Result<Vec<TextUnit>, ApplicationError> {
        Ok(self
            .documents
            .read()
            .map_err(|_| {
                ApplicationError::TextUnitIndexFailed("text unit index lock poisoned".into())
            })?
            .get(document_id)
            .cloned()
            .unwrap_or_default())
    }

    async fn replace_document_sets(
        &self,
        document_id: &DocumentId,
        paragraphs: Arc<ParagraphTextUnitSet>,
        sentences: Arc<SentenceTextUnitSet>,
    ) -> Result<(), ApplicationError> {
        self.documents
            .write()
            .map_err(|_| {
                ApplicationError::TextUnitIndexFailed("text unit index lock poisoned".into())
            })?
            .insert(document_id.clone(), paragraphs.units.clone());
        self.sets
            .write()
            .map_err(|_| {
                ApplicationError::TextUnitIndexFailed("text unit index lock poisoned".into())
            })?
            .insert(document_id.clone(), (paragraphs, sentences));
        Ok(())
    }

    async fn section_units(
        &self,
        document_id: &DocumentId,
        section_id: &SectionId,
    ) -> Result<Option<SectionTextUnits>, ApplicationError> {
        let sets = self
            .sets
            .read()
            .map_err(|_| {
                ApplicationError::TextUnitIndexFailed("text unit index lock poisoned".into())
            })?
            .get(document_id)
            .map(|(paragraphs, sentences)| (Arc::clone(paragraphs), Arc::clone(sentences)));
        Ok(sets.map(|(paragraphs, sentences)| {
            SectionTextUnits::from_sets(&paragraphs, &sentences, section_id)
        }))
    }
}
