use sha2::{Digest, Sha256};

use super::{
    ContentHash, Document, DocumentId, NormalizedDocumentHash, NormalizedTextRange, SectionId,
    SegmentationError,
};

pub const TEXT_SEGMENTATION_VERSION: &str = "text-segmentation/v3";
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ParagraphContentClass {
    /// Persisted text has no strong native/non-prose signal. This is intentionally not a claim of
    /// parser-native prose provenance; it only enables deterministic fallback segmentation.
    ProseOrUnknown,
    NativeParagraph,
    /// Flat `normalized-block-model/v1` may suppress nested leaf blocks, so BlockQuote remains
    /// coarse until stronger nested/leaf evidence is persisted.
    BlockQuote,
    /// Flat `normalized-block-model/v1` may suppress nested leaf blocks, so ListItem remains
    /// coarse until stronger nested/leaf evidence is persisted.
    ListItem,
    CodeBlock,
    Preformatted,
    Table,
}

impl ParagraphContentClass {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ProseOrUnknown => "prose_or_unknown",
            Self::NativeParagraph => "native_paragraph",
            Self::BlockQuote => "blockquote",
            Self::ListItem => "list_item",
            Self::CodeBlock => "code_block",
            Self::Preformatted => "preformatted",
            Self::Table => "table",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SentenceEligibility {
    Eligible,
    CoarseParagraphOnly,
}

impl SentenceEligibility {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Eligible => "eligible",
            Self::CoarseParagraphOnly => "coarse_paragraph_only",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SentenceTextUnit {
    pub id: TextUnitId,
    pub document_id: DocumentId,
    pub content_hash: ContentHash,
    pub normalized_document_hash: NormalizedDocumentHash,
    pub owner_section_id: SectionId,
    /// Human-facing, 1-based containing Paragraph ordinal within the owner Section.
    pub paragraph_index: usize,
    /// Human-facing, 1-based Sentence ordinal within the containing Paragraph.
    pub sentence_index: usize,
    /// Stable containing Paragraph TextUnit handle for container handoff.
    pub parent_paragraph_id: TextUnitId,
    /// Deterministic order within the document's Sentence stream.
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
    pub native_paragraph_chars: usize,
    pub native_structural_container_chars: usize,
    pub native_non_prose_chars: usize,
    pub fallback_chars: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParagraphTextUnitSet {
    pub normalized_document_hash: NormalizedDocumentHash,
    pub units: Vec<TextUnit>,
    pub coverage: Vec<ParagraphSectionCoverage>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SentenceParagraphCoverage {
    pub owner_section_id: SectionId,
    pub paragraph_id: TextUnitId,
    pub paragraph_index: usize,
    pub content_class: ParagraphContentClass,
    pub eligibility: SentenceEligibility,
    pub paragraph_chars: usize,
    pub sentence_chars: usize,
    pub separator_chars: usize,
    pub coarse_only_chars: usize,
    pub sentence_count: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SentenceTextUnitSet {
    pub normalized_document_hash: NormalizedDocumentHash,
    pub units: Vec<SentenceTextUnit>,
    pub coverage: Vec<SentenceParagraphCoverage>,
}

impl Document {
    pub fn try_paragraph_text_units(&self) -> Result<ParagraphTextUnitSet, SegmentationError> {
        Ok(self.try_text_unit_sets()?.0)
    }

    pub fn paragraph_text_units(&self) -> ParagraphTextUnitSet {
        self.try_paragraph_text_units()
            .expect("canonical normalized block map must be valid for TextUnit materialization")
    }

    pub fn try_sentence_text_units(&self) -> Result<SentenceTextUnitSet, SegmentationError> {
        Ok(self.try_text_unit_sets()?.1)
    }

    pub fn sentence_text_units(&self) -> SentenceTextUnitSet {
        self.try_sentence_text_units()
            .expect("canonical normalized block map must be valid for Sentence materialization")
    }

    /// Builds both unit sets from one segmentation of the persisted block evidence.
    pub fn try_text_unit_sets(
        &self,
    ) -> Result<(ParagraphTextUnitSet, SentenceTextUnitSet), SegmentationError> {
        let segmentation = self.try_segmentation()?;
        let normalized_document_hash = self.normalized_document_hash();
        Ok(segmentation.materialize(self, &normalized_document_hash))
    }

    pub fn text_unit_sets(&self) -> (ParagraphTextUnitSet, SentenceTextUnitSet) {
        self.try_text_unit_sets()
            .expect("canonical normalized block map must be valid for Sentence materialization")
    }
}

pub(crate) fn paragraph_text_unit_id(
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

pub(crate) fn sentence_text_unit_id(
    document_id: &DocumentId,
    normalized_document_hash: &NormalizedDocumentHash,
    owner_section_id: &SectionId,
    paragraph_index: usize,
    sentence_index: usize,
    range: NormalizedTextRange,
) -> TextUnitId {
    let mut hasher = Sha256::new();
    hasher.update(TEXT_UNIT_ID_DOMAIN);
    hash_text(&mut hasher, document_id.0.as_str());
    hash_text(&mut hasher, normalized_document_hash.as_ref());
    hash_text(&mut hasher, owner_section_id.0.as_str());
    hash_text(&mut hasher, "sentence");
    hash_usize(&mut hasher, paragraph_index);
    hash_usize(&mut hasher, sentence_index);
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
