use std::collections::BTreeMap;

mod normalized_text;
mod text_locator;
mod text_unit;

pub use normalized_text::{
    NORMALIZATION_VERSION, NORMALIZED_DOCUMENT_HASH_VERSION, NORMALIZED_TEXT_COORDINATE_SPACE,
    NormalizedDocumentHash, NormalizedTextRange, NormalizedTextRangeError,
};
pub use text_locator::TextLocator;
pub use text_unit::{
    ParagraphContentClass, ParagraphSectionCoverage, ParagraphTextUnitSet, SentenceEligibility,
    SentenceParagraphCoverage, SentenceTextUnit, SentenceTextUnitSet, TEXT_SEGMENTATION_VERSION,
    TEXT_UNIT_ID_VERSION, TextUnit, TextUnitId, TextUnitKind,
};

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct DocumentId(pub String);

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct SectionId(pub String);

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct DocumentSource(pub String);

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct MediaType(pub String);

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ContentHash(pub String);

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Location {
    pub page: Option<u32>,
    pub chapter: Option<String>,
    pub section_path: Vec<String>,
    pub anchor: Option<String>,
    pub paragraph: Option<u32>,
    /// Legacy parser-defined/source coordinate. This is not a normalized
    /// `Section.content` range and must not be silently reinterpreted as one.
    pub char_start: Option<usize>,
    /// Legacy parser-defined/source coordinate. This is not a normalized
    /// `Section.content` range and must not be silently reinterpreted as one.
    pub char_end: Option<usize>,
    pub native_location: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Section {
    pub id: SectionId,
    pub parent_id: Option<SectionId>,
    pub title: String,
    pub level: u8,
    pub content: String,
    pub location: Location,
    pub children: Vec<Section>,
}

impl Section {
    pub fn find(&self, id: &SectionId) -> Option<&Section> {
        if &self.id == id {
            return Some(self);
        }

        self.children.iter().find_map(|child| child.find(id))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Document {
    pub id: DocumentId,
    pub source: DocumentSource,
    pub title: String,
    pub media_type: MediaType,
    pub content_hash: ContentHash,
    pub metadata: BTreeMap<String, String>,
    pub root_sections: Vec<Section>,
}

impl Document {
    pub fn find_section(&self, id: &SectionId) -> Option<&Section> {
        self.root_sections
            .iter()
            .find_map(|section| section.find(id))
    }

    pub fn section_count(&self) -> usize {
        fn count(section: &Section) -> usize {
            1 + section.children.iter().map(count).sum::<usize>()
        }

        self.root_sections.iter().map(count).sum()
    }
}
