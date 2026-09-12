use reading_mcp::mcp::contracts::{
    GetTextUnitsResponse, OpenDocumentResponse, ReadDocumentResponse, TextUnitItemDto,
};
use reading_mcp::mcp::source_view_contracts::GetSourceViewResponse;
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use rmcp::{ServiceExt, model::CallToolRequestParams, transport::StreamableHttpClientTransport};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::{process::Stdio, time::Duration};
use tokio::process::Command;

// Public test credential, never a production secret.
const TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

fn arguments(value: Value) -> Map<String, Value> {
    value.as_object().unwrap().clone()
}

#[tokio::test]
#[ignore = "requires pinned hosted OCR dependencies"]
async fn real_ocr_http_saved_locator_survives_server_restart() {
    tokio::time::timeout(Duration::from_secs(120), async {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("public-F07.pdf");
        let raw = std::fs::read("tests/fixtures/scanned_pdf/pdf/F07.pdf").unwrap();
        let raw_hash = format!("sha256:{:x}", Sha256::digest(&raw));
        std::fs::write(&path, raw).unwrap();
        let state = directory.path().join("state");
        let mut saved: Option<TextUnitItemDto> = None;
        for _ in 0..2 {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            drop(listener);
            let base = format!("http://127.0.0.1:{port}");
            let mut server = Command::new(env!("CARGO_BIN_EXE_reading-mcp-http"))
                .env("READING_MCP_HTTP_TOKEN", TOKEN)
                .env("READING_MCP_HTTP_BIND", format!("127.0.0.1:{port}"))
                .env("READING_MCP_LOCAL_ROOTS", directory.path())
                .env("READING_MCP_STATE_DIR", &state)
                .env("READING_MCP_TELEMETRY", "false")
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
                .env("READING_MCP_SOURCE_VIEW_MAX_PIXELS", "16000000")
                // Explicit development profile already exercised by stdio.
                .env(
                    "READING_MCP_SOURCE_VIEW_MAX_DECODED_STREAM_BYTES",
                    "33554432",
                )
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .kill_on_drop(true)
                .spawn()
                .unwrap();
            tokio::time::timeout(Duration::from_secs(10), async {
                loop {
                    assert!(
                        server.try_wait().unwrap().is_none(),
                        "HTTP OCR server exited during startup"
                    );
                    if let Ok(response) = reqwest::get(format!("{base}/healthz")).await
                        && response.status().is_success()
                    {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            })
            .await
            .unwrap();
            assert_eq!(
                reqwest::get(format!("{base}/mcp")).await.unwrap().status(),
                reqwest::StatusCode::UNAUTHORIZED
            );
            let transport = StreamableHttpClientTransport::from_config(
                StreamableHttpClientTransportConfig::with_uri(format!("{base}/mcp"))
                    .auth_header(TOKEN),
            );
            let client = ().serve(transport).await.unwrap();
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
            if let Some(item) = &saved {
                assert_eq!(&units.items[0], item);
            } else {
                saved = Some(units.items[0].clone());
            }
            let item = saved.as_ref().unwrap();
            assert!(!item.text.is_empty());
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
            assert_eq!(
                view.normalized_document_hash,
                item.locator.normalized_document_hash
            );
            client.cancel().await.unwrap();
            server.kill().await.unwrap();
        }
    })
    .await
    .expect("real HTTP OCR lifecycle exceeded hosted test deadline");
}
