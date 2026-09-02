use std::time::Duration;

use lopdf::content::{Content, Operation};
use lopdf::{Document as PdfDocument, Object, Stream, dictionary};
use reading_mcp::application::ports::{
    ApplicationError, SourceViewRenderOptions, SourceViewRenderer,
};
use reading_mcp::mcp::contracts::{
    GetSourceViewResponse, GetTextUnitsResponse, OpenDocumentResponse,
};
use reading_mcp::parsing::ProcessIsolatedPdfSourceViewRenderer;
use rmcp::ServiceExt;
use rmcp::model::CallToolRequestParams;
use rmcp::transport::TokioChildProcess;
use serde_json::{Map, Value, json};
use tokio::process::Command;

#[tokio::test]
async fn stdio_source_view_returns_structured_audit_metadata_and_png_image_block() {
    let root = tempfile::tempdir().expect("source root should be created");
    let path = root.path().join("fidelity.pdf");
    tokio::fs::write(&path, build_pdf())
        .await
        .expect("PDF fixture should be written");

    let local_roots = std::env::join_paths([root.path()]).expect("local roots should be valid");
    let mut command = Command::new(env!("CARGO_BIN_EXE_reading-mcp"));
    command
        .env("READING_MCP_LOCAL_ROOTS", local_roots)
        .env("READING_MCP_STATE_DIR", "memory")
        .env("READING_MCP_TELEMETRY", "false");
    let transport = TokioChildProcess::new(command).expect("reading-mcp should start");
    let client = ().serve(transport).await.expect("MCP initialization should succeed");

    let opened = client
        .call_tool(
            CallToolRequestParams::new("open_document").with_arguments(arguments(json!({
                "source": path
            }))),
        )
        .await
        .expect("PDF should open through MCP")
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
        .expect("sentence locator should be available")
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
        .expect("source view should render through stdio MCP");

    let wire = serde_json::to_value(&result).expect("tool result should serialize");
    let content = wire["content"]
        .as_array()
        .expect("tool result content should be an array");
    assert!(content.iter().any(|block| {
        block["type"] == "image" && block["mimeType"] == "image/png"
    }));

    let response = result
        .into_typed::<GetSourceViewResponse>()
        .expect("structured source-view response should deserialize");
    assert_eq!(response.page_number, 1);
    assert_eq!(response.page_count, 1);
    assert_eq!(response.image_media_type, "image/png");
    assert!(response.content_hash.starts_with("sha256:"));
    assert!(response.normalized_document_hash.starts_with("sha256:"));

    client.cancel().await.expect("MCP process should close cleanly");
}

#[cfg(unix)]
#[test]
fn isolated_renderer_terminates_a_worker_that_exceeds_the_deadline() {
    use std::os::unix::fs::PermissionsExt;

    let directory = tempfile::tempdir().expect("worker directory should be created");
    let worker = directory.path().join("sleep-worker.sh");
    std::fs::write(
        &worker,
        "#!/bin/sh\ncat >/dev/null\nsleep 5\n",
    )
    .expect("worker script should be written");
    let mut permissions = std::fs::metadata(&worker)
        .expect("worker metadata should be available")
        .permissions();
    permissions.set_mode(0o700);
    std::fs::set_permissions(&worker, permissions).expect("worker should be executable");

    let renderer = ProcessIsolatedPdfSourceViewRenderer::with_executable(
        worker,
        Duration::from_millis(25),
    );
    let error = renderer
        .render(
            build_pdf(),
            reading_mcp::domain::MediaType("application/pdf".into()),
            1,
            SourceViewRenderOptions {
                dpi: 72,
                max_pages: 10,
                max_width: 1_000,
                max_height: 1_000,
                max_pixels: 1_000_000,
                max_image_bytes: 1024 * 1024,
                max_decoded_stream_bytes: 1024 * 1024,
            },
        )
        .expect_err("deadline must terminate the worker process");
    assert!(matches!(error, ApplicationError::ResourceLimitExceeded(_)));
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
            Operation::new("Tj", vec![Object::string_literal("SOURCE VIEW SENTENCE.")]),
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
