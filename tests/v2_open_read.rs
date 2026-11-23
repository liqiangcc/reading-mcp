//! The v2 `open`/`read` contract: short handles, the reading order of every format, `back` and
//! errors.

use std::io::{Cursor, Write};
use std::path::Path;

use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, CallToolResult, ContentBlock};
use rmcp::transport::TokioChildProcess;
use serde_json::{Map, Value, json};
use tokio::process::Command;
use zip::write::{SimpleFileOptions, ZipWriter};

type Client = rmcp::service::RunningService<rmcp::RoleClient, ()>;

/// The v2 tool set: `open`, `read` and the five navigation/citation tools, as `tools/list`
/// returns them (alphabetical).
const V2_TOOLS: [&str; 7] = [
    "browse",
    "cite",
    "open",
    "outline",
    "read",
    "search",
    "source_view",
];

// ------------------------------------------------------------------------------------------------
// Fixtures
// ------------------------------------------------------------------------------------------------

fn markdown_book() -> &'static str {
    "# Book\n\nIntroduction sentence. Second introduction sentence.\n\n## One\n\nAlpha one is the first sentence. Alpha two is the second sentence. Alpha three is the third sentence.\n\n### Empty\n\n## Two\n\nBeta one is the first sentence. Beta two is the second sentence. Beta three is the third sentence.\n\n## Empty Tail\n"
}

fn plain_text_book() -> &'static str {
    "Plain first sentence. Plain second sentence.\n\nPlain third sentence.\n"
}

fn html_book() -> &'static str {
    "<html><body><h1>Html Book</h1><p>Html one. Html two.</p><h2>Html Two</h2><p>Html three.</p></body></html>"
}

fn epub_book() -> Vec<u8> {
    let cursor = Cursor::new(Vec::new());
    let mut writer = ZipWriter::new(cursor);
    let options = SimpleFileOptions::default();
    // The spine order is deliberately not the navigation order, so the body order is non-trivial.
    let entries: [(&str, &str); 6] = [
        ("mimetype", "application/epub+zip"),
        (
            "META-INF/container.xml",
            r#"<?xml version="1.0"?>
<container xmlns="urn:oasis:names:tc:opendocument:xmlns:container"><rootfiles>
<rootfile full-path="OPS/package.opf" media-type="application/oebps-package+xml"/>
</rootfiles></container>"#,
        ),
        (
            "OPS/package.opf",
            r#"<?xml version="1.0"?>
<package xmlns="http://www.idpf.org/2007/opf" version="3.0">
  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:title>V2 Epub</dc:title></metadata>
  <manifest>
    <item id="nav" href="nav.xhtml" media-type="application/xhtml+xml" properties="nav"/>
    <item id="second" href="second.xhtml" media-type="application/xhtml+xml"/>
    <item id="first" href="first.xhtml" media-type="application/xhtml+xml"/>
  </manifest>
  <spine><itemref idref="second"/><itemref idref="first"/></spine>
</package>"#,
        ),
        (
            "OPS/nav.xhtml",
            r#"<html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops"><body>
<nav epub:type="toc"><ol>
<li><a href="first.xhtml#top">First Chapter</a></li>
<li><a href="second.xhtml#top">Second Chapter</a></li>
</ol></nav>
</body></html>"#,
        ),
        (
            "OPS/first.xhtml",
            r#"<html xmlns="http://www.w3.org/1999/xhtml"><body>
<h1 id="top">First Chapter</h1><p>First one. First two.</p><table><tr><td>cell</td></tr></table>
</body></html>"#,
        ),
        (
            "OPS/second.xhtml",
            r#"<html xmlns="http://www.w3.org/1999/xhtml"><body>
<h1 id="top">Second Chapter</h1><p>Second one. Second two.</p>
</body></html>"#,
        ),
    ];
    for (name, content) in entries {
        writer.start_file(name, options).expect("ZIP entry");
        writer.write_all(content.as_bytes()).expect("ZIP content");
    }
    writer.finish().expect("ZIP finish").into_inner()
}

/// Every fixture: one per supported format, plus a multi-section book with empty sections.
fn fixtures() -> Vec<(&'static str, Vec<u8>)> {
    vec![
        ("markdown.md", markdown_book().as_bytes().to_vec()),
        ("plain.txt", plain_text_book().as_bytes().to_vec()),
        ("page.html", html_book().as_bytes().to_vec()),
        ("book.epub", epub_book()),
    ]
}

// ------------------------------------------------------------------------------------------------
// Transport helpers
// ------------------------------------------------------------------------------------------------

async fn start_client(local_root: &Path, state_dir: &Path) -> Client {
    let local_roots = std::env::join_paths([local_root]).expect("local roots should be valid");
    let mut command = Command::new(env!("CARGO_BIN_EXE_reading-mcp"));
    command
        .env("READING_MCP_LOCAL_ROOTS", local_roots)
        .env("READING_MCP_STATE_DIR", state_dir)
        .env("READING_MCP_TELEMETRY", "false")
        .env_remove("READING_MCP_TOOLS");
    let transport = TokioChildProcess::new(command).expect("MCP process should start");
    ().serve(transport)
        .await
        .expect("MCP initialization should succeed")
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

fn tool_error(result: &CallToolResult) -> Value {
    assert_eq!(
        result.is_error,
        Some(true),
        "expected a tool error, got {result:?}"
    );
    payload(result)
}

async fn v2_open(client: &Client, path: &Path) -> Value {
    let result = client
        .call_tool(
            CallToolRequestParams::new("open")
                .with_arguments(arguments(json!({ "source": path.to_string_lossy() }))),
        )
        .await
        .expect("open should succeed");
    payload(&result)
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

/// One v2 item: handle, text and the optional content class.
#[derive(Clone, Debug, PartialEq, Eq)]
struct V2Item {
    handle: String,
    text: String,
    class: Option<String>,
}

/// The single item of a flat answer: its handle is `at`, or exactly `from` when `at` is absent.
fn flat_item(payload: &Value, from: &str) -> Option<V2Item> {
    let text = payload.get("text")?.as_str()?.to_owned();
    Some(V2Item {
        handle: payload
            .get("at")
            .and_then(Value::as_str)
            .unwrap_or(from)
            .to_owned(),
        text,
        class: payload
            .get("kind")
            .and_then(Value::as_str)
            .map(str::to_owned),
    })
}

/// The items of a batch answer.
fn batch_items(payload: &Value) -> Vec<V2Item> {
    let items = payload
        .get("items")
        .and_then(Value::as_array)
        .expect("a batch read answers with items");
    items
        .iter()
        .map(|item| {
            let parts = item.as_array().expect("an item is an array");
            V2Item {
                handle: parts[0].as_str().expect("handle").to_owned(),
                text: parts[1].as_str().expect("text").to_owned(),
                class: parts.get(2).and_then(Value::as_str).map(str::to_owned),
            }
        })
        .collect()
}

/// The `next` value of one `read` answer, when it has one.
fn next_of(payload: &Value) -> Option<String> {
    payload
        .get("next")
        .and_then(Value::as_str)
        .map(str::to_owned)
}

/// Every item of a batch answer, from its JSON payload.
fn items(result: &CallToolResult) -> Vec<V2Item> {
    batch_items(&payload(result))
}

/// Walks with `read`, following `next`, and returns every item plus every section boundary.
async fn v2_walk(client: &Client, from: &str, extra: Value) -> (Vec<V2Item>, Vec<(usize, String)>) {
    let mut collected = Vec::new();
    let mut boundaries = Vec::new();
    let mut next = Some(from.to_owned());
    while let Some(from) = next {
        let result = v2_read(client, &from, extra.clone()).await;
        let payload = payload(&result);
        assert!(
            result.is_error != Some(true),
            "read {from} failed: {payload}"
        );
        let base = collected.len();
        if payload.get("items").is_some() {
            collected.extend(batch_items(&payload));
            for section in payload
                .get("sections")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                boundaries.push((
                    base + section["at"].as_u64().expect("at") as usize,
                    section["s"].as_str().expect("s").to_owned(),
                ));
            }
        } else {
            if let Some(item) = flat_item(&payload, &from) {
                collected.push(item);
            }
            if let Some(section) = payload.get("section") {
                boundaries.push((base, section["s"].as_str().expect("s").to_owned()));
            }
        }
        next = payload
            .get("next")
            .and_then(Value::as_str)
            .map(str::to_owned);
    }
    (collected, boundaries)
}

fn texts_of(items: &[V2Item]) -> Vec<String> {
    items.iter().map(|item| item.text.clone()).collect()
}

fn arguments(value: Value) -> Map<String, Value> {
    value.as_object().expect("an object").clone()
}

// ------------------------------------------------------------------------------------------------
// 2. Reading order: the main gate
// ------------------------------------------------------------------------------------------------

/// The exact sentence sequence, paragraph sequence and section-start positions of each fixture.
///
/// These are the fixtures' own texts. The v1 comparison that used to produce them is gone with the
/// v1 tools; a v2 change that reorders, drops or repeats an item fails here.
fn expected_reading(name: &str) -> (Vec<&'static str>, Vec<&'static str>, Vec<usize>) {
    match name {
        "markdown.md" => (
            vec![
                "Introduction sentence.",
                "Second introduction sentence.",
                "Alpha one is the first sentence.",
                "Alpha two is the second sentence.",
                "Alpha three is the third sentence.",
                "Beta one is the first sentence.",
                "Beta two is the second sentence.",
                "Beta three is the third sentence.",
            ],
            vec![
                "Introduction sentence. Second introduction sentence.",
                "Alpha one is the first sentence. Alpha two is the second sentence. Alpha three is the third sentence.",
                "Beta one is the first sentence. Beta two is the second sentence. Beta three is the third sentence.",
            ],
            vec![0, 2, 5],
        ),
        "plain.txt" => (
            vec![
                "Plain first sentence.",
                "Plain second sentence.",
                "Plain third sentence.",
            ],
            vec![
                "Plain first sentence. Plain second sentence.",
                "Plain third sentence.",
            ],
            vec![0],
        ),
        "page.html" => (
            vec!["Html one.", "Html two.", "Html three."],
            vec!["Html one. Html two.", "Html three."],
            vec![0, 2],
        ),
        "book.epub" => (
            vec![
                "Second one.",
                "Second two.",
                "First one.",
                "First two.",
                "cell",
            ],
            vec!["Second one. Second two.", "First one. First two.", "cell"],
            vec![0, 2],
        ),
        other => panic!("no expected reading for {other}"),
    }
}

fn assert_sequence(items: &[V2Item], expected: &[&str], name: &str, mode: &str) {
    assert_eq!(
        texts_of(items),
        expected
            .iter()
            .map(|text| (*text).to_owned())
            .collect::<Vec<_>>(),
        "{name}: the {mode} sequence"
    );
}

#[tokio::test]
async fn reads_every_format_in_the_expected_sentence_order() {
    let directory = tempfile::tempdir().expect("temporary directory should be created");
    let client = start_client(directory.path(), &directory.path().join("state")).await;

    for (name, bytes) in fixtures() {
        let path = directory.path().join(name);
        tokio::fs::write(&path, &bytes)
            .await
            .expect("fixture should be written");

        let doc = v2_open(&client, &path).await["doc"]
            .as_str()
            .expect("doc handle")
            .to_owned();
        let (sentence, paragraph, starts) = expected_reading(name);

        // Sentence-step mode: one item per call, following `next`.
        let (sentence_items, boundaries) = v2_walk(&client, &doc, json!({})).await;
        assert_sequence(&sentence_items, &sentence, name, "sentence");
        assert_eq!(
            boundaries.iter().map(|(at, _)| *at).collect::<Vec<_>>(),
            starts,
            "{name}: the sections start at the first item of their own stream"
        );

        // Batch mode at two very different budgets: the same sequence, never a repeat or a gap.
        for max_chars in [4000usize, 37] {
            let (batch_walked, _) = v2_walk(&client, &doc, json!({ "max_chars": max_chars })).await;
            assert_sequence(&batch_walked, &sentence, name, "batched sentence");
        }

        // Paragraph mode: one item per paragraph, in the same body order.
        let (paragraph_items, _) = v2_walk(&client, &doc, json!({ "unit": "paragraph" })).await;
        assert_sequence(&paragraph_items, &paragraph, name, "paragraph");

        // Backward from the end reverses the forward sentence sequence.
        if let Some(last) = sentence_items.last() {
            let (mut backward_items, _) =
                v2_walk(&client, &last.handle, json!({ "direction": "backward" })).await;
            backward_items.reverse();
            assert_sequence(&backward_items, &sentence, name, "backward sentence");
        }
    }

    client.cancel().await.expect("client should close cleanly");
}

// ------------------------------------------------------------------------------------------------
// 3. `next` is the next item's own handle
// ------------------------------------------------------------------------------------------------

#[tokio::test]
async fn forward_next_is_the_next_items_own_handle() {
    let directory = tempfile::tempdir().expect("temporary directory should be created");
    let client = start_client(directory.path(), &directory.path().join("state")).await;

    for (name, bytes) in fixtures() {
        let path = directory.path().join(name);
        tokio::fs::write(&path, &bytes)
            .await
            .expect("fixture should be written");
        let doc = v2_open(&client, &path).await["doc"]
            .as_str()
            .expect("doc handle")
            .to_owned();

        let mut from: Option<String> = Some(doc);
        let mut expected: Option<String> = None;
        let mut count = 0usize;
        while let Some(handle) = from.take() {
            let payload = payload(&v2_read(&client, &handle, json!({})).await);
            let item = flat_item(&payload, &handle).expect("{name}: a single item");
            if let Some(expected) = expected.take() {
                assert_eq!(
                    item.handle, expected,
                    "{name}: the previous `next` must read at its own item"
                );
            }
            count += 1;
            assert!(count < 100_000, "{name}: the walk must terminate");
            if let Some(next) = next_of(&payload) {
                assert!(
                    !next.starts_with("r:") && !next.starts_with("b:"),
                    "{name}: a forward `next` must be a plain handle, got {next}"
                );
                expected = Some(next.clone());
                from = Some(next);
            }
        }
        assert!(count > 0, "{name}: the fixture must have items");
    }

    client.cancel().await.expect("client should close cleanly");
}

// ------------------------------------------------------------------------------------------------
// 4. Handle start
// ------------------------------------------------------------------------------------------------

#[tokio::test]
async fn every_item_handle_starts_at_its_own_item() {
    let directory = tempfile::tempdir().expect("temporary directory should be created");
    let path = directory.path().join("book.md");
    tokio::fs::write(&path, markdown_book())
        .await
        .expect("fixture should be written");
    let client = start_client(directory.path(), &directory.path().join("state")).await;

    let doc = v2_open(&client, &path).await["doc"]
        .as_str()
        .expect("doc handle")
        .to_owned();
    let (walked, _) = v2_walk(&client, &doc, json!({})).await;
    assert!(!walked.is_empty(), "the fixture must have items");

    for item in &walked {
        let result = v2_read(&client, &item.handle, json!({})).await;
        let payload = payload(&result);
        let first = flat_item(&payload, &item.handle).expect("a handle must start at an item");
        assert_eq!(
            first, *item,
            "handle {} did not start at its own item",
            item.handle
        );
        assert!(
            payload.get("at").is_none(),
            "the item's own handle must not repeat itself in `at`: {payload}"
        );
    }

    client.cancel().await.expect("client should close cleanly");
}

// ------------------------------------------------------------------------------------------------
// 5. back
// ------------------------------------------------------------------------------------------------// ------------------------------------------------------------------------------------------------

#[tokio::test]
async fn back_returns_the_previous_items_plus_the_anchor() {
    let directory = tempfile::tempdir().expect("temporary directory should be created");
    let path = directory.path().join("book.md");
    tokio::fs::write(&path, markdown_book())
        .await
        .expect("fixture should be written");
    let client = start_client(directory.path(), &directory.path().join("state")).await;

    let doc = v2_open(&client, &path).await["doc"]
        .as_str()
        .expect("doc handle")
        .to_owned();
    let (walked, _) = v2_walk(&client, &doc, json!({})).await;
    let anchor_index = walked
        .iter()
        .position(|item| item.text.starts_with("Beta one"))
        .expect("the second section's first sentence must be in the walk");
    assert!(anchor_index >= 2, "the fixture must have two earlier items");
    assert!(
        walked[anchor_index - 1].text.starts_with("Alpha three"),
        "the item before the anchor must be the previous section's last sentence, got {:?}",
        walked[anchor_index - 1].text
    );

    let result = v2_read(&client, &walked[anchor_index].handle, json!({ "back": 2 })).await;
    let payload = payload(&result);
    let returned = items(&result);
    assert_eq!(
        texts_of(&returned),
        texts_of(&walked[anchor_index - 2..=anchor_index]),
        "back must return the last two items of the previous section plus the anchor"
    );
    assert_eq!(
        payload["anchor"].as_u64(),
        Some(2),
        "the anchor must be the index of the from item"
    );

    client.cancel().await.expect("client should close cleanly");
}

// ------------------------------------------------------------------------------------------------
// 6. The single-item `at`, `section` and `kind` keys
// ------------------------------------------------------------------------------------------------

#[tokio::test]
async fn at_names_the_item_only_when_from_is_not_its_own_handle() {
    let directory = tempfile::tempdir().expect("temporary directory should be created");
    let path = directory.path().join("book.md");
    tokio::fs::write(&path, markdown_book())
        .await
        .expect("fixture should be written");
    let client = start_client(directory.path(), &directory.path().join("state")).await;

    let doc = v2_open(&client, &path).await["doc"]
        .as_str()
        .expect("doc handle")
        .to_owned();
    let (walked, boundaries) = v2_walk(&client, &doc, json!({})).await;
    assert!(walked.len() > 3, "the fixture must have several items");
    assert!(
        boundaries.len() >= 2,
        "the fixture must have two non-empty sections"
    );

    // A document handle names no item, so the answer says which item it read.
    let from_doc = payload(&v2_read(&client, &doc, json!({})).await);
    assert_eq!(
        from_doc["at"].as_str(),
        Some(walked[0].handle.as_str()),
        "a document handle must report the item it read: {from_doc}"
    );

    // A section handle names no item either.
    let (section_at, section_handle) = &boundaries[1];
    let from_section = payload(&v2_read(&client, section_handle, json!({})).await);
    assert_eq!(
        from_section["at"].as_str(),
        Some(walked[*section_at].handle.as_str()),
        "a section handle must report the item it read: {from_section}"
    );

    // A paragraph handle in sentence mode reads that paragraph's first sentence.
    let paragraph = walked
        .iter()
        .find(|item| item.text.starts_with("Alpha two"))
        .expect("the fixture must have a second sentence")
        .handle
        .rsplit_once('.')
        .map(|(head, _)| head)
        .expect("a sentence handle has a sentence part")
        .to_owned();
    let from_paragraph = payload(&v2_read(&client, &paragraph, json!({})).await);
    assert!(
        from_paragraph.get("at").is_some(),
        "a paragraph handle must report the sentence it read: {from_paragraph}"
    );

    // A backward step from a handle reads at that handle's own item, so `at` stays absent; every
    // step after it follows a `b:` cursor, and then `at` names the item it read.
    let mut from = walked.last().expect("the walk is non-empty").handle.clone();
    let mut expected = walked.len() - 1;
    let mut step = payload(&v2_read(&client, &from, json!({ "direction": "backward" })).await);
    assert_eq!(
        flat_item(&step, &from).map(|item| item.text),
        Some(walked[expected].text.clone()),
        "a backward step from a handle must read at that handle: {step}"
    );
    assert!(
        step.get("at").is_none(),
        "the handle's own item must not repeat in `at`: {step}"
    );
    while let Some(next) = next_of(&step) {
        from = next;
        assert!(
            from.starts_with("b:"),
            "a backward `next` must be a b: cursor: {from}"
        );
        assert!(
            expected > 0,
            "the backward walk must not pass the book start"
        );
        // No `direction` here: the cursor prefix carries it.
        step = payload(&v2_read(&client, &from, json!({})).await);
        expected -= 1;
        assert_eq!(
            step["at"].as_str(),
            Some(walked[expected].handle.as_str()),
            "a backward step from a cursor must report the item it read: {step}"
        );
    }
    assert_eq!(
        expected, 0,
        "backward single steps must reach the start of the book"
    );

    client.cancel().await.expect("client should close cleanly");
}

#[tokio::test]
async fn section_marks_exactly_the_first_item_of_each_section() {
    let directory = tempfile::tempdir().expect("temporary directory should be created");
    let client = start_client(directory.path(), &directory.path().join("state")).await;

    for (name, bytes) in fixtures() {
        let path = directory.path().join(name);
        tokio::fs::write(&path, &bytes)
            .await
            .expect("fixture should be written");
        let doc = v2_open(&client, &path).await["doc"]
            .as_str()
            .expect("doc handle")
            .to_owned();

        let mut from = Some(doc);
        let mut index = 0usize;
        let mut starts = Vec::new();
        let mut handles = Vec::new();
        while let Some(handle) = from {
            let payload = payload(&v2_read(&client, &handle, json!({})).await);
            let item = flat_item(&payload, &handle).expect("a single item");
            let section = payload.get("section").map(|section| {
                assert_eq!(
                    section["s"].as_str(),
                    Some(section_of(&item.handle).as_str()),
                    "{name}: the section key must name the item's own section"
                );
                section["s"].as_str().expect("s").to_owned()
            });
            starts.push(section.is_some());
            handles.push(item.handle);
            from = payload
                .get("next")
                .and_then(Value::as_str)
                .map(str::to_owned);
            index += 1;
            assert!(index < 100_000, "{name}: the walk must terminate");
        }
        assert!(!handles.is_empty(), "{name}: the fixture must have items");

        // A section starts exactly when the item's section differs from the previous item's.
        for at in 0..handles.len() {
            let expected = at == 0 || section_of(&handles[at]) != section_of(&handles[at - 1]);
            assert_eq!(
                starts[at],
                expected,
                "{name}: item {at} ({}) must {}carry `section`",
                handles[at],
                if expected { "" } else { "not " }
            );
        }
    }

    client.cancel().await.expect("client should close cleanly");
}

/// The section part of an item handle: `d3~9f2c.s12` of `d3~9f2c.s12.p4.2`.
fn section_of(handle: &str) -> String {
    handle
        .rsplit_once(".p")
        .map(|(head, _)| head)
        .unwrap_or(handle)
        .to_owned()
}

#[tokio::test]
async fn kind_marks_non_prose_items_and_stays_off_prose() {
    let directory = tempfile::tempdir().expect("temporary directory should be created");
    let client = start_client(directory.path(), &directory.path().join("state")).await;

    // The EPUB fixture carries a real table next to prose.
    let path = directory.path().join("book.epub");
    tokio::fs::write(&path, epub_book())
        .await
        .expect("fixture should be written");
    let doc = v2_open(&client, &path).await["doc"]
        .as_str()
        .expect("doc handle")
        .to_owned();
    let (walked, _) = v2_walk(&client, &doc, json!({})).await;
    let table = walked
        .iter()
        .find(|item| item.class.as_deref() == Some("table"))
        .expect("the table paragraph must carry kind=table");
    assert!(
        table.text.contains("cell"),
        "the table item must be the table text, got {:?}",
        table.text
    );
    for item in &walked {
        if item.text.contains("First one") || item.text.contains("Second two") {
            assert_eq!(item.class, None, "prose must not carry `kind`: {item:?}");
        }
    }

    // A fenced code block is coarse too.
    let path = directory.path().join("code.txt");
    tokio::fs::write(
        &path,
        "```rust\nfn main() { println!(\"Hi.\"); }\n```\n\nReal prose. Next sentence.\n",
    )
    .await
    .expect("fixture should be written");
    let doc = v2_open(&client, &path).await["doc"]
        .as_str()
        .expect("doc handle")
        .to_owned();
    let (walked, _) = v2_walk(&client, &doc, json!({})).await;
    let code = walked
        .iter()
        .find(|item| item.text.contains("fn main"))
        .expect("the code paragraph must be readable");
    assert!(
        matches!(
            code.class.as_deref(),
            Some("code_block") | Some("preformatted")
        ),
        "a code paragraph must carry its content class, got {:?}",
        code.class
    );
    assert!(
        walked
            .iter()
            .any(|item| item.text.contains("Real prose") && item.class.is_none()),
        "prose next to code must stay plain: {walked:?}"
    );

    client.cancel().await.expect("client should close cleanly");
}

// ------------------------------------------------------------------------------------------------
// 7. Errors
// ------------------------------------------------------------------------------------------------

#[tokio::test]
async fn errors_use_the_v2_vocabulary() {
    let directory = tempfile::tempdir().expect("temporary directory should be created");
    let path = directory.path().join("book.md");
    tokio::fs::write(&path, markdown_book())
        .await
        .expect("fixture should be written");
    let client = start_client(directory.path(), &directory.path().join("state")).await;

    let doc = v2_open(&client, &path).await["doc"]
        .as_str()
        .expect("doc handle")
        .to_owned();

    // Malformed handle.
    let error = tool_error(&v2_read(&client, "not-a-handle", json!({})).await);
    assert_eq!(error["error"], "invalid_argument");
    assert!(
        error["hint"].as_str().expect("hint").contains("from"),
        "the hint must name the field: {error}"
    );

    // Unknown doc key.
    let error = tool_error(&v2_read(&client, "d999999~abcd", json!({})).await);
    assert_eq!(error["error"], "unknown_handle");

    // Wrong checksum.
    let (head, _) = doc.split_once('~').expect("the handle has a checksum");
    let wrong = if doc.ends_with("0000") {
        format!("{head}~1111")
    } else {
        format!("{head}~0000")
    };
    let error = tool_error(&v2_read(&client, &wrong, json!({})).await);
    assert_eq!(error["error"], "stale_handle");

    // too_long, with a retry that repeats the call at the item's own char count.
    let error = tool_error(&v2_read(&client, &doc, json!({ "max_chars": 1 })).await);
    assert_eq!(error["error"], "too_long");
    let retry_chars = error["retry"]["max_chars"]
        .as_u64()
        .expect("a retry with the item char count");
    assert!(retry_chars > 1, "the retry must widen max_chars");
    let retried = v2_read(&client, &doc, json!({ "max_chars": retry_chars })).await;
    assert!(
        retried.is_error != Some(true),
        "the suggested retry must succeed: {retried:?}"
    );
    assert_eq!(items(&retried).len(), 1);

    // Forward `next` is a handle, so a plain follow-up reads the next item.
    let (walked, _) = v2_walk(&client, &doc, json!({})).await;
    let first = payload(&v2_read(&client, &walked[0].handle, json!({})).await);
    let next = first["next"].as_str().expect("a next handle").to_owned();
    assert!(
        !next.starts_with("r:"),
        "forward next must be a handle: {next}"
    );
    assert_eq!(next, walked[1].handle);

    // The `r:` cursor is gone: it is no longer an accepted `from`.
    let error = tool_error(&v2_read(&client, "r:d3~9f2c.s0.S.1", json!({})).await);
    assert_eq!(error["error"], "invalid_argument");
    assert!(
        error["hint"].as_str().expect("hint").contains("from"),
        "the hint must name the field: {error}"
    );

    // A backward `b:` cursor carries its direction, so `direction=forward` conflicts.
    let last = walked.last().expect("the fixture must have items");
    let backward =
        payload(&v2_read(&client, &last.handle, json!({ "direction": "backward" })).await);
    let cursor = backward["next"]
        .as_str()
        .expect("a backward cursor")
        .to_owned();
    assert!(
        cursor.starts_with("b:"),
        "backward next must be a b: cursor: {cursor}"
    );
    let error = tool_error(&v2_read(&client, &cursor, json!({ "direction": "forward" })).await);
    assert_eq!(error["error"], "invalid_argument");
    assert!(
        error["hint"].as_str().expect("hint").contains("direction"),
        "the hint must name the field: {error}"
    );

    client.cancel().await.expect("client should close cleanly");
}

#[tokio::test]
async fn the_default_tool_set_is_the_seven_v2_tools() {
    let directory = tempfile::tempdir().expect("temporary directory should be created");
    let client = start_client(directory.path(), &directory.path().join("default")).await;
    let names = client
        .list_all_tools()
        .await
        .expect("tools/list should succeed")
        .into_iter()
        .map(|tool| tool.name.to_string())
        .collect::<Vec<_>>();
    assert_eq!(
        names,
        V2_TOOLS
            .iter()
            .map(|name| (*name).to_owned())
            .collect::<Vec<_>>(),
        "with READING_MCP_TOOLS unset the default tool set is the seven v2 tools"
    );
    client.cancel().await.expect("client should close cleanly");
}

#[tokio::test]
async fn the_seven_v2_tool_definitions_stay_compact() {
    let directory = tempfile::tempdir().expect("temporary directory should be created");
    let client = start_client(directory.path(), &directory.path().join("v2")).await;
    let tools = client
        .list_all_tools()
        .await
        .expect("tools/list should succeed");
    let bytes = serde_json::to_string(&tools)
        .expect("tools serialize")
        .len();
    assert!(
        bytes < 3_600,
        "the seven v2 tools must stay under 3600 bytes of tools/list JSON, got {bytes}"
    );
    client.cancel().await.expect("client should close cleanly");
}

// ------------------------------------------------------------------------------------------------
// 9. Exclusive continuation after a handle anchor
// ------------------------------------------------------------------------------------------------

#[tokio::test]
async fn read_from_a_handle_continues_exclusively_at_the_next_item() {
    let directory = tempfile::tempdir().expect("temporary directory should be created");
    let path = directory.path().join("anchor.md");
    tokio::fs::write(
        &path,
        "# Root\n\nOne. Two. Three. Four.\n\n# Next\n\nDo not enumerate this section.\n",
    )
    .await
    .expect("fixture should be written");
    let client = start_client(directory.path(), &directory.path().join("state")).await;

    let doc = v2_open(&client, &path).await["doc"]
        .as_str()
        .expect("doc handle")
        .to_owned();
    let (walked, _) = v2_walk(&client, &doc, json!({})).await;
    assert_eq!(
        texts_of(&walked),
        vec![
            "One.",
            "Two.",
            "Three.",
            "Four.",
            "Do not enumerate this section.",
        ],
        "the fixture must walk in body order"
    );

    // The anchor reads at its own item; the continuation after it is the next item, exclusively:
    // the anchor is never repeated, and every step follows the previous answer's own `next`.
    let anchor = walked[1].clone();
    let anchored = payload(&v2_read(&client, &anchor.handle, json!({})).await);
    assert_eq!(flat_item(&anchored, &anchor.handle), Some(anchor.clone()));
    assert!(
        anchored.get("at").is_none(),
        "the anchor's own handle must not repeat itself in `at`: {anchored}"
    );

    let mut from = next_of(&anchored).expect("the anchor continues");
    let mut expected = 2usize;
    let mut seen = vec![anchor.handle.clone()];
    loop {
        let step = payload(&v2_read(&client, &from, json!({})).await);
        let item = flat_item(&step, &from).expect("a single item");
        assert_eq!(
            item.text, walked[expected].text,
            "the continuation must read the item after the anchor"
        );
        assert!(
            !seen.contains(&item.handle),
            "the continuation must never repeat an item: {seen:?} then {item:?}"
        );
        seen.push(item.handle.clone());
        expected += 1;
        match next_of(&step) {
            Some(next) => {
                assert_ne!(next, anchor.handle, "the anchor must not come back");
                from = next;
            }
            None => break,
        }
    }
    assert_eq!(
        expected,
        walked.len(),
        "the continuation must reach the end of the document exactly once"
    );

    // Paragraph mode: the paragraph handle continues at the next paragraph, not at itself.
    let (paragraphs, _) = v2_walk(&client, &doc, json!({ "unit": "paragraph" })).await;
    assert_eq!(
        texts_of(&paragraphs),
        vec!["One. Two. Three. Four.", "Do not enumerate this section."]
    );
    let first_paragraph = payload(
        &v2_read(
            &client,
            &paragraphs[0].handle,
            json!({ "unit": "paragraph" }),
        )
        .await,
    );
    let next_paragraph = next_of(&first_paragraph).expect("the paragraph continues");
    assert_ne!(next_paragraph, paragraphs[0].handle);
    let second_paragraph =
        payload(&v2_read(&client, &next_paragraph, json!({ "unit": "paragraph" })).await);
    assert_eq!(
        flat_item(&second_paragraph, &next_paragraph).map(|item| item.text),
        Some(paragraphs[1].text.clone()),
        "the paragraph continuation is the next paragraph"
    );

    client.cancel().await.expect("client should close cleanly");
}

// ------------------------------------------------------------------------------------------------
// 10. Whole-document composition in body order
// ------------------------------------------------------------------------------------------------

#[tokio::test]
async fn reads_every_section_in_body_order_without_repeating_an_item() {
    let directory = tempfile::tempdir().expect("temporary directory should be created");
    let path = directory.path().join("book.md");
    tokio::fs::write(
        &path,
        "# Book\n\nIntroduction.\n\n## Duplicate\n\nParent sentence. Child-independent sentence.\n\n### Empty\n\n## Duplicate\n\nSibling sentence.\n",
    )
    .await
    .expect("fixture should be written");
    let client = start_client(directory.path(), &directory.path().join("state")).await;

    let doc = v2_open(&client, &path).await["doc"]
        .as_str()
        .expect("doc handle")
        .to_owned();
    let outline = payload(
        &client
            .call_tool(
                CallToolRequestParams::new("outline")
                    .with_arguments(arguments(json!({ "at": doc, "depth": 8, "limit": 100 }))),
            )
            .await
            .expect("outline should answer"),
    );
    let rows = outline["items"].as_array().expect("outline items");
    assert_eq!(
        rows.iter()
            .map(|row| row[1].as_str().expect("title"))
            .collect::<Vec<_>>(),
        vec!["Book", "Duplicate", "Empty", "Duplicate"],
        "the duplicate titles are separate sections"
    );
    assert_eq!(
        rows.iter()
            .map(|row| row[2].as_u64().expect("level"))
            .collect::<Vec<_>>(),
        vec![1, 2, 3, 2]
    );
    let sections = rows
        .iter()
        .map(|row| row[0].as_str().expect("section handle").to_owned())
        .collect::<Vec<_>>();

    // The whole book, one item at a time: every section in body order, the empty one contributing
    // nothing, and no item read twice.
    let (walked, _) = v2_walk(&client, &doc, json!({})).await;
    assert_eq!(
        texts_of(&walked),
        vec![
            "Introduction.",
            "Parent sentence.",
            "Child-independent sentence.",
            "Sibling sentence.",
        ]
    );
    let owners = walked
        .iter()
        .map(|item| section_of(&item.handle))
        .collect::<Vec<_>>();
    assert_eq!(
        owners,
        vec![
            sections[0].clone(),
            sections[1].clone(),
            sections[1].clone(),
            sections[3].clone(),
        ],
        "the items follow their sections' body order"
    );
    assert!(
        !owners.contains(&sections[2]),
        "the empty section owns no item"
    );
    let mut unique = std::collections::BTreeSet::new();
    assert!(
        walked.iter().all(|item| unique.insert(item.handle.clone())),
        "no item may be read twice: {walked:?}"
    );

    client.cancel().await.expect("client should close cleanly");
}
