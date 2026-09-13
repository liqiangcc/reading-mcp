use reading_mcp::mcp::contracts::{
    GetContextResponse, GetTextUnitsResponse, OpenDocumentResponse, ReadDocumentResponse,
    SearchCandidateKindDto, SearchDocumentResponse, TextUnitItemDto,
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
            let mut command = Command::new(env!("CARGO_BIN_EXE_reading-mcp"));
            command
                .env("READING_MCP_LOCAL_ROOTS", directory.path())
                .env("READING_MCP_STATE_DIR", &state)
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
                let opened = client
                    .call_tool(CallToolRequestParams::new("open_document").with_arguments(
                        arguments(json!({"source":directory.path().join(format!("{case}.pdf"))})),
                    ))
                    .await
                    .unwrap()
                    .into_typed::<OpenDocumentResponse>()
                    .unwrap();
                let units = client
                    .call_tool(CallToolRequestParams::new("get_text_units").with_arguments(
                        arguments(json!({"document_id":opened.document_id,
                        "section_id":"section://pdf-layout/1", "requested_kind":"paragraph",
                        "coverage_policy":"preserve_source", "max_items":100})),
                    ))
                    .await
                    .unwrap()
                    .into_typed::<GetTextUnitsResponse>()
                    .unwrap();
                // Authored expectations are not consulted until the real MCP output exists.
                let expected: Value = serde_json::from_slice(
                    &std::fs::read(inputs.join(format!("{case}.expected.json"))).unwrap(),
                )
                .unwrap();
                let texts: Vec<_> = units.items.iter().map(|item| item.text.as_str()).collect();
                assert_eq!(json!(texts), expected["paragraphs"], "{case}");
                let actual = (opened.normalized_document_hash, units.items);
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
    for case in ["F01", "F02", "F03", "F05", "F12"] {
        std::fs::copy(
            format!("tests/fixtures/scanned_pdf/pdf/{case}.pdf"),
            directory.path().join(format!("{case}.pdf")),
        )
        .unwrap();
    }
    let state = directory.path().join("state");
    let mut command = Command::new(env!("CARGO_BIN_EXE_reading-mcp"));
    command
        .env("READING_MCP_LOCAL_ROOTS", directory.path())
        .env("READING_MCP_STATE_DIR", &state)
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
            client.call_tool(CallToolRequestParams::new("open_document").with_arguments(
                arguments(json!({"source":directory.path().join(format!("{case}.pdf"))})),
            )),
        )
        .await
        .unwrap()
        .unwrap()
        .into_typed::<OpenDocumentResponse>()
        .unwrap();
        assert!(!opened.normalized_document_hash.is_empty());
    }
    let before = published_documents(&state);
    assert_eq!(before.len(), 2);
    for case in ["F02", "F05", "F12"] {
        let error = tokio::time::timeout(std::time::Duration::from_secs(30),
            client.call_tool(CallToolRequestParams::new("open_document").with_arguments(
                arguments(json!({"source":directory.path().join(format!("{case}.pdf")), "force_refresh":true})),
            )))
            .await.unwrap().expect_err("scanned pages must not become partial successful documents");
        assert!(
            error.to_string().contains("OCR_REQUIRED"),
            "{case}: {error}"
        );
        assert!(error.to_string().contains("\"retryable\":false"));
        assert!(
            !error
                .to_string()
                .contains(directory.path().to_str().unwrap())
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
            let mut command = Command::new(env!("CARGO_BIN_EXE_reading-mcp"));
            command
                .env(
                    "READING_MCP_LOCAL_ROOTS",
                    std::env::join_paths([directory.path()]).unwrap(),
                )
                .env("READING_MCP_STATE_DIR", &state)
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
                client.call_tool(CallToolRequestParams::new("open_document").with_arguments(
                    arguments(json!({
                        "source":path, "force_refresh":true
                    })),
                )),
            )
            .await
            .expect("open exceeded shared benchmark budget")
            .unwrap()
            .into_typed::<OpenDocumentResponse>()
            .unwrap();
            let elapsed = started.elapsed();
            assert_eq!(opened.content_hash, raw_hash);
            if let Some(identity) = &cold_identity {
                assert_eq!(&opened.normalized_document_hash, identity);
            } else {
                cold_identity = Some(opened.normalized_document_hash.clone());
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
                "normalized_document_hash":opened.normalized_document_hash});
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
            let search = client.call_tool(
                CallToolRequestParams::new("search_document").with_arguments(arguments(json!({
                    "document_id":opened.document_id, "query":"观测站", "limit":10
                })))
            ).await.unwrap().into_typed::<SearchDocumentResponse>().unwrap();
            let hit = search.hits.iter().find(|hit|
                hit.candidate_kind == SearchCandidateKindDto::Sentence
                    && hit.text_locator == item.locator
            ).expect("real OCR CJK search must return the saved canonical sentence locator");
            assert_eq!(&hit.text_locator.normalized_document_hash, hash);
            let hit_read = client.call_tool(
                CallToolRequestParams::new("read_document").with_arguments(arguments(json!({
                    "document_id":opened.document_id, "target_locator":hit.text_locator, "max_chars":8192
                })))
            ).await.unwrap().into_typed::<ReadDocumentResponse>().unwrap();
            assert!(hit_read.complete);
            assert_eq!(hit_read.content, item.text);
            assert_eq!(hit_read.resolved_target_locator, hit.text_locator);
            let context = client.call_tool(
                CallToolRequestParams::new("get_context").with_arguments(arguments(json!({
                    "document_id":opened.document_id, "target_locator":hit.text_locator,
                    "relation":{"type":"neighbor", "unit":"sentence", "before":0, "after":1},
                    "max_chars":8192
                })))
            ).await.unwrap().into_typed::<GetContextResponse>().unwrap();
            assert!(context.complete);
            assert_eq!(context.anchor_locator, item.locator);
            assert_eq!(context.items.len(), 2);
            assert_eq!(context.items[0].content.as_deref(), Some(item.text.as_str()));
            assert!(context.items[1].content.as_deref().is_some_and(|text| !text.is_empty()));
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
