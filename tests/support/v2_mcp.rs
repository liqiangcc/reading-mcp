//! Shared stdio helpers for the v2 tools: one child process per client, the compact-JSON payload
//! decoder, the fixture builders, and the `read` walk. The v2 test files include this module by
//! path so every new test speaks to the server exactly like `tests/v2_open_read.rs` and
//! `tests/v2_tools.rs` do, without setting `READING_MCP_TOOLS`: the default tool set is v2.
#![allow(dead_code)]

use std::io::{Cursor, Write};
use std::path::Path;

use lopdf::content::{Content, Operation};
use lopdf::{Document as PdfDocument, Object, Stream, dictionary};
use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, CallToolResult, ContentBlock};
use rmcp::transport::TokioChildProcess;
use serde_json::{Map, Value, json};
use tokio::process::Command;
use zip::write::{SimpleFileOptions, ZipWriter};

pub type Client = rmcp::service::RunningService<rmcp::RoleClient, ()>;

// ------------------------------------------------------------------------------------------------
// Transport
// ------------------------------------------------------------------------------------------------

/// Starts the server with the default tool set (v2): `READING_MCP_TOOLS` is removed, never set.
pub async fn start_client(local_root: &Path, state_dir: &Path) -> Client {
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

pub fn payload(result: &CallToolResult) -> Value {
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

pub fn tool_error(result: &CallToolResult) -> Value {
    assert_eq!(
        result.is_error,
        Some(true),
        "expected a tool error, got {result:?}"
    );
    payload(result)
}

pub fn arguments(value: Value) -> Map<String, Value> {
    value.as_object().expect("an object").clone()
}

pub async fn call(client: &Client, name: &'static str, value: Value) -> CallToolResult {
    client
        .call_tool(CallToolRequestParams::new(name).with_arguments(arguments(value)))
        .await
        .unwrap_or_else(|error| panic!("{name} should answer: {error}"))
}

pub async fn v2_open(client: &Client, path: &Path) -> Value {
    payload(&call(client, "open", json!({ "source": path.to_string_lossy() })).await)
}

pub async fn v2_read(client: &Client, from: &str, extra: Value) -> CallToolResult {
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

pub fn flat_text(payload: &Value) -> String {
    payload
        .get("text")
        .and_then(Value::as_str)
        .expect("a flat read carries text")
        .to_owned()
}

pub fn next_of(payload: &Value) -> Option<String> {
    payload
        .get("next")
        .and_then(Value::as_str)
        .map(str::to_owned)
}

/// The `[handle, text]` pairs of a `read` answer: a batch's `items`, or a flat item.
pub fn read_items(payload: &Value, from: &str) -> Vec<(String, String)> {
    if let Some(items) = payload.get("items").and_then(Value::as_array) {
        return items
            .iter()
            .map(|item| {
                let parts = item.as_array().expect("an item is an array");
                (
                    parts[0].as_str().expect("handle").to_owned(),
                    parts[1].as_str().expect("text").to_owned(),
                )
            })
            .collect();
    }
    payload
        .get("text")
        .and_then(Value::as_str)
        .map(|text| {
            let handle = payload
                .get("at")
                .and_then(Value::as_str)
                .unwrap_or(from)
                .to_owned();
            vec![(handle, text.to_owned())]
        })
        .unwrap_or_default()
}

/// Walks `read` from `from`, following `next`, and returns every `[handle, text]` pair.
pub async fn walk(client: &Client, from: &str, extra: Value) -> Vec<(String, String)> {
    let mut collected = Vec::new();
    let mut next = Some(from.to_owned());
    let mut pages = 0usize;
    while let Some(from) = next {
        let result = v2_read(client, &from, extra.clone()).await;
        let payload = payload(&result);
        assert!(
            result.is_error != Some(true),
            "read {from} failed: {payload}"
        );
        collected.extend(read_items(&payload, &from));
        next = next_of(&payload);
        pages += 1;
        assert!(pages < 100_000, "the walk must terminate");
    }
    collected
}

/// The section part of an item handle: `d3~9f2c.s12` of `d3~9f2c.s12.p4.2`.
pub fn section_of(handle: &str) -> String {
    handle
        .rsplit_once(".p")
        .map(|(head, _)| head)
        .unwrap_or(handle)
        .to_owned()
}

// ------------------------------------------------------------------------------------------------
// Fixtures
// ------------------------------------------------------------------------------------------------

/// An EPUB whose spine order is deliberately not the navigation order, with one table.
pub fn epub_book() -> Vec<u8> {
    let cursor = Cursor::new(Vec::new());
    let mut writer = ZipWriter::new(cursor);
    let options = SimpleFileOptions::default();
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
<h1 id="top">First Chapter</h1><p>Orbital memory appears in EPUB. Second EPUB sentence.</p>
<table><tr><td>cell</td></tr></table>
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

pub fn build_pdf(text: &str) -> Vec<u8> {
    let mut document = PdfDocument::with_version("1.5");
    let pages_id = document.new_object_id();
    let font_id = document.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => "Helvetica",
    });
    let resources_id = document.add_object(dictionary! {
        "Font" => dictionary! { "F1" => font_id },
    });
    let content = Content {
        operations: vec![
            Operation::new("BT", vec![]),
            Operation::new("Tf", vec!["F1".into(), 12.into()]),
            Operation::new("Td", vec![72.into(), 720.into()]),
            Operation::new("Tj", vec![Object::string_literal(text)]),
            Operation::new("ET", vec![]),
        ],
    };
    let content_id = document.add_object(Stream::new(
        dictionary! {},
        content.encode().expect("fixture content should encode"),
    ));
    let page_id = document.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => pages_id,
        "Contents" => content_id,
    });
    document.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => vec![Object::Reference(page_id)],
            "Count" => 1,
            "Resources" => resources_id,
            "MediaBox" => vec![0.into(), 0.into(), 595.into(), 842.into()],
        }),
    );
    let catalog_id = document.add_object(dictionary! {
        "Type" => "Catalog",
        "Pages" => pages_id,
    });
    document.trailer.set("Root", catalog_id);
    let mut bytes = Vec::new();
    document
        .save_to(&mut bytes)
        .expect("fixture PDF should serialize");
    bytes
}
