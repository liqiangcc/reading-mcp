use reading_mcp::mcp::contracts::{GetDocumentStructureResponse, OpenDocumentResponse};
use rmcp::ServiceExt;
use rmcp::model::CallToolRequestParams;
use rmcp::transport::TokioChildProcess;
use serde_json::{Map, Value, json};
use tokio::process::Command;

#[tokio::test]
async fn stdio_structure_cursor_continues_a_page_forest_without_repeating_ancestors() {
    let directory = tempfile::tempdir().expect("temporary document directory should be created");
    let document_path = directory.path().join("structure.md");
    tokio::fs::write(
        &document_path,
        r#"# Root

Root intro.

## One

One body.

### Deep

Deep body.

## Two

Two body.

# Second

Second body.
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
    assert_eq!(tool_names.len(), 9);
    assert!(
        tool_names
            .iter()
            .any(|name| name == "get_document_structure")
    );
    assert!(tool_names.iter().any(|name| name == "list_directory"));
    assert!(tool_names.iter().any(|name| name == "get_source_view"));

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
    assert_eq!(opened.section_count, 5);

    let first = client
        .call_tool(
            CallToolRequestParams::new("get_document_structure").with_arguments(arguments(json!({
                "document_id": opened.document_id,
                "max_nodes": 2
            }))),
        )
        .await
        .expect("first structure page should succeed")
        .into_typed::<GetDocumentStructureResponse>()
        .expect("first structure page should deserialize");

    assert_eq!(flatten_ids(&first).len(), 2);
    assert_eq!(first.stream.start_index, 0);
    assert_eq!(first.stream.end_index, 2);
    assert_eq!(first.stream.total_nodes, 5);
    assert_eq!(first.stream.traversal_version, "structure-preorder/v1");
    assert!(!first.complete);
    assert!(first.truncated);
    assert_eq!(first.sections.len(), 1);
    assert!(!first.sections[0].children_complete);
    let cursor = first.next_cursor.expect("first page should provide cursor");

    let second = client
        .call_tool(
            CallToolRequestParams::new("get_document_structure").with_arguments(arguments(json!({
                "document_id": opened.document_id,
                "max_nodes": 2,
                "cursor": cursor
            }))),
        )
        .await
        .expect("second structure page should succeed")
        .into_typed::<GetDocumentStructureResponse>()
        .expect("second structure page should deserialize");

    assert_eq!(flatten_ids(&second).len(), 2);
    assert_eq!(first.stream.end_index, second.stream.start_index);
    assert_eq!(second.sections.len(), 2);
    assert!(second.sections.iter().all(|node| node.parent_id.is_some()));
    assert!(second.sections.iter().all(|node| node.children_complete));
    assert!(!second.complete);
    let cursor = second
        .next_cursor
        .expect("second page should provide cursor");

    let third = client
        .call_tool(
            CallToolRequestParams::new("get_document_structure").with_arguments(arguments(json!({
                "document_id": opened.document_id,
                "max_nodes": 2,
                "cursor": cursor
            }))),
        )
        .await
        .expect("third structure page should succeed")
        .into_typed::<GetDocumentStructureResponse>()
        .expect("third structure page should deserialize");

    assert_eq!(flatten_ids(&third).len(), 1);
    assert_eq!(second.stream.end_index, third.stream.start_index);
    assert_eq!(third.stream.end_index, third.stream.total_nodes);
    assert!(third.complete);
    assert!(!third.truncated);
    assert!(third.next_cursor.is_none());

    client
        .cancel()
        .await
        .expect("MCP child process should close cleanly");
}

fn flatten_ids(response: &GetDocumentStructureResponse) -> Vec<String> {
    fn collect(node: &reading_mcp::mcp::contracts::SectionNode, output: &mut Vec<String>) {
        output.push(node.section_id.clone());
        for child in &node.children {
            collect(child, output);
        }
    }

    let mut output = Vec::new();
    for section in &response.sections {
        collect(section, &mut output);
    }
    output
}

fn arguments(value: Value) -> Map<String, Value> {
    value
        .as_object()
        .expect("tool arguments should be a JSON object")
        .clone()
}
