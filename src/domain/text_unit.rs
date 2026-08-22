use sha2::{Digest, Sha256};

use super::{
    ContentHash, Document, DocumentId, NormalizedDocumentHash, NormalizedTextRange, Section,
    SectionId,
};

pub const TEXT_SEGMENTATION_VERSION: &str = "text-segmentation/v1";
pub const TEXT_UNIT_ID_VERSION: &str = "text-unit-id/v1";

const TEXT_UNIT_ID_DOMAIN: &[u8] = b"reading-mcp/text-unit-id/v1\0";

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct TextUnitId(pub String);

impl AsRef<str> for TextUnitId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TextUnitKind {
    Paragraph,
}

impl TextUnitKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Paragraph => "paragraph",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextUnit {
    pub id: TextUnitId,
    pub document_id: DocumentId,
    pub content_hash: ContentHash,
    pub normalized_document_hash: NormalizedDocumentHash,
    pub owner_section_id: SectionId,
    pub kind: TextUnitKind,
    /// Human-facing, 1-based Paragraph ordinal within the owner Section.
    pub paragraph_index: usize,
    /// Global deterministic traversal order within the normalized Document.
    pub source_order: usize,
    pub normalized_range: NormalizedTextRange,
    pub text: String,
    pub segmentation_version: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParagraphSectionCoverage {
    pub owner_section_id: SectionId,
    pub owner_chars: usize,
    pub paragraph_chars: usize,
    pub separator_chars: usize,
    pub paragraph_count: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParagraphTextUnitSet {
    pub normalized_document_hash: NormalizedDocumentHash,
    pub units: Vec<TextUnit>,
    pub coverage: Vec<ParagraphSectionCoverage>,
}

impl Document {
    pub fn paragraph_text_units(&self) -> ParagraphTextUnitSet {
        let normalized_document_hash = self.normalized_document_hash();
        let mut units = Vec::new();
        let mut coverage = Vec::new();

        for section in &self.root_sections {
            collect_section_paragraphs(
                self,
                &normalized_document_hash,
                section,
                &mut units,
                &mut coverage,
            );
        }

        ParagraphTextUnitSet {
            normalized_document_hash,
            units,
            coverage,
        }
    }
}

fn collect_section_paragraphs(
    document: &Document,
    normalized_document_hash: &NormalizedDocumentHash,
    section: &Section,
    units: &mut Vec<TextUnit>,
    coverage: &mut Vec<ParagraphSectionCoverage>,
) {
    let ranges = paragraph_ranges(&section.content);
    let paragraph_chars = ranges.iter().map(|range| range.len()).sum::<usize>();
    let owner_chars = section.normalized_text_len();

    coverage.push(ParagraphSectionCoverage {
        owner_section_id: section.id.clone(),
        owner_chars,
        paragraph_chars,
        separator_chars: owner_chars.saturating_sub(paragraph_chars),
        paragraph_count: ranges.len(),
    });

    for (offset, range) in ranges.into_iter().enumerate() {
        let paragraph_index = offset + 1;
        let source_order = units.len();
        let text = section
            .normalized_text_slice(range)
            .expect("generated paragraph range must be a valid owner slice")
            .to_string();
        let id = text_unit_id(
            &document.id,
            normalized_document_hash,
            &section.id,
            paragraph_index,
            range,
        );

        units.push(TextUnit {
            id,
            document_id: document.id.clone(),
            content_hash: document.content_hash.clone(),
            normalized_document_hash: normalized_document_hash.clone(),
            owner_section_id: section.id.clone(),
            kind: TextUnitKind::Paragraph,
            paragraph_index,
            source_order,
            normalized_range: range,
            text,
            segmentation_version: TEXT_SEGMENTATION_VERSION.into(),
        });
    }

    for child in &section.children {
        collect_section_paragraphs(
            document,
            normalized_document_hash,
            child,
            units,
            coverage,
        );
    }
}

fn paragraph_ranges(content: &str) -> Vec<NormalizedTextRange> {
    let mut ranges = Vec::new();
    let mut paragraph_start = None;
    let mut paragraph_end = 0usize;
    let mut scalar_offset = 0usize;

    for line in content.split_inclusive('\n') {
        let line_chars = line.chars().count();
        let line_body = strip_line_ending(line);
        let line_body_chars = line_body.chars().count();

        if line_body.trim().is_empty() {
            if let Some(start) = paragraph_start.take() {
                ranges.push(
                    NormalizedTextRange::new(start, paragraph_end)
                        .expect("paragraph boundaries must be ordered"),
                );
            }
        } else {
            paragraph_start.get_or_insert(scalar_offset);
            paragraph_end = scalar_offset + line_body_chars;
        }

        scalar_offset += line_chars;
    }

    if let Some(start) = paragraph_start {
        ranges.push(
            NormalizedTextRange::new(start, paragraph_end)
                .expect("paragraph boundaries must be ordered"),
        );
    }

    ranges
}

fn strip_line_ending(line: &str) -> &str {
    let line = line.strip_suffix('\n').unwrap_or(line);
    line.strip_suffix('\r').unwrap_or(line)
}

fn text_unit_id(
    document_id: &DocumentId,
    normalized_document_hash: &NormalizedDocumentHash,
    owner_section_id: &SectionId,
    paragraph_index: usize,
    range: NormalizedTextRange,
) -> TextUnitId {
    let mut hasher = Sha256::new();
    hasher.update(TEXT_UNIT_ID_DOMAIN);
    hash_text(&mut hasher, document_id.0.as_str());
    hash_text(&mut hasher, normalized_document_hash.as_ref());
    hash_text(&mut hasher, owner_section_id.0.as_str());
    hash_text(&mut hasher, TextUnitKind::Paragraph.as_str());
    hash_usize(&mut hasher, paragraph_index);
    hash_usize(&mut hasher, range.start());
    hash_usize(&mut hasher, range.end());
    hash_text(&mut hasher, TEXT_SEGMENTATION_VERSION);
    TextUnitId(format!("tu1:{:x}", hasher.finalize()))
}

fn hash_text(hasher: &mut Sha256, value: &str) {
    hash_usize(hasher, value.len());
    hasher.update(value.as_bytes());
}

fn hash_usize(hasher: &mut Sha256, value: usize) {
    let value = u64::try_from(value).expect("text-unit identity values must fit in u64");
    hasher.update(value.to_be_bytes());
}
