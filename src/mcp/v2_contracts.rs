//! The v2 `open` and `read` wire contracts.
//!
//! Both tools answer with one compact JSON text in `content` and never with `structuredContent` or
//! an `outputSchema`, so the whole payload is exactly the bytes the model reads.

use rmcp::model::{CallToolResult, ContentBlock};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::application::open_reading::OpenReadingResult;
use crate::application::read_reading::{ReadReadingItem, ReadReadingResult};
use crate::application::reading_error::{ReadingError, ReadingErrorCode};

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
    /// Handle, or an `r:`/`b:` cursor.
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

/// The compact `read` answer.
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
        ReadingErrorCode::BlockedSource => {
            "该来源被安全策略拒绝：只允许已授权的本地目录和公开 HTTPS 地址。".to_owned()
        }
        ReadingErrorCode::FetchFailed => format!(
            "获取或解析来源失败，请等待 {} 秒后用相同参数重试：{}",
            FETCH_RETRY_WAIT_SECONDS, error.detail
        ),
        ReadingErrorCode::OcrRequired => {
            "该来源需要本地 OCR 才能读取，请启用 OCR 后重新 open。".to_owned()
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
            "from 必须是文档/节/段落/句子句柄（d<key>~<chk>[.s<key>[.p<n>[.<n>]]]）或游标（r:/b:<section>.<S|P>.<pos>）。{}",
            error.detail
        ),
        Some("unit") => "unit 只能是 sentence 或 paragraph。".to_owned(),
        Some("direction") => {
            "direction 只能是 forward 或 backward，且必须与游标前缀一致（r: 为 forward，b: 为 backward）。"
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

/// The `retry` object for a `fetch_failed` answer: the same call after a short wait.
pub fn fetch_retry() -> Option<Value> {
    Some(json!({ "wait_seconds": FETCH_RETRY_WAIT_SECONDS }))
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
    use super::{ErrorView, OpenView, ReadView, SectionView, item_value, too_long_retry};
    use crate::application::open_reading::OpenReadingResult;
    use crate::application::read_reading::{
        ReadReadingItem, ReadReadingResult, ReadReadingSection,
    };
    use crate::application::reading_error::ReadingError;

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
            next: Some("r:d3~9f2c.s13.S.1".into()),
            anchor: None,
        });
        assert_eq!(
            compact(&view),
            r#"{"items":[["d3~9f2c.s12.p4.2","…text…"],["d3~9f2c.s12.p5","…table text…","table"]],"sections":[{"at":1,"s":"d3~9f2c.s13","title":"本纪第十三"}],"next":"r:d3~9f2c.s13.S.1"}"#
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
}
