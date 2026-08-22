use rmcp::ServiceExt;
use rmcp::model::CallToolRequestParams;
use rmcp::transport::TokioChildProcess;
use serde_json::{Map, Value, json};
use tokio::process::Command;

use reading_mcp::mcp::contracts::{
    ContextItemRoleDto, GetContextResponse, GetTextUnitsResponse, OpenDocumentResponse,
};

#[tokio::test]
async fn stdio_context_consumes_text_locator_with_tagged_relations() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let document_path = directory.path().join("context.md");
    tokio::fs::write(
        &document_path,
        "# Book\n\n## Topic\n\nFirst sentence. Second sentence.\n\nThird paragraph sentence.\n",
    )
    .await
    .expect("fixture");

    let local_roots =
        std::env::join_paths([directory.path()]).expect("local root should be valid");
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

    let units = client
        .call_tool(
            CallToolRequestParams::new("get_text_units").with_arguments(arguments(json!({
                "document_id": opened.document_id,
                "section_id": "section://book/topic",
                "requested_kind": "sentence",
                "coverage_policy": "preserve_source",
                "max_items": 10
            }))),
        )
        .await
        .expect("text units")
        .into_typed::<GetTextUnitsResponse>()
        .expect("typed units");
    assert_eq!(units.items.len(), 3);
    let anchor = serde_json::to_value(&units.items[1].locator).expect("locator json");

    let neighbors = client
        .call_tool(
            CallToolRequestParams::new("get_context").with_arguments(arguments(json!({
                "document_id": units.document_id,
                "target_locator": anchor,
                "relation": {
                    "type": "neighbor",
                    "unit": "sentence",
                    "before": 1,
                    "after": 1
                }
            }))),
        )
        .await
        .expect("sentence context")
        .into_typed::<GetContextResponse>()
        .expect("typed context");

    assert_eq!(neighbors.items.len(), 3);
    assert_eq!(neighbors.items[0].role, ContextItemRoleDto::Before);
    assert_eq!(neighbors.items[1].role, ContextItemRoleDto::Anchor);
    assert_eq!(neighbors.items[2].role, ContextItemRoleDto::After);
    assert_eq!(
        neighbors.items[1].content.as_deref(),
        Some("Second sentence.")
    );
    assert!(neighbors.complete);

    let container = client
        .call_tool(
            CallToolRequestParams::new("get_context").with_arguments(arguments(json!({
                "document_id": neighbors.document_id,
                "target_locator": serde_json::to_value(&neighbors.anchor_locator)
                    .expect("anchor locator json"),
                "relation": {
                    "type": "container",
                    "kind": "paragraph"
                }
            }))),
        )
        .await
        .expect("paragraph container")
        .into_typed::<GetContextResponse>()
        .expect("typed container");
    assert_eq!(container.items.len(), 1);
    assert_eq!(
        container.items[0].content.as_deref(),
        Some("First sentence. Second sentence.")
    );

    let legacy = client
        .call_tool(
            CallToolRequestParams::new("get_context").with_arguments(arguments(json!({
                "document_id": container.document_id,
                "section_id": "section://book/topic",
                "before": 0,
                "after": 0
            }))),
        )
        .await
        .expect("legacy context")
        .into_typed::<GetContextResponse>()
        .expect("typed legacy context");
    assert!(legacy.content.contains("## Topic"));
    assert!(legacy.content.contains("First sentence."));
    assert_eq!(legacy.items.len(), 1);

    client.cancel().await.expect("close server");
}

fn arguments(value: Value) -> Map<String, Value> {
    value
        .as_object()
        .expect("tool arguments should be an object")
        .clone()
}
