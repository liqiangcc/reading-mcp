//! V2-4b PR-2, §5: ports `tests/source_view_http.rs` to the v2 `source_view` tool over streamable
//! HTTP — the only HTTP source-view test.
//!
//! The v1 test asked `get_source_view` for the original page of a sentence locator and asserted the
//! audit metadata. v2 `source_view` answers `{at, page, pages}` plus the PNG image block; the
//! normalized-hash version the v1 response carried is asserted through `cite`, and
//! `source_binding_version` is a v1 response field with no v2 name (§2.1 item 3).

#[path = "support/v2_mcp.rs"]
mod v2_mcp;

use lopdf::content::{Content, Operation};
use lopdf::{Document as PdfDocument, Object, Stream, dictionary};
use reading_mcp::domain::NORMALIZED_DOCUMENT_HASH_VERSION;
use serde_json::json;

use v2_mcp::{
    call, flat_text, http_server_command, identity_of, locator_of, payload, reserve_port,
    start_http_client, v2_open, v2_read, wait_until_ready,
};

#[tokio::test]
async fn v2_streamable_http_source_view_returns_bound_original_png() {
    let root = tempfile::tempdir().expect("source root should be created");
    let state = tempfile::tempdir().expect("state root should be created");
    let path = root.path().join("fidelity.pdf");
    tokio::fs::write(&path, build_pdf())
        .await
        .expect("PDF fixture should be written");

    let port = reserve_port();
    let base = format!("http://127.0.0.1:{port}");
    let mut server = http_server_command(port, root.path(), state.path())
        .spawn()
        .expect("HTTP server should start");
    wait_until_ready(&base, &mut server).await;

    let client = start_http_client(&base).await;
    let opened = v2_open(&client, &path).await;
    let doc = opened["doc"].as_str().expect("doc handle").to_owned();

    let sentence = payload(&v2_read(&client, &doc, json!({})).await);
    assert!(
        sentence.get("items").is_none(),
        "the v1 max_items=1 page is one flat item: {sentence}"
    );
    assert_eq!(flat_text(&sentence), "HTTP SOURCE VIEW SENTENCE.");
    let sentence = sentence["at"]
        .as_str()
        .expect("the first sentence's own handle")
        .to_owned();
    let locator = locator_of(&client, &sentence).await;
    assert_eq!(locator["owner_section_id"], "section://page-1");

    let result = call(&client, "source_view", json!({ "at": sentence })).await;
    assert!(
        result.is_error != Some(true),
        "source view should render through HTTP MCP: {result:?}"
    );
    let wire = serde_json::to_value(&result).expect("tool result should serialize");
    let content = wire["content"]
        .as_array()
        .expect("tool result content should be an array");
    assert!(
        content
            .iter()
            .any(|block| block["type"] == "image" && block["mimeType"] == "image/png"),
        "the page renders as a PNG image block: {wire}"
    );

    let metadata = payload(&result);
    assert_eq!(metadata["page"], 1, "{metadata}");
    assert_eq!(metadata["pages"], 1, "{metadata}");

    let identity = identity_of(&client, &sentence).await;
    assert_eq!(
        identity["ndh"], NORMALIZED_DOCUMENT_HASH_VERSION,
        "the v1 normalized_document_hash_version"
    );
    assert_eq!(
        identity["content_hash"], locator["content_hash"],
        "the rendered page is bound to the cited locator's document"
    );

    client
        .cancel()
        .await
        .expect("HTTP client session should close cleanly");
    server.kill().await.expect("HTTP server should stop");
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
            Operation::new(
                "Tj",
                vec![Object::string_literal("HTTP SOURCE VIEW SENTENCE.")],
            ),
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
