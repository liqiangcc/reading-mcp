use reading_mcp::mcp::contracts::{
    GetTextUnitsResponse, OpenDocumentResponse, ReadDocumentResponse, TextUnitItemDto,
};
use reading_mcp::mcp::source_view_contracts::GetSourceViewResponse;
use rmcp::{ServiceExt, model::CallToolRequestParams, transport::TokioChildProcess};
use serde_json::{Map, Value, json};
use sha2::Digest;
use tokio::process::Command;

fn arguments(value: Value) -> Map<String, Value> {
    value.as_object().unwrap().clone()
}

#[tokio::test]
#[ignore = "requires pinned hosted OCR dependencies"]
async fn scanned_pdf_stdio_locator_reads_original_page_after_server_restart() {
    tokio::time::timeout(std::time::Duration::from_secs(120), async {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("public-F07.pdf");
        let raw = std::fs::read("tests/fixtures/scanned_pdf/pdf/F07.pdf").unwrap();
        std::fs::write(&path, &raw).unwrap();
        let raw_hash = format!("sha256:{:x}", sha2::Sha256::digest(&raw));
        let state = directory.path().join("state");
        let mut saved: Option<(String, TextUnitItemDto)> = None;
        for _ in 0..2 {
            let mut command = Command::new(env!("CARGO_BIN_EXE_reading-mcp"));
            command
                .env(
                    "READING_MCP_LOCAL_ROOTS",
                    std::env::join_paths([directory.path()]).unwrap(),
                )
                .env("READING_MCP_STATE_DIR", &state)
                .env("READING_MCP_TELEMETRY", "false")
                // Existing #92 deployment profile; retain the production and
                // default guards rather than bypassing embedded-image checks.
                .env("READING_MCP_SOURCE_VIEW_MAX_PIXELS", "16000000")
                .env(
                    "READING_MCP_PDF_LAYOUT_PYTHON",
                    std::env::var("READING_MCP_PDF_LAYOUT_PYTHON").unwrap(),
                )
                .env("READING_MCP_OCR_ENABLED", "true")
                .env("READING_MCP_OCR_ENGINE", "/usr/bin/tesseract")
                .env(
                    "READING_MCP_OCR_TESSDATA",
                    "/usr/share/tesseract-ocr/5/tessdata",
                )
                .env("READING_MCP_OCR_LANG", "chi_sim")
                .env("READING_MCP_OCR_REVISION", "1")
                .kill_on_drop(true);
            let client = ().serve(TokioChildProcess::new(command).unwrap()).await.unwrap();
            let opened = client
                .call_tool(
                    CallToolRequestParams::new("open_document")
                        .with_arguments(arguments(json!({"source":path}))),
                )
                .await
                .unwrap()
                .into_typed::<OpenDocumentResponse>()
                .unwrap();
            assert_eq!(opened.content_hash, raw_hash);
            let units =
                client
                    .call_tool(CallToolRequestParams::new("get_text_units").with_arguments(
                        arguments(json!({"document_id":opened.document_id,
                    "section_id":"section://pdf-layout/1", "requested_kind":"sentence",
                    "coverage_policy":"preserve_source", "max_items":1})),
                    ))
                    .await
                    .unwrap()
                    .into_typed::<GetTextUnitsResponse>()
                    .unwrap();
            assert_eq!(units.items.len(), 1);
            assert!(!units.items[0].text.is_empty());
            if let Some((hash, item)) = &saved {
                assert_eq!(&opened.normalized_document_hash, hash);
                assert_eq!(&units.items[0], item);
            } else {
                saved = Some((
                    opened.normalized_document_hash.clone(),
                    units.items[0].clone(),
                ));
            }
            let (hash, item) = saved.as_ref().unwrap();
            let read = client
                .call_tool(
                    CallToolRequestParams::new("read_document")
                        .with_arguments(arguments(json!({"document_id":opened.document_id,
                    "target_locator":item.locator, "max_chars":8192}))),
                )
                .await
                .unwrap()
                .into_typed::<ReadDocumentResponse>()
                .unwrap();
            assert!(read.complete);
            assert_eq!(read.content, item.text);
            assert_eq!(read.resolved_target_locator, item.locator);
            let result = client
                .call_tool(
                    CallToolRequestParams::new("get_source_view")
                        .with_arguments(arguments(json!({"document_id":opened.document_id,
                    "target_locator":item.locator, "representation":"original", "dpi":72}))),
                )
                .await
                .unwrap();
            let wire = serde_json::to_value(&result).unwrap();
            assert!(
                wire["content"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|block| block["type"] == "image" && block["mimeType"] == "image/png")
            );
            let view = result.into_typed::<GetSourceViewResponse>().unwrap();
            assert_eq!(view.page_number, 1);
            assert_eq!(view.page_count, 1);
            assert_eq!(view.content_hash, raw_hash);
            assert_eq!(&view.normalized_document_hash, hash);
            client.cancel().await.unwrap();
        }
    })
    .await
    .expect("OCR stdio lifecycle exceeded the hosted test deadline");
}
