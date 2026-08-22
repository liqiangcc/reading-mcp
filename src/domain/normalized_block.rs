use std::collections::HashMap;
use std::error::Error;
use std::fmt;

use serde::{Deserialize, Serialize};

use super::{Document, NormalizedTextRange, SectionId};

pub const NORMALIZED_BLOCK_MODEL_VERSION: &str = "normalized-block-model/v1";
pub const NORMALIZED_BLOCK_MAP_METADATA_KEY: &str = "normalized_block_map";
pub const NORMALIZED_BLOCK_MAP_VERSION_METADATA_KEY: &str = "normalized_block_map_version";
pub const NORMALIZED_BLOCK_COUNT_METADATA_KEY: &str = "normalized_blocks";

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum NormalizedBlockKind {
    Paragraph,
    BlockQuote,
    ListItem,
    Preformatted,
    Table,
}

impl NormalizedBlockKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Paragraph => "paragraph",
            Self::BlockQuote => "blockquote",
            Self::ListItem => "list_item",
            Self::Preformatted => "preformatted",
            Self::Table => "table",
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum NormalizedBlockProvenance {
    XhtmlNativeBlock,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct NormalizedBlock {
    pub owner_section_id: SectionId,
    /// Human-facing, 1-based block ordinal within the owner Section.
    pub block_index: usize,
    /// Deterministic global source order emitted by the parser. This is independent from
    /// reconciled Section-tree traversal order.
    pub source_order: usize,
    pub kind: NormalizedBlockKind,
    pub normalized_range: NormalizedTextRange,
    pub native_anchor: Option<String>,
    pub native_location: Option<String>,
    pub provenance: NormalizedBlockProvenance,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct NormalizedBlockMap {
    pub schema_version: String,
    pub blocks: Vec<NormalizedBlock>,
}

impl NormalizedBlockMap {
    pub fn new(blocks: Vec<NormalizedBlock>) -> Self {
        Self {
            schema_version: NORMALIZED_BLOCK_MODEL_VERSION.into(),
            blocks,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NormalizedBlockMapError {
    UnsupportedVersion(String),
    InvalidJson(String),
    UnknownOwner(String),
    InvalidBlockIndex {
        owner_section_id: String,
        expected: usize,
        actual: usize,
    },
    InvalidSourceOrder {
        expected: usize,
        actual: usize,
    },
    InvalidRange {
        owner_section_id: String,
        message: String,
    },
    EmptyRange {
        owner_section_id: String,
        block_index: usize,
    },
    OverlapOrReorder {
        owner_section_id: String,
        previous_end: usize,
        next_start: usize,
    },
}

impl fmt::Display for NormalizedBlockMapError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedVersion(version) => {
                write!(formatter, "unsupported normalized block map version {version:?}")
            }
            Self::InvalidJson(message) => {
                write!(formatter, "invalid normalized block map JSON: {message}")
            }
            Self::UnknownOwner(owner) => {
                write!(formatter, "normalized block owner Section {owner:?} does not exist")
            }
            Self::InvalidBlockIndex {
                owner_section_id,
                expected,
                actual,
            } => write!(
                formatter,
                "normalized block in Section {owner_section_id:?} has block index {actual}, expected {expected}"
            ),
            Self::InvalidSourceOrder { expected, actual } => write!(
                formatter,
                "normalized block has source order {actual}, expected {expected}"
            ),
            Self::InvalidRange {
                owner_section_id,
                message,
            } => write!(
                formatter,
                "normalized block range for Section {owner_section_id:?} is invalid: {message}"
            ),
            Self::EmptyRange {
                owner_section_id,
                block_index,
            } => write!(
                formatter,
                "normalized block {block_index} in Section {owner_section_id:?} has an empty range"
            ),
            Self::OverlapOrReorder {
                owner_section_id,
                previous_end,
                next_start,
            } => write!(
                formatter,
                "normalized blocks in Section {owner_section_id:?} overlap or reorder: previous end {previous_end}, next start {next_start}"
            ),
        }
    }
}

impl Error for NormalizedBlockMapError {}

impl Document {
    pub fn normalized_block_map(
        &self,
    ) -> Result<Option<NormalizedBlockMap>, NormalizedBlockMapError> {
        let Some(json) = self.metadata.get(NORMALIZED_BLOCK_MAP_METADATA_KEY) else {
            return Ok(None);
        };
        let map = serde_json::from_str::<NormalizedBlockMap>(json)
            .map_err(|error| NormalizedBlockMapError::InvalidJson(error.to_string()))?;
        self.validate_normalized_block_map(&map)?;
        Ok(Some(map))
    }

    pub fn set_normalized_block_map(
        &mut self,
        map: NormalizedBlockMap,
    ) -> Result<(), NormalizedBlockMapError> {
        self.validate_normalized_block_map(&map)?;
        let json = serde_json::to_string(&map)
            .map_err(|error| NormalizedBlockMapError::InvalidJson(error.to_string()))?;
        self.metadata.insert(
            NORMALIZED_BLOCK_MAP_VERSION_METADATA_KEY.into(),
            map.schema_version.clone(),
        );
        self.metadata.insert(
            NORMALIZED_BLOCK_COUNT_METADATA_KEY.into(),
            map.blocks.len().to_string(),
        );
        self.metadata
            .insert(NORMALIZED_BLOCK_MAP_METADATA_KEY.into(), json);
        Ok(())
    }

    pub fn validate_normalized_block_map(
        &self,
        map: &NormalizedBlockMap,
    ) -> Result<(), NormalizedBlockMapError> {
        if map.schema_version != NORMALIZED_BLOCK_MODEL_VERSION {
            return Err(NormalizedBlockMapError::UnsupportedVersion(
                map.schema_version.clone(),
            ));
        }

        let mut expected_block_index = HashMap::<SectionId, usize>::new();
        let mut last_range_end = HashMap::<SectionId, usize>::new();

        for (expected_source_order, block) in map.blocks.iter().enumerate() {
            if block.source_order != expected_source_order {
                return Err(NormalizedBlockMapError::InvalidSourceOrder {
                    expected: expected_source_order,
                    actual: block.source_order,
                });
            }

            let owner = self.find_section(&block.owner_section_id).ok_or_else(|| {
                NormalizedBlockMapError::UnknownOwner(block.owner_section_id.0.clone())
            })?;

            let expected = expected_block_index
                .entry(block.owner_section_id.clone())
                .or_insert(1);
            if block.block_index != *expected {
                return Err(NormalizedBlockMapError::InvalidBlockIndex {
                    owner_section_id: block.owner_section_id.0.clone(),
                    expected: *expected,
                    actual: block.block_index,
                });
            }
            *expected += 1;

            owner
                .validate_normalized_range(block.normalized_range)
                .map_err(|error| NormalizedBlockMapError::InvalidRange {
                    owner_section_id: block.owner_section_id.0.clone(),
                    message: error.to_string(),
                })?;
            if block.normalized_range.is_empty() {
                return Err(NormalizedBlockMapError::EmptyRange {
                    owner_section_id: block.owner_section_id.0.clone(),
                    block_index: block.block_index,
                });
            }

            if let Some(previous_end) = last_range_end.get(&block.owner_section_id).copied()
                && block.normalized_range.start() < previous_end
            {
                return Err(NormalizedBlockMapError::OverlapOrReorder {
                    owner_section_id: block.owner_section_id.0.clone(),
                    previous_end,
                    next_start: block.normalized_range.start(),
                });
            }
            last_range_end.insert(block.owner_section_id.clone(), block.normalized_range.end());
        }

        Ok(())
    }
}
