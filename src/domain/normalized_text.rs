use std::error::Error;
use std::fmt;

use sha2::{Digest, Sha256};

use super::{Document, Section};

pub const NORMALIZATION_VERSION: &str = "reading-mcp-normalization/v3";
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
            Self::StartAfterEnd { start, end } => write!(
                formatter,
                "normalized range start {start} exceeds end {end}"
            ),
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

impl Document {
    pub fn normalized_document_hash(&self) -> NormalizedDocumentHash {
        let mut hasher = Sha256::new();
        hasher.update(NORMALIZED_DOCUMENT_HASH_DOMAIN);
        hash_usize(&mut hasher, self.root_sections.len());
        for section in &self.root_sections {
            hash_section(&mut hasher, section);
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
    hash_usize(hasher, section.children.len());
    for child in &section.children {
        hash_section(hasher, child);
    }
}

fn hash_text(hasher: &mut Sha256, value: &str) {
    hash_usize(hasher, value.len());
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

fn hash_usize(hasher: &mut Sha256, value: usize) {
    let value = u64::try_from(value).expect("normalized document size must fit in u64");
    hasher.update(value.to_be_bytes());
}

fn byte_offset_for_scalar(text: &str, scalar_index: usize) -> usize {
    match text.char_indices().nth(scalar_index) {
        Some((byte_index, _)) => byte_index,
        None => text.len(),
    }
}
