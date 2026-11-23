use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct TextLocatorDto {
    pub document_id: String,
    pub content_hash: String,
    pub normalized_document_hash: String,
    pub owner_section_id: String,
    pub section_path: Vec<String>,
    #[serde(default)]
    pub paragraph_index: Option<usize>,
    #[serde(default)]
    pub sentence_index: Option<usize>,
    #[serde(default)]
    pub normalized_range: Option<NormalizedRangeDto>,
    #[serde(default)]
    pub segmentation_version: Option<String>,
    #[serde(default)]
    pub native_location: Option<String>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct NormalizedRangeDto {
    pub start: usize,
    pub end: usize,
}
