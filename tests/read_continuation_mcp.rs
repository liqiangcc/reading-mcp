use rmcp::ServiceExt;
use rmcp::model::CallToolRequestParams;
use rmcp::transport::TokioChildProcess;
use serde_json::{Map, Value, json};
use tokio::process::Command;

use reading_mcp::mcp::contracts::{OpenDocumentResponse, ReadDocumentResponse};

#[tokio::test]
async fn stdio_client_continues_a_section_tree_until_complete() {
    let directory = tempfile::tempdir().expect("temporary directory should be created");
    let document_path = directory.path().join("continuation.md");
    tokio::fs::write(
        &document_path,
        r#"# Continuation

A long first paragraph with enough content to cross several deliberately small response windows. Unicode stays ordered: 系统调用、进程、内存、🙂.

A second paragraph follows the first paragraph exactly once.

## Child

Child section content is part of the legacy section-tree stream.
"#,
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
    let section_id = "section://continuation";

    let full = read(
        &client,
        &opened.document_id,
        section_id,
        None,
        Some(4_000),
    )
    .await;
    assert!(full.complete);
    assert!(!full.truncated);
    assert!(full.next_cursor.is_none());

    let mut segment = read(
        &client,
        &opened.document_id,
        section_id,
        None,
        Some(29),
    )
    .await;
    let total_chars = segment.stream.total_chars;
    let mut end = 0;
    let mut reconstructed = String::new();
    let mut calls = 0;

    loop {
        calls += 1;
        assert!(calls < 100, "cursor must make finite progress");
        assert_eq!(segment.stream.start_char, end);
        assert_eq!(segment.stream.total_chars, total_chars);
        assert_eq!(segment.truncated, !segment.complete);
        reconstructed.push_str(&segment.content);
        end = segment.stream.end_char;

        if segment.complete {
            assert!(segment.next_cursor.is_none());
            break;
        }

        let cursor = segment
            .next_cursor
            .clone()
            .expect("incomplete response must return next_cursor");
        segment = read(
            &client,
            &opened.document_id,
            section_id,
            Some(cursor),
            Some(23),
        )
        .await;
    }

    assert_eq!(end, total_chars);
    assert_eq!(reconstructed, full.content);

    client
        .cancel()
        .await
        .expect("MCP process should close cleanly");
}

async fn read(
    client: &rmcp::service::RunningService<rmcp::RoleClient, ()>,
    document_id: &str,
    section_id: &str,
    cursor: Option<String>,
    max_chars: Option<usize>,
) -> ReadDocumentResponse {
    let mut request = json!({
        "document_id": document_id,
        "section_id": section_id,
    });
    if let Some(cursor) = cursor {
        request["cursor"] = Value::String(cursor);
    }
    if let Some(max_chars) = max_chars {
        request["max_chars"] = json!(max_chars);
    }

    client
        .call_tool(
            CallToolRequestParams::new("read_document").with_arguments(arguments(request)),
        )
        .await
        .expect("read_document should succeed")
        .into_typed::<ReadDocumentResponse>()
        .expect("read response should be typed")
}

fn arguments(value: Value) -> Map<String, Value> {
    value
        .as_object()
        .expect("tool arguments should be an object")
        .clone()
}
