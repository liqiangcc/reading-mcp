use rmcp::ServiceExt;
use rmcp::model::CallToolRequestParams;
use rmcp::transport::TokioChildProcess;
use serde_json::{Map, Value, json};
use tokio::process::Command;

use reading_mcp::domain::{
    NORMALIZATION_VERSION, NORMALIZED_DOCUMENT_HASH_VERSION, NORMALIZED_TEXT_COORDINATE_SPACE,
};
use reading_mcp::mcp::contracts::{OpenDocumentResponse, ReadDocumentResponse};

#[tokio::test]
async fn stdio_open_and_read_expose_distinct_normalized_and_rendered_coordinate_contracts() {
    let directory = tempfile::tempdir().expect("temporary directory should be created");
    let document_path = directory.path().join("normalized.md");
    tokio::fs::write(
        &document_path,
        "# Normalized\n\nExact persisted text: A中🙂Z.\n",
    )
    .await
    .expect("fixture should be written");

    let local_roots = std::env::join_paths([directory.path()])
        .expect("temporary directory should be a valid local root list");
    let mut command = Command::new(env!("CARGO_BIN_EXE_reading-mcp"));
    command
        .env("READING_MCP_LOCAL_ROOTS", local_roots)
        .env("READING_MCP_STATE_DIR", "memory")
        .env("READING_MCP_TELEMETRY", "false");
    let transport = TokioChildProcess::new(command).expect("MCP process should start");
    let client = ().serve(transport).await.expect("MCP initialization should succeed");

    let opened = client
        .call_tool(
            CallToolRequestParams::new("open_document").with_arguments(arguments(json!({
                "source": document_path.to_string_lossy()
            }))),
        )
        .await
        .expect("document should open")
        .into_typed::<OpenDocumentResponse>()
        .expect("open response should be typed");

    assert!(opened.content_hash.starts_with("sha256:"));
    assert!(opened.normalized_document_hash.starts_with("sha256:"));
    assert_eq!(
        opened.normalized_document_hash_version,
        NORMALIZED_DOCUMENT_HASH_VERSION
    );
    assert_eq!(opened.normalization_version, NORMALIZATION_VERSION);
    assert_eq!(
        opened.normalized_text_coordinate_space,
        NORMALIZED_TEXT_COORDINATE_SPACE
    );

    let read = client
        .call_tool(
            CallToolRequestParams::new("read_document").with_arguments(arguments(json!({
                "document_id": opened.document_id,
                "section_id": "section://normalized",
                "max_chars": 8
            }))),
        )
        .await
        .expect("bounded read should succeed")
        .into_typed::<ReadDocumentResponse>()
        .expect("read response should be typed");

    assert_eq!(
        read.stream.coordinate_space,
        "section-tree-rendered-unicode-scalar/v1"
    );
    assert_ne!(
        read.stream.coordinate_space,
        opened.normalized_text_coordinate_space
    );

    client
        .cancel()
        .await
        .expect("MCP process should close cleanly");
}

fn arguments(value: Value) -> Map<String, Value> {
    value
        .as_object()
        .expect("tool arguments should be an object")
        .clone()
}
