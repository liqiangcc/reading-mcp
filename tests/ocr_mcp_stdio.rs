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

fn published_documents(state: &std::path::Path) -> Vec<String> {
    let connection = rusqlite::Connection::open_with_flags(
        state.join("reading-mcp.sqlite"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let mut statement = connection
        .prepare("SELECT document_json FROM documents ORDER BY id")
        .unwrap();
    statement
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
}

#[tokio::test]
#[ignore = "requires pinned hosted OCR dependencies"]
async fn scanned_pdf_stdio_locator_reads_original_page_after_server_restart() {
    tokio::time::timeout(std::time::Duration::from_secs(120), async {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("public-F07.pdf");
        let raw = std::fs::read("tests/fixtures/scanned_pdf/pdf/F07.pdf").unwrap();
        std::fs::write(&path, &raw).unwrap();
        let blank_path = directory.path().join("public-F09.pdf");
        std::fs::copy("tests/fixtures/scanned_pdf/pdf/F09.pdf", &blank_path).unwrap();
        let raw_hash = format!("sha256:{:x}", sha2::Sha256::digest(&raw));
        let state = directory.path().join("state");
        let mut saved: Option<(String, TextUnitItemDto)> = None;
        // The 300-DPI RGB fixture decodes to about 26.1 MB. Preserve the
        // default 16-MiB rejection, then test an explicit development profile.
        // This does not change defaults or approve a production budget change.
        for stream_limit in [16 * 1024 * 1024_u64, 32 * 1024 * 1024, 32 * 1024 * 1024] {
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
                    "READING_MCP_SOURCE_VIEW_MAX_DECODED_STREAM_BYTES",
                    stream_limit.to_string(),
                )
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
            let codes: Vec<_> = opened
                .reading_profile
                .reliability
                .evidence
                .iter()
                .flat_map(|evidence| evidence.degradation_codes.iter().map(String::as_str))
                .collect();
            assert!(codes.contains(&"pdf_local_ocr_unverified"));
            assert!(!codes.contains(&"pdf_ocr_disabled"));
            assert!(!codes.contains(&"pdf_local_ocr_not_applied"));
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
            let before_failure = published_documents(&state);
            assert_eq!(before_failure.len(), 1);
            let blank_error = client
                .call_tool(
                    CallToolRequestParams::new("open_document")
                        .with_arguments(arguments(json!({"source":blank_path}))),
                )
                .await
                .expect_err("a blank PDF must not publish fabricated supported prose");
            println!("F09 MCP failure: {blank_error}");
            let error_text = blank_error.to_string();
            assert!(error_text.contains("OCR_NO_SUPPORTED_PROJECTION"));
            assert!(error_text.contains("\"retryable\":false"));
            assert_eq!(
                published_documents(&state),
                before_failure,
                "failed blank ingestion must leave canonical documents byte-identical"
            );
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
                .await;
            if stream_limit == 16 * 1024 * 1024 {
                assert!(
                    result
                        .unwrap_err()
                        .to_string()
                        .contains("decoded PDF stream exceeds configured limit")
                );
                client.cancel().await.unwrap();
                continue;
            }
            let result = result.unwrap();
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
