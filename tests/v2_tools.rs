//! The v2 `browse`, `outline`, `search`, `cite` and `source_view` contracts: happy paths, every
//! error code they can produce, handle round-trips into `read`, and `cite`/v1 locator equality.

use std::io::{Cursor, Write};
use std::path::Path;

use lopdf::content::{Content, Operation};
use lopdf::{Document as PdfDocument, Object, Stream, dictionary};
use reading_mcp::mcp::contracts::{GetTextUnitsResponse, OpenDocumentResponse, TextLocatorDto};
use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, CallToolResult, ContentBlock};
use rmcp::transport::TokioChildProcess;
use serde_json::{Map, Value, json};
use tokio::process::Command;
use zip::write::{SimpleFileOptions, ZipWriter};

type Client = rmcp::service::RunningService<rmcp::RoleClient, ()>;

// ------------------------------------------------------------------------------------------------
// Fixtures
// ------------------------------------------------------------------------------------------------

fn markdown_book() -> &'static str {
    "# Book\n\nIntroduction sentence. Second introduction sentence.\n\n## One\n\nAlpha one is the first sentence. Alpha two is the second sentence. Alpha three is the third sentence.\n\n### Nested\n\nNested one sentence. Nested two sentence.\n\n## Two\n\nBeta one is the first sentence. Beta two is the second sentence.\n"
}

fn epub_book() -> Vec<u8> {
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
  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:title>V2 Tools</dc:title></metadata>
  <manifest>
    <item id="nav" href="nav.xhtml" media-type="application/xhtml+xml" properties="nav"/>
    <item id="first" href="first.xhtml" media-type="application/xhtml+xml"/>
  </manifest>
  <spine><itemref idref="first"/></spine>
</package>"#,
        ),
        (
            "OPS/nav.xhtml",
            r#"<html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops"><body>
<nav epub:type="toc"><ol><li><a href="first.xhtml#top">First Chapter</a></li></ol></nav>
</body></html>"#,
        ),
        (
            "OPS/first.xhtml",
            r#"<html xmlns="http://www.w3.org/1999/xhtml"><body>
<h1 id="top">First Chapter</h1><p>First one. First two.</p>
</body></html>"#,
        ),
    ];
    for (name, content) in entries {
        writer.start_file(name, options).expect("ZIP entry");
        writer.write_all(content.as_bytes()).expect("ZIP content");
    }
    writer.finish().expect("ZIP finish").into_inner()
}

fn build_pdf() -> Vec<u8> {
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
            Operation::new("Tf", vec!["F1".into(), 14.into()]),
            Operation::new("Td", vec![72.into(), 720.into()]),
            Operation::new("Tj", vec![Object::string_literal("SOURCE VIEW SENTENCE.")]),
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

fn arguments(value: Value) -> Map<String, Value> {
    value.as_object().expect("an object").clone()
}

async fn call(client: &Client, name: &str, value: Value) -> CallToolResult {
    client
        .call_tool(CallToolRequestParams::new(name).with_arguments(arguments(value)))
        .await
        .unwrap_or_else(|error| panic!("{name} should answer: {error}"))
}

async fn v2_open(client: &Client, path: &Path) -> Value {
    payload(&call(client, "open", json!({ "source": path.to_string_lossy() })).await)
}

async fn v1_open(client: &Client, path: &Path) -> OpenDocumentResponse {
    call(
        client,
        "open_document",
        json!({ "source": path.to_string_lossy() }),
    )
    .await
    .into_typed::<OpenDocumentResponse>()
    .expect("open response should deserialize")
}

async fn v1_text_units(
    client: &Client,
    document_id: &str,
    section_id: &str,
    extra: Value,
) -> GetTextUnitsResponse {
    let mut value = Map::new();
    value.insert("document_id".into(), json!(document_id));
    value.insert("section_id".into(), json!(section_id));
    value.insert("requested_kind".into(), json!("sentence"));
    value.insert("direction".into(), json!("forward"));
    value.insert("coverage_policy".into(), json!("preserve_source"));
    value.insert("max_items".into(), json!(1));
    if let Some(extra) = extra.as_object() {
        for (key, item) in extra {
            value.insert(key.clone(), item.clone());
        }
    }
    client
        .call_tool(CallToolRequestParams::new("get_text_units").with_arguments(value))
        .await
        .expect("get_text_units should succeed")
        .into_typed::<GetTextUnitsResponse>()
        .expect("text-unit response should deserialize")
}

fn locator_of(cite: &Value, index: usize) -> TextLocatorDto {
    serde_json::from_value(cite["items"][index]["locator"].clone())
        .expect("cite returns a v1 TextLocator")
}

/// The single item text of a flat `read`.
fn flat_text(payload: &Value) -> String {
    payload
        .get("text")
        .and_then(Value::as_str)
        .expect("a flat read carries text")
        .to_owned()
}

// ------------------------------------------------------------------------------------------------
// browse
// ------------------------------------------------------------------------------------------------

#[tokio::test]
async fn browse_lists_documents_and_directories_as_compact_triples() {
    let directory = tempfile::tempdir().expect("temporary directory should be created");
    let path = directory.path().join("book.md");
    tokio::fs::write(&path, markdown_book())
        .await
        .expect("fixture should be written");
    tokio::fs::create_dir(directory.path().join("nested"))
        .await
        .expect("nested directory should be created");
    let client = start_client(
        directory.path(),
        &directory.path().join("state"),
        Some("v2"),
    )
    .await;

    // No path: the documents under the configured roots.
    let roots = payload(&call(&client, "browse", json!({})).await);
    let entries = roots.as_array().expect("browse answers with an array");
    assert_eq!(entries[0][0], "book.md");
    assert_eq!(entries[0][1], "doc");
    assert_eq!(entries[0][2], markdown_book().len());

    // A path: the directory's children, directories and documents.
    let listing = payload(
        &call(
            &client,
            "browse",
            json!({ "path": directory.path().to_string_lossy() }),
        )
        .await,
    );
    let entries = listing.as_array().expect("browse answers with an array");
    let names = entries
        .iter()
        .map(|entry| {
            (
                entry[0].as_str().unwrap().to_owned(),
                entry[1].as_str().unwrap().to_owned(),
            )
        })
        .collect::<Vec<_>>();
    assert!(names.contains(&("book.md".to_owned(), "doc".to_owned())));
    assert!(names.contains(&("nested".to_owned(), "dir".to_owned())));

    client.cancel().await.expect("client should close cleanly");
}

#[tokio::test]
async fn browse_rejects_a_path_outside_the_authorized_roots() {
    let directory = tempfile::tempdir().expect("temporary directory should be created");
    let client = start_client(
        directory.path(),
        &directory.path().join("state"),
        Some("v2"),
    )
    .await;
    let error = tool_error(&call(&client, "browse", json!({ "path": "/etc" })).await);
    assert_eq!(error["error"], "blocked_source");
    client.cancel().await.expect("client should close cleanly");
}

// ------------------------------------------------------------------------------------------------
// outline
// ------------------------------------------------------------------------------------------------

#[tokio::test]
async fn outline_lists_the_tree_and_its_handles_read() {
    let directory = tempfile::tempdir().expect("temporary directory should be created");
    let path = directory.path().join("book.md");
    tokio::fs::write(&path, markdown_book())
        .await
        .expect("fixture should be written");
    let client = start_client(
        directory.path(),
        &directory.path().join("state"),
        Some("both"),
    )
    .await;
    let opened = v1_open(&client, &path).await;
    let doc = v2_open(&client, &path).await["doc"]
        .as_str()
        .expect("doc handle")
        .to_owned();

    let outline = payload(
        &call(
            &client,
            "outline",
            json!({ "at": doc, "depth": 3, "limit": 100 }),
        )
        .await,
    );
    let items = outline["items"].as_array().expect("outline items");
    assert_eq!(
        items.len(),
        4,
        "two roots, one nested child and one tail: {outline}"
    );
    assert_eq!(items[0][1], "Book");
    assert_eq!(items[1][1], "One");
    assert_eq!(items[2][1], "Nested");
    assert_eq!(items[3][1], "Two");
    assert_eq!(items[1][2], 2);
    assert!(
        items[1][3].as_u64().unwrap() > 0,
        "a section reports its chars"
    );

    // The first section handle reads at its first sentence, exactly like v1.
    let first = items[1][0].as_str().expect("section handle").to_owned();
    let cite = payload(&call(&client, "cite", json!({ "at": [first.clone()] })).await);
    let locator = locator_of(&cite, 0);
    assert_eq!(locator.paragraph_index, None);
    let v1 = v1_text_units(
        &client,
        &opened.document_id,
        &locator.owner_section_id,
        json!({}),
    )
    .await;
    let read = payload(&call(&client, "read", json!({ "from": first })).await);
    assert_eq!(flat_text(&read), v1.items[0].text);

    // A truncated outline continues with its cursor and repeats nothing.
    let page = payload(
        &call(
            &client,
            "outline",
            json!({ "at": doc, "depth": 3, "limit": 1 }),
        )
        .await,
    );
    let cursor = page["next"].as_str().expect("a next cursor").to_owned();
    assert!(
        cursor.starts_with("o:"),
        "outline next is an o: cursor: {cursor}"
    );
    let second = payload(
        &call(
            &client,
            "outline",
            json!({ "at": doc, "depth": 3, "limit": 1, "cursor": cursor }),
        )
        .await,
    );
    assert_eq!(second["items"][0][0], items[1][0]);

    // A cursor issued for another depth is stale.
    let mut tampered = page["next"].as_str().unwrap().to_owned();
    let last = tampered.pop().unwrap();
    tampered.push(if last == '0' { '1' } else { '0' });
    let error = tool_error(
        &call(
            &client,
            "outline",
            json!({ "at": doc, "depth": 3, "cursor": tampered }),
        )
        .await,
    );
    assert_eq!(error["error"], "stale_handle");

    client.cancel().await.expect("client should close cleanly");
}

#[tokio::test]
async fn outline_errors_use_the_v2_vocabulary() {
    let directory = tempfile::tempdir().expect("temporary directory should be created");
    let path = directory.path().join("book.md");
    tokio::fs::write(&path, markdown_book())
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

    for request in [
        json!({ "at": doc, "depth": 0 }),
        json!({ "at": doc, "limit": 0 }),
        json!({ "at": "not-a-handle" }),
    ] {
        let error = tool_error(&call(&client, "outline", request).await);
        assert_eq!(error["error"], "invalid_argument", "got {error}");
    }

    // A paragraph handle is a child level, not an outline root.
    let read = payload(&call(&client, "read", json!({ "from": doc })).await);
    let sentence = read["at"]
        .as_str()
        .or_else(|| read["next"].as_str())
        .expect("a sentence handle")
        .to_owned();
    let error = tool_error(&call(&client, "outline", json!({ "at": sentence })).await);
    assert_eq!(error["error"], "invalid_argument");

    let error = tool_error(&call(&client, "outline", json!({ "at": "d999999~abcd" })).await);
    assert_eq!(error["error"], "unknown_handle");

    let (head, _) = doc.split_once('~').expect("a checksum");
    let wrong = format!("{head}~0000");
    let error = tool_error(&call(&client, "outline", json!({ "at": wrong })).await);
    assert_eq!(error["error"], "stale_handle");

    client.cancel().await.expect("client should close cleanly");
}

// ------------------------------------------------------------------------------------------------
// search
// ------------------------------------------------------------------------------------------------

#[tokio::test]
async fn search_returns_sentence_handles_that_read_at_the_match() {
    let directory = tempfile::tempdir().expect("temporary directory should be created");
    let path = directory.path().join("book.md");
    tokio::fs::write(&path, markdown_book())
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

    let found = payload(
        &call(
            &client,
            "search",
            json!({ "doc": doc, "query": "Alpha", "limit": 10 }),
        )
        .await,
    );
    let hits = found["hits"].as_array().expect("hits");
    assert_eq!(hits.len(), 3, "three Alpha sentences: {found}");
    assert_eq!(found["total"], 3);
    assert!(
        found["sections"].as_object().expect("sections").len() >= 1,
        "the sections map names the hit sections"
    );
    for hit in hits {
        let handle = hit[0].as_str().expect("sentence handle");
        let snippet = hit[1].as_str().expect("snippet");
        assert!(snippet.chars().count() <= 120);
        assert!(snippet.contains("Alpha"), "the snippet carries the match");
        let read = payload(&call(&client, "read", json!({ "from": handle })).await);
        assert!(
            flat_text(&read).contains("Alpha"),
            "the hit handle reads its own sentence: {read}"
        );
    }

    client.cancel().await.expect("client should close cleanly");
}

#[tokio::test]
async fn search_errors_use_the_v2_vocabulary() {
    let directory = tempfile::tempdir().expect("temporary directory should be created");
    let path = directory.path().join("book.md");
    tokio::fs::write(&path, markdown_book())
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

    let error = tool_error(
        &call(
            &client,
            "search",
            json!({ "doc": doc, "query": "Alpha", "limit": 0 }),
        )
        .await,
    );
    assert_eq!(error["error"], "invalid_argument");

    let error = tool_error(
        &call(
            &client,
            "search",
            json!({ "doc": "d999999~abcd", "query": "Alpha" }),
        )
        .await,
    );
    assert_eq!(error["error"], "unknown_handle");

    let error = tool_error(
        &call(
            &client,
            "search",
            json!({ "doc": "nope", "query": "Alpha" }),
        )
        .await,
    );
    assert_eq!(error["error"], "invalid_argument");

    let error = tool_error(&call(&client, "search", json!({ "doc": doc, "query": "" })).await);
    assert_eq!(error["error"], "invalid_argument");

    client.cancel().await.expect("client should close cleanly");
}

// ------------------------------------------------------------------------------------------------
// cite
// ------------------------------------------------------------------------------------------------

#[tokio::test]
async fn cite_returns_the_same_locator_v1_produces_for_that_sentence() {
    let directory = tempfile::tempdir().expect("temporary directory should be created");
    let path = directory.path().join("book.md");
    tokio::fs::write(&path, markdown_book())
        .await
        .expect("fixture should be written");
    let client = start_client(
        directory.path(),
        &directory.path().join("state"),
        Some("both"),
    )
    .await;
    let opened = v1_open(&client, &path).await;
    let doc = v2_open(&client, &path).await["doc"]
        .as_str()
        .expect("doc handle")
        .to_owned();

    // The first sentence of the book, as v1 reports it.
    let read = payload(&call(&client, "read", json!({ "from": doc })).await);
    let sentence = read["at"]
        .as_str()
        .expect("the first item's own handle")
        .to_owned();
    let cite = payload(&call(&client, "cite", json!({ "at": [sentence] })).await);
    assert_eq!(cite["items"].as_array().expect("items").len(), 1);
    let v1_locator = locator_of(&cite, 0);
    let v1_sentence = v1_text_units(
        &client,
        &opened.document_id,
        &v1_locator.owner_section_id,
        json!({}),
    )
    .await;
    assert_eq!(v1_locator, v1_sentence.items[0].locator);
    assert_eq!(
        cite["items"][0]["doc"]["id"].as_str(),
        Some(opened.document_id.as_str()),
        "cite carries the document identity"
    );
    assert_eq!(
        cite["items"][0]["doc"]["content_hash"].as_str(),
        Some(opened.content_hash.as_str())
    );
    assert!(
        cite["items"][0]["src"].is_string(),
        "cite carries a source position"
    );

    // A section handle cites the v1 section locator.
    let outline = payload(&call(&client, "outline", json!({ "at": doc, "depth": 2 })).await);
    let section = outline["items"][0][0].as_str().unwrap().to_owned();
    let cite = payload(&call(&client, "cite", json!({ "at": [section.clone()] })).await);
    let section_locator = locator_of(&cite, 0);
    let v1_section_units = v1_text_units(
        &client,
        &opened.document_id,
        &section_locator.owner_section_id,
        json!({}),
    )
    .await;
    assert_eq!(section_locator, v1_section_units.target_section_locator);

    // Several handles in one call, in request order.
    let cite = payload(
        &call(
            &client,
            "cite",
            json!({ "at": [section.clone(), doc.clone()] }),
        )
        .await,
    );
    assert_eq!(cite["items"].as_array().unwrap().len(), 2);
    assert_eq!(cite["items"][0]["at"].as_str(), Some(section.as_str()));
    assert_eq!(cite["items"][1]["at"].as_str(), Some(doc.as_str()));

    client.cancel().await.expect("client should close cleanly");
}

#[tokio::test]
async fn cite_errors_use_the_v2_vocabulary() {
    let directory = tempfile::tempdir().expect("temporary directory should be created");
    let path = directory.path().join("book.md");
    tokio::fs::write(&path, markdown_book())
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

    let error = tool_error(&call(&client, "cite", json!({ "at": [] })).await);
    assert_eq!(error["error"], "invalid_argument");

    let error = tool_error(&call(&client, "cite", json!({ "at": ["nope"] })).await);
    assert_eq!(error["error"], "invalid_argument");

    let error = tool_error(&call(&client, "cite", json!({ "at": ["d999999~abcd"] })).await);
    assert_eq!(error["error"], "unknown_handle");

    let (head, _) = doc.split_once('~').expect("a checksum");
    let wrong = format!("{head}~0000");
    let error = tool_error(&call(&client, "cite", json!({ "at": [wrong] })).await);
    assert_eq!(error["error"], "stale_handle");

    client.cancel().await.expect("client should close cleanly");
}

// ------------------------------------------------------------------------------------------------
// source_view
// ------------------------------------------------------------------------------------------------

#[tokio::test]
async fn source_view_renders_the_page_of_a_handle() {
    let directory = tempfile::tempdir().expect("temporary directory should be created");
    let path = directory.path().join("fidelity.pdf");
    tokio::fs::write(&path, build_pdf())
        .await
        .expect("PDF fixture should be written");
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

    let result = call(&client, "source_view", json!({ "at": doc })).await;
    assert!(
        result.is_error != Some(true),
        "source view should render: {result:?}"
    );
    let metadata = payload(&result);
    assert_eq!(metadata["page"], 1);
    assert!(metadata["pages"].as_u64().unwrap() >= 1);
    assert!(
        result
            .content
            .iter()
            .any(|block| matches!(block, ContentBlock::Image(_))),
        "source view returns the page image"
    );
    assert!(
        result.structured_content.is_none(),
        "v2 source_view never sets structuredContent"
    );

    client.cancel().await.expect("client should close cleanly");
}

#[tokio::test]
async fn source_view_errors_use_the_v2_vocabulary() {
    let directory = tempfile::tempdir().expect("temporary directory should be created");
    let path = directory.path().join("book.md");
    tokio::fs::write(&path, markdown_book())
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

    // A non-PDF source has no original page render.
    let error = tool_error(&call(&client, "source_view", json!({ "at": doc })).await);
    assert_eq!(error["error"], "invalid_argument");

    let error = tool_error(&call(&client, "source_view", json!({ "at": "d999999~abcd" })).await);
    assert_eq!(error["error"], "unknown_handle");

    let error = tool_error(&call(&client, "source_view", json!({ "at": "nope" })).await);
    assert_eq!(error["error"], "invalid_argument");

    client.cancel().await.expect("client should close cleanly");
}

// ------------------------------------------------------------------------------------------------
// The switch: v1 stays byte-identical, v2 has exactly the seven tools.
// ------------------------------------------------------------------------------------------------

#[tokio::test]
async fn v2_registers_exactly_the_seven_tools_and_v1_stays_default() {
    let directory = tempfile::tempdir().expect("temporary directory should be created");

    let default = start_client(directory.path(), &directory.path().join("default"), None).await;
    let names = default
        .list_all_tools()
        .await
        .expect("tools/list should succeed")
        .into_iter()
        .map(|tool| tool.name.to_string())
        .collect::<Vec<_>>();
    assert_eq!(names.len(), 9, "the default tool set is v1");
    default.cancel().await.expect("client should close cleanly");

    let v2 = start_client(directory.path(), &directory.path().join("v2"), Some("v2")).await;
    let names = v2
        .list_all_tools()
        .await
        .expect("tools/list should succeed")
        .into_iter()
        .map(|tool| tool.name.to_string())
        .collect::<Vec<_>>();
    assert_eq!(
        names,
        vec![
            "browse",
            "cite",
            "open",
            "outline",
            "read",
            "search",
            "source_view"
        ]
    );
    v2.cancel().await.expect("client should close cleanly");
}

// ------------------------------------------------------------------------------------------------
// An EPUB source position is the file and anchor.
// ------------------------------------------------------------------------------------------------

#[tokio::test]
async fn cite_reports_the_epub_file_and_anchor() {
    let directory = tempfile::tempdir().expect("temporary directory should be created");
    let path = directory.path().join("book.epub");
    tokio::fs::write(&path, epub_book())
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
    let outline = payload(&call(&client, "outline", json!({ "at": doc, "depth": 2 })).await);
    let section = outline["items"][0][0].as_str().unwrap().to_owned();
    let cite = payload(&call(&client, "cite", json!({ "at": [section] })).await);
    let source = cite["items"][0]["src"]
        .as_str()
        .expect("an EPUB source position");
    assert!(
        source.starts_with("epub:") && source.contains("first.xhtml"),
        "the source position names the EPUB file: {source}"
    );
    client.cancel().await.expect("client should close cleanly");
}
