//! The opt-in usage log contract: off by default, one privacy-safe line per tool call across all
//! seven tools, the exact field set, the `stale_handle` retry flag, rotation/retention, and the
//! no-measurable-latency probe.

#[path = "support/v2_mcp.rs"]
mod v2_mcp;

use std::collections::BTreeSet;
use std::path::Path;
use std::time::{Duration, Instant};

use reading_mcp::mcp::usage_log::USAGE_LINE_KEYS;
use serde_json::{Value, json};
use v2_mcp::{
    call, flat_text, payload, start_client, start_client_with_env, tool_error, v2_open, v2_read,
};

/// The privacy probe markers: they appear in the document text, the query, the source path and
/// the handle checksums, and must never appear in a usage line.
const TEXT_MARKER: &str = "XjqPrivMark47";
const QUERY_MARKER: &str = "ZqUniqueterm88";

fn book() -> String {
    format!(
        "# Book\n\n## Alpha\n\nAlpha sentence one has {TEXT_MARKER} inside it. Alpha sentence two.\n\n## Beta\n\nBeta sentence one carries {QUERY_MARKER} for search.\n"
    )
}

fn usage_dir(state: &Path) -> std::path::PathBuf {
    state.join("usage")
}

/// Poll until every usage file together carries `count` lines; the writer is asynchronous.
fn read_lines_when(dir: &Path, count: usize) -> Vec<Value> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let mut lines = Vec::new();
        if dir.is_dir() {
            let mut entries: Vec<_> = std::fs::read_dir(dir)
                .expect("usage dir readable")
                .flatten()
                .map(|entry| entry.path())
                .collect();
            entries.sort();
            for path in entries {
                for line in std::fs::read_to_string(&path).unwrap_or_default().lines() {
                    lines.push(serde_json::from_str(line).expect("each line is one JSON object"));
                }
            }
        }
        if lines.len() >= count || Instant::now() > deadline {
            return lines;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

fn raw_log_bytes(dir: &Path) -> String {
    let mut bytes = String::new();
    if dir.is_dir() {
        for entry in std::fs::read_dir(dir).unwrap().flatten() {
            bytes.push_str(&std::fs::read_to_string(entry.path()).unwrap_or_default());
        }
    }
    bytes
}

#[tokio::test]
async fn off_by_default_creates_no_usage_directory_stdio() {
    let directory = tempfile::tempdir().expect("temporary directory should be created");
    let state = tempfile::tempdir().expect("temporary state directory should be created");
    let path = directory.path().join("book.md");
    tokio::fs::write(&path, book())
        .await
        .expect("fixture should be written");

    // `server_command` never sets READING_MCP_USAGE_LOG: the default.
    let client = start_client(directory.path(), state.path()).await;
    v2_open(&client, &path).await;
    v2_read(&client, "next", json!({})).await;
    client.cancel().await.expect("client should stop");

    assert!(
        !usage_dir(state.path()).exists(),
        "off by default: no usage/ directory is created"
    );
}

/// Every assertion a usage line must satisfy: exact keys, privacy-safe values.
fn check_line_keys(line: &Value) {
    let keys: BTreeSet<String> = line
        .as_object()
        .expect("a line is a JSON object")
        .keys()
        .cloned()
        .collect();
    for key in &keys {
        assert!(
            USAGE_LINE_KEYS.contains(&key.as_str()),
            "the line carries a key outside the contract: {key} in {line}"
        );
    }
    for required in ["ts", "tool", "outcome", "ms", "resp_bytes"] {
        assert!(keys.contains(required), "missing {required} in {line}");
    }
}

fn lines_for<'a>(lines: &'a [Value], tool: &str) -> Vec<&'a Value> {
    lines.iter().filter(|line| line["tool"] == tool).collect()
}

#[tokio::test]
async fn one_privacy_safe_line_per_call_across_all_seven_tools_stdio() {
    let directory = tempfile::tempdir().expect("temporary directory should be created");
    let state = tempfile::tempdir().expect("temporary state directory should be created");
    let path = directory.path().join("book.md");
    tokio::fs::write(&path, book())
        .await
        .expect("fixture should be written");

    let client = start_client_with_env(
        directory.path(),
        state.path(),
        &[("READING_MCP_USAGE_LOG", "on")],
    )
    .await;

    // 1. open
    let opened = v2_open(&client, &path).await;
    let doc = opened["doc"].as_str().expect("doc handle").to_owned();
    // 2. browse
    call(&client, "browse", json!({})).await;
    // 3. read, one item (sentence default)
    let first = payload(&v2_read(&client, &doc, json!({})).await);
    let sentence = first["at"].as_str().expect("sentence handle").to_owned();
    // 4. read, a batch of several items
    let batch = payload(&v2_read(&client, &doc, json!({"max_chars": 16000})).await);
    let batch_len = batch["items"].as_array().expect("batch items").len() as u64;
    assert!(batch_len >= 3, "the batch must hold several items");
    // 5. read, paragraph unit
    v2_read(&client, &doc, json!({"unit": "paragraph"})).await;
    // 6. too_long, with its retry hint
    let error = tool_error(&v2_read(&client, &doc, json!({"max_chars": 1})).await);
    assert_eq!(error["error"], "too_long");
    // 7. stale_handle without a retry: wrong checksum, no document recovery possible
    let (head, _) = doc.split_once('~').expect("the handle has a checksum");
    let wrong = if doc.ends_with("0000") {
        format!("{head}~1111")
    } else {
        format!("{head}~0000")
    };
    let error = tool_error(&v2_read(&client, &wrong, json!({})).await);
    assert_eq!(error["error"], "stale_handle");
    assert!(error.get("retry").is_none(), "no recovery: no retry");
    // 8. outline, 9. cite
    let outline = payload(&call(&client, "outline", json!({"at": doc})).await);
    assert!(
        !outline["items"]
            .as_array()
            .expect("outline items")
            .is_empty()
    );
    call(&client, "cite", json!({"at": [sentence]})).await;
    // 10. search with the unique query marker
    let search = payload(
        &call(
            &client,
            "search",
            json!({"doc": doc, "query": QUERY_MARKER}),
        )
        .await,
    );
    assert!(
        !search["hits"].as_array().expect("search hits").is_empty(),
        "the marker is in the book: {search}"
    );
    // 11. source_view errors on a markdown document, still one line
    call(&client, "source_view", json!({"at": sentence})).await;
    client.cancel().await.expect("client should stop");

    let lines = read_lines_when(&usage_dir(state.path()), 11);
    assert_eq!(lines.len(), 11, "one line per call: {lines:?}");
    for line in &lines {
        check_line_keys(line);
        // The timestamp is UTC with millisecond precision.
        let ts = line["ts"].as_str().expect("ts");
        assert!(ts.ends_with('Z') && ts.len() == 24, "ts shape: {ts}");
    }

    // Every one of the seven tools appears.
    for tool in [
        "open",
        "browse",
        "read",
        "outline",
        "search",
        "cite",
        "source_view",
    ] {
        assert!(
            !lines_for(&lines, tool).is_empty(),
            "no line for {tool}: {lines:?}"
        );
    }

    let reads = lines_for(&lines, "read");
    // The single sentence read: sentence unit, no batch count, a doc key without the checksum.
    let single = reads
        .iter()
        .find(|line| line["outcome"] == "ok" && line.get("batch_items").is_none())
        .expect("a single read line");
    assert_eq!(single["unit"], "sentence");
    assert_eq!(single["doc"], head, "doc is d<key> without checksum");
    assert!(!single["doc"].as_str().unwrap().contains('~'));
    assert!(single["ms"].is_u64() && single["resp_bytes"].as_u64().unwrap() > 0);
    // The batch read carries the exact item count.
    let batch_line = reads
        .iter()
        .find(|line| line.get("batch_items").is_some())
        .expect("a batched read line");
    assert_eq!(batch_line["batch_items"], batch_len);
    assert_eq!(batch_line["outcome"], "ok");
    // The paragraph read names its unit.
    assert!(
        reads.iter().any(|line| line["unit"] == "paragraph"),
        "a paragraph read line: {reads:?}"
    );
    // too_long is one line with the error kind as outcome.
    let too_long = reads
        .iter()
        .find(|line| line["outcome"] == "too_long")
        .expect("a too_long line");
    assert!(
        too_long.get("retry").is_none(),
        "retry only on stale_handle"
    );
    // stale_handle without a retry answer.
    let stale = reads
        .iter()
        .find(|line| line["outcome"] == "stale_handle")
        .expect("a stale_handle line");
    assert_eq!(stale["retry"], false, "no retry object was emitted");

    // The search line names the document, never the query.
    let search_line = lines_for(&lines, "search")[0];
    assert_eq!(search_line["doc"], head);
    assert_eq!(search_line["outcome"], "ok");
    // cite and outline carry their handle's document key.
    assert_eq!(lines_for(&lines, "cite")[0]["doc"], head);
    assert_eq!(lines_for(&lines, "outline")[0]["doc"], head);
    // open names the document it just minted; browse has no document at all.
    assert_eq!(lines_for(&lines, "open")[0]["doc"], head);
    assert_eq!(lines_for(&lines, "open")[0]["outcome"], "ok");
    assert!(lines_for(&lines, "browse")[0].get("doc").is_none());

    // The privacy proof: the raw log bytes never contain the document text marker, the query
    // marker, the source path, or any handle checksum.
    let raw = raw_log_bytes(&usage_dir(state.path()));
    assert!(!raw.contains(TEXT_MARKER), "document text leaked");
    assert!(!raw.contains(QUERY_MARKER), "the query leaked");
    assert!(
        !raw.contains(&path.to_string_lossy().as_ref().to_owned()),
        "the source path leaked"
    );
    assert!(!raw.contains('~'), "no handle checksum ever appears");
    // Every doc field is exactly `d<digits>`: even digit-only checksums cannot hide in it.
    for line in &lines {
        if let Some(doc_value) = line.get("doc").and_then(Value::as_str) {
            assert!(
                doc_value.strip_prefix('d').is_some_and(
                    |rest| !rest.is_empty() && rest.chars().all(|ch| ch.is_ascii_digit())
                ),
                "doc is exactly d<key>: {doc_value}"
            );
        }
    }
}

#[tokio::test]
async fn stale_handle_with_retry_is_flagged_stdio() {
    let directory = tempfile::tempdir().expect("temporary directory should be created");
    let state = tempfile::tempdir().expect("temporary state directory should be created");
    let path = directory.path().join("book.md");
    tokio::fs::write(
        &path,
        "# Book\n\n## Alpha\n\nAlpha sentence one. Alpha sentence two.\n\nSecond paragraph here.\n",
    )
    .await
    .expect("fixture should be written");

    let first = start_client_with_env(
        directory.path(),
        state.path(),
        &[("READING_MCP_USAGE_LOG", "on")],
    )
    .await;
    let opened = v2_open(&first, &path).await;
    let doc = opened["doc"].as_str().expect("doc handle").to_owned();
    let read = payload(&v2_read(&first, &doc, json!({})).await);
    assert_eq!(flat_text(&read), "Alpha sentence one.");
    let sentence = read["at"].as_str().expect("sentence handle").to_owned();
    first.cancel().await.expect("first client should stop");

    // A whitespace-only extraction deploy: the old sentence is stale, and handle recovery proves
    // it, so the error carries a concrete retry call.
    let second = start_client_with_env(
        directory.path(),
        state.path(),
        &[
            ("READING_MCP_USAGE_LOG", "on"),
            ("READING_MCP_TEST_EXTRACTION_VARIANT", "spaces"),
        ],
    )
    .await;
    let stale = v2_read(&second, &sentence, json!({})).await;
    let error = tool_error(&stale);
    assert_eq!(error["error"], "stale_handle");
    assert!(
        error["retry"]["from"].is_string(),
        "a proven retry: {error}"
    );
    second.cancel().await.expect("second client should stop");

    // Both servers wrote into the same usage directory: one stale line with retry false (none),
    // one with retry true (the recovery-proven one above).
    let lines = read_lines_when(&usage_dir(state.path()), 4);
    let stale_lines: Vec<_> = lines
        .iter()
        .filter(|line| line["outcome"] == "stale_handle")
        .collect();
    assert_eq!(stale_lines.len(), 1, "one stale line: {lines:?}");
    assert_eq!(
        stale_lines[0]["retry"], true,
        "the recovery-proven stale is flagged retry=true: {stale_lines:?}"
    );
}

// ------------------------------------------------------------------------------------------------
// Latency probe: 1,000 sequential reads, log on vs off. Not a CI gate — run with --ignored.
// ------------------------------------------------------------------------------------------------

fn percentile(samples: &[u128], percentile: f64) -> u128 {
    let mut sorted = samples.to_vec();
    sorted.sort_unstable();
    let index = ((sorted.len() as f64) * percentile / 100.0) as usize;
    sorted[index.min(sorted.len() - 1)]
}

async fn sequential_reads(state: &Path, directory: &Path, extra: &[(&str, &str)]) -> Vec<u128> {
    let path = directory.join("bench-book.md");
    let client = start_client_with_env(directory, state, extra).await;
    let opened = v2_open(&client, &path).await;
    let doc = opened["doc"].as_str().expect("doc handle").to_owned();
    // Warm up: the first read materializes the sentence stream.
    v2_read(&client, &doc, json!({})).await;
    let mut samples = Vec::with_capacity(1_000);
    let mut from = doc.clone();
    for _ in 0..1_000 {
        let start = Instant::now();
        let result = payload(&v2_read(&client, &from, json!({})).await);
        samples.push(start.elapsed().as_micros());
        match result["next"].as_str() {
            Some(next) => from = next.to_owned(),
            None => from = doc.clone(),
        }
    }
    client.cancel().await.expect("client should stop");
    samples
}

#[tokio::test]
#[ignore = "manual latency probe: cargo test --test usage_log latency_probe -- --ignored --nocapture"]
async fn latency_probe_one_thousand_sequential_reads_on_and_off() {
    let directory = tempfile::tempdir().expect("temporary directory should be created");
    let path = directory.path().join("bench-book.md");
    let mut book_text = String::from("# Bench\n\n## Section\n\n");
    for index in 0..1_200 {
        book_text.push_str(&format!("Sentence number {index} of the bench book. "));
    }
    book_text.push('\n');
    tokio::fs::write(&path, book_text)
        .await
        .expect("fixture should be written");

    let state_off = tempfile::tempdir().expect("state");
    let off = sequential_reads(state_off.path(), directory.path(), &[]).await;
    let state_on = tempfile::tempdir().expect("state");
    let on = sequential_reads(
        state_on.path(),
        directory.path(),
        &[("READING_MCP_USAGE_LOG", "on")],
    )
    .await;

    // The log lines of the `on` run also carry the server-side latency, isolating the logging
    // cost from the stdio round trip the client-side samples include.
    let server_ms: Vec<u128> = read_lines_when(&state_on.path().join("usage"), 1_001)
        .iter()
        .filter(|line| line["tool"] == "read" && line["outcome"] == "ok")
        .filter_map(|line| line["ms"].as_u64().map(u128::from))
        .collect();

    println!("usage-log latency probe, 1,000 sequential reads:");
    println!("                    p50      p95");
    println!(
        "client µs, off  {:7} {:8}",
        percentile(&off, 50.0),
        percentile(&off, 95.0)
    );
    println!(
        "client µs, on   {:7} {:8}",
        percentile(&on, 50.0),
        percentile(&on, 95.0)
    );
    println!(
        "server ms, on   {:7} {:8}   (the ms field of the log lines themselves)",
        percentile(&server_ms, 50.0),
        percentile(&server_ms, 95.0)
    );
}
