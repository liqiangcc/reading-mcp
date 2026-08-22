use rmcp::ServiceExt;
use rmcp::model::CallToolRequestParams;
use rmcp::transport::TokioChildProcess;
use serde_json::{Map, Value, json};
use tokio::process::Command;

use reading_mcp::mcp::contracts::{
    GetTextUnitsResponse, OpenDocumentResponse, ReadDocumentResponse,
};

#[tokio::test]
async fn stdio_text_locator_hands_off_directly_to_exact_read_with_continuation() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let document_path = directory.path().join("precise.md");
    let paragraph = "Exact locator reads preserve this entire paragraph across a deliberately small response budget without turning rendered offsets into source identity.";
    tokio::fs::write(
        &document_path,
        format!("# Book\n\n## Topic\n\n{paragraph}\n\nSecond paragraph.\n"),
    )
    .await
    .expect("fixture");

    let local_roots = std::env::join_paths([directory.path()]).expect("local root");
    let mut command = Command::new(env!("CARGO_BIN_EXE_reading-mcp"));
    command
        .env("READING_MCP_LOCAL_ROOTS", local_roots)
        .env("READING_MCP_STATE_DIR", "memory")
        .env("READING_MCP_TELEMETRY", "false");
    let transport = TokioChildProcess::new(command).expect("server process");
    let client = ().serve(transport).await.expect("MCP initialize");

    let opened = client
        .call_tool(
            CallToolRequestParams::new("open_document").with_arguments(arguments(json!({
                "source": document_path.to_string_lossy()
            }))),
        )
        .await
        .expect("open")
        .into_typed::<OpenDocumentResponse>()
        .expect("typed open");

    let paragraphs = client
        .call_tool(
            CallToolRequestParams::new("get_text_units").with_arguments(arguments(json!({
                "document_id": opened.document_id,
                "section_id": "section://book/topic",
                "requested_kind": "paragraph",
                "max_items": 10
            }))),
        )
        .await
        .expect("paragraph enumeration")
        .into_typed::<GetTextUnitsResponse>()
        .expect("typed paragraphs");
    assert_eq!(paragraphs.items.len(), 2);
    let target = serde_json::to_value(&paragraphs.items[0].locator).expect("locator JSON");

    let mut page = client
        .call_tool(
            CallToolRequestParams::new("read_document").with_arguments(arguments(json!({
                "document_id": paragraphs.document_id,
                "target_locator": target,
                "max_chars": 23
            }))),
        )
        .await
        .expect("exact read")
        .into_typed::<ReadDocumentResponse>()
        .expect("typed exact read");
    assert_eq!(page.stream.read_mode, "exact_target");
    assert_eq!(
        page.stream.coordinate_space,
        "exact-target-unicode-scalar/v1"
    );
    assert!(page.returned_locator.is_some());
    assert!(!page.complete);

    let target = serde_json::to_value(&page.resolved_target_locator).expect("resolved target JSON");
    let mut rebuilt = String::new();
    let mut previous_end = 0usize;
    loop {
        assert_eq!(page.stream.start_char, previous_end);
        rebuilt.push_str(&page.content);
        previous_end = page.stream.end_char;
        if page.complete {
            assert!(page.next_cursor.is_none());
            break;
        }
        let cursor = page.next_cursor.clone().expect("continuation cursor");
        page = client
            .call_tool(
                CallToolRequestParams::new("read_document").with_arguments(arguments(json!({
                    "document_id": page.document_id,
                    "target_locator": target,
                    "cursor": cursor,
                    "max_chars": 19
                }))),
            )
            .await
            .expect("exact continuation")
            .into_typed::<ReadDocumentResponse>()
            .expect("typed exact continuation");
    }
    assert_eq!(rebuilt, paragraph);

    let legacy = client
        .call_tool(
            CallToolRequestParams::new("read_document").with_arguments(arguments(json!({
                "document_id": page.document_id,
                "section_id": "section://book/topic",
                "max_chars": 4000
            }))),
        )
        .await
        .expect("legacy read")
        .into_typed::<ReadDocumentResponse>()
        .expect("typed legacy read");
    assert_eq!(legacy.stream.read_mode, "section_tree");
    assert!(legacy.content.contains("## Topic"));
    assert!(legacy.returned_locator.is_none());
    assert_eq!(
        legacy.resolved_target_locator.owner_section_id,
        "section://book/topic"
    );

    client.cancel().await.expect("close server");
}

fn arguments(value: Value) -> Map<String, Value> {
    value
        .as_object()
        .expect("tool arguments should be an object")
        .clone()
}
