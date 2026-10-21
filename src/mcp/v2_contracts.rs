//! The v2 tool wire contracts: `open`, `read`, `browse`, `outline`, `search`, `cite` and
//! `source_view`.
//!
//! Every v2 tool answers with one compact JSON text in `content` and never with
//! `structuredContent` or an `outputSchema`, so the whole payload is exactly the bytes the model
//! reads. `cite` is the only tool that returns long fields on purpose.

use rmcp::model::{CallToolResult, ContentBlock};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use super::contracts::TextLocatorDto;
use crate::application::cite_reading::{CITE_VERSIONS, CiteReadingItem, CiteReadingResult};
use crate::application::open_reading::OpenReadingResult;
use crate::application::outline_reading::{
    DEFAULT_OUTLINE_DEPTH, DEFAULT_OUTLINE_LIMIT, OutlineReadingResult,
};
use crate::application::read_reading::{ReadReadingItem, ReadReadingResult};
use crate::application::reading_error::{ReadingError, ReadingErrorCode};
use crate::application::search_reading::{DEFAULT_SEARCH_LIMIT, SearchReadingResult};
use crate::domain::TextLocator;

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct OpenRequest {
    /// Local path or public HTTPS URL.
    pub source: String,
    /// Re-ingest even when the bytes are cached.
    #[serde(default)]
    pub refresh: Option<bool>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct ReadRequest {
    /// Handle, or a `b:` cursor.
    pub from: String,
    /// sentence (default) or paragraph.
    #[serde(default)]
    pub unit: Option<String>,
    /// 1-16000 characters; omit for one item.
    #[serde(default)]
    pub max_chars: Option<usize>,
    /// forward (default) or backward.
    #[serde(default)]
    pub direction: Option<String>,
    /// Preceding items to re-read first, 0-50.
    #[serde(default)]
    pub back: Option<usize>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct BrowseRequest {
    /// An authorized directory to list; omit to list the configured roots' documents.
    #[serde(default)]
    pub path: Option<String>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct OutlineRequest {
    /// A document or section handle.
    pub at: String,
    /// Levels below `at` to list, 1-32.
    #[serde(default = "default_outline_depth")]
    pub depth: usize,
    /// Nodes per answer, 1-1000.
    #[serde(default = "default_outline_limit")]
    pub limit: usize,
    /// The `next` of a previous outline answer.
    #[serde(default)]
    pub cursor: Option<String>,
}

fn default_outline_depth() -> usize {
    DEFAULT_OUTLINE_DEPTH
}

fn default_outline_limit() -> usize {
    DEFAULT_OUTLINE_LIMIT
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct SearchRequest {
    /// A document handle.
    pub doc: String,
    pub query: String,
    /// Hits to return, 1-50.
    #[serde(default = "default_search_reading_limit")]
    pub limit: usize,
}

fn default_search_reading_limit() -> usize {
    DEFAULT_SEARCH_LIMIT
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct CiteRequest {
    /// One or more document, section, paragraph or sentence handles.
    pub at: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct SourceViewRequest {
    /// A document, section, paragraph or sentence handle.
    pub at: String,
}

/// The compact `open` answer.
#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct OpenView {
    pub doc: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub format: &'static str,
    pub sections: usize,
    #[serde(skip_serializing_if = "is_zero")]
    pub chars: usize,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub top: Vec<(String, String)>,
    #[serde(skip_serializing_if = "is_zero")]
    pub more_top: usize,
}

impl From<OpenReadingResult> for OpenView {
    fn from(result: OpenReadingResult) -> Self {
        Self {
            doc: result.handle,
            title: result.title,
            format: result.format,
            sections: result.sections,
            chars: result.chars,
            top: result.top,
            more_top: result.more_top,
        }
    }
}

/// The compact `read` answer for one item: the item's text, its next handle, and what a caller
/// cannot see in the item alone.
#[derive(Debug, Serialize, PartialEq)]
pub struct FlatReadView {
    /// The returned item's handle, only when it is not byte-identical to `from`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub at: Option<String>,
    /// The section, only when the item is position 0 of its section stream.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub section: Option<SectionRef>,
    /// The lowercase snake-case content class of a coarse or non-prose item.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next: Option<String>,
}

/// A section named without a position, for the single-item answer.
#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct SectionRef {
    pub s: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub title: String,
}

impl FlatReadView {
    pub fn new(result: ReadReadingResult, from: &str) -> Self {
        let section = result
            .sections
            .into_iter()
            .next()
            .map(|section| SectionRef {
                s: section.handle,
                title: section.title,
            });
        let item = result.items.into_iter().next();
        let (at, kind, text) = match item {
            Some(item) => (
                (item.handle != from).then_some(item.handle),
                item.content_class,
                Some(item.text),
            ),
            None => (None, None, None),
        };
        Self {
            at,
            section,
            kind,
            text,
            next: result.next,
        }
    }
}

/// The compact `read` answer for a batch.
#[derive(Debug, Serialize, PartialEq)]
pub struct ReadView {
    pub items: Vec<Value>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub sections: Vec<SectionView>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub anchor: Option<usize>,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct SectionView {
    pub at: usize,
    pub s: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub title: String,
}

impl From<ReadReadingResult> for ReadView {
    fn from(result: ReadReadingResult) -> Self {
        Self {
            items: result.items.into_iter().map(item_value).collect(),
            sections: result
                .sections
                .into_iter()
                .map(|section| SectionView {
                    at: section.at,
                    s: section.handle,
                    title: section.title,
                })
                .collect(),
            next: result.next,
            anchor: result.anchor,
        }
    }
}

fn item_value(item: ReadReadingItem) -> Value {
    let mut parts = vec![Value::String(item.handle), Value::String(item.text)];
    if let Some(content_class) = item.content_class {
        parts.push(Value::String(content_class));
    }
    Value::Array(parts)
}

/// The compact `outline` answer: `[[handle, title, level, chars], ...]` and the next cursor.
#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct OutlineView {
    pub items: Vec<(String, String, u8, usize)>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next: Option<String>,
}

impl From<OutlineReadingResult> for OutlineView {
    fn from(result: OutlineReadingResult) -> Self {
        Self {
            items: result
                .items
                .into_iter()
                .map(|item| (item.handle, item.title, item.level, item.chars))
                .collect(),
            next: result.next,
        }
    }
}

/// The compact `search` answer: the hit count, the sections the hits live in, and the hits.
#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct SearchView {
    pub total: usize,
    #[serde(skip_serializing_if = "Map::is_empty")]
    pub sections: Map<String, Value>,
    pub hits: Vec<(String, String)>,
}

impl From<SearchReadingResult> for SearchView {
    fn from(result: SearchReadingResult) -> Self {
        let mut sections = Map::new();
        for (handle, title) in result.sections {
            sections.insert(handle, Value::String(title));
        }
        Self {
            total: result.total,
            sections,
            hits: result
                .hits
                .into_iter()
                .map(|hit| (hit.handle, hit.snippet))
                .collect(),
        }
    }
}

/// The compact `cite` answer: one citation per requested handle.
#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct CiteView {
    pub items: Vec<Value>,
}

impl From<CiteReadingResult> for CiteView {
    fn from(result: CiteReadingResult) -> Self {
        Self {
            items: result.items.into_iter().map(cite_value).collect(),
        }
    }
}

fn cite_value(item: CiteReadingItem) -> Value {
    let (normalization_version, normalized_document_hash_version, segmentation_version) =
        CITE_VERSIONS;
    let mut citation = Map::new();
    citation.insert("at".into(), Value::String(item.at));
    if let Some(locator) = item.locator {
        citation.insert("locator".into(), locator_value(&locator));
    }
    if let Some(source_position) = item.source_position {
        citation.insert("src".into(), Value::String(source_position));
    }
    if let Some(page) = item.page {
        citation.insert("page".into(), json!(page));
    }
    let mut identity = Map::new();
    identity.insert("id".into(), Value::String(item.document_id));
    identity.insert("content_hash".into(), Value::String(item.content_hash));
    identity.insert(
        "normalized_document_hash".into(),
        Value::String(item.normalized_document_hash),
    );
    identity.insert("norm".into(), Value::String(normalization_version.into()));
    identity.insert(
        "ndh".into(),
        Value::String(normalized_document_hash_version.into()),
    );
    identity.insert("seg".into(), Value::String(segmentation_version.into()));
    citation.insert("doc".into(), Value::Object(identity));
    Value::Object(citation)
}

fn locator_value(locator: &TextLocator) -> Value {
    let dto = TextLocatorDto {
        document_id: locator.document_id.0.clone(),
        content_hash: locator.content_hash.0.clone(),
        normalized_document_hash: locator.normalized_document_hash.0.clone(),
        owner_section_id: locator.owner_section_id.0.clone(),
        section_path: locator.section_path.clone(),
        paragraph_index: locator.paragraph_index,
        sentence_index: locator.sentence_index,
        normalized_range: locator.normalized_range.map(|range| {
            super::contracts::NormalizedRangeDto {
                start: range.start(),
                end: range.end(),
            }
        }),
        segmentation_version: locator.segmentation_version.clone(),
        native_location: locator.native_location.clone(),
    };
    serde_json::to_value(dto).unwrap_or(Value::Null)
}

/// The v2 error body: one code, one short Chinese hint, and a retry only when it is exactly right.
#[derive(Debug, Serialize, PartialEq)]
pub struct ErrorView {
    pub error: &'static str,
    pub hint: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retry: Option<Value>,
}

impl ErrorView {
    /// `retry` is the tool arguments to repeat the call with, when the tool layer can build them.
    pub fn new(error: &ReadingError, retry: Option<Value>) -> Self {
        Self {
            error: error.code.as_str(),
            hint: hint(error),
            retry,
        }
    }
}

fn hint(error: &ReadingError) -> String {
    match error.code {
        ReadingErrorCode::NotOpened => {
            "该文档尚未在本服务中打开，请先用 open 打开文件。".to_owned()
        }
        ReadingErrorCode::UnknownHandle => {
            "句柄指向的内容不存在，请用 open 重新打开文件后使用新返回的句柄。".to_owned()
        }
        ReadingErrorCode::StaleHandle => {
            "句柄已过期（文档内容或分段版本已变化），请用 open 重新打开文件。".to_owned()
        }
        ReadingErrorCode::TooLong => too_long_hint(error),
        ReadingErrorCode::InvalidArgument => invalid_argument_hint(error),
        ReadingErrorCode::TooLarge => {
            "来源超出资源上限（文件或 OCR 输入过大），请换一个更小的来源。".to_owned()
        }
        ReadingErrorCode::BlockedSource => {
            "该来源被安全策略拒绝：只允许已授权的本地目录和公开 HTTPS 地址。".to_owned()
        }
        ReadingErrorCode::FetchFailed => format!(
            "获取来源失败，请等待 {} 秒后用相同参数重试。",
            FETCH_RETRY_WAIT_SECONDS
        ),
        ReadingErrorCode::OcrRequired => {
            "该来源需要本地 OCR 才能读取，请启用 OCR 后重新 open。".to_owned()
        }
        ReadingErrorCode::Unreadable => {
            "来源无法解析或文本编码不受支持，请确认文件格式与编码后重新 open。".to_owned()
        }
        ReadingErrorCode::Internal => {
            "服务内部错误（存储或索引失败），请稍后重试；若持续失败请检查服务状态。".to_owned()
        }
    }
}

/// How long a `fetch_failed` hint asks the caller to wait.
pub const FETCH_RETRY_WAIT_SECONDS: u64 = 30;

fn too_long_hint(error: &ReadingError) -> String {
    let Some(chars) = error.chars else {
        return format!(
            "请求超出资源上限，请减小 max_chars 或改用 unit=sentence：{}",
            error.detail
        );
    };
    if chars <= crate::application::read_reading::MAX_BATCH_CHARS {
        format!("首项有 {chars} 个字符，超过 max_chars；已按该字符数给出重试参数。")
    } else {
        format!("首项有 {chars} 个字符，超过 16000 上限；请改用 unit=sentence 逐句读取。")
    }
}

fn invalid_argument_hint(error: &ReadingError) -> String {
    match error.field {
        Some("from") => format!(
            "from 必须是文档/节/段落/句子句柄（d<key>~<chk>[.s<key>[.p<n>[.<n>]]]）或后向游标（b:<section>.<S|P>.<pos>）。{}",
            error.detail
        ),
        Some("unit") => "unit 只能是 sentence 或 paragraph。".to_owned(),
        Some("direction") => {
            "direction 只能是 forward 或 backward，且必须与游标前缀一致（b: 为 backward）。"
                .to_owned()
        }
        Some("max_chars") => "max_chars 必须是 1 到 16000 之间的整数。".to_owned(),
        Some("back") => "back 必须是 0 到 50 之间的整数。".to_owned(),
        _ => format!("请求参数无效：{}", error.detail),
    }
}

/// The `retry` object for a `too_long` answer: the same call with the item's own char count.
pub fn too_long_retry(request: &ReadRequest, error: &ReadingError) -> Option<Value> {
    let chars = error.chars?;
    if chars > crate::application::read_reading::MAX_BATCH_CHARS {
        return None;
    }
    let mut retry = Map::new();
    retry.insert("from".into(), Value::String(request.from.clone()));
    retry.insert("max_chars".into(), json!(chars));
    if let Some(unit) = &request.unit {
        retry.insert("unit".into(), Value::String(unit.clone()));
    }
    if let Some(direction) = &request.direction {
        retry.insert("direction".into(), Value::String(direction.clone()));
    }
    if let Some(back) = request.back.filter(|back| *back > 0) {
        retry.insert("back".into(), json!(back));
    }
    Some(Value::Object(retry))
}

/// The `retry` object of a v2 error, or `None` when repeating the call would not be exactly right.
pub fn retry_for(error: &ReadingError) -> Option<Value> {
    match error.code {
        ReadingErrorCode::FetchFailed => Some(json!({ "wait_seconds": FETCH_RETRY_WAIT_SECONDS })),
        // `too_long` carries its own retry, built from the request by `too_long_retry`.
        ReadingErrorCode::NotOpened
        | ReadingErrorCode::UnknownHandle
        | ReadingErrorCode::StaleHandle
        | ReadingErrorCode::TooLong
        | ReadingErrorCode::InvalidArgument
        | ReadingErrorCode::TooLarge
        | ReadingErrorCode::BlockedSource
        | ReadingErrorCode::OcrRequired
        | ReadingErrorCode::Unreadable
        | ReadingErrorCode::Internal => None,
    }
}

fn is_zero(value: &usize) -> bool {
    *value == 0
}

/// One tool answer: the compact JSON text, and nothing else.
pub fn text_result<T: Serialize>(value: &T) -> CallToolResult {
    let text = serde_json::to_string(value).unwrap_or_else(|_| "{}".to_owned());
    CallToolResult::success(vec![ContentBlock::text(text)])
}

/// One v2 error answer: `isError`, with the compact JSON body as the only text.
pub fn error_result(error: &ReadingError, retry: Option<Value>) -> CallToolResult {
    CallToolResult::error(vec![ContentBlock::text(
        serde_json::to_string(&ErrorView::new(error, retry)).unwrap_or_else(|_| "{}".to_owned()),
    )])
}

#[cfg(test)]
mod tests {
    use super::{
        CiteView, ErrorView, FlatReadView, OpenView, OutlineView, ReadView, SearchView,
        SectionView, item_value, retry_for, too_long_retry,
    };
    use crate::application::cite_reading::{CiteReadingItem, CiteReadingResult};
    use crate::application::open_reading::OpenReadingResult;
    use crate::application::outline_reading::{OutlineReadingItem, OutlineReadingResult};
    use crate::application::ports::ApplicationError;
    use crate::application::read_reading::{
        ReadReadingItem, ReadReadingResult, ReadReadingSection,
    };
    use crate::application::reading_error::ReadingError;
    use crate::application::search_reading::{SearchReadingHit, SearchReadingResult};

    fn compact<T: serde::Serialize>(value: &T) -> String {
        serde_json::to_string(value).expect("the view serializes")
    }

    #[test]
    fn open_view_omits_empty_and_default_fields() {
        let view = OpenView::from(OpenReadingResult {
            handle: "d3~9f2c".into(),
            title: None,
            format: "epub",
            sections: 543,
            chars: 0,
            top: Vec::new(),
            more_top: 0,
        });
        assert_eq!(
            compact(&view),
            r#"{"doc":"d3~9f2c","format":"epub","sections":543}"#
        );
    }

    #[test]
    fn open_view_matches_the_contract_example_shape() {
        let view = OpenView::from(OpenReadingResult {
            handle: "d3~9f2c".into(),
            title: Some("宋史".into()),
            format: "epub",
            sections: 543,
            chars: 5_210_000,
            top: vec![("d3~9f2c.s0".into(), "本纪第一".into())],
            more_top: 45,
        });
        assert_eq!(
            compact(&view),
            r#"{"doc":"d3~9f2c","title":"宋史","format":"epub","sections":543,"chars":5210000,"top":[["d3~9f2c.s0","本纪第一"]],"more_top":45}"#
        );
    }

    #[test]
    fn read_view_carries_handles_text_and_optional_class() {
        let view = ReadView::from(ReadReadingResult {
            items: vec![
                ReadReadingItem {
                    handle: "d3~9f2c.s12.p4.2".into(),
                    text: "…text…".into(),
                    content_class: None,
                },
                ReadReadingItem {
                    handle: "d3~9f2c.s12.p5".into(),
                    text: "…table text…".into(),
                    content_class: Some("table".into()),
                },
            ],
            sections: vec![ReadReadingSection {
                at: 1,
                handle: "d3~9f2c.s13".into(),
                title: "本纪第十三".into(),
            }],
            next: Some("d3~9f2c.s13.p1.1".into()),
            anchor: None,
        });
        assert_eq!(
            compact(&view),
            r#"{"items":[["d3~9f2c.s12.p4.2","…text…"],["d3~9f2c.s12.p5","…table text…","table"]],"sections":[{"at":1,"s":"d3~9f2c.s13","title":"本纪第十三"}],"next":"d3~9f2c.s13.p1.1"}"#
        );
    }

    #[test]
    fn read_view_omits_absent_fields() {
        let view = ReadView {
            items: vec![item_value(ReadReadingItem {
                handle: "d3~9f2c.s0.p1.1".into(),
                text: "a".into(),
                content_class: None,
            })],
            sections: Vec::<SectionView>::new(),
            next: None,
            anchor: None,
        };
        assert_eq!(compact(&view), r#"{"items":[["d3~9f2c.s0.p1.1","a"]]}"#);
    }

    #[test]
    fn error_view_is_one_compact_object_and_retries_only_when_exact() {
        let error = ReadingError::too_long(900);
        assert_eq!(
            compact(&ErrorView::new(&error, None)),
            r#"{"error":"too_long","hint":"首项有 900 个字符，超过 max_chars；已按该字符数给出重试参数。"}"#
        );
        let huge = ReadingError::too_long(20_000);
        assert!(
            compact(&ErrorView::new(&huge, None)).contains("unit=sentence"),
            "an item above the batch limit must suggest sentence mode"
        );
        assert_eq!(
            compact(&ErrorView::new(&ReadingError::not_opened(), None)),
            r#"{"error":"not_opened","hint":"该文档尚未在本服务中打开，请先用 open 打开文件。"}"#
        );
    }

    #[test]
    fn too_long_retry_repeats_the_call_with_the_item_char_count() {
        let request = super::ReadRequest {
            from: "d3~9f2c.s1.p2".into(),
            unit: Some("sentence".into()),
            max_chars: Some(100),
            direction: None,
            back: Some(0),
        };
        let retry = too_long_retry(&request, &ReadingError::too_long(900)).expect("a retry");
        assert_eq!(
            retry,
            serde_json::json!({"from":"d3~9f2c.s1.p2","max_chars":900,"unit":"sentence"})
        );
        assert!(too_long_retry(&request, &ReadingError::too_long(20_000)).is_none());
    }

    fn item(handle: &str, text: &str, content_class: Option<&str>) -> ReadReadingItem {
        ReadReadingItem {
            handle: handle.into(),
            text: text.into(),
            content_class: content_class.map(str::to_owned),
        }
    }

    #[test]
    fn flat_view_is_one_object_in_contract_key_order() {
        let view = FlatReadView::new(
            ReadReadingResult {
                items: vec![item("d3~9f2c.s12.p4.3", "…text…", None)],
                sections: vec![ReadReadingSection {
                    at: 0,
                    handle: "d3~9f2c.s12".into(),
                    title: "本纪第十二".into(),
                }],
                next: Some("d3~9f2c.s12.p4.4".into()),
                anchor: None,
            },
            "d3~9f2c.s12",
        );
        assert_eq!(
            compact(&view),
            r#"{"at":"d3~9f2c.s12.p4.3","section":{"s":"d3~9f2c.s12","title":"本纪第十二"},"text":"…text…","next":"d3~9f2c.s12.p4.4"}"#
        );
    }

    #[test]
    fn flat_view_omits_at_when_from_is_the_items_own_handle() {
        let view = FlatReadView::new(
            ReadReadingResult {
                items: vec![item("d3~9f2c.s12.p4.3", "a", None)],
                sections: Vec::new(),
                next: None,
                anchor: None,
            },
            "d3~9f2c.s12.p4.3",
        );
        assert_eq!(compact(&view), r#"{"text":"a"}"#);
    }

    #[test]
    fn flat_view_carries_the_kind_of_a_non_prose_item() {
        let view = FlatReadView::new(
            ReadReadingResult {
                items: vec![item("d3~9f2c.s12.p5", "cell", Some("table"))],
                sections: Vec::new(),
                next: None,
                anchor: None,
            },
            "d3~9f2c.s12.p5",
        );
        assert_eq!(compact(&view), r#"{"kind":"table","text":"cell"}"#);
    }

    #[test]
    fn only_fetch_failed_carries_a_retry_of_its_own() {
        // The approved table, one row per `ApplicationError` variant: its code and whether the
        // answer carries `retry`.
        let cases: Vec<(ApplicationError, &str, bool)> = vec![
            (ApplicationError::DocumentNotFound, "not_opened", false),
            (ApplicationError::SectionNotFound, "unknown_handle", false),
            (
                ApplicationError::StaleDocument("old".into()),
                "stale_handle",
                false,
            ),
            (
                ApplicationError::StaleLocator("old".into()),
                "stale_handle",
                false,
            ),
            (
                ApplicationError::StaleStructure("old".into()),
                "stale_handle",
                false,
            ),
            (
                ApplicationError::InvalidRequest("bad".into()),
                "invalid_argument",
                false,
            ),
            (
                ApplicationError::InvalidLocator("bad".into()),
                "invalid_argument",
                false,
            ),
            (
                ApplicationError::InvalidCursor("bad".into()),
                "invalid_argument",
                false,
            ),
            (
                ApplicationError::StaleCursor("old".into()),
                "invalid_argument",
                false,
            ),
            (
                ApplicationError::CursorTargetMismatch("other".into()),
                "invalid_argument",
                false,
            ),
            (
                ApplicationError::CursorEncodingFailed("bad".into()),
                "invalid_argument",
                false,
            ),
            (
                ApplicationError::ResourceLimitExceeded("big".into()),
                "too_large",
                false,
            ),
            (ApplicationError::OcrResourceLimit, "too_large", false),
            (
                ApplicationError::BlockedSource("private".into()),
                "blocked_source",
                false,
            ),
            (
                ApplicationError::AuthenticationFailed("profile".into()),
                "blocked_source",
                false,
            ),
            (ApplicationError::OcrRequired, "ocr_required", false),
            (
                ApplicationError::OcrNoSupportedProjection,
                "ocr_required",
                false,
            ),
            (ApplicationError::OcrFailed, "ocr_required", false),
            (ApplicationError::OcrUnavailable, "ocr_required", false),
            (
                ApplicationError::RetrievalFailed("offline".into()),
                "fetch_failed",
                true,
            ),
            (ApplicationError::OcrBusy, "fetch_failed", true),
            (ApplicationError::OcrTimeout, "fetch_failed", true),
            (
                ApplicationError::ParseFailed("broken".into()),
                "unreadable",
                false,
            ),
            (
                ApplicationError::UnsupportedTextEncoding("utf-16".into()),
                "unreadable",
                false,
            ),
            (
                ApplicationError::RepositoryFailed("disk".into()),
                "internal",
                false,
            ),
            (
                ApplicationError::CacheFailed("disk".into()),
                "internal",
                false,
            ),
            (
                ApplicationError::IndexFailed("index".into()),
                "internal",
                false,
            ),
            (
                ApplicationError::TextUnitIndexFailed("sqlite".into()),
                "internal",
                false,
            ),
            (
                ApplicationError::SourceViewFailed("view".into()),
                "internal",
                false,
            ),
        ];
        for (error, code, has_retry) in cases {
            let mapped = ReadingError::from(error);
            assert_eq!(mapped.code.as_str(), code, "wrong code: {}", mapped.detail);
            assert_eq!(
                retry_for(&mapped).is_some(),
                has_retry,
                "wrong retry presence for {code}: {}",
                mapped.detail
            );
            if has_retry {
                assert_eq!(
                    retry_for(&mapped),
                    Some(serde_json::json!({ "wait_seconds": 30 }))
                );
            }
        }
        // `too_long` is built by `read` itself, and its retry needs the request.
        assert!(retry_for(&ReadingError::too_long(900)).is_none());
    }

    #[test]
    fn outline_view_is_compact_tuples_and_omits_an_absent_next() {
        let view = OutlineView::from(OutlineReadingResult {
            items: vec![OutlineReadingItem {
                handle: "d3~9f2c.s0".into(),
                title: "本纪第一".into(),
                level: 1,
                chars: 1_204,
            }],
            next: None,
        });
        assert_eq!(
            compact(&view),
            r#"{"items":[["d3~9f2c.s0","本纪第一",1,1204]]}"#
        );
    }

    #[test]
    fn search_view_omits_empty_sections_and_keeps_the_hit_pairs() {
        let view = SearchView::from(SearchReadingResult {
            total: 1,
            sections: Vec::new(),
            hits: vec![SearchReadingHit {
                handle: "d3~9f2c.s1.p2.3".into(),
                snippet: "寶玉".into(),
            }],
        });
        assert_eq!(
            compact(&view),
            r#"{"total":1,"hits":[["d3~9f2c.s1.p2.3","寶玉"]]}"#
        );
    }

    #[test]
    fn cite_view_omits_the_locator_of_a_document_handle_but_keeps_identity() {
        let view = CiteView::from(CiteReadingResult {
            items: vec![CiteReadingItem {
                at: "d3~9f2c".into(),
                locator: None,
                source_position: None,
                page: None,
                document_id: "doc:alpha".into(),
                content_hash: "sha256:raw".into(),
                normalized_document_hash: "sha256:norm".into(),
            }],
        });
        let text = compact(&view);
        assert!(
            !text.contains("locator"),
            "a document handle has no locator"
        );
        assert!(text.contains(r#""id":"doc:alpha""#));
        assert!(text.contains(r#""content_hash":"sha256:raw""#));
        assert!(text.contains(r#""normalized_document_hash":"sha256:norm""#));
    }
}
