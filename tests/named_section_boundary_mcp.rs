use reading_mcp::mcp::contracts::{
    GetDocumentStructureResponse, NamedSectionResolutionStatusDto, OpenDocumentResponse,
};
use rmcp::ServiceExt;
use rmcp::model::CallToolRequestParams;
use rmcp::transport::TokioChildProcess;
use serde_json::{Map, Value, json};
use tokio::process::Command;

#[tokio::test]
async fn stdio_named_structure_resolution_returns_boundary_without_body_leakage() {
    let directory = tempfile::tempdir().expect("temporary document directory should be created");
    let document_path = directory.path().join("named-boundary.md");
    tokio::fs::write(
        &document_path,
        r#"# 1 Introduction

INTRO_BODY_SENTINEL_SHOULD_NOT_APPEAR_IN_STRUCTURE.

## 1.1 Scope

CHILD_BODY_SENTINEL_SHOULD_NOT_APPEAR_IN_STRUCTURE.

# 2 Future

FUTURE_BODY_SENTINEL_SHOULD_NOT_APPEAR_IN_STRUCTURE.
"#,
    )
    .await
    .expect("Markdown fixture should be written");

    let local_roots = std::env::join_paths([directory.path()])
        .expect("temporary directory should be a valid local root list");
    let mut command = Command::new(env!("CARGO_BIN_EXE_reading-mcp"));
    command
        .env("READING_MCP_LOCAL_ROOTS", local_roots)
        .env("READING_MCP_STATE_DIR", "memory")
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
        .expect("open_document should succeed")
        .into_typed::<OpenDocumentResponse>()
        .expect("open response should deserialize");

    let raw = client
        .call_tool(
            CallToolRequestParams::new("get_document_structure").with_arguments(arguments(json!({
                "document_id": opened.document_id,
                "max_nodes": 10,
                "named_section_query": "Section 1 — Introduction",
                "expected_content_hash": opened.content_hash,
                "expected_normalized_document_hash": opened.normalized_document_hash,
                "expected_structure_resolution_version": "named-section-resolution/v1"
            }))),
        )
        .await
        .expect("named structure lookup should succeed");

    let structured = raw
        .structured_content
        .as_ref()
        .expect("get_document_structure should return structured content")
        .to_string();
    for sentinel in [
        "INTRO_BODY_SENTINEL_SHOULD_NOT_APPEAR_IN_STRUCTURE",
        "CHILD_BODY_SENTINEL_SHOULD_NOT_APPEAR_IN_STRUCTURE",
        "FUTURE_BODY_SENTINEL_SHOULD_NOT_APPEAR_IN_STRUCTURE",
    ] {
        assert!(
            !structured.contains(sentinel),
            "structure-only response leaked body sentinel {sentinel}"
        );
    }

    let response = raw
        .into_typed::<GetDocumentStructureResponse>()
        .expect("structure response should deserialize");
    assert_eq!(
        response.normalization_version,
        "reading-mcp-normalization/v8"
    );
    let resolution = response
        .resolution
        .expect("named-section resolution metadata should be present");
    assert_eq!(resolution.status, NamedSectionResolutionStatusDto::Resolved);
    let matched = resolution
        .matched
        .expect("resolved metadata should include match");
    assert_eq!(matched.title, "1 Introduction");
    assert!(matched.start_locator.normalized_range.is_none());
    let boundary = resolution
        .boundary
        .expect("resolved scope should include executable boundary");
    assert_eq!(boundary.intervals.len(), 1);
    let next = boundary
        .end_exclusive
        .expect("contiguous scope should expose next owner metadata");
    assert_eq!(next.title, "2 Future");
    assert!(
        boundary
            .intervals
            .iter()
            .all(|interval| next.body_order < interval.start || next.body_order >= interval.end)
    );

    client
        .cancel()
        .await
        .expect("MCP child process should close cleanly");
}

fn arguments(value: Value) -> Map<String, Value> {
    value
        .as_object()
        .expect("tool arguments should be a JSON object")
        .clone()
}
