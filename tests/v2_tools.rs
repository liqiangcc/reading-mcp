//! The v2 `browse`, `outline`, `search`, `cite` and `source_view` contracts: happy paths, every
//! error code they can produce, handle round-trips into `read`, and the stored locators `cite`
//! reports.

use std::io::{Cursor, Write};
use std::path::Path;

use lopdf::content::{Content, Operation};
use lopdf::{Document as PdfDocument, Object, Stream, dictionary};
use reading_mcp::domain::{ContentHash, DocumentId, DocumentSource};
use reading_mcp::mcp::contracts::{NormalizedRangeDto, TextLocatorDto};
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
    let entries: [(&str, &str); 5] = [
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
    let client = start_client(directory.path(), &directory.path().join("state")).await;

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
    let client = start_client(directory.path(), &directory.path().join("state")).await;
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
    let client = start_client(directory.path(), &directory.path().join("state")).await;
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

    // The first section handle reads at its first sentence, and the two cited locators agree: the
    // section locator names no sentence, the sentence locator names the same owner and identity.
    let first = items[1][0].as_str().expect("section handle").to_owned();
    let cite = payload(&call(&client, "cite", json!({ "at": [first.clone()] })).await);
    let section_locator = locator_of(&cite, 0);
    assert_eq!(section_locator.paragraph_index, None);
    assert_eq!(section_locator.sentence_index, None);
    let read = payload(&call(&client, "read", json!({ "from": first })).await);
    let sentence = read["at"]
        .as_str()
        .expect("the section's first item is its own handle")
        .to_owned();
    let sentence_cite = payload(&call(&client, "cite", json!({ "at": [sentence] })).await);
    let sentence_locator = locator_of(&sentence_cite, 0);
    assert_eq!(
        sentence_locator.owner_section_id,
        section_locator.owner_section_id
    );
    assert_eq!(
        sentence_locator.document_id, section_locator.document_id,
        "both locators name the stored document"
    );
    assert_eq!(sentence_locator.content_hash, section_locator.content_hash);
    assert!(
        sentence_locator.paragraph_index.is_some() && sentence_locator.sentence_index.is_some(),
        "a sentence locator names its paragraph and sentence: {sentence_locator:?}"
    );
    assert_eq!(
        flat_text(&read),
        "Alpha one is the first sentence.",
        "the section handle reads at its first sentence"
    );

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
    let client = start_client(directory.path(), &directory.path().join("state")).await;
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
    let client = start_client(directory.path(), &directory.path().join("state")).await;
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
        !found["sections"].as_object().expect("sections").is_empty(),
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
    let client = start_client(directory.path(), &directory.path().join("state")).await;
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
async fn cite_returns_the_stored_locator_of_the_sentence() {
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

    // The first sentence of the book: its cite locator is the exact locator the store keeps for
    // that sentence. These are the values the deleted v1 comparison produced on `main`
    // (`v1_locator == v1_sentence.items[0].locator`); a cite that returned a different paragraph,
    // sentence or range fails here.
    let read = payload(&call(&client, "read", json!({ "from": doc })).await);
    let sentence = read["at"]
        .as_str()
        .expect("the first item's own handle")
        .to_owned();
    let cite = payload(&call(&client, "cite", json!({ "at": [sentence.clone()] })).await);
    assert_eq!(cite["items"].as_array().expect("items").len(), 1);
    let locator = locator_of(&cite, 0);
    // The document id is derived independently of `cite`: the server's own rule over the canonical
    // source and the content hash (the v1 open response carried this same id on `main`).
    let content_hash = "sha256:83770fabe10fb5024a3c6eef2358b4210d223185692ce8e88fe46969a013627b";
    let document_id = DocumentId::derive(
        &DocumentSource(format!(
            "file://{}",
            path.canonicalize()
                .expect("the fixture has a canonical path")
                .to_string_lossy()
        )),
        &ContentHash(content_hash.to_owned()),
    );
    let expected_sentence = TextLocatorDto {
        document_id: document_id.0.clone(),
        content_hash: content_hash.into(),
        normalized_document_hash:
            "sha256:f53db39534433f381297ddfc6afb722097fdceb3e4178f176ae2bbf6733073f5".into(),
        owner_section_id: "section://book".into(),
        section_path: vec!["Book".into()],
        paragraph_index: Some(1),
        sentence_index: Some(1),
        normalized_range: Some(NormalizedRangeDto { start: 0, end: 22 }),
        segmentation_version: Some("text-segmentation/v3".into()),
        native_location: Some("markdown:line:1".into()),
    };
    assert_eq!(locator, expected_sentence);
    // The v1 open response's document id and content hash: `cite` must not report a different
    // identity than the one the locator was stored under.
    assert_eq!(
        cite["items"][0]["doc"]["id"].as_str(),
        Some(document_id.0.as_str()),
        "cite carries the document identity"
    );
    assert_eq!(
        cite["items"][0]["doc"]["content_hash"].as_str(),
        Some(content_hash),
        "cite carries the content hash"
    );
    assert!(
        cite["items"][0]["src"].is_string(),
        "cite carries a source position"
    );
    let again = payload(&call(&client, "cite", json!({ "at": [sentence] })).await);
    assert_eq!(
        locator_of(&again, 0),
        locator,
        "the same handle cites the same stored locator"
    );

    // A section handle cites the exact stored section locator the deleted v1 comparison asserted
    // (`section_locator == v1_section_units.target_section_locator`): no paragraph, no sentence and
    // no range, and the same document identity.
    let outline = payload(&call(&client, "outline", json!({ "at": doc, "depth": 2 })).await);
    let section = outline["items"][0][0].as_str().unwrap().to_owned();
    let cite = payload(&call(&client, "cite", json!({ "at": [section.clone()] })).await);
    let section_locator = locator_of(&cite, 0);
    let expected_section = TextLocatorDto {
        document_id: expected_sentence.document_id.clone(),
        content_hash: expected_sentence.content_hash.clone(),
        normalized_document_hash: expected_sentence.normalized_document_hash.clone(),
        owner_section_id: "section://book".into(),
        section_path: vec!["Book".into()],
        paragraph_index: None,
        sentence_index: None,
        normalized_range: None,
        segmentation_version: None,
        native_location: Some("markdown:line:1".into()),
    };
    assert_eq!(section_locator, expected_section);

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
    let client = start_client(directory.path(), &directory.path().join("state")).await;
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
    let client = start_client(directory.path(), &directory.path().join("state")).await;
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
    let client = start_client(directory.path(), &directory.path().join("state")).await;
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
// An EPUB source position is the file and anchor.
// ------------------------------------------------------------------------------------------------

#[tokio::test]
async fn cite_reports_the_epub_file_and_anchor() {
    let directory = tempfile::tempdir().expect("temporary directory should be created");
    let path = directory.path().join("book.epub");
    tokio::fs::write(&path, epub_book())
        .await
        .expect("fixture should be written");
    let client = start_client(directory.path(), &directory.path().join("state")).await;
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
