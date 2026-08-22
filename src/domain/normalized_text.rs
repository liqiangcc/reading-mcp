use std::error::Error;
use std::fmt;

use sha2::{Digest, Sha256};

use super::{Document, Section};

pub const NORMALIZATION_VERSION: &str = "reading-mcp-normalization/v2";
pub const NORMALIZED_DOCUMENT_HASH_VERSION: &str = "normalized-document-hash/v1";
pub const NORMALIZED_TEXT_COORDINATE_SPACE: &str = "section-content-unicode-scalar/v1";

const NORMALIZED_DOCUMENT_HASH_DOMAIN: &[u8] = b"reading-mcp/normalized-document-hash/v1\0";

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct NormalizedDocumentHash(pub String);

impl fmt::Display for NormalizedDocumentHash {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl AsRef<str> for NormalizedDocumentHash {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct NormalizedTextRange {
    start: usize,
    end: usize,
}

impl NormalizedTextRange {
    pub fn new(start: usize, end: usize) -> Result<Self, NormalizedTextRangeError> {
        if start > end {
            return Err(NormalizedTextRangeError::StartAfterEnd { start, end });
        }
        Ok(Self { start, end })
    }

    pub const fn start(self) -> usize {
        self.start
    }

    pub const fn end(self) -> usize {
        self.end
    }

    pub const fn len(self) -> usize {
        self.end - self.start
    }

    pub const fn is_empty(self) -> bool {
        self.start == self.end
    }

    pub fn validate_for_text(self, owner_text: &str) -> Result<(), NormalizedTextRangeError> {
        let owner_len = owner_text.chars().count();
        if self.end > owner_len {
            return Err(NormalizedTextRangeError::OutOfBounds {
                start: self.start,
                end: self.end,
                owner_len,
            });
        }
        Ok(())
    }

    pub fn slice(self, owner_text: &str) -> Result<&str, NormalizedTextRangeError> {
        self.validate_for_text(owner_text)?;
        let start_byte = byte_offset_for_scalar(owner_text, self.start);
        let end_byte = byte_offset_for_scalar(owner_text, self.end);
        Ok(&owner_text[start_byte..end_byte])
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NormalizedTextRangeError {
    StartAfterEnd {
        start: usize,
        end: usize,
    },
    OutOfBounds {
        start: usize,
        end: usize,
        owner_len: usize,
    },
}

impl fmt::Display for NormalizedTextRangeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::StartAfterEnd { start, end } => {
                write!(formatter, "normalized range start {start} is after end {end}")
            }
            Self::OutOfBounds {
                start,
                end,
                owner_len,
            } => write!(
                formatter,
                "normalized range [{start}, {end}) exceeds owner text length {owner_len}"
            ),
        }
    }
}

impl Error for NormalizedTextRangeError {}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct NormalizedDocumentHashVersion(pub String);

impl Document {
    pub fn normalized_document_hash(&self) -> NormalizedDocumentHash {
        let mut hasher = Sha256::new();
        hasher.update(NORMALIZED_DOCUMENT_HASH_DOMAIN);
        update_string(&mut hasher, &self.title);
        update_string(&mut hasher, &self.media_type.0);
        update_usize(&mut hasher, self.root_sections.len());
        for section in &self.root_sections {
            update_section(&mut hasher, section);
        }
        NormalizedDocumentHash(format!("sha256:{:x}", hasher.finalize()))
    }
}

impl Section {
    pub fn normalized_text_len(&self) -> usize {
        self.content.chars().count()
    }

    pub fn validate_normalized_range(
        &self,
        range: NormalizedTextRange,
    ) -> Result<(), NormalizedTextRangeError> {
        range.validate_for_text(&self.content)
    }

    pub fn normalized_text_slice(
        &self,
        range: NormalizedTextRange,
    ) -> Result<&str, NormalizedTextRangeError> {
        range.slice(&self.content)
    }
}

fn update_section(hasher: &mut Sha256, section: &Section) {
    update_string(hasher, &section.id.0);
    update_optional_string(hasher, section.parent_id.as_ref().map(|value| value.0.as_str()));
    update_string(hasher, &section.title);
    hasher.update([section.level]);
    update_string(hasher, &section.content);
    update_usize(hasher, section.children.len());
    for child in &section.children {
        update_section(hasher, child);
    }
}

fn update_string(hasher: &mut Sha256, value: &str) {
    update_usize(hasher, value.len());
    hasher.update(value.as_bytes());
}

fn update_optional_string(hasher: &mut Sha256, value: Option<&str>) {
    match value {
        Some(value) => {
            hasher.update([1]);
            update_string(hasher, value);
        }
        None => hasher.update([0]),
    }
}

fn update_usize(hasher: &mut Sha256, value: usize) {
    hasher.update((value as u64).to_le_bytes());
}

fn byte_offset_for_scalar(text: &str, scalar_offset: usize) -> usize {
    if scalar_offset == text.chars().count() {
        return text.len();
    }
    text.char_indices()
        .nth(scalar_offset)
        .map(|(offset, _)| offset)
        .unwrap_or(text.len())
}
