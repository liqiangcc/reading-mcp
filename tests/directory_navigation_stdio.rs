use reading_mcp::mcp::contracts::{
    DirectoryEntryKindDto, ListDirectoryResponse, ListDocumentsResponse, OpenDocumentResponse,
};
use rmcp::ServiceExt;
use rmcp::model::CallToolRequestParams;
use rmcp::transport::TokioChildProcess;
use serde_json::{Map, Value, json};
use tokio::process::Command;

#[tokio::test]
async fn stdio_agent_can_browse_a_source_workspace_then_open_a_document() {
    let root = tempfile::tempdir().expect("source workspace should be created");
    let papers = root.path().join("papers");
    let paper = papers.join("kafka-2011-distributed-messaging");
    let revision = paper.join("kafka-2011-netdb11");
    tokio::fs::create_dir_all(&revision)
        .await
        .expect("nested source workspace should be created");
    tokio::fs::write(revision.join("paper.md"), "# Kafka\n\nA source document.\n")
        .await
        .expect("document fixture should be written");
    tokio::fs::write(revision.join("source.json"), b"{}")
        .await
        .expect("metadata fixture should be written");

    let local_roots = std::env::join_paths([root.path()]).expect("local roots should be valid");
    let mut command = Command::new(env!("CARGO_BIN_EXE_reading-mcp"));
    command
        .env("READING_MCP_LOCAL_ROOTS", local_roots)
        .env("READING_MCP_STATE_DIR", "memory")
        .env("READING_MCP_TELEMETRY", "false");
    let transport = TokioChildProcess::new(command).expect("reading-mcp should start");
    let client = ().serve(transport).await.expect("MCP initialization should succeed");

    let roots = client
        .call_tool(
            CallToolRequestParams::new("list_directory")
                .with_arguments(arguments(json!({"max_results": 10}))),
        )
        .await
        .expect("root directory listing should succeed")
        .into_typed::<ListDirectoryResponse>()
        .expect("root listing should deserialize");
    assert_eq!(roots.entries.len(), 1);
    assert_eq!(roots.entries[0].kind, DirectoryEntryKindDto::Directory);

    let workspace = call_directory(&client, root.path()).await;
    assert_eq!(workspace.entries[0].name, "papers");

    let papers_listing = call_directory(&client, &papers).await;
    assert_eq!(
        papers_listing.entries[0].name,
        "kafka-2011-distributed-messaging"
    );
    let paper_listing = call_directory(&client, &paper).await;
    assert_eq!(paper_listing.entries[0].name, "kafka-2011-netdb11");
    let revision_listing = call_directory(&client, &revision).await;
    assert!(
        revision_listing
            .entries
            .iter()
            .all(|entry| entry.kind == DirectoryEntryKindDto::Document)
    );

    let documents = client
        .call_tool(
            CallToolRequestParams::new("list_documents").with_arguments(arguments(json!({
                "path": revision,
                "recursive": false,
                "max_results": 10
            }))),
        )
        .await
        .expect("known directory document listing should succeed")
        .into_typed::<ListDocumentsResponse>()
        .expect("document listing should deserialize");
    let paper_path = documents
        .documents
        .iter()
        .find(|document| document.name == "paper.md")
        .expect("browsed document should be discoverable")
        .path
        .clone();

    let opened = client
        .call_tool(
            CallToolRequestParams::new("open_document").with_arguments(arguments(json!({
                "source": paper_path
            }))),
        )
        .await
        .expect("discovered document should open")
        .into_typed::<OpenDocumentResponse>()
        .expect("open response should deserialize");
    assert!(opened.source.ends_with("/paper.md"));
}

async fn call_directory(
    client: &rmcp::service::RunningService<rmcp::RoleClient, ()>,
    path: &std::path::Path,
) -> ListDirectoryResponse {
    client
        .call_tool(
            CallToolRequestParams::new("list_directory").with_arguments(arguments(json!({
                "path": path,
                "max_results": 10
            }))),
        )
        .await
        .expect("directory listing should succeed")
        .into_typed::<ListDirectoryResponse>()
        .expect("directory listing should deserialize")
}

fn arguments(value: Value) -> Map<String, Value> {
    value
        .as_object()
        .cloned()
        .expect("tool arguments should be an object")
}
