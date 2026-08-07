use rmcp::ServiceExt;
use rmcp::model::CallToolRequestParams;
use rmcp::transport::TokioChildProcess;
use serde_json::{Map, Value, json};
use tokio::process::Command;

use reading_mcp::mcp::contracts::{
    GetContextResponse, GetDocumentStructureResponse, OpenDocumentResponse, ReadDocumentResponse,
    SearchDocumentResponse,
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
    command.env("READING_MCP_LOCAL_ROOTS", local_roots);
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
            "open_document",
            "read_document",
            "search_document",
        ]
    );

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

    let structure = client
        .call_tool(
            CallToolRequestParams::new("get_document_structure").with_arguments(arguments(json!({
                "document_id": opened.document_id,
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
    assert_eq!(structure.sections[0].children.len(), 2);
    assert_eq!(
        structure.sections[0].children[0].section_id,
        "section://operating-systems/virtual-memory"
    );

    let searched = client
        .call_tool(
            CallToolRequestParams::new("search_document").with_arguments(arguments(json!({
                "document_id": structure.document_id,
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
    assert!(searched.hits[0].snippet.contains("replacement algorithms"));

    let context = client
        .call_tool(
            CallToolRequestParams::new("get_context").with_arguments(arguments(json!({
                "document_id": searched.document_id,
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
    assert!(context.content.contains("Virtual Memory"));
    assert!(context.content.contains("Page Tables"));

    let read = client
        .call_tool(
            CallToolRequestParams::new("read_document").with_arguments(arguments(json!({
                "document_id": context.document_id,
                "section_id": context.owner_section_id,
                "max_chars": 4000
            }))),
        )
        .await
        .expect("read_document MCP call should succeed")
        .into_typed::<ReadDocumentResponse>()
        .expect("read should return typed structured content");
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

fn arguments(value: Value) -> Map<String, Value> {
    value
        .as_object()
        .expect("tool arguments should be a JSON object")
        .clone()
}
