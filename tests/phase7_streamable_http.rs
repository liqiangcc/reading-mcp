use rmcp::ServiceExt;
use rmcp::model::CallToolRequestParams;
use rmcp::transport::StreamableHttpClientTransport;
use serde_json::{Map, Value, json};

use reading_mcp::mcp::contracts::{OpenDocumentResponse, ReadDocumentResponse, SearchDocumentResponse};
use reading_mcp::mcp::{HttpTransportConfig, ReadingMcpServer, streamable_http_router};
use reading_mcp::runtime::{RuntimeConfig, build_server};

#[tokio::test]
async fn streamable_http_client_completes_the_real_reading_tool_flow() {
    let directory = tempfile::tempdir().expect("temporary document directory should be created");
    let document_path = directory.path().join("remote-reading.md");
    tokio::fs::write(
        &document_path,
        "# Remote Reading\n\n## Memory\n\nPage replacement algorithms choose an eviction candidate.\n",
    )
    .await
    .expect("Markdown fixture should be written");

    let runtime = RuntimeConfig {
        local_roots: vec![directory.path().to_path_buf()],
        state_dir: None,
        telemetry: false,
        ..RuntimeConfig::default()
    };
    let server = build_server(runtime).expect("HTTP MCP runtime should build");
    let transport_config = HttpTransportConfig::default();
    let router = streamable_http_router(server, &transport_config);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("test HTTP listener should bind");
    let address = listener.local_addr().expect("listener should have an address");
    let server_task = tokio::spawn(async move {
        axum::serve(listener, router)
            .await
            .expect("Streamable HTTP server should run");
    });

    let transport = StreamableHttpClientTransport::from_uri(format!("http://{address}/mcp"));
    let client = ().serve(transport).await.expect("HTTP MCP client should initialize");

    let mut tool_names = client
        .list_all_tools()
        .await
        .expect("tools/list over HTTP should succeed")
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
        .expect("open_document over HTTP should succeed")
        .into_typed::<OpenDocumentResponse>()
        .expect("open_document should return typed structured content");
    assert_eq!(opened.title, "Remote Reading");

    let searched = client
        .call_tool(
            CallToolRequestParams::new("search_document").with_arguments(arguments(json!({
                "document_id": opened.document_id,
                "query": "replacement algorithms",
                "limit": 10
            }))),
        )
        .await
        .expect("search_document over HTTP should succeed")
        .into_typed::<SearchDocumentResponse>()
        .expect("search response should be typed");
    assert_eq!(searched.hits.len(), 1);
    assert_eq!(searched.hits[0].title, "Memory");

    let read = client
        .call_tool(
            CallToolRequestParams::new("read_document").with_arguments(arguments(json!({
                "document_id": searched.document_id,
                "section_id": searched.hits[0].section_id,
                "max_chars": 4000
            }))),
        )
        .await
        .expect("read_document over HTTP should succeed")
        .into_typed::<ReadDocumentResponse>()
        .expect("read response should be typed");
    assert!(read.content.contains("Page replacement algorithms"));

    client.cancel().await.expect("HTTP MCP client should close cleanly");
    server_task.abort();
}

#[test]
fn http_transport_config_rejects_non_loopback_by_design() {
    let config = HttpTransportConfig::default();
    assert!(config.bind.ip().is_loopback());
}

fn arguments(value: Value) -> Map<String, Value> {
    value
        .as_object()
        .expect("tool arguments should be a JSON object")
        .clone()
}
