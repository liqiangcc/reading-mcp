//! V2-4b PR-2, §5: ports `tests/ocr_mcp_stdio.rs` — four real-OCR stdio tests — to the v2 tools.
//!
//! The v1 tests drove `open_document`, `get_text_units`, `search_document`, `read_document`,
//! `get_context` and `get_source_view`. The v2 journey is `open` → `outline` (the page section) →
//! `read` (paragraph stream for the mixed order, sentence for the locator test) → `cite` (the
//! locator and identity) → `search` → `read` from the hit → `source_view`. `get_context`'s
//! neighbour window and the `reading_profile` degradation codes are dropped per §2.1 items 1, 3
//! and 5; v1 response fields with no v2 name (`candidate_kind`, `resolved_target_locator`,
//! `complete`) are dropped per §2.1 item 3, and their locators are asserted through `cite`.
//!
//! Gated like the v1 file: `#[ignore]` with the same `READING_MCP_MIXED_ORDER_DIR` /
//! `READING_MCP_PDF_LAYOUT_PYTHON` environment. The four ported tests keep the v1 gate verbatim.
//! One extra test, `scanned_pdf_stdio_default_stream_limit_is_reported`, was a **v2 gap** until
//! #264: v2 `source_view` reported the default 16-MiB decoded-stream rejection as `internal`
//! because `ApplicationError::SourceViewFailed` had no v2 code. The worker now reports the
//! rejection as data (`ApplicationError::SourceViewLimitExceeded`), so the test asserts
//! `too_large` and a hint naming the page, the decoded size and the limit.

#[path = "support/v2_mcp.rs"]
mod v2_mcp;

use rmcp::ServiceExt;
use rmcp::transport::TokioChildProcess;
use serde_json::{Value, json};
use sha2::Digest;

use v2_mcp::{
    call, flat_text, identity_of, locator_of, payload, section_handle_of, section_items_with,
    server_command, tool_error, v2_open, v2_read,
};

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

/// The identity and the cited locators of a paragraph stream: the v1 `(normalized_document_hash,
/// items)` tuple.
async fn identity_and_locators(
    client: &v2_mcp::Client,
    doc: &str,
    section: &str,
) -> (Value, Vec<(String, Value)>) {
    let items = section_items_with(client, section, json!({ "unit": "paragraph" }), 16_000).await;
    let mut located = Vec::new();
    for (handle, text) in items {
        located.push((text, locator_of(client, &handle).await));
    }
    (
        identity_of(client, doc).await["normalized_document_hash"].clone(),
        located,
    )
}

#[tokio::test]
#[ignore = "requires pinned hosted OCR dependencies"]
async fn mixed_native_order_survives_real_mcp_publication_and_restart() {
    tokio::time::timeout(std::time::Duration::from_secs(180), async {
        let inputs =
            std::path::PathBuf::from(std::env::var("READING_MCP_MIXED_ORDER_DIR").unwrap());
        let directory = tempfile::tempdir().unwrap();
        let state = directory.path().join("state");
        let cases = [
            "scan_then_native",
            "native_then_scan",
            "alternating_vertical",
        ];
        for case in cases {
            std::fs::copy(
                inputs.join(format!("{case}.pdf")),
                directory.path().join(format!("{case}.pdf")),
            )
            .unwrap();
        }
        let mut previous = Vec::new();
        for restart in [false, true] {
            let mut command = server_command(directory.path(), &state);
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
                .env("READING_MCP_OCR_LANG", "eng")
                .env("READING_MCP_OCR_REVISION", "1")
                .kill_on_drop(true);
            let client = ().serve(TokioChildProcess::new(command).unwrap()).await.unwrap();
            for (index, case) in cases.iter().enumerate() {
                let opened = v2_open(&client, &directory.path().join(format!("{case}.pdf"))).await;
                let doc = opened["doc"].as_str().expect("doc handle").to_owned();
                let page = section_handle_of(&client, &doc, "section://pdf-layout/1").await;
                let (identity, located) = identity_and_locators(&client, &doc, &page).await;
                // Authored expectations are not consulted until the real MCP output exists.
                let expected: Value = serde_json::from_slice(
                    &std::fs::read(inputs.join(format!("{case}.expected.json"))).unwrap(),
                )
                .unwrap();
                let texts: Vec<_> = located.iter().map(|(text, _)| text.as_str()).collect();
                assert_eq!(json!(texts), expected["paragraphs"], "{case}");
                let actual = (identity, located);
                if restart {
                    assert_eq!(
                        &actual, &previous[index],
                        "{case}: restart changed identity or locators"
                    );
                } else {
                    previous.push(actual);
                }
            }
            assert_eq!(published_documents(&state).len(), cases.len());
            client.cancel().await.unwrap();
        }
    })
    .await
    .expect("bounded mixed-page MCP acceptance");
}

#[tokio::test]
#[ignore = "requires pinned hosted OCR dependencies"]
async fn disabled_ocr_preserves_native_documents_and_rejects_incomplete_scans() {
    let directory = tempfile::tempdir().unwrap();
    let inputs = std::path::PathBuf::from(std::env::var("READING_MCP_MIXED_ORDER_DIR").unwrap());
    let mixed_cases = [
        "scan_then_native",
        "native_then_scan",
        "alternating_vertical",
        "scan_left_native_right",
        "native_left_scan_right",
        "alternating_columns",
    ];
    for case in mixed_cases {
        std::fs::copy(
            inputs.join(format!("{case}.pdf")),
            directory.path().join(format!("{case}.pdf")),
        )
        .unwrap();
    }
    for case in ["F01", "F02", "F03", "F05", "F12"] {
        std::fs::copy(
            format!("tests/fixtures/scanned_pdf/pdf/{case}.pdf"),
            directory.path().join(format!("{case}.pdf")),
        )
        .unwrap();
    }
    let state = directory.path().join("state");
    let mut command = server_command(directory.path(), &state);
    command
        .env(
            "READING_MCP_PDF_LAYOUT_PYTHON",
            std::env::var("READING_MCP_PDF_LAYOUT_PYTHON").unwrap(),
        )
        .env("READING_MCP_OCR_ENABLED", "false")
        .env("READING_MCP_OCR_ENGINE", "/nonexistent/disabled-engine")
        .env("READING_MCP_OCR_TESSDATA", "/nonexistent/disabled-models")
        .kill_on_drop(true);
    let (transport, _) = TokioChildProcess::builder(command).spawn().unwrap();
    let client = ().serve(transport).await.unwrap();
    for case in ["F01", "F03"] {
        let opened = tokio::time::timeout(
            std::time::Duration::from_secs(30),
            v2_open(&client, &directory.path().join(format!("{case}.pdf"))),
        )
        .await
        .unwrap();
        let doc = opened["doc"].as_str().expect("doc handle").to_owned();
        assert!(
            !identity_of(&client, &doc).await["normalized_document_hash"]
                .as_str()
                .unwrap()
                .is_empty()
        );
    }
    let before = published_documents(&state);
    assert_eq!(before.len(), 2);
    for case in ["F02", "F05", "F12"].into_iter().chain(mixed_cases) {
        let error = tokio::time::timeout(
            std::time::Duration::from_secs(30),
            call(
                &client,
                "open",
                json!({
                    "source": directory.path().join(format!("{case}.pdf")),
                    "refresh": true
                }),
            ),
        )
        .await
        .unwrap();
        let error = tool_error(&error);
        assert_eq!(error["error"], "ocr_required", "{case}: {error}");
        assert!(
            error.get("retry").is_none(),
            "{case}: an ocr_required answer is not retryable: {error}"
        );
        assert!(
            !error
                .to_string()
                .contains(directory.path().to_str().unwrap()),
            "{case}: the error must not leak the source path: {error}"
        );
        assert_eq!(published_documents(&state), before);
    }
    client.cancel().await.unwrap();
}

#[tokio::test]
#[ignore = "requires pinned hosted OCR dependencies"]
async fn frozen_four_page_cold_and_restarted_warm_open_budgets() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("public-F05.pdf");
    let raw = std::fs::read("tests/fixtures/scanned_pdf/pdf/F05.pdf").unwrap();
    std::fs::write(&path, &raw).unwrap();
    let raw_hash = format!("sha256:{:x}", sha2::Sha256::digest(&raw));
    let mut report = Vec::new();
    for trial in 0..5 {
        // Each cold run starts with a distinct absent persistent state. Warm
        // runs restart the actual server, retaining only that trial's state.
        let state = directory.path().join(format!("state-{trial}"));
        assert!(!state.exists());
        let mut cold_identity = None;
        for warm in [false, true] {
            let telemetry_path = directory.path().join(format!("telemetry-{trial}-{warm}"));
            let telemetry = std::fs::File::create(&telemetry_path).unwrap();
            let mut command = server_command(directory.path(), &state);
            command
                .env("READING_MCP_TELEMETRY", "true")
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
                .env("READING_MCP_OCR_LANG", "eng")
                .env("READING_MCP_OCR_REVISION", "1")
                .kill_on_drop(true);
            let budget = std::time::Duration::from_secs(if warm { 5 } else { 45 });
            let started = tokio::time::Instant::now();
            let deadline = started + budget;
            let (transport, _) = TokioChildProcess::builder(command)
                .stderr(telemetry)
                .spawn()
                .unwrap();
            let client = tokio::time::timeout_at(deadline, ().serve(transport))
                .await
                .expect("server startup exceeded shared benchmark budget")
                .unwrap();
            let opened = tokio::time::timeout_at(
                deadline,
                call(&client, "open", json!({ "source": path, "refresh": true })),
            )
            .await
            .expect("open exceeded shared benchmark budget");
            let doc = payload(&opened)["doc"]
                .as_str()
                .expect("doc handle")
                .to_owned();
            let elapsed = started.elapsed();
            let identity = identity_of(&client, &doc).await;
            assert_eq!(identity["content_hash"], raw_hash);
            if let Some(known) = &cold_identity {
                assert_eq!(&identity["normalized_document_hash"], known);
            } else {
                cold_identity = Some(identity["normalized_document_hash"].clone());
            }
            client.cancel().await.unwrap();
            let events: Vec<Value> = std::fs::read_to_string(&telemetry_path)
                .unwrap()
                .lines()
                .filter_map(|line| serde_json::from_str(line).ok())
                .collect();
            let cache_gets: Vec<_> = events
                .iter()
                .filter(|event| event["event"] == "parsed_cache_get")
                .collect();
            // Cold opens recheck after single-flight admission; warm hits return
            // before admission and must have exactly one successful lookup.
            assert_eq!(cache_gets.len(), if warm { 1 } else { 2 });
            for event in cache_gets {
                assert_eq!(event["success"], true);
                assert_eq!(event["hit"], warm);
            }
            let cache_puts = events
                .iter()
                .filter(|event| event["event"] == "parsed_cache_put")
                .count();
            assert_eq!(cache_puts, usize::from(!warm));
            // A successful hit returns before the inner Parser in CachingParser;
            // this is a cache bypass proof, not a fabricated engine counter.
            let record = json!({"trial":trial, "warm_after_restart":warm,
                "startup_and_open_seconds":elapsed.as_secs_f64(), "limit_seconds":budget.as_secs(),
                "parsed_cache_hit":warm, "parsed_cache_put_events":cache_puts,
                "normalized_document_hash":identity["normalized_document_hash"]});
            println!("frozen_F05_mcp_performance {}", record);
            report.push(record);
        }
    }
    println!(
        "{}",
        json!({"schema":"ocr-frozen-four-page-mcp-performance/v1",
        "fixture":"F05", "scope":"hosted stdio; not production connector or private-paper acceptance",
        "raw_sha256":raw_hash, "runs":report})
    );
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
        let mut saved: Option<(Value, String, Value)> = None;
        // The 300-DPI RGB fixture decodes to about 26.1 MB. The explicit development
        // profile renders it; the default 16-MiB rejection is the recorded gap test
        // below. This does not change defaults or approve a production budget change.
        for stream_limit in [32 * 1024 * 1024_u64, 32 * 1024 * 1024] {
            let mut command = server_command(directory.path(), &state);
            command
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
            assert!(!text.is_empty());
            let sentence = units["at"]
                .as_str()
                .expect("the first sentence's own handle")
                .to_owned();
            let locator = locator_of(&client, &sentence).await;
            if let Some((hash, saved_text, saved_locator)) = &saved {
                assert_eq!(&identity["normalized_document_hash"], hash);
                assert_eq!(&text, saved_text);
                assert_eq!(&locator, saved_locator);
            } else {
                saved = Some((
                    identity["normalized_document_hash"].clone(),
                    text.clone(),
                    locator.clone(),
                ));
            }
            let (hash, saved_text, _) = saved.as_ref().unwrap();
            let search = payload(
                &call(
                    &client,
                    "search",
                    json!({ "doc": doc, "query": "观测站", "limit": 10 }),
                )
                .await,
            );
            let mut hit = None;
            for candidate in search["hits"].as_array().unwrap() {
                let handle = candidate[0].as_str().unwrap();
                if locator_of(&client, handle).await == locator {
                    hit = Some(handle.to_owned());
                    break;
                }
            }
            let hit =
                hit.expect("real OCR CJK search must return the saved canonical sentence locator");
            assert_eq!(
                locator_of(&client, &hit).await["normalized_document_hash"],
                *hash,
                "the hit locator is bound to the saved normalized identity"
            );
            let hit_read = payload(&v2_read(&client, &hit, json!({})).await);
            assert_eq!(&flat_text(&hit_read), saved_text);
            let before_failure = published_documents(&state);
            assert_eq!(before_failure.len(), 1);
            let blank_error = call(&client, "open", json!({ "source": blank_path })).await;
            println!("F09 MCP failure: {blank_error:?}");
            let error = tool_error(&blank_error);
            assert_eq!(error["error"], "ocr_required", "{error}");
            assert!(
                error.get("retry").is_none(),
                "an ocr_required answer is not retryable: {error}"
            );
            assert_eq!(
                published_documents(&state),
                before_failure,
                "failed blank ingestion must leave canonical documents byte-identical"
            );
            let read = payload(&v2_read(&client, &sentence, json!({})).await);
            assert_eq!(&flat_text(&read), saved_text);
            let result = call(&client, "source_view", json!({ "at": sentence })).await;
            assert!(
                result.is_error != Some(true),
                "source view should render: {result:?}"
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
            assert_eq!(&locator["normalized_document_hash"], hash);
            client.cancel().await.unwrap();
        }
    })
    .await
    .expect("OCR stdio lifecycle exceeded the hosted test deadline");
}

/// The v1 default-limit leg, which ran skipped by name in
/// `.github/workflows/ocr-first-engine-probe.yml` while the mapping was a v2 gap. #264 made the
/// worker report the rejection as data, so v2 now answers `too_large` with the page, the decoded
/// size and the limit in the hint.
///
/// v1 asserted that the default 16-MiB decoded-stream limit rejects the 26.1-MB page with
/// `SOURCE_VIEW_FAILED` and the message "decoded PDF stream exceeds configured limit"; v1 keeps
/// that code and message. v2 `source_view` maps the same rejection to `too_large`.
#[tokio::test]
#[ignore = "requires pinned hosted OCR dependencies"]
async fn scanned_pdf_stdio_default_stream_limit_is_reported() {
    tokio::time::timeout(std::time::Duration::from_secs(120), async {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("public-F07.pdf");
        let raw = std::fs::read("tests/fixtures/scanned_pdf/pdf/F07.pdf").unwrap();
        std::fs::write(&path, &raw).unwrap();
        let state = directory.path().join("state");
        let mut command = server_command(directory.path(), &state);
        command
            // Existing #92 deployment profile; retain the production and
            // default guards rather than bypassing embedded-image checks.
            .env("READING_MCP_SOURCE_VIEW_MAX_PIXELS", "16000000")
            .env(
                "READING_MCP_SOURCE_VIEW_MAX_DECODED_STREAM_BYTES",
                (16 * 1024 * 1024_u64).to_string(),
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
        let opened = v2_open(&client, &path).await;
        let doc = opened["doc"].as_str().expect("doc handle").to_owned();
        let page = section_handle_of(&client, &doc, "section://pdf-layout/1").await;
        let units = payload(&v2_read(&client, &page, json!({})).await);
        let sentence = units["at"]
            .as_str()
            .expect("the first sentence's own handle")
            .to_owned();
        let result = call(&client, "source_view", json!({ "at": sentence })).await;
        let error = tool_error(&result);
        assert_eq!(
            error["error"], "too_large",
            "the default decoded-stream limit rejects the page: {error}"
        );
        let hint = error["hint"].as_str().expect("a too_large hint");
        assert!(hint.contains("第 1 页"), "{hint}");
        assert!(hint.contains("26,106,960"), "{hint}");
        assert!(hint.contains("16,777,216"), "{hint}");
        client.cancel().await.unwrap();
    })
    .await
    .expect("the default-limit rejection exceeded the hosted test deadline");
}
