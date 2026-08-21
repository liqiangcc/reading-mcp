use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct OpenDocumentRequest {
    pub source: String,
    #[serde(default)]
    pub auth_profile: Option<String>,
    #[serde(default)]
    pub force_refresh: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct OpenDocumentResponse {
    pub document_id: String,
    pub source: String,
    pub title: String,
    pub media_type: String,
    pub content_hash: String,
    pub section_count: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct ListDocumentsRequest {
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default = "default_recursive")]
    pub recursive: bool,
    #[serde(default = "default_list_limit")]
    pub max_results: usize,
}

fn default_recursive() -> bool {
    true
}

fn default_list_limit() -> usize {
    100
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct ListDocumentsResponse {
    pub documents: Vec<ListedDocumentDto>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct ListedDocumentDto {
    pub path: String,
    pub name: String,
    pub media_type: String,
    pub size_bytes: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct GetDocumentStructureRequest {
    pub document_id: String,
    #[serde(default)]
    pub max_depth: Option<u8>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct GetDocumentStructureResponse {
    pub document_id: String,
    pub sections: Vec<SectionNode>,
    pub truncated: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct SectionNode {
    pub section_id: String,
    #[serde(default)]
    pub parent_id: Option<String>,
    pub title: String,
    pub level: u8,
    pub location: LocationDto,
    pub children: Vec<SectionNode>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct SearchDocumentRequest {
    pub document_id: String,
    pub query: String,
    #[serde(default = "default_search_limit")]
    pub limit: usize,
}

fn default_search_limit() -> usize {
    10
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct SearchDocumentResponse {
    pub document_id: String,
    pub hits: Vec<SearchHitDto>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct SearchHitDto {
    pub section_id: String,
    pub title: String,
    pub source: String,
    pub snippet: String,
    pub score: f32,
    pub location: LocationDto,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct ReadDocumentRequest {
    pub document_id: String,
    pub section_id: String,
    #[serde(default)]
    pub max_chars: Option<usize>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct ReadDocumentResponse {
    pub document_id: String,
    pub source: String,
    pub section_id: String,
    pub content: String,
    pub location: LocationDto,
    pub truncated: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct GetContextRequest {
    pub document_id: String,
    pub section_id: String,
    #[serde(default = "default_context_window")]
    pub before: usize,
    #[serde(default = "default_context_window")]
    pub after: usize,
    #[serde(default)]
    pub max_chars: Option<usize>,
}

fn default_context_window() -> usize {
    1
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct GetContextResponse {
    pub document_id: String,
    pub source: String,
    pub owner_section_id: String,
    pub content: String,
    pub location: LocationDto,
    pub truncated: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct LocationDto {
    #[serde(default)]
    pub page: Option<u32>,
    #[serde(default)]
    pub chapter: Option<String>,
    #[serde(default)]
    pub section_path: Vec<String>,
    #[serde(default)]
    pub anchor: Option<String>,
    #[serde(default)]
    pub paragraph: Option<u32>,
    #[serde(default)]
    pub char_start: Option<usize>,
    #[serde(default)]
    pub char_end: Option<usize>,
    #[serde(default)]
    pub native_location: Option<String>,
}
