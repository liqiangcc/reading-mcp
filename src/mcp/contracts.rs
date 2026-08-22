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
    pub normalized_document_hash: String,
    pub normalized_document_hash_version: String,
    pub normalization_version: String,
    pub normalized_text_coordinate_space: String,
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

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TextUnitKindDto {
    Paragraph,
    #[default]
    Sentence,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TextUnitDirectionDto {
    #[default]
    Forward,
    Backward,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TextUnitCoveragePolicyDto {
    #[default]
    PreserveSource,
    EligibleOnly,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TextUnitContentClassDto {
    Unknown,
    NonProse,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct GetTextUnitsRequest {
    pub document_id: String,
    pub section_id: String,
    #[serde(default)]
    pub requested_kind: TextUnitKindDto,
    #[serde(default)]
    pub direction: TextUnitDirectionDto,
    #[serde(default)]
    pub coverage_policy: TextUnitCoveragePolicyDto,
    #[serde(default = "default_text_unit_max_items")]
    pub max_items: usize,
    #[serde(default)]
    pub max_chars: Option<usize>,
    #[serde(default)]
    pub cursor: Option<String>,
}

fn default_text_unit_max_items() -> usize {
    32
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct GetTextUnitsResponse {
    pub document_id: String,
    pub target_section_locator: TextLocatorDto,
    pub requested_kind: TextUnitKindDto,
    pub direction: TextUnitDirectionDto,
    pub coverage_policy: TextUnitCoveragePolicyDto,
    pub items: Vec<TextUnitItemDto>,
    pub complete: bool,
    pub section_complete: bool,
    #[serde(default)]
    pub next_cursor: Option<String>,
    pub coverage: TextUnitCoverageDto,
    pub stream: TextUnitStreamSegmentDto,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct TextUnitItemDto {
    pub text: String,
    pub locator: TextLocatorDto,
    pub effective_kind: TextUnitKindDto,
    pub content_class: TextUnitContentClassDto,
    pub content_class_detail: String,
    #[serde(default)]
    pub degradation: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct TextUnitCoverageDto {
    pub owner_chars: usize,
    pub section_separator_chars: usize,
    pub sentence_separator_chars: usize,
    pub paragraph_count: usize,
    pub sentence_eligible_paragraphs: usize,
    pub non_prose_paragraphs: usize,
    pub represented_paragraphs: usize,
    pub represented_sentences: usize,
    pub coarse_non_prose_items: usize,
    pub intentionally_skipped: usize,
    pub unsupported_gaps: usize,
    pub source_complete: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct TextUnitStreamSegmentDto {
    pub direction: TextUnitDirectionDto,
    pub start_index: usize,
    pub end_index: usize,
    pub total_items: usize,
}

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

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SearchCandidateKindDto {
    Section,
    Paragraph,
    Sentence,
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
    pub candidate_kind: SearchCandidateKindDto,
    pub text_locator: TextLocatorDto,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct ReadDocumentRequest {
    pub document_id: String,
    #[serde(default)]
    pub section_id: Option<String>,
    #[serde(default)]
    pub target_locator: Option<TextLocatorDto>,
    #[serde(default)]
    pub max_chars: Option<usize>,
    #[serde(default)]
    pub cursor: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct ReadDocumentResponse {
    pub document_id: String,
    pub source: String,
    pub section_id: String,
    pub content: String,
    pub location: LocationDto,
    pub truncated: bool,
    pub complete: bool,
    #[serde(default)]
    pub next_cursor: Option<String>,
    pub stream: ReadStreamSegmentDto,
    pub resolved_target_locator: TextLocatorDto,
    #[serde(default)]
    pub returned_locator: Option<TextLocatorDto>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct ReadStreamSegmentDto {
    pub read_mode: String,
    pub rendering_version: String,
    pub coordinate_space: String,
    pub start_char: usize,
    pub end_char: usize,
    pub total_chars: usize,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ContextUnitDto {
    Section,
    Paragraph,
    Sentence,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ContextContainerKindDto {
    Paragraph,
    Section,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum StructuralContextKindDto {
    OwnerSection,
    Ancestors,
    Siblings,
    Children,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContextRelationDto {
    Neighbor {
        unit: ContextUnitDto,
        #[serde(default = "default_context_window")]
        before: usize,
        #[serde(default = "default_context_window")]
        after: usize,
    },
    Container {
        kind: ContextContainerKindDto,
    },
    Structural {
        kind: StructuralContextKindDto,
    },
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ContextItemRoleDto {
    Before,
    Anchor,
    After,
    Container,
    Structural,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ContextItemKindDto {
    Section,
    Paragraph,
    Sentence,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct ContextItemDto {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub content: Option<String>,
    pub locator: TextLocatorDto,
    pub role: ContextItemRoleDto,
    pub effective_kind: ContextItemKindDto,
    #[serde(default)]
    pub content_class: Option<String>,
    #[serde(default)]
    pub degradation: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct GetContextRequest {
    pub document_id: String,
    #[serde(default)]
    pub section_id: Option<String>,
    #[serde(default)]
    pub target_locator: Option<TextLocatorDto>,
    #[serde(default)]
    pub relation: Option<ContextRelationDto>,
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
    pub complete: bool,
    pub anchor_locator: TextLocatorDto,
    pub relation: ContextRelationDto,
    pub items: Vec<ContextItemDto>,
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
