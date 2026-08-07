use std::collections::BTreeMap;

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
    pub char_start: Option<usize>,
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
