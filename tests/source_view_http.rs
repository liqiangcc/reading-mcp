use std::process::Stdio;
use std::time::Duration;

use lopdf::content::{Content, Operation};
use lopdf::{Document as PdfDocument, Object, Stream, dictionary};
use reading_mcp::domain::{
    NORMALIZED_DOCUMENT_HASH_VERSION, ORIGINAL_SOURCE_BINDING_MODEL_VERSION,
};
use reading_mcp::mcp::contracts::{GetTextUnitsResponse, OpenDocumentResponse};
use reading_mcp::mcp::source_view_contracts::GetSourceViewResponse;
use rmcp::ServiceExt;
use rmcp::model::CallToolRequestParams;
use rmcp::transport::StreamableHttpClientTransport;
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use serde_json::{Map, Value, json};
use tokio::process::{Child, Command};

const TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

#[tokio::test]
async fn streamable_http_source_view_returns_bound_original_png() {
    let root = tempfile::tempdir().expect("source root should be created");
    let state_dir = root.path().join("state");
    let path = root.path().join("fidelity.pdf");
    tokio::fs::write(&path, build_pdf())
        .await
        .expect("PDF fixture should be written");

    let local_roots = std::env::join_paths([root.path()]).expect("local roots should be valid");
    let port = reserve_port();
    let mut server = spawn_server(port, local_roots.as_os_str(), &state_dir);
    let base = format!("http://127.0.0.1:{port}");
    wait_until_ready(&base, &mut server).await;

    let client = connect(&base).await;
    let opened = client
        .call_tool(
            CallToolRequestParams::new("open_document").with_arguments(arguments(json!({
                "source": path
            }))),
        )
        .await
        .expect("PDF should open through HTTP MCP")
        .into_typed::<OpenDocumentResponse>()
        .expect("open response should deserialize");

    let units = client
        .call_tool(
            CallToolRequestParams::new("get_text_units").with_arguments(arguments(json!({
                "document_id": opened.document_id,
                "section_id": "section://page-1",
                "requested_kind": "sentence",
                "coverage_policy": "preserve_source",
                "max_items": 1
            }))),
        )
        .await
        .expect("sentence locator should be available over HTTP")
        .into_typed::<GetTextUnitsResponse>()
        .expect("text-unit response should deserialize");

    let result = client
        .call_tool(
            CallToolRequestParams::new("get_source_view").with_arguments(arguments(json!({
                "document_id": units.document_id,
                "target_locator": units.items[0].locator,
                "representation": "original",
                "dpi": 72
            }))),
        )
        .await
        .expect("source view should render through HTTP MCP");

    let wire = serde_json::to_value(&result).expect("tool result should serialize");
    let content = wire["content"]
        .as_array()
        .expect("tool result content should be an array");
    assert!(
        content
            .iter()
            .any(|block| { block["type"] == "image" && block["mimeType"] == "image/png" })
    );

    let response = result
        .into_typed::<GetSourceViewResponse>()
        .expect("structured source-view response should deserialize");
    assert_eq!(response.page_number, 1);
    assert_eq!(response.page_count, 1);
    assert_eq!(response.image_media_type, "image/png");
    assert_eq!(
        response.normalized_document_hash_version,
        NORMALIZED_DOCUMENT_HASH_VERSION
    );
    assert_eq!(
        response.source_binding_version,
        ORIGINAL_SOURCE_BINDING_MODEL_VERSION
    );

    client
        .cancel()
        .await
        .expect("HTTP client session should close cleanly");
    server.kill().await.expect("HTTP server should stop");
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

fn build_pdf() -> Vec<u8> {
    let mut document = PdfDocument::with_version("1.5");
    let pages_id = document.new_object_id();
    let font_id = document.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => "Helvetica",
    });
    let resources_id = document.add_object(dictionary! {
        "Font" => dictionary! { "F1" => font_id },
    });
    let content = Content {
        operations: vec![
            Operation::new("BT", vec![]),
            Operation::new("Tf", vec!["F1".into(), 14.into()]),
            Operation::new("Td", vec![72.into(), 720.into()]),
            Operation::new("Tj", vec![Object::string_literal("HTTP SOURCE VIEW SENTENCE.")]),
            Operation::new("ET", vec![]),
        ],
    };
    let content_id = document.add_object(Stream::new(
        dictionary! {},
        content.encode().expect("fixture content should encode"),
    ));
    let page_id = document.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => pages_id,
        "Contents" => content_id,
    });
    document.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => vec![Object::Reference(page_id)],
            "Count" => 1,
            "Resources" => resources_id,
            "MediaBox" => vec![0.into(), 0.into(), 595.into(), 842.into()],
        }),
    );
    let catalog_id = document.add_object(dictionary! {
        "Type" => "Catalog",
        "Pages" => pages_id,
    });
    document.trailer.set("Root", catalog_id);

    let mut bytes = Vec::new();
    document
        .save_to(&mut bytes)
        .expect("fixture PDF should serialize");
    bytes
}

fn arguments(value: Value) -> Map<String, Value> {
    value
        .as_object()
        .cloned()
        .expect("tool arguments should be an object")
}
