//! Real-book v2/v1 sequence equivalence. Ignored by default: it needs the downloaded books.
//!
//! Run with `READING_MCP_BOOK_DIR=<dir> cargo test --release --test v2_book_equivalence --
//! --ignored --nocapture`. Every book in that directory must report 0 mismatches.

use std::path::{Path, PathBuf};

use reading_mcp::mcp::contracts::{
    GetDocumentStructureResponse, GetTextUnitsResponse, OpenDocumentResponse, SectionNode,
};
use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, CallToolResult, ContentBlock};
use rmcp::transport::TokioChildProcess;
use serde_json::{Map, Value, json};
use tokio::process::Command;

type Client = rmcp::service::RunningService<rmcp::RoleClient, ()>;

/// How many items the sentence-step walk compares.
const SENTENCE_STEP_ITEMS: usize = 2_000;

#[tokio::test]
#[ignore = "needs READING_MCP_BOOK_DIR with the downloaded books"]
async fn real_books_read_identically_through_v2_and_v1() {
    let directory = std::env::var("READING_MCP_BOOK_DIR")
        .expect("READING_MCP_BOOK_DIR must point at the downloaded books");
    let books = book_files(Path::new(&directory));
    assert!(!books.is_empty(), "no books found in {directory}");

    let state = tempfile::tempdir().expect("temporary state directory");
    let mut mismatches = 0usize;
    for book in books {
        println!("=== {}", book.display());
        let client = start_client(Path::new(&directory), state.path()).await;
        let mismatches_here = compare_book(&client, &book).await;
        mismatches += mismatches_here;
        println!("{}: mismatches={mismatches_here}", book.display());
        client.cancel().await.expect("client should close cleanly");
    }
    println!("TOTAL mismatches={mismatches}");
    assert_eq!(mismatches, 0, "v2 and v1 must read every book identically");
}

fn book_files(directory: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let Ok(entries) = std::fs::read_dir(directory) else {
        return files;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) == Some("epub") {
            files.push(path);
        }
    }
    files.sort();
    files
}

async fn compare_book(client: &Client, path: &Path) -> usize {
    let opened = client
        .call_tool(
            CallToolRequestParams::new("open_document")
                .with_arguments(arguments(json!({ "source": path.to_string_lossy() }))),
        )
        .await
        .expect("open_document should succeed")
        .into_typed::<OpenDocumentResponse>()
        .expect("open response should deserialize");
    let doc = {
        let result = client
            .call_tool(
                CallToolRequestParams::new("open")
                    .with_arguments(arguments(json!({ "source": path.to_string_lossy() }))),
            )
            .await
            .expect("open should succeed");
        payload(&result)["doc"]
            .as_str()
            .expect("a doc handle")
            .to_owned()
    };

    let reference = v1_sentence_texts(client, &opened.document_id).await;
    println!("v1 sentence items: {}", reference.len());

    // Batch mode at 16000 characters: the whole book, every item text.
    let mut batch = Vec::new();
    let mut next = Some(doc.clone());
    while let Some(from) = next {
        let result = read(client, &from, json!({ "max_chars": 16_000 })).await;
        batch.extend(texts(&result));
        next = payload(&result)
            .get("next")
            .and_then(Value::as_str)
            .map(str::to_owned);
    }
    let mut mismatches = count_mismatches("batch 16000", &reference, &batch);

    // Sentence-step mode over the first 2000 items, following `next` handles.
    let mut stepped = Vec::new();
    let mut next = Some(doc);
    while let Some(from) = next {
        if stepped.len() >= SENTENCE_STEP_ITEMS {
            break;
        }
        let result = read(client, &from, json!({})).await;
        stepped.extend(texts(&result));
        next = payload(&result)
            .get("next")
            .and_then(Value::as_str)
            .map(str::to_owned);
    }
    let expected = reference
        .iter()
        .take(SENTENCE_STEP_ITEMS)
        .cloned()
        .collect::<Vec<_>>();
    let observed = stepped
        .into_iter()
        .take(SENTENCE_STEP_ITEMS)
        .collect::<Vec<_>>();
    mismatches += count_mismatches("sentence-step", &expected, &observed);
    mismatches
}

fn count_mismatches(label: &str, expected: &[String], observed: &[String]) -> usize {
    let mut mismatches = 0usize;
    let mut first = None;
    for index in 0..expected.len().max(observed.len()) {
        let left = expected.get(index);
        let right = observed.get(index);
        if left != right {
            mismatches += 1;
            if first.is_none() {
                first = Some((index, left.cloned(), right.cloned()));
            }
        }
    }
    println!(
        "{label}: expected={} observed={} mismatches={mismatches}",
        expected.len(),
        observed.len()
    );
    if let Some((index, left, right)) = first {
        println!(
            "{label}: first mismatch at {index}: v1={:?} v2={:?}",
            truncate(left),
            truncate(right)
        );
    }
    mismatches
}

fn truncate(value: Option<String>) -> Option<String> {
    value.map(|text| text.chars().take(60).collect())
}

async fn v1_sentence_texts(client: &Client, document_id: &str) -> Vec<String> {
    let mut texts = Vec::new();
    for section_id in v1_section_order(client, document_id).await {
        let mut cursor: Option<String> = None;
        for _ in 0..1_000_000 {
            let mut value = Map::new();
            value.insert("document_id".into(), json!(document_id));
            value.insert("section_id".into(), json!(section_id));
            value.insert("requested_kind".into(), json!("sentence"));
            value.insert("direction".into(), json!("forward"));
            value.insert("coverage_policy".into(), json!("preserve_source"));
            value.insert("max_items".into(), json!(256));
            if let Some(cursor) = &cursor {
                value.insert("cursor".into(), json!(cursor));
            }
            let response = client
                .call_tool(CallToolRequestParams::new("get_text_units").with_arguments(value))
                .await
                .expect("get_text_units should succeed")
                .into_typed::<GetTextUnitsResponse>()
                .expect("text-unit response should deserialize");
            texts.extend(response.items.into_iter().map(|item| item.text));
            if response.complete {
                break;
            }
            cursor = response.next_cursor;
            assert!(cursor.is_some(), "an incomplete page must carry a cursor");
        }
    }
    texts
}

async fn v1_section_order(client: &Client, document_id: &str) -> Vec<String> {
    let mut sections = Vec::new();
    let mut cursor: Option<String> = None;
    for _ in 0..100_000 {
        let mut value = Map::new();
        value.insert("document_id".into(), json!(document_id));
        value.insert("max_nodes".into(), json!(200));
        if let Some(cursor) = &cursor {
            value.insert("cursor".into(), json!(cursor));
        }
        let response = client
            .call_tool(CallToolRequestParams::new("get_document_structure").with_arguments(value))
            .await
            .expect("get_document_structure should succeed")
            .into_typed::<GetDocumentStructureResponse>()
            .expect("structure response should deserialize");
        collect(&response.sections, &mut sections);
        cursor = response.next_cursor;
        if cursor.is_none() || response.complete {
            break;
        }
    }
    sections.sort_by_key(|(order, _)| *order);
    sections.into_iter().map(|(_, id)| id).collect()
}

fn collect(nodes: &[SectionNode], output: &mut Vec<(usize, String)>) {
    for node in nodes {
        output.push((node.body_order, node.section_id.clone()));
        collect(&node.children, output);
    }
}

async fn read(client: &Client, from: &str, extra: Value) -> CallToolResult {
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

fn payload(result: &CallToolResult) -> Value {
    let text = result
        .content
        .iter()
        .find_map(|block| match block {
            ContentBlock::Text(text) => Some(text.text.clone()),
            _ => None,
        })
        .expect("the tool answers with one text block");
    serde_json::from_str(&text).expect("the tool answers with compact JSON")
}

/// The item texts of one `read` answer, in the flat single-item shape or the batch shape.
fn texts(result: &CallToolResult) -> Vec<String> {
    let payload = payload(result);
    if let Some(items) = payload.get("items").and_then(Value::as_array) {
        return items
            .iter()
            .map(|item| {
                item.as_array().expect("an item is an array")[1]
                    .as_str()
                    .expect("text")
                    .to_owned()
            })
            .collect();
    }
    payload
        .get("text")
        .and_then(Value::as_str)
        .map(|text| vec![text.to_owned()])
        .unwrap_or_default()
}

fn arguments(value: Value) -> Map<String, Value> {
    value.as_object().expect("an object").clone()
}

async fn start_client(local_root: &Path, state_dir: &Path) -> Client {
    let local_roots = std::env::join_paths([local_root]).expect("local roots should be valid");
    let mut command = Command::new(env!("CARGO_BIN_EXE_reading-mcp"));
    command
        .env("READING_MCP_LOCAL_ROOTS", local_roots)
        .env("READING_MCP_STATE_DIR", state_dir)
        .env("READING_MCP_TELEMETRY", "false")
        .env("READING_MCP_TOOLS", "both");
    let transport = TokioChildProcess::new(command).expect("MCP process should start");
    ().serve(transport)
        .await
        .expect("MCP initialization should succeed")
}
