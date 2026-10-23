use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::{Document, NormalizedTextRange, SectionId};

pub const ORIGINAL_SOURCE_BINDING_MODEL_VERSION: &str = "original-source-binding/v1";
pub const ORIGINAL_SOURCE_BINDING_METADATA_KEY: &str = "original_source_binding_map";
pub const ORIGINAL_SOURCE_BINDING_VERSION_METADATA_KEY: &str = "original_source_binding_version";
pub const ORIGINAL_SOURCE_BINDING_COUNT_METADATA_KEY: &str = "original_source_bindings";

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum OriginalSourceTarget {
    Page { page_number: u32 },
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct OriginalSourceBinding {
    pub owner_section_id: SectionId,
    pub normalized_range: NormalizedTextRange,
    pub target: OriginalSourceTarget,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct OriginalSourceBindingMap {
    pub schema_version: String,
    pub bindings: Vec<OriginalSourceBinding>,
}

impl OriginalSourceBindingMap {
    pub fn new(bindings: Vec<OriginalSourceBinding>) -> Self {
        Self {
            schema_version: ORIGINAL_SOURCE_BINDING_MODEL_VERSION.into(),
            bindings,
        }
    }
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum OriginalSourceBindingError {
    #[error("unsupported original source binding version {0:?}")]
    UnsupportedVersion(String),
    #[error("invalid original source binding JSON: {0}")]
    InvalidJson(String),
    #[error("original source binding owner Section {0:?} does not exist")]
    UnknownOwner(String),
    #[error("original source binding range for Section {owner_section_id:?} is invalid: {message}")]
    InvalidRange {
        owner_section_id: String,
        message: String,
    },
    #[error(
        "original source binding for Section {owner_section_id:?} has an empty normalized range"
    )]
    EmptyRange { owner_section_id: String },
    #[error("original source page numbers are 1-based")]
    InvalidPage,
    #[error("original source bindings in Section {owner_section_id:?} overlap or reorder")]
    OverlapOrReorder { owner_section_id: String },
    #[error("target locator range spans more than one original source location")]
    AmbiguousTarget,
}

impl Document {
    pub fn original_source_binding_map(
        &self,
    ) -> Result<Option<OriginalSourceBindingMap>, OriginalSourceBindingError> {
        let Some(json) = self.metadata.get(ORIGINAL_SOURCE_BINDING_METADATA_KEY) else {
            return Ok(None);
        };
        let map = serde_json::from_str::<OriginalSourceBindingMap>(json)
            .map_err(|error| OriginalSourceBindingError::InvalidJson(error.to_string()))?;
        self.validate_original_source_binding_map(&map)?;
        Ok(Some(map))
    }

    pub fn set_original_source_binding_map(
        &mut self,
        map: OriginalSourceBindingMap,
    ) -> Result<(), OriginalSourceBindingError> {
        self.validate_original_source_binding_map(&map)?;
        let json = crate::json::to_string_exact(&map)
            .map_err(|error| OriginalSourceBindingError::InvalidJson(error.to_string()))?;
        self.metadata.insert(
            ORIGINAL_SOURCE_BINDING_VERSION_METADATA_KEY.into(),
            map.schema_version.clone(),
        );
        self.metadata.insert(
            ORIGINAL_SOURCE_BINDING_COUNT_METADATA_KEY.into(),
            map.bindings.len().to_string(),
        );
        self.metadata
            .insert(ORIGINAL_SOURCE_BINDING_METADATA_KEY.into(), json);
        Ok(())
    }

    pub fn original_source_target_for_range(
        &self,
        owner_section_id: &SectionId,
        range: NormalizedTextRange,
    ) -> Result<Option<OriginalSourceTarget>, OriginalSourceBindingError> {
        let Some(map) = self.original_source_binding_map()? else {
            return Ok(None);
        };
        map.target_for_range(owner_section_id, range)
    }

    pub fn validate_original_source_binding_map(
        &self,
        map: &OriginalSourceBindingMap,
    ) -> Result<(), OriginalSourceBindingError> {
        // The owner's length is its text's scalar count, exactly what the range check needs.
        map.validate_against_section_lengths(|section_id| {
            self.find_section(section_id)
                .map(|section| section.normalized_text_len())
        })
    }
}

impl OriginalSourceBindingMap {
    /// The binding of `owner_section_id` that contains `range`, or the error a lookup reports.
    pub fn target_for_range(
        &self,
        owner_section_id: &SectionId,
        range: NormalizedTextRange,
    ) -> Result<Option<OriginalSourceTarget>, OriginalSourceBindingError> {
        let mut containing = None;
        let mut overlap_count = 0usize;
        for binding in self
            .bindings
            .iter()
            .filter(|binding| &binding.owner_section_id == owner_section_id)
        {
            if ranges_overlap(binding.normalized_range, range) {
                overlap_count += 1;
            }
            if binding.normalized_range.start() <= range.start()
                && range.end() <= binding.normalized_range.end()
            {
                containing = Some(binding.target.clone());
            }
        }

        if overlap_count > 1 || (overlap_count == 1 && containing.is_none()) {
            return Err(OriginalSourceBindingError::AmbiguousTarget);
        }
        Ok(containing)
    }

    /// Validates the map against the scalar length of each owner section, as
    /// [`Document::validate_original_source_binding_map`] does for a document that carries its own
    /// section text. A reader that holds only the light document supplies the stored lengths.
    pub fn validate_against_section_lengths(
        &self,
        length_of: impl Fn(&SectionId) -> Option<usize>,
    ) -> Result<(), OriginalSourceBindingError> {
        if self.schema_version != ORIGINAL_SOURCE_BINDING_MODEL_VERSION {
            return Err(OriginalSourceBindingError::UnsupportedVersion(
                self.schema_version.clone(),
            ));
        }

        let mut last_range_end = HashMap::<SectionId, usize>::new();
        for binding in &self.bindings {
            let owner_len = length_of(&binding.owner_section_id).ok_or_else(|| {
                OriginalSourceBindingError::UnknownOwner(binding.owner_section_id.0.clone())
            })?;
            binding
                .normalized_range
                .validate_for_text_len(owner_len)
                .map_err(|error| OriginalSourceBindingError::InvalidRange {
                    owner_section_id: binding.owner_section_id.0.clone(),
                    message: error.to_string(),
                })?;
            if binding.normalized_range.is_empty() {
                return Err(OriginalSourceBindingError::EmptyRange {
                    owner_section_id: binding.owner_section_id.0.clone(),
                });
            }
            if matches!(
                binding.target,
                OriginalSourceTarget::Page { page_number: 0 }
            ) {
                return Err(OriginalSourceBindingError::InvalidPage);
            }
            if let Some(previous_end) = last_range_end.get(&binding.owner_section_id).copied()
                && binding.normalized_range.start() < previous_end
            {
                return Err(OriginalSourceBindingError::OverlapOrReorder {
                    owner_section_id: binding.owner_section_id.0.clone(),
                });
            }
            last_range_end.insert(
                binding.owner_section_id.clone(),
                binding.normalized_range.end(),
            );
        }
        Ok(())
    }
}

fn ranges_overlap(left: NormalizedTextRange, right: NormalizedTextRange) -> bool {
    left.start() < right.end() && right.start() < left.end()
}
