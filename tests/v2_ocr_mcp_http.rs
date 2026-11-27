//! V2-4b PR-2, §5: ports `tests/ocr_mcp_http.rs` — a real-OCR saved locator survives an HTTP
//! server restart — to the v2 tools.
//!
//! v1: `open_document` → `get_text_units` → `read_document{target_locator}` → `get_source_view`.
//! v2: `open` → `read` (the page's first sentence) → `cite` (its locator and identity) → `read`
//! from the sentence handle → `source_view`. `resolved_target_locator` is a v1 response field with
//! no v2 name (§2.1 item 3); the locator it carried is asserted through `cite`.
//!
//! Gated exactly like the v1 file: `#[ignore = "requires pinned hosted OCR dependencies"]`.

#[path = "support/v2_mcp.rs"]
mod v2_mcp;

use std::time::Duration;

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use v2_mcp::{
    call, flat_text, http_server_command, identity_of, locator_of, payload, reserve_port,
    section_handle_of, start_http_client, v2_open, v2_read,
};

#[tokio::test]
#[ignore = "requires pinned hosted OCR dependencies"]
async fn real_ocr_http_saved_locator_survives_server_restart() {
    tokio::time::timeout(Duration::from_secs(120), async {
        let directory = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let path = directory.path().join("public-F07.pdf");
        let raw = std::fs::read("tests/fixtures/scanned_pdf/pdf/F07.pdf").unwrap();
        let raw_hash = format!("sha256:{:x}", Sha256::digest(&raw));
        std::fs::write(&path, raw).unwrap();
        let mut saved: Option<(String, Value)> = None;
        for _ in 0..2 {
            let port = reserve_port();
            let base = format!("http://127.0.0.1:{port}");
            let mut command = http_server_command(port, directory.path(), state.path());
            command
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
                );
            let mut server = command.spawn().unwrap();
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
            let client = start_http_client(&base).await;
            let opened = v2_open(&client, &path).await;
            let doc = opened["doc"].as_str().expect("doc handle").to_owned();
            let identity = identity_of(&client, &doc).await;
            assert_eq!(identity["content_hash"], raw_hash);
            let page = section_handle_of(&client, &doc, "section://pdf-layout/1").await;
            let units = payload(&v2_read(&client, &page, json!({})).await);
            assert!(
                units.get("items").is_none(),
                "a flat read is exactly one item: {units}"
            );
            let text = flat_text(&units);
            let sentence = units["at"]
                .as_str()
                .expect("the first sentence's own handle")
                .to_owned();
            let locator = locator_of(&client, &sentence).await;
            if let Some((saved_text, saved_locator)) = &saved {
                assert_eq!(&text, saved_text);
                assert_eq!(&locator, saved_locator);
            } else {
                saved = Some((text.clone(), locator.clone()));
            }
            let (saved_text, _) = saved.as_ref().unwrap();
            assert!(!saved_text.is_empty());
            let read = payload(&v2_read(&client, &sentence, json!({})).await);
            assert_eq!(&flat_text(&read), saved_text);
            let result = call(&client, "source_view", json!({ "at": sentence })).await;
            assert!(
                result.is_error != Some(true),
                "source view should render over HTTP: {result:?}"
            );
            let wire = serde_json::to_value(&result).unwrap();
            assert!(
                wire["content"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|block| block["type"] == "image" && block["mimeType"] == "image/jpeg")
            );
            let metadata = payload(&result);
            assert_eq!(metadata["page"], 1);
            assert_eq!(metadata["pages"], 1);
            assert_eq!(locator["content_hash"], raw_hash);
            assert_eq!(
                identity["normalized_document_hash"], locator["normalized_document_hash"],
                "the rendered page is bound to the cited locator's document"
            );
            client.cancel().await.unwrap();
            server.kill().await.unwrap();
        }
    })
    .await
    .expect("real HTTP OCR lifecycle exceeded hosted test deadline");
}
