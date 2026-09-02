use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::contracts::TextLocatorDto;

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SourceViewRepresentationDto {
    #[default]
    Original,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct GetSourceViewRequest {
    pub document_id: String,
    pub target_locator: TextLocatorDto,
    #[serde(default)]
    pub representation: SourceViewRepresentationDto,
    #[serde(default)]
    pub dpi: Option<u32>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct GetSourceViewResponse {
    pub document_id: String,
    pub source: String,
    pub content_hash: String,
    pub normalized_document_hash: String,
    pub normalized_document_hash_version: String,
    pub source_binding_version: String,
    pub representation: SourceViewRepresentationDto,
    pub page_number: u32,
    pub page_count: usize,
    pub dpi: u32,
    pub image_media_type: String,
    pub image_width: u32,
    pub image_height: u32,
    pub image_bytes: usize,
    pub target_locator: TextLocatorDto,
}
