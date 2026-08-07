use lopdf::content::{Content, Operation};
use lopdf::{Document as PdfDocument, Object, Stream, dictionary};
use rmcp::ServiceExt;
use rmcp::model::CallToolRequestParams;
use rmcp::transport::TokioChildProcess;
use serde_json::{Map, Value, json};
use tokio::process::Command;

use reading_mcp::mcp::contracts::{
    GetDocumentStructureResponse, OpenDocumentResponse, ReadDocumentResponse,
    SearchDocumentResponse,
};

#[tokio::test]
async fn stdio_acceptance_matrix_covers_all_mvp_formats() {
    let directory = tempfile::tempdir().expect("acceptance directory should be created");
    let text = directory.path().join("notes.txt");
    let markdown = directory.path().join("guide.md");
    let html = directory.path().join("guide.html");
    let pdf = directory.path().join("guide.pdf");

    tokio::fs::write(&text, "Orbital memory appears in plain text.\n")
        .await
        .expect("text fixture should be written");
    tokio::fs::write(
        &markdown,
        "# Markdown Guide\n\n## Memory\n\nOrbital memory appears in Markdown.\n",
    )
    .await
    .expect("Markdown fixture should be written");
    tokio::fs::write(
        &html,
        "<!doctype html><html><head><title>HTML Guide</title></head><body><main><h1 id=\"memory\">Memory</h1><p>Orbital memory appears in HTML.</p></main></body></html>",
    )
    .await
    .expect("HTML fixture should be written");
    tokio::fs::write(&pdf, build_pdf("Orbital memory appears in PDF."))
        .await
        .expect("PDF fixture should be written");

    let local_roots =
        std::env::join_paths([directory.path()]).expect("acceptance root should be valid");
    let mut command = Command::new(env!("CARGO_BIN_EXE_reading-mcp"));
    command
        .env("READING_MCP_LOCAL_ROOTS", local_roots)
        .env("READING_MCP_STATE_DIR", "memory")
        .env("READING_MCP_TELEMETRY", "false");
    let transport = TokioChildProcess::new(command).expect("MCP process should start");
    let client = ().serve(transport).await.expect("MCP process should initialize");

    assert_flow(&client, &text, "text/plain", "section://document").await;
    assert_flow(
        &client,
        &markdown,
        "text/markdown",
        "section://markdown-guide/memory",
    )
    .await;
    assert_flow(&client, &html, "text/html", "section://memory").await;
    assert_flow(&client, &pdf, "application/pdf", "section://page-1").await;

    client
        .cancel()
        .await
        .expect("MCP process should close cleanly");
}

async fn assert_flow(
    client: &rmcp::service::RunningService<rmcp::RoleClient, ()>,
    path: &std::path::Path,
    expected_media_type: &str,
    expected_section_id: &str,
) {
    let opened = client
        .call_tool(
            CallToolRequestParams::new("open_document").with_arguments(arguments(json!({
                "source": path.to_string_lossy()
            }))),
        )
        .await
        .expect("open should succeed")
        .into_typed::<OpenDocumentResponse>()
        .expect("open result should be typed");
    assert_eq!(opened.media_type, expected_media_type);

    let structure = client
        .call_tool(
            CallToolRequestParams::new("get_document_structure").with_arguments(arguments(json!({
                "document_id": opened.document_id,
                "max_depth": 8
            }))),
        )
        .await
        .expect("structure should succeed")
        .into_typed::<GetDocumentStructureResponse>()
        .expect("structure result should be typed");
    assert!(!structure.sections.is_empty());

    let searched = client
        .call_tool(
            CallToolRequestParams::new("search_document").with_arguments(arguments(json!({
                "document_id": structure.document_id,
                "query": "orbital memory",
                "limit": 5
            }))),
        )
        .await
        .expect("search should succeed")
        .into_typed::<SearchDocumentResponse>()
        .expect("search result should be typed");
    assert!(!searched.hits.is_empty());
    assert_eq!(searched.hits[0].section_id, expected_section_id);

    let read = client
        .call_tool(
            CallToolRequestParams::new("read_document").with_arguments(arguments(json!({
                "document_id": searched.document_id,
                "section_id": expected_section_id,
                "max_chars": 4000
            }))),
        )
        .await
        .expect("read should succeed")
        .into_typed::<ReadDocumentResponse>()
        .expect("read result should be typed");
    assert!(read.content.to_ascii_lowercase().contains("orbital memory"));
}

fn arguments(value: Value) -> Map<String, Value> {
    value
        .as_object()
        .expect("tool arguments should be object")
        .clone()
}

fn build_pdf(text: &str) -> Vec<u8> {
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
            Operation::new("Tf", vec!["F1".into(), 12.into()]),
            Operation::new("Td", vec![72.into(), 720.into()]),
            Operation::new("Tj", vec![Object::string_literal(text)]),
            Operation::new("ET", vec![]),
        ],
    };
    let content_id = document.add_object(Stream::new(
        dictionary! {},
        content.encode().expect("PDF content should encode"),
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
    document.save_to(&mut bytes).expect("PDF should serialize");
    bytes
}
