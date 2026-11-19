//! V2-KIND (#232 A/C): a v2 `read` in sentence mode that hands back a whole paragraph says so,
//! and `cite` reports the same fact for the paragraph's handle.
//!
//! The paragraph guard of #139 stays as it is: a fallback paragraph whose text shows merge or
//! corruption evidence is still returned whole. What changes is that the answer now carries the
//! `kind` (read) and the v1 `effective_kind`/`degradation` pair (cite), so a caller that reads one
//! sentence at a time can see it got a paragraph. A normal sentence's answer is byte-identical.

use std::path::Path;

use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, CallToolResult, ContentBlock};
use rmcp::transport::TokioChildProcess;
use serde_json::{Map, Value, json};
use tokio::process::Command;

type Client = rmcp::service::RunningService<rmcp::RoleClient, ()>;

/// The masthead blob `tests/fallback_sentence_degradation.rs` proves trips the fallback guard.
const COARSE_PARAGRAPH: &str = "16 October 1964, Volume 146, Number 3642 Strong Inference Certain \
                                 systematic methods of scientific thinking may produce much more \
                                 rapid progress than others.";

const CLEAN_SENTENCES: &str = "Body prose sentence one. Body prose sentence two.";

/// The v1 degradation codes of a coarse prose paragraph and of a non-prose one.
const COARSE_ONLY: &str = "requested_sentence_but_paragraph_is_coarse_only";
const NON_PROSE: &str = "requested_sentence_but_non_prose_is_paragraph_only";

// ------------------------------------------------------------------------------------------------
// Fixtures
// ------------------------------------------------------------------------------------------------

/// A plain-text book: the guarded fallback paragraph, then two clean sentences. Plain text carries
/// no block model, so the first paragraph is `ProseOrUnknown` and the guard can fire on it.
fn coarse_prose_book() -> String {
    format!("{COARSE_PARAGRAPH}\n\n{CLEAN_SENTENCES}\n")
}

/// An HTML book: one clean paragraph and one table, both in the same section.
fn table_book() -> &'static str {
    "<html><body><h1>Table Book</h1><p>Prose one. Prose two.</p>\
     <table><tr><td>cell one</td><td>cell two</td></tr></table></body></html>"
}

// ------------------------------------------------------------------------------------------------
// Transport helpers
// ------------------------------------------------------------------------------------------------

async fn start_client(local_root: &Path, state_dir: &Path, tools: Option<&str>) -> Client {
    let local_roots = std::env::join_paths([local_root]).expect("local roots should be valid");
    let mut command = Command::new(env!("CARGO_BIN_EXE_reading-mcp"));
    command
        .env("READING_MCP_LOCAL_ROOTS", local_roots)
        .env("READING_MCP_STATE_DIR", state_dir)
        .env("READING_MCP_TELEMETRY", "false");
    if let Some(tools) = tools {
        command.env("READING_MCP_TOOLS", tools);
    } else {
        command.env_remove("READING_MCP_TOOLS");
    }
    let transport = TokioChildProcess::new(command).expect("MCP process should start");
    ().serve(transport)
        .await
        .expect("MCP initialization should succeed")
}

/// The answer's one text block, unparsed: the byte-exact JSON a caller receives.
fn raw(result: &CallToolResult) -> String {
    result
        .content
        .iter()
        .find_map(|block| match block {
            ContentBlock::Text(text) => Some(text.text.clone()),
            _ => None,
        })
        .expect("the tool answers with one text block")
}

fn payload(result: &CallToolResult) -> Value {
    serde_json::from_str(&raw(result)).expect("the tool answers with compact JSON")
}

fn arguments(value: Value) -> Map<String, Value> {
    value.as_object().expect("an object").clone()
}

async fn call(client: &Client, name: &'static str, value: Value) -> CallToolResult {
    client
        .call_tool(CallToolRequestParams::new(name).with_arguments(arguments(value)))
        .await
        .unwrap_or_else(|error| panic!("{name} should answer: {error}"))
}

async fn v2_open(client: &Client, path: &Path) -> Value {
    payload(&call(client, "open", json!({ "source": path.to_string_lossy() })).await)
}

async fn v2_read(client: &Client, from: &str, extra: Value) -> CallToolResult {
    let mut value = Map::new();
    value.insert("from".into(), json!(from));
    if let Some(extra) = extra.as_object() {
        for (key, item) in extra {
            value.insert(key.clone(), item.clone());
        }
    }
    client
        .call_tool(CallToolRequestParams::new("read").with_arguments(value))
        .await
        .expect("read should answer")
}

/// The single-item answer of a normal sentence: text and next, nothing else.
fn flat_sentence(text: &str, next: &str) -> String {
    format!(
        r#"{{"text":{},"next":{}}}"#,
        serde_json::to_string(text).expect("text serializes"),
        serde_json::to_string(next).expect("handle serializes"),
    )
}

/// The items of a batch answer as `[handle, text, kind?]`.
fn batch_items(payload: &Value) -> Vec<(String, String, Option<String>)> {
    payload["items"]
        .as_array()
        .expect("a batch read answers with items")
        .iter()
        .map(|item| {
            let parts = item.as_array().expect("an item is an array");
            (
                parts[0].as_str().expect("handle").to_owned(),
                parts[1].as_str().expect("text").to_owned(),
                parts.get(2).and_then(Value::as_str).map(str::to_owned),
            )
        })
        .collect()
}

// ------------------------------------------------------------------------------------------------
// read
// ------------------------------------------------------------------------------------------------

#[tokio::test]
async fn a_guarded_paragraph_comes_back_whole_and_marked_in_both_read_views() {
    let directory = tempfile::tempdir().expect("temporary directory should be created");
    let path = directory.path().join("coarse.txt");
    tokio::fs::write(&path, coarse_prose_book())
        .await
        .expect("fixture should be written");
    let client = start_client(
        directory.path(),
        &directory.path().join("state"),
        Some("v2"),
    )
    .await;
    let doc = v2_open(&client, &path).await["doc"]
        .as_str()
        .expect("doc handle")
        .to_owned();

    // Flat view: the guarded paragraph is still whole, and it now says it is a paragraph.
    let first = v2_read(&client, &doc, json!({})).await;
    let flat = payload(&first);
    assert_eq!(
        flat["text"], COARSE_PARAGRAPH,
        "the guard must still return the paragraph whole: {flat}"
    );
    assert_eq!(
        flat["kind"], "paragraph",
        "a whole paragraph in sentence mode must carry kind=paragraph: {flat}"
    );
    let at = flat["at"].as_str().expect("the item's own handle");
    assert_eq!(
        at.matches(".p").count(),
        1,
        "the guarded item is a paragraph handle, not a sentence handle: {at}"
    );
    assert!(
        at.rsplit_once(".p")
            .map(|(_, index)| !index.contains('.'))
            .unwrap_or(false),
        "a paragraph handle has no sentence part: {at}"
    );
    // The marker sits in the flat view's contract position, between `section` and `text`.
    let text = raw(&first);
    assert!(
        text.contains(r#""kind":"paragraph","text":"16 October 1964"#),
        "kind must precede text in the flat object: {text}"
    );

    // Batch view: the same paragraph is the third element of its item.
    let batch = payload(&v2_read(&client, &doc, json!({ "max_chars": 4000 })).await);
    let items = batch_items(&batch);
    let (handle, text, kind) = items
        .first()
        .expect("the batch must carry the guarded paragraph");
    assert_eq!(text.as_str(), COARSE_PARAGRAPH);
    assert_eq!(
        kind.as_deref(),
        Some("paragraph"),
        "the batch item's third element must be paragraph: {batch}"
    );
    assert_eq!(
        handle.as_str(),
        flat["at"].as_str().expect("the flat item's handle"),
        "both views must name the same paragraph"
    );

    client.cancel().await.expect("client should close cleanly");
}

#[tokio::test]
async fn a_normal_sentence_answer_is_byte_identical() {
    let directory = tempfile::tempdir().expect("temporary directory should be created");
    let path = directory.path().join("coarse.txt");
    tokio::fs::write(&path, coarse_prose_book())
        .await
        .expect("fixture should be written");
    let client = start_client(
        directory.path(),
        &directory.path().join("state"),
        Some("v2"),
    )
    .await;
    let doc = v2_open(&client, &path).await["doc"]
        .as_str()
        .expect("doc handle")
        .to_owned();

    // The guarded paragraph's next handle is the first clean sentence.
    let guarded = payload(&v2_read(&client, &doc, json!({})).await);
    let sentence = guarded["next"]
        .as_str()
        .expect("the next item is a sentence handle")
        .to_owned();

    let answer = v2_read(&client, &sentence, json!({})).await;
    let first = payload(&answer);
    assert_eq!(
        first["text"], "Body prose sentence one.",
        "the guarded paragraph's next item is its first clean sentence: {first}"
    );
    let next = first["next"]
        .as_str()
        .expect("the next sentence")
        .to_owned();
    assert_eq!(
        raw(&answer),
        flat_sentence("Body prose sentence one.", &next),
        "a normal sentence answer must stay exactly {{text, next}}"
    );

    // The second clean sentence is the last item: its answer carries no next at all.
    let last = v2_read(&client, &next, json!({})).await;
    assert_eq!(
        raw(&last),
        r#"{"text":"Body prose sentence two."}"#,
        "the last sentence's answer must stay exactly {{text}}"
    );

    client.cancel().await.expect("client should close cleanly");
}

#[tokio::test]
async fn paragraph_mode_on_a_guarded_paragraph_is_unchanged() {
    let directory = tempfile::tempdir().expect("temporary directory should be created");
    let path = directory.path().join("coarse.txt");
    tokio::fs::write(&path, coarse_prose_book())
        .await
        .expect("fixture should be written");
    let client = start_client(
        directory.path(),
        &directory.path().join("state"),
        Some("v2"),
    )
    .await;
    let doc = v2_open(&client, &path).await["doc"]
        .as_str()
        .expect("doc handle")
        .to_owned();
    let guarded = payload(&v2_read(&client, &doc, json!({})).await);
    let paragraph = guarded["at"]
        .as_str()
        .expect("the paragraph handle")
        .to_owned();

    // The caller asked for paragraphs, so nothing is added to the answer: the item keeps the flat
    // view's usual keys and gains no `kind`.
    let answer = v2_read(&client, &paragraph, json!({ "unit": "paragraph" })).await;
    let flat = payload(&answer);
    assert_eq!(flat["text"], COARSE_PARAGRAPH);
    assert!(
        flat.get("kind").is_none(),
        "unit=paragraph must not add a kind: {flat}"
    );
    for key in flat.as_object().expect("the flat view is an object").keys() {
        assert!(
            matches!(key.as_str(), "at" | "section" | "text" | "next"),
            "unit=paragraph must not add a key, found {key}: {flat}"
        );
    }
    assert!(
        !raw(&answer).contains(r#""kind""#),
        "unit=paragraph must not name a kind: {}",
        raw(&answer)
    );

    // A batch in paragraph mode carries two-element items only.
    let batch = payload(
        &v2_read(
            &client,
            &doc,
            json!({ "unit": "paragraph", "max_chars": 4000 }),
        )
        .await,
    );
    for item in batch["items"].as_array().expect("items") {
        assert_eq!(
            item.as_array().expect("an item is an array").len(),
            2,
            "paragraph mode must not add a kind: {batch}"
        );
    }

    client.cancel().await.expect("client should close cleanly");
}

#[tokio::test]
async fn a_table_paragraph_keeps_its_own_kind() {
    let directory = tempfile::tempdir().expect("temporary directory should be created");
    let path = directory.path().join("table.html");
    tokio::fs::write(&path, table_book())
        .await
        .expect("fixture should be written");
    let client = start_client(
        directory.path(),
        &directory.path().join("state"),
        Some("v2"),
    )
    .await;
    let doc = v2_open(&client, &path).await["doc"]
        .as_str()
        .expect("doc handle")
        .to_owned();

    let batch = payload(&v2_read(&client, &doc, json!({ "max_chars": 4000 })).await);
    let items = batch_items(&batch);
    let (table_handle, table_text, table_kind) = items
        .iter()
        .find(|(_, text, _)| text.contains("cell"))
        .expect("the fixture's table paragraph");
    assert_eq!(
        table_kind.as_deref(),
        Some("table"),
        "a table paragraph must keep kind=table: {batch}"
    );

    // The flat view of the same table says the same thing.
    let flat = payload(&v2_read(&client, table_handle, json!({})).await);
    assert_eq!(flat["text"], Value::String(table_text.clone()));
    assert_eq!(flat["kind"], "table");

    // A prose sentence next to it still carries no kind.
    for (_, text, kind) in &items {
        if text.starts_with("Prose one") || text.starts_with("Prose two") {
            assert!(kind.is_none(), "clean prose must not carry a kind: {batch}");
        }
    }

    client.cancel().await.expect("client should close cleanly");
}

// ------------------------------------------------------------------------------------------------
// cite
// ------------------------------------------------------------------------------------------------

#[tokio::test]
async fn cite_reports_why_a_paragraph_handle_is_whole() {
    let directory = tempfile::tempdir().expect("temporary directory should be created");
    let path = directory.path().join("coarse.txt");
    tokio::fs::write(&path, coarse_prose_book())
        .await
        .expect("fixture should be written");
    let client = start_client(
        directory.path(),
        &directory.path().join("state"),
        Some("v2"),
    )
    .await;
    let doc = v2_open(&client, &path).await["doc"]
        .as_str()
        .expect("doc handle")
        .to_owned();
    let guarded = payload(&v2_read(&client, &doc, json!({})).await);
    let paragraph = guarded["at"]
        .as_str()
        .expect("the paragraph handle")
        .to_owned();
    let sentence = guarded["next"]
        .as_str()
        .expect("a sentence handle")
        .to_owned();

    let cite = payload(&call(&client, "cite", json!({ "at": [paragraph, sentence] })).await);
    let items = cite["items"].as_array().expect("one citation per handle");
    assert_eq!(
        items[0]["locator"]["sentence_index"],
        Value::Null,
        "the guarded paragraph has no sentence index"
    );
    assert_eq!(
        items[0]["effective_kind"], "paragraph",
        "cite must report the v1 effective_kind: {cite}"
    );
    assert_eq!(
        items[0]["degradation"], COARSE_ONLY,
        "cite must report the v1 degradation code: {cite}"
    );

    // A sentence handle is unchanged: neither field appears.
    assert_eq!(items[1]["locator"]["sentence_index"], 1);
    assert!(
        items[1].get("effective_kind").is_none(),
        "a sentence handle must not carry effective_kind: {cite}"
    );
    assert!(
        items[1].get("degradation").is_none(),
        "a sentence handle must not carry degradation: {cite}"
    );

    client.cancel().await.expect("client should close cleanly");
}

#[tokio::test]
async fn cite_reports_the_non_prose_code_of_a_table_paragraph() {
    let directory = tempfile::tempdir().expect("temporary directory should be created");
    let path = directory.path().join("table.html");
    tokio::fs::write(&path, table_book())
        .await
        .expect("fixture should be written");
    let client = start_client(
        directory.path(),
        &directory.path().join("state"),
        Some("v2"),
    )
    .await;
    let doc = v2_open(&client, &path).await["doc"]
        .as_str()
        .expect("doc handle")
        .to_owned();
    let batch = payload(&v2_read(&client, &doc, json!({ "max_chars": 4000 })).await);
    let table = batch_items(&batch)
        .into_iter()
        .find(|(_, text, _)| text.contains("cell"))
        .expect("the fixture's table paragraph")
        .0;

    let cite = payload(&call(&client, "cite", json!({ "at": [table] })).await);
    assert_eq!(
        cite["items"][0]["effective_kind"], "paragraph",
        "a table is a paragraph unit: {cite}"
    );
    assert_eq!(
        cite["items"][0]["degradation"], NON_PROSE,
        "a table must report the v1 non-prose degradation: {cite}"
    );

    client.cancel().await.expect("client should close cleanly");
}

// ------------------------------------------------------------------------------------------------
// tools/list
// ------------------------------------------------------------------------------------------------

#[tokio::test]
async fn the_tool_descriptions_name_the_new_fields() {
    let directory = tempfile::tempdir().expect("temporary directory should be created");
    let client = start_client(
        directory.path(),
        &directory.path().join("state"),
        Some("v2"),
    )
    .await;
    let tools = client
        .list_all_tools()
        .await
        .expect("tools/list should succeed");
    let description = |name: &str| {
        tools
            .iter()
            .find(|tool| &*tool.name == name)
            .unwrap_or_else(|| panic!("{name} must be listed"))
            .description
            .as_deref()
            .unwrap_or_default()
            .to_owned()
    };

    let read = description("read");
    assert!(
        read.contains("kind=paragraph"),
        "read must say a whole paragraph comes back marked: {read}"
    );
    let cite = description("cite");
    assert!(
        cite.contains("effective_kind") && cite.contains("degradation"),
        "cite must name both new fields: {cite}"
    );

    client.cancel().await.expect("client should close cleanly");
}
