use rmcp::ServiceExt;
use rmcp::model::CallToolRequestParams;
use rmcp::transport::TokioChildProcess;
use serde_json::{Map, Value, json};
use tokio::process::Command;

use reading_mcp::mcp::contracts::{
    GetContextResponse, OpenDocumentResponse, ReadDocumentResponse, SearchCandidateKindDto,
    SearchDocumentResponse,
};

#[tokio::test]
async fn stdio_sentence_search_hit_hands_exact_locator_directly_to_read_and_context() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let document_path = directory.path().join("search.md");
    tokio::fs::write(
        &document_path,
        "# Book\n\nOverview.\n\n## Topic\n\nFirst paragraph gives context.\n\nNeedle phrase is searchable here.\n\n### Child\n\nChild-only text.\n",
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

    let searched = client
        .call_tool(
            CallToolRequestParams::new("search_document").with_arguments(arguments(json!({
                "document_id": opened.document_id,
                "query": "needle phrase",
                "limit": 10
            }))),
        )
        .await
        .expect("search")
        .into_typed::<SearchDocumentResponse>()
        .expect("typed search");
    let hit = searched
        .hits
        .iter()
        .find(|hit| hit.candidate_kind == SearchCandidateKindDto::Sentence)
        .expect("sentence hit");
    assert_eq!(hit.section_id, "section://book/topic");
    assert_eq!(hit.text_locator.owner_section_id, hit.section_id);
    assert_eq!(hit.text_locator.paragraph_index, Some(2));
    assert_eq!(hit.text_locator.sentence_index, Some(1));
    assert!(hit.text_locator.normalized_range.is_some());

    let locator = serde_json::to_value(&hit.text_locator).expect("locator JSON");
    let read = client
        .call_tool(
            CallToolRequestParams::new("read_document").with_arguments(arguments(json!({
                "document_id": searched.document_id,
                "target_locator": locator
            }))),
        )
        .await
        .expect("exact read")
        .into_typed::<ReadDocumentResponse>()
        .expect("typed read");
    assert_eq!(read.stream.read_mode, "exact_target");
    assert_eq!(read.content, "Needle phrase is searchable here.");

    let locator = serde_json::to_value(&hit.text_locator).expect("locator JSON");
    let context = client
        .call_tool(
            CallToolRequestParams::new("get_context").with_arguments(arguments(json!({
                "document_id": searched.document_id,
                "target_locator": locator,
                "relation": {
                    "type": "neighbor",
                    "unit": "sentence",
                    "before": 1,
                    "after": 0
                }
            }))),
        )
        .await
        .expect("context")
        .into_typed::<GetContextResponse>()
        .expect("typed context");
    assert_eq!(context.items.len(), 2);
    assert_eq!(
        context.items[0].content.as_deref(),
        Some("First paragraph gives context.")
    );
    assert_eq!(
        context.items[1].content.as_deref(),
        Some("Needle phrase is searchable here.")
    );

    client.cancel().await.expect("close server");
}

fn arguments(value: Value) -> Map<String, Value> {
    value
        .as_object()
        .expect("tool arguments should be an object")
        .clone()
}
