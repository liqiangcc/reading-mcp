//! A text-unit index that also answers the v2 short-handle keys.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use reading_mcp::application::ports::{ApplicationError, SectionTextUnits, TextUnitIndex};
use reading_mcp::application::text_unit_store::store_text_units;
use reading_mcp::domain::{
    Document, DocumentId, ParagraphTextUnitSet, Section, SectionId, SentenceTextUnitSet, TextUnit,
};
use reading_mcp::infrastructure::InMemoryTextUnitIndex;

/// [`InMemoryTextUnitIndex`] plus the `document_key` / `section_key` mapping, in document order.
#[derive(Default)]
pub struct KeyedTextUnitIndex {
    inner: InMemoryTextUnitIndex,
    sections: RwLock<HashMap<DocumentId, Vec<SectionId>>>,
}

impl KeyedTextUnitIndex {
    pub async fn store(&self, document: &Document) {
        store_text_units(&self.inner, document)
            .await
            .expect("units should persist");
        self.record_sections(document);
    }

    fn record_sections(&self, document: &Document) {
        let mut ordered = Vec::new();
        collect_ids(&document.root_sections, &mut ordered);
        self.sections
            .write()
            .expect("lock")
            .insert(document.id.clone(), ordered);
    }

    fn section_ids(&self, document_id: &DocumentId) -> Option<Vec<SectionId>> {
        self.sections
            .read()
            .expect("lock")
            .get(document_id)
            .cloned()
    }
}

fn collect_ids(sections: &[Section], output: &mut Vec<SectionId>) {
    for section in sections {
        output.push(section.id.clone());
        collect_ids(&section.children, output);
    }
}

#[async_trait]
impl TextUnitIndex for KeyedTextUnitIndex {
    async fn replace_document(
        &self,
        document_id: &DocumentId,
        units: &[TextUnit],
    ) -> Result<(), ApplicationError> {
        self.inner.replace_document(document_id, units).await
    }

    async fn list_document(
        &self,
        document_id: &DocumentId,
    ) -> Result<Vec<TextUnit>, ApplicationError> {
        self.inner.list_document(document_id).await
    }

    async fn replace_document_sets(
        &self,
        document: &Arc<Document>,
        paragraphs: Arc<ParagraphTextUnitSet>,
        sentences: Arc<SentenceTextUnitSet>,
    ) -> Result<(), ApplicationError> {
        self.record_sections(document);
        self.inner
            .replace_document_sets(document, paragraphs, sentences)
            .await
    }

    async fn section_units(
        &self,
        document_id: &DocumentId,
        section_id: &SectionId,
    ) -> Result<Option<SectionTextUnits>, ApplicationError> {
        self.inner.section_units(document_id, section_id).await
    }

    async fn document_key(
        &self,
        document_id: &DocumentId,
    ) -> Result<Option<i64>, ApplicationError> {
        // A handle's document key starts at 1, so this fixture document is key 1.
        Ok(self.section_ids(document_id).map(|_| 1))
    }

    async fn document_id_for_key(
        &self,
        doc_key: i64,
    ) -> Result<Option<DocumentId>, ApplicationError> {
        if doc_key != 1 {
            return Ok(None);
        }
        Ok(self.sections.read().expect("lock").keys().next().cloned())
    }

    async fn section_key(
        &self,
        document_id: &DocumentId,
        section_id: &SectionId,
    ) -> Result<Option<i64>, ApplicationError> {
        let Some(sections) = self.section_ids(document_id) else {
            return Ok(None);
        };
        Ok(sections
            .iter()
            .position(|candidate| candidate == section_id)
            .and_then(|index| i64::try_from(index).ok()))
    }

    async fn section_id_for_key(
        &self,
        document_id: &DocumentId,
        section_key: i64,
    ) -> Result<Option<SectionId>, ApplicationError> {
        let Some(sections) = self.section_ids(document_id) else {
            return Ok(None);
        };
        Ok(usize::try_from(section_key)
            .ok()
            .and_then(|index| sections.get(index).cloned()))
    }
}
