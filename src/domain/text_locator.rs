use super::{
    ContentHash, Document, DocumentId, NormalizedDocumentHash, NormalizedTextRange, Section,
    SectionId, SentenceTextUnit, TextUnit,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextLocator {
    pub document_id: DocumentId,
    pub content_hash: ContentHash,
    pub normalized_document_hash: NormalizedDocumentHash,
    pub owner_section_id: SectionId,
    pub section_path: Vec<String>,
    pub paragraph_index: Option<usize>,
    pub sentence_index: Option<usize>,
    pub normalized_range: Option<NormalizedTextRange>,
    pub segmentation_version: Option<String>,
    pub native_location: Option<String>,
}

impl TextLocator {
    pub fn for_section(document: &Document, section: &Section) -> Self {
        Self {
            document_id: document.id.clone(),
            content_hash: document.content_hash.clone(),
            normalized_document_hash: document.normalized_document_hash(),
            owner_section_id: section.id.clone(),
            section_path: section.location.section_path.clone(),
            paragraph_index: None,
            sentence_index: None,
            normalized_range: None,
            segmentation_version: None,
            native_location: section.location.native_location.clone(),
        }
    }

    pub fn for_paragraph(document: &Document, section: &Section, paragraph: &TextUnit) -> Self {
        Self {
            document_id: document.id.clone(),
            content_hash: document.content_hash.clone(),
            normalized_document_hash: paragraph.normalized_document_hash.clone(),
            owner_section_id: paragraph.owner_section_id.clone(),
            section_path: section.location.section_path.clone(),
            paragraph_index: Some(paragraph.paragraph_index),
            sentence_index: None,
            normalized_range: Some(paragraph.normalized_range),
            segmentation_version: Some(paragraph.segmentation_version.clone()),
            native_location: section.location.native_location.clone(),
        }
    }

    pub fn for_sentence(
        document: &Document,
        section: &Section,
        sentence: &SentenceTextUnit,
    ) -> Self {
        Self {
            document_id: document.id.clone(),
            content_hash: document.content_hash.clone(),
            normalized_document_hash: sentence.normalized_document_hash.clone(),
            owner_section_id: sentence.owner_section_id.clone(),
            section_path: section.location.section_path.clone(),
            paragraph_index: Some(sentence.paragraph_index),
            sentence_index: Some(sentence.sentence_index),
            normalized_range: Some(sentence.normalized_range),
            segmentation_version: Some(sentence.segmentation_version.clone()),
            native_location: section.location.native_location.clone(),
        }
    }
}
