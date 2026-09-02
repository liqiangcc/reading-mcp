use std::process::Stdio;
use std::time::Duration;

use reading_mcp::mcp::contracts::{
    GetContextResponse, GetDocumentStructureResponse, GetTextUnitsResponse, ListDocumentsResponse,
    OpenDocumentResponse, ReadDocumentResponse, SearchDocumentResponse,
};
use rmcp::ServiceExt;
use rmcp::model::CallToolRequestParams;
use rmcp::transport::StreamableHttpClientTransport;
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use serde_json::{Map, Value, json};
use tokio::process::{Child, Command};

const TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

#[tokio::test]
async fn streamable_http_completes_the_real_reading_lifecycle_and_restart_resume() {
    let directory = tempfile::tempdir().expect("temporary fixture directory should be created");
    let state_dir = directory.path().join("state");
    let document_path = directory.path().join("book.md");
    tokio::fs::write(
        &document_path,
        r#"# Reading Book

Introduction sentence one. Introduction sentence two.

## Alpha

Alpha sentence one. Alpha sentence two.

## Beta

Beta sentence one. Beta sentence two.
"#,
    )
    .await
    .expect("HTTP fixture should be written");
    tokio::fs::write(directory.path().join("appendix.md"), "Appendix body.")
        .await
        .expect("discovery fixture should be written");

    let local_roots = std::env::join_paths([directory.path()])
        .expect("temporary fixture path should be a valid local root list");
    let port = reserve_port();
    let mut server = spawn_server(port, local_roots.as_os_str(), &state_dir);
    let base = format!("http://127.0.0.1:{port}");
    wait_until_ready(&base, &mut server).await;

    let client = connect(&base).await;
    let mut tool_names = client
        .list_all_tools()
        .await
        .expect("HTTP tools/list should succeed")
        .into_iter()
        .map(|tool| tool.name.into_owned())
        .collect::<Vec<_>>();
    tool_names.sort();
    assert_eq!(
        tool_names,
        vec![
            "get_context",
            "get_document_structure",
            "get_source_view",
            "get_text_units",
            "list_directory",
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
                "max_results": 1
            }))),
        )
        .await
        .expect("HTTP list_documents should succeed")
        .into_typed::<ListDocumentsResponse>()
        .expect("HTTP list response should be typed");
    assert_eq!(listed.documents.len(), 1);
    assert!(!listed.complete);
    let listing_cursor = listed.next_cursor.expect("HTTP discovery should continue");
    let listed_tail = client
        .call_tool(
            CallToolRequestParams::new("list_documents").with_arguments(arguments(json!({
                "path": directory.path().to_string_lossy(),
                "recursive": true,
                "max_results": 10,
                "cursor": listing_cursor
            }))),
        )
        .await
        .expect("HTTP discovery continuation should succeed")
        .into_typed::<ListDocumentsResponse>()
        .expect("HTTP discovery continuation should be typed");
    assert!(listed_tail.complete);
    assert!(listed_tail.next_cursor.is_none());

    let opened = client
        .call_tool(
            CallToolRequestParams::new("open_document").with_arguments(arguments(json!({
                "source": document_path.to_string_lossy(),
                "force_refresh": false
            }))),
        )
        .await
        .expect("HTTP open_document should succeed")
        .into_typed::<OpenDocumentResponse>()
        .expect("HTTP open response should be typed");
    assert_eq!(opened.section_count, 3);
    assert_eq!(opened.reading_profile.schema_version, "reading-profile/v1");
    assert_eq!(
        opened
            .reading_profile
            .capabilities
            .sentence_first_enumeration
            .segmentation_version,
        "text-segmentation/v2"
    );

    let first_structure = client
        .call_tool(
            CallToolRequestParams::new("get_document_structure").with_arguments(arguments(
                json!({"document_id": opened.document_id, "max_nodes": 2}),
            )),
        )
        .await
        .expect("HTTP structure request should succeed")
        .into_typed::<GetDocumentStructureResponse>()
        .expect("HTTP structure response should be typed");
    assert_eq!(first_structure.stream.body_order_version, "body-order/v1");
    assert_eq!(first_structure.stream.start_index, 0);
    assert!(!first_structure.complete);
    let structure_cursor = first_structure
        .next_cursor
        .clone()
        .expect("HTTP structure should provide continuation");
    let final_structure = client
        .call_tool(
            CallToolRequestParams::new("get_document_structure").with_arguments(arguments(json!({
                "document_id": opened.document_id,
                "max_nodes": 10,
                "cursor": structure_cursor
            }))),
        )
        .await
        .expect("HTTP structure continuation should succeed")
        .into_typed::<GetDocumentStructureResponse>()
        .expect("HTTP structure continuation should be typed");
    assert!(final_structure.complete);
    assert!(final_structure.next_cursor.is_none());
    assert_eq!(final_structure.stream.end_index, 3);
    assert!(
        final_structure
            .sections
            .iter()
            .all(|section| section.body_order > 0)
    );

    let alpha_id = "section://reading-book/alpha";
    let beta_id = "section://reading-book/beta";
    let first_units = client
        .call_tool(
            CallToolRequestParams::new("get_text_units").with_arguments(arguments(json!({
                "document_id": opened.document_id,
                "section_id": alpha_id,
                "requested_kind": "sentence",
                "coverage_policy": "preserve_source",
                "max_items": 1
            }))),
        )
        .await
        .expect("HTTP text-unit request should succeed")
        .into_typed::<GetTextUnitsResponse>()
        .expect("HTTP text-unit response should be typed");
    assert_eq!(first_units.items.len(), 1);
    assert!(!first_units.complete);
    assert!(first_units.coverage.source_complete);
    let first_locator = first_units.items[0].locator.clone();
    let text_cursor = first_units
        .next_cursor
        .clone()
        .expect("HTTP text units should provide continuation");
    let second_units = client
        .call_tool(
            CallToolRequestParams::new("get_text_units").with_arguments(arguments(json!({
                "document_id": opened.document_id,
                "section_id": alpha_id,
                "requested_kind": "sentence",
                "coverage_policy": "preserve_source",
                "max_items": 1,
                "cursor": text_cursor
            }))),
        )
        .await
        .expect("HTTP text-unit continuation should succeed")
        .into_typed::<GetTextUnitsResponse>()
        .expect("HTTP text-unit continuation should be typed");
    assert!(second_units.complete);
    assert!(second_units.section_complete);
    assert!(second_units.coverage.source_complete);

    let exact_read = client
        .call_tool(
            CallToolRequestParams::new("read_document").with_arguments(arguments(json!({
                "document_id": opened.document_id,
                "target_locator": first_locator
            }))),
        )
        .await
        .expect("HTTP exact read handoff should succeed")
        .into_typed::<ReadDocumentResponse>()
        .expect("HTTP exact read response should be typed");
    assert!(exact_read.content.contains("Alpha sentence one."));
    assert_eq!(exact_read.resolved_target_locator, first_locator);

    let exact_context = client
        .call_tool(
            CallToolRequestParams::new("get_context").with_arguments(arguments(json!({
                "document_id": opened.document_id,
                "target_locator": first_locator,
                "relation": {
                    "type": "neighbor",
                    "unit": "sentence",
                    "before": 0,
                    "after": 1
                },
                "before": 0,
                "after": 1
            }))),
        )
        .await
        .expect("HTTP locator context handoff should succeed")
        .into_typed::<GetContextResponse>()
        .expect("HTTP locator context response should be typed");
    assert_eq!(exact_context.anchor_locator, first_locator);
    assert!(exact_context.items.iter().any(|item| {
        item.content
            .as_deref()
            .is_some_and(|content| content.contains("Alpha sentence one."))
    }));

    for section_id in [alpha_id, beta_id] {
        let units = client
            .call_tool(
                CallToolRequestParams::new("get_text_units").with_arguments(arguments(json!({
                    "document_id": opened.document_id,
                    "section_id": section_id,
                    "requested_kind": "sentence",
                    "coverage_policy": "preserve_source",
                    "max_items": 32
                }))),
            )
            .await
            .expect("HTTP multi-Section text stream should succeed")
            .into_typed::<GetTextUnitsResponse>()
            .expect("HTTP multi-Section text stream should be typed");
        assert!(units.complete);
        assert!(units.section_complete);
        assert!(units.coverage.source_complete);
        assert!(
            units
                .items
                .iter()
                .all(|item| item.locator.owner_section_id == section_id)
        );
    }

    let searched = client
        .call_tool(
            CallToolRequestParams::new("search_document").with_arguments(arguments(json!({
                "document_id": opened.document_id,
                "query": "Beta sentence one",
                "limit": 10
            }))),
        )
        .await
        .expect("HTTP search should succeed")
        .into_typed::<SearchDocumentResponse>()
        .expect("HTTP search response should be typed");
    let hit = searched
        .hits
        .iter()
        .find(|hit| hit.section_id == beta_id)
        .expect("HTTP search should return the Beta Section hit");
    assert_eq!(hit.text_locator.owner_section_id, beta_id);
    let search_read = client
        .call_tool(
            CallToolRequestParams::new("read_document").with_arguments(arguments(json!({
                "document_id": opened.document_id,
                "target_locator": hit.text_locator
            }))),
        )
        .await
        .expect("SearchHit locator should hand off to HTTP read")
        .into_typed::<ReadDocumentResponse>()
        .expect("SearchHit read response should be typed");
    assert!(search_read.content.contains("Beta sentence one."));
    let search_context = client
        .call_tool(
            CallToolRequestParams::new("get_context").with_arguments(arguments(json!({
                "document_id": opened.document_id,
                "target_locator": hit.text_locator,
                "relation": {
                    "type": "neighbor",
                    "unit": "sentence",
                    "before": 0,
                    "after": 1
                },
                "before": 0,
                "after": 1
            }))),
        )
        .await
        .expect("SearchHit locator should hand off to HTTP context")
        .into_typed::<GetContextResponse>()
        .expect("SearchHit context response should be typed");
    assert_eq!(search_context.anchor_locator.owner_section_id, beta_id);

    client
        .cancel()
        .await
        .expect("first HTTP client session should close cleanly");
    server.kill().await.expect("first HTTP server should stop");

    let mut restarted_server = spawn_server(port, local_roots.as_os_str(), &state_dir);
    wait_until_ready(&base, &mut restarted_server).await;
    let resumed_client = connect(&base).await;
    let reopened = resumed_client
        .call_tool(
            CallToolRequestParams::new("open_document").with_arguments(arguments(json!({
                "source": document_path.to_string_lossy()
            }))),
        )
        .await
        .expect("HTTP reopen after server restart should succeed")
        .into_typed::<OpenDocumentResponse>()
        .expect("HTTP reopened document should be typed");
    let resumed = resumed_client
        .call_tool(
            CallToolRequestParams::new("get_text_units").with_arguments(arguments(json!({
                "document_id": reopened.document_id,
                "section_id": alpha_id,
                "anchor_locator": first_locator,
                "requested_kind": "sentence",
                "direction": "forward",
                "coverage_policy": "preserve_source",
                "max_items": 1
            }))),
        )
        .await
        .expect("HTTP TextLocator restart/resume should succeed")
        .into_typed::<GetTextUnitsResponse>()
        .expect("HTTP resumed text units should be typed");
    assert_eq!(resumed.items.len(), 1);
    assert_eq!(resumed.items[0].text, "Alpha sentence two.");
    assert!(resumed.complete);
    assert!(!resumed.section_complete);
    assert_eq!(resumed.start_anchor_locator, Some(first_locator));

    resumed_client
        .cancel()
        .await
        .expect("resumed HTTP client session should close cleanly");
    restarted_server
        .kill()
        .await
        .expect("restarted HTTP server should stop");
}

async fn connect(base: &str) -> rmcp::service::RunningService<rmcp::RoleClient, ()> {
    let config =
        StreamableHttpClientTransportConfig::with_uri(format!("{base}/mcp")).auth_header(TOKEN);
    let transport = StreamableHttpClientTransport::from_config(config);
    ().serve(transport)
        .await
        .expect("HTTP MCP initialization should succeed")
}

fn reserve_port() -> u16 {
    let listener =
        std::net::TcpListener::bind("127.0.0.1:0").expect("temporary HTTP port should be reserved");
    listener
        .local_addr()
        .expect("temporary HTTP port should have an address")
        .port()
}

fn spawn_server(port: u16, local_roots: &std::ffi::OsStr, state_dir: &std::path::Path) -> Child {
    Command::new(env!("CARGO_BIN_EXE_reading-mcp-http"))
        .env("READING_MCP_HTTP_TOKEN", TOKEN)
        .env("READING_MCP_HTTP_BIND", format!("127.0.0.1:{port}"))
        .env("READING_MCP_LOCAL_ROOTS", local_roots)
        .env("READING_MCP_STATE_DIR", state_dir)
        .env("READING_MCP_TELEMETRY", "false")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .expect("HTTP server should start")
}

async fn wait_until_ready(base: &str, child: &mut Child) {
    for _ in 0..100 {
        if let Some(status) = child
            .try_wait()
            .expect("HTTP child status should be readable")
        {
            panic!("HTTP server exited before becoming ready: {status}");
        }
        if let Ok(response) = reqwest::get(format!("{base}/healthz")).await
            && response.status().is_success()
        {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("HTTP server did not become ready");
}

fn arguments(value: Value) -> Map<String, Value> {
    value
        .as_object()
        .expect("tool arguments should be a JSON object")
        .clone()
}
