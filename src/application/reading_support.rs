use sha2::{Digest, Sha256};

use crate::domain::{Document, Location, Section};

const DEFAULT_CONTENT_RESPONSE_CHARS: usize = 32_000;
const MAX_CONTENT_RESPONSE_CHARS: usize = 64_000;
pub(crate) const SECTION_TREE_READ_MODE: &str = "section_tree";
pub(crate) const SECTION_TREE_RENDERING_VERSION: &str = "section-tree-markdown/v1";
const NORMALIZED_DOCUMENT_HASH_DOMAIN: &[u8] = b"reading-mcp/normalized-document/v1\0";

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CharacterSlice {
    pub content: String,
    pub start_char: usize,
    pub end_char: usize,
    pub total_chars: usize,
    pub complete: bool,
}

pub(crate) fn render_section_tree(section: &Section) -> String {
    let mut output = String::new();
    render_tree_into(section, &mut output);
    output.trim().to_string()
}

pub(crate) fn render_section_shallow(section: &Section) -> String {
    let mut output = String::new();
    let heading_level = usize::from(section.level.clamp(1, 6));
    output.push_str(&"#".repeat(heading_level));
    output.push(' ');
    output.push_str(&section.title);

    if !section.content.trim().is_empty() {
        output.push_str("\n\n");
        output.push_str(section.content.trim());
    }

    output
}

pub(crate) fn flatten_sections<'a>(sections: &'a [Section], output: &mut Vec<&'a Section>) {
    for section in sections {
        output.push(section);
        flatten_sections(&section.children, output);
    }
}

pub(crate) fn content_response_limit(requested_max_chars: Option<usize>) -> usize {
    requested_max_chars
        .unwrap_or(DEFAULT_CONTENT_RESPONSE_CHARS)
        .min(MAX_CONTENT_RESPONSE_CHARS)
}

pub(crate) fn slice_chars(content: &str, start_char: usize, limit: usize) -> CharacterSlice {
    let total_chars = content.chars().count();
    let end_char = start_char.saturating_add(limit).min(total_chars);
    let returned = content
        .chars()
        .skip(start_char)
        .take(end_char.saturating_sub(start_char))
        .collect();

    CharacterSlice {
        content: returned,
        start_char,
        end_char,
        total_chars,
        complete: end_char == total_chars,
    }
}

pub(crate) fn truncate_chars(
    content: String,
    requested_max_chars: Option<usize>,
) -> (String, bool) {
    let slice = slice_chars(&content, 0, content_response_limit(requested_max_chars));
    (slice.content, !slice.complete)
}

pub(crate) fn normalized_document_hash(document: &Document) -> String {
    let mut hasher = Sha256::new();
    hasher.update(NORMALIZED_DOCUMENT_HASH_DOMAIN);
    hash_text(&mut hasher, &document.title);
    hash_text(&mut hasher, &document.media_type.0);
    hash_usize(&mut hasher, document.root_sections.len());
    for section in &document.root_sections {
        hash_section(&mut hasher, section);
    }
    format!("sha256:{:x}", hasher.finalize())
}

fn render_tree_into(section: &Section, output: &mut String) {
    output.push_str(&render_section_shallow(section));
    output.push('\n');

    for child in &section.children {
        output.push('\n');
        render_tree_into(child, output);
    }
}

fn hash_section(hasher: &mut Sha256, section: &Section) {
    hasher.update(b"section\0");
    hash_text(hasher, &section.id.0);
    hash_optional_text(
        hasher,
        section.parent_id.as_ref().map(|value| value.0.as_str()),
    );
    hash_text(hasher, &section.title);
    hasher.update([section.level]);
    hash_text(hasher, &section.content);
    hash_location(hasher, &section.location);
    hash_usize(hasher, section.children.len());
    for child in &section.children {
        hash_section(hasher, child);
    }
}

fn hash_location(hasher: &mut Sha256, location: &Location) {
    hasher.update(b"location\0");
    hash_optional_u64(hasher, location.page.map(u64::from));
    hash_optional_text(hasher, location.chapter.as_deref());
    hash_usize(hasher, location.section_path.len());
    for component in &location.section_path {
        hash_text(hasher, component);
    }
    hash_optional_text(hasher, location.anchor.as_deref());
    hash_optional_u64(hasher, location.paragraph.map(u64::from));
    hash_optional_usize(hasher, location.char_start);
    hash_optional_usize(hasher, location.char_end);
    hash_optional_text(hasher, location.native_location.as_deref());
}

fn hash_text(hasher: &mut Sha256, value: &str) {
    hash_u64(hasher, u64::try_from(value.len()).unwrap_or(u64::MAX));
    hasher.update(value.as_bytes());
}

fn hash_optional_text(hasher: &mut Sha256, value: Option<&str>) {
    match value {
        Some(value) => {
            hasher.update([1]);
            hash_text(hasher, value);
        }
        None => hasher.update([0]),
    }
}

fn hash_optional_usize(hasher: &mut Sha256, value: Option<usize>) {
    hash_optional_u64(
        hasher,
        value.map(|value| u64::try_from(value).unwrap_or(u64::MAX)),
    );
}

fn hash_optional_u64(hasher: &mut Sha256, value: Option<u64>) {
    match value {
        Some(value) => {
            hasher.update([1]);
            hash_u64(hasher, value);
        }
        None => hasher.update([0]),
    }
}

fn hash_usize(hasher: &mut Sha256, value: usize) {
    hash_u64(hasher, u64::try_from(value).unwrap_or(u64::MAX));
}

fn hash_u64(hasher: &mut Sha256, value: u64) {
    hasher.update(value.to_be_bytes());
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::{normalized_document_hash, slice_chars};
    use crate::domain::{
        ContentHash, Document, DocumentId, DocumentSource, Location, MediaType, Section, SectionId,
    };

    #[test]
    fn character_slice_uses_unicode_scalar_stream_coordinates() {
        let slice = slice_chars("A中🙂Z", 1, 2);
        assert_eq!(slice.content, "中🙂");
        assert_eq!(slice.start_char, 1);
        assert_eq!(slice.end_char, 3);
        assert_eq!(slice.total_chars, 4);
        assert!(!slice.complete);
    }

    #[test]
    fn normalized_hash_changes_when_canonical_section_content_changes() {
        let first = document("first");
        let second = document("second");
        assert_ne!(
            normalized_document_hash(&first),
            normalized_document_hash(&second)
        );
    }

    fn document(content: &str) -> Document {
        Document {
            id: DocumentId("doc:normalized".into()),
            source: DocumentSource("memory:normalized".into()),
            title: "Normalized".into(),
            media_type: MediaType("text/plain".into()),
            content_hash: ContentHash("sha256:raw-unchanged".into()),
            metadata: BTreeMap::new(),
            root_sections: vec![Section {
                id: SectionId("section://root".into()),
                parent_id: None,
                title: "Root".into(),
                level: 1,
                content: content.into(),
                location: Location::default(),
                children: vec![],
            }],
        }
    }
}
