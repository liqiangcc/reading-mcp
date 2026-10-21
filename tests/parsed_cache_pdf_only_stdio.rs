use rmcp::ServiceExt;
use rmcp::model::CallToolRequestParams;
use rmcp::transport::TokioChildProcess;
use serde_json::{Map, Value, json};
use tokio::process::Command;

use reading_mcp::mcp::contracts::OpenDocumentResponse;

#[tokio::test]
async fn opening_markdown_through_stdio_writes_no_parsed_cache_entry() {
    let directory = tempfile::tempdir().expect("temporary directory should be created");
    let document_path = directory.path().join("operating-systems.md");
    tokio::fs::write(
        &document_path,
        "# Operating Systems\n\n## Virtual Memory\n\nPage tables map memory.\n",
    )
    .await
    .expect("Markdown fixture should be written");
    let state = directory.path().join("state");

    let local_roots = std::env::join_paths([directory.path()])
        .expect("temporary directory should be a valid local root list");
    let mut command = Command::new(env!("CARGO_BIN_EXE_reading-mcp"));
    command
        .env("READING_MCP_LOCAL_ROOTS", local_roots)
        .env("READING_MCP_STATE_DIR", &state)
        .env("READING_MCP_TELEMETRY", "false");
    let transport =
        TokioChildProcess::new(command).expect("reading-mcp child process should start");
    let client = ().serve(transport).await.expect("MCP initialization should succeed");

    let opened = client
        .call_tool(
            CallToolRequestParams::new("open_document").with_arguments(arguments(json!({
                "source": document_path.to_string_lossy(),
                "force_refresh": false
            }))),
        )
        .await
        .expect("open_document MCP call should succeed")
        .into_typed::<OpenDocumentResponse>()
        .expect("open_document should return typed structured content");
    assert_eq!(opened.media_type, "text/markdown");

    for directory_name in ["parsed", "parsed-v2"] {
        let entries = std::fs::read_dir(state.join("cache").join(directory_name))
            .map(|entries| {
                entries
                    .flatten()
                    .map(|entry| entry.path())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        assert!(
            entries.is_empty(),
            "opening Markdown must not write {directory_name} entries: {entries:?}"
        );
    }

    let reopened = client
        .call_tool(
            CallToolRequestParams::new("open_document").with_arguments(arguments(json!({
                "source": document_path.to_string_lossy(),
                "force_refresh": false
            }))),
        )
        .await
        .expect("reopening the same document should succeed")
        .into_typed::<OpenDocumentResponse>()
        .expect("reopen should return typed structured content");
    assert_eq!(reopened, opened);

    client.cancel().await.unwrap();
}

fn arguments(value: Value) -> Map<String, Value> {
    value
        .as_object()
        .expect("tool arguments should be a JSON object")
        .clone()
}
