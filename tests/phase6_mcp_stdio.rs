use rmcp::ServiceExt;
use rmcp::model::CallToolRequestParams;
use rmcp::transport::TokioChildProcess;
use serde_json::{Map, Value, json};
use tokio::process::Command;

use reading_mcp::mcp::contracts::{
    GetContextResponse, GetDocumentStructureResponse, GetTextUnitsResponse, ListDocumentsResponse,
    OpenDocumentResponse, ReadDocumentResponse, SearchDocumentResponse,
};

#[tokio::test]
async fn stdio_client_completes_the_real_reading_tool_flow() {
    let directory = tempfile::tempdir().expect("temporary document directory should be created");
    let document_path = directory.path().join("operating-systems.md");
    tokio::fs::write(
        &document_path,
        r#"# Operating Systems

A compact operating systems study guide.

## Virtual Memory

Address spaces give each process an isolated view of memory.

Page replacement algorithms decide which resident page should be evicted.

### Page Tables

Page table entries map virtual pages to physical frames.

## Processes

Processes own resources and execution state.
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

    let mut tool_names = client
        .list_all_tools()
        .await
        .expect("tools/list should succeed")
        .into_iter()
        .map(|tool| tool.name.into_owned())
        .collect::<Vec<_>>();
    tool_names.sort();
    assert_eq!(
        tool_names,
        vec![
            "get_context",
            "get_document_structure",
            "get_text_units",
            "list_documents",
            "open_document",
            "read_document",
            "search_document",
        ]
    );

    let listed = client
        .call_tool(
            CallToolRequestParams::new("list_documents").with_arguments(arguments(json!({
                "path": directory.path().to_string_lossy(),
                "recursive": true,
                "max_results": 10
            }))),
        )
        .await
        .expect("list_documents MCP call should succeed")
        .into_typed::<ListDocumentsResponse>()
        .expect("list_documents should return typed structured content");
    assert_eq!(listed.documents.len(), 1);
    assert_eq!(listed.documents[0].name, "operating-systems.md");

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
    assert_eq!(opened.title, "Operating Systems");
    assert_eq!(opened.media_type, "text/markdown");
    assert_eq!(opened.section_count, 4);
    assert!(opened.source.starts_with("file://"));
    assert!(opened.content_hash.starts_with("sha256:"));

    let opened_document_id = opened.document_id.clone();
    let opened_source = opened.source.clone();

    let structure = client
        .call_tool(
            CallToolRequestParams::new("get_document_structure").with_arguments(arguments(json!({
                "document_id": opened_document_id,
                "max_depth": 4
            }))),
        )
        .await
        .expect("get_document_structure MCP call should succeed")
        .into_typed::<GetDocumentStructureResponse>()
        .expect("structure should return typed structured content");
    assert_eq!(structure.sections.len(), 1);
    assert_eq!(
        structure.sections[0].section_id,
        "section://operating-systems"
    );
    assert_eq!(structure.sections[0].parent_id, None);
    assert_eq!(structure.sections[0].children.len(), 2);
    assert_eq!(
        structure.sections[0].children[0].section_id,
        "section://operating-systems/virtual-memory"
    );
    assert_eq!(
        structure.sections[0].children[0].parent_id.as_deref(),
        Some("section://operating-systems")
    );

    let text_units = client
        .call_tool(
            CallToolRequestParams::new("get_text_units").with_arguments(arguments(json!({
                "document_id": structure.document_id,
                "section_id": "section://operating-systems/virtual-memory",
                "requested_kind": "sentence",
                "coverage_policy": "preserve_source",
                "max_items": 1,
                "max_chars": 4000
            }))),
        )
        .await
        .expect("get_text_units MCP call should succeed")
        .into_typed::<GetTextUnitsResponse>()
        .expect("get_text_units should return typed structured content");
    assert_eq!(text_units.items.len(), 1);
    assert_eq!(
        text_units.items[0].text,
        "Address spaces give each process an isolated view of memory."
    );
    assert_eq!(text_units.items[0].locator.paragraph_index, Some(1));
    assert_eq!(text_units.items[0].locator.sentence_index, Some(1));
    assert_eq!(text_units.stream.start_index, 0);
    assert_eq!(text_units.stream.end_index, 1);
    assert_eq!(text_units.stream.total_items, 2);
    assert!(!text_units.complete);
    let text_unit_cursor = text_units
        .next_cursor
        .clone()
        .expect("first sentence page should be resumable");

    let continued_units = client
        .call_tool(
            CallToolRequestParams::new("get_text_units").with_arguments(arguments(json!({
                "document_id": opened.document_id,
                "section_id": "section://operating-systems/virtual-memory",
                "requested_kind": "sentence",
                "coverage_policy": "preserve_source",
                "max_items": 1,
                "max_chars": 4000,
                "cursor": text_unit_cursor
            }))),
        )
        .await
        .expect("get_text_units continuation should succeed")
        .into_typed::<GetTextUnitsResponse>()
        .expect("continued get_text_units should return typed structured content");
    assert_eq!(continued_units.items.len(), 1);
    assert_eq!(
        continued_units.items[0].text,
        "Page replacement algorithms decide which resident page should be evicted."
    );
    assert_eq!(
        text_units.stream.end_index,
        continued_units.stream.start_index
    );
    assert!(continued_units.complete);
    assert!(continued_units.section_complete);
    assert!(continued_units.next_cursor.is_none());

    let searched = client
        .call_tool(
            CallToolRequestParams::new("search_document").with_arguments(arguments(json!({
                "document_id": continued_units.document_id,
                "query": "replacement algorithms",
                "limit": 10
            }))),
        )
        .await
        .expect("search_document MCP call should succeed")
        .into_typed::<SearchDocumentResponse>()
        .expect("search should return typed structured content");
    assert!(!searched.hits.is_empty());
    let owner_section_id = searched.hits[0].section_id.clone();
    assert_eq!(
        owner_section_id,
        "section://operating-systems/virtual-memory"
    );
    assert_eq!(searched.hits[0].title, "Virtual Memory");
    assert_eq!(searched.hits[0].source, opened_source);
    assert!(searched.hits[0].snippet.contains("replacement algorithms"));

    let searched_document_id = searched.document_id.clone();
    let context = client
        .call_tool(
            CallToolRequestParams::new("get_context").with_arguments(arguments(json!({
                "document_id": searched_document_id,
                "section_id": owner_section_id,
                "before": 0,
                "after": 1,
                "max_chars": 4000
            }))),
        )
        .await
        .expect("get_context MCP call should succeed")
        .into_typed::<GetContextResponse>()
        .expect("context should return typed structured content");
    assert_eq!(context.source, opened.source);
    assert!(context.content.contains("Virtual Memory"));
    assert!(context.content.contains("Page Tables"));

    let context_document_id = context.document_id.clone();
    let context_owner_section_id = context.owner_section_id.clone();
    let read = client
        .call_tool(
            CallToolRequestParams::new("read_document").with_arguments(arguments(json!({
                "document_id": context_document_id,
                "section_id": context_owner_section_id,
                "max_chars": 4000
            }))),
        )
        .await
        .expect("read_document MCP call should succeed")
        .into_typed::<ReadDocumentResponse>()
        .expect("read should return typed structured content");
    assert_eq!(read.source, opened.source);
    assert_eq!(
        read.section_id,
        "section://operating-systems/virtual-memory"
    );
    assert!(read.content.contains("Address spaces give each process"));
    assert!(read.content.contains("Page replacement algorithms"));
    assert!(read.content.contains("### Page Tables"));
    assert!(read.content.contains("physical frames"));
    assert!(!read.truncated);

    client
        .cancel()
        .await
        .expect("MCP child process should close cleanly");
}

#[tokio::test]
async fn persistent_stdio_runtime_survives_server_restart() {
    let directory = tempfile::tempdir().expect("temporary directory should be created");
    let state_dir = directory.path().join("state");
    let document_path = directory.path().join("persistent.md");
    tokio::fs::write(
        &document_path,
        "# Persistent Book\n\n## Memory\n\nReplacement algorithms survive restart.\n",
    )
    .await
    .expect("persistent fixture should be written");

    let local_roots = std::env::join_paths([directory.path()])
        .expect("temporary directory should be a valid local root list");

    let mut first_command = Command::new(env!("CARGO_BIN_EXE_reading-mcp"));
    first_command
        .env("READING_MCP_LOCAL_ROOTS", &local_roots)
        .env("READING_MCP_STATE_DIR", &state_dir)
        .env("READING_MCP_TELEMETRY", "false");
    let first_transport =
        TokioChildProcess::new(first_command).expect("first MCP process should start");
    let first_client =
        ().serve(first_transport)
            .await
            .expect("first MCP process should initialize");

    let opened = first_client
        .call_tool(
            CallToolRequestParams::new("open_document").with_arguments(arguments(json!({
                "source": document_path.to_string_lossy()
            }))),
        )
        .await
        .expect("document should open before restart")
        .into_typed::<OpenDocumentResponse>()
        .expect("open response should be typed");
    let document_id = opened.document_id.clone();
    first_client
        .cancel()
        .await
        .expect("first MCP process should close cleanly");

    let mut second_command = Command::new(env!("CARGO_BIN_EXE_reading-mcp"));
    second_command
        .env("READING_MCP_LOCAL_ROOTS", local_roots)
        .env("READING_MCP_STATE_DIR", &state_dir)
        .env("READING_MCP_TELEMETRY", "false");
    let second_transport =
        TokioChildProcess::new(second_command).expect("second MCP process should start");
    let second_client =
        ().serve(second_transport)
            .await
            .expect("second MCP process should initialize");

    let read = second_client
        .call_tool(
            CallToolRequestParams::new("read_document").with_arguments(arguments(json!({
                "document_id": document_id,
                "section_id": "section://persistent-book/memory"
            }))),
        )
        .await
        .expect("persisted document should be readable without reopening")
        .into_typed::<ReadDocumentResponse>()
        .expect("read response should be typed");
    assert!(
        read.content
            .contains("Replacement algorithms survive restart.")
    );

    let searched = second_client
        .call_tool(
            CallToolRequestParams::new("search_document").with_arguments(arguments(json!({
                "document_id": opened.document_id,
                "query": "replacement algorithms",
                "limit": 10
            }))),
        )
        .await
        .expect("persistent FTS should survive restart")
        .into_typed::<SearchDocumentResponse>()
        .expect("search response should be typed");
    assert!(!searched.hits.is_empty());

    second_client
        .cancel()
        .await
        .expect("second MCP process should close cleanly");
}

fn arguments(value: Value) -> Map<String, Value> {
    value
        .as_object()
        .expect("tool arguments should be a JSON object")
        .clone()
}
