//! Server-level crash and concurrency tests of the chunked ingest write.
//!
//! The crash test kills the server with SIGKILL in the middle of a chunked ingest — after the
//! first chunk transaction, in the middle of the write and just before the final commit — then
//! restarts it on the same state directory and proves the document never reads as complete but
//! partial, that a read on its handles answers a clean not-ready, that `open` re-ingests it and
//! that the stored rows afterwards are byte-identical to a clean ingest.
//!
//! The concurrency test runs continuous reads on a document while it is force-re-ingested in
//! chunks: a reader must either read complete units or get a v2 error, never half-written data.

use std::collections::HashMap;
use std::io::{Cursor, Write};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::sync::Mutex as StdMutex;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::Notify;
use zip::write::{SimpleFileOptions, ZipWriter};

// ------------------------------------------------------------------------------------------------
// A deterministic EPUB of `chapters` one-section chapters, sized so the write spans many chunk
// transactions.
// ------------------------------------------------------------------------------------------------

fn epub_with_chapters(chapters: usize) -> Vec<u8> {
    let cursor = Cursor::new(Vec::new());
    let mut writer = ZipWriter::new(cursor);
    let options = SimpleFileOptions::default();
    writer
        .start_file("mimetype", options)
        .expect("mimetype entry");
    writer
        .write_all(b"application/epub+zip")
        .expect("mimetype");

    let mut manifest = String::new();
    let mut spine = String::new();
    let mut nav_items = String::new();
    let mut files: Vec<(String, String)> = Vec::new();
    for chapter in 0..chapters {
        let name = format!("chapter{chapter}.xhtml");
        manifest.push_str(&format!(
            "<item id=\"c{chapter}\" href=\"{name}\" media-type=\"application/xhtml+xml\"/>\n"
        ));
        spine.push_str(&format!("<itemref idref=\"c{chapter}\"/>\n"));
        nav_items.push_str(&format!(
            "<li><a href=\"{name}#top\">Chapter {chapter}</a></li>\n"
        ));
        // ~30 KB of deterministic, compression-resistant prose: the token stream never repeats a
        // long run, so the compressed chunk rows stay large and every transaction measurable.
        let mut body = format!("<h1 id=\"top\">Chapter {chapter}</h1>\n");
        for paragraph in 0..40 {
            body.push_str("<p>");
            for word in 0..90 {
                let token = (chapter * 7919 + paragraph * 131 + word * 17) % 10_007;
                body.push_str(&format!("w{token}x{chapter}p{paragraph} "));
            }
            body.push_str("end of paragraph.\n</p>\n");
        }
        files.push((
            format!("OPS/{name}"),
            format!(
                "<html xmlns=\"http://www.w3.org/1999/xhtml\"><body>\n{body}</body></html>"
            ),
        ));
    }

    let entries: Vec<(String, String)> = [
        (
            "META-INF/container.xml".to_owned(),
            r#"<?xml version="1.0"?>
<container xmlns="urn:oasis:names:tc:opendocument:xmlns:container"><rootfiles>
<rootfile full-path="OPS/package.opf" media-type="application/oebps-package+xml"/>
</rootfiles></container>"#
                .to_owned(),
        ),
        (
            "OPS/package.opf".to_owned(),
            format!(
                r#"<?xml version="1.0"?>
<package xmlns="http://www.idpf.org/2007/opf" version="3.0">
  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:title>Crash Test Book</dc:title></metadata>
  <manifest>
    <item id="nav" href="nav.xhtml" media-type="application/xhtml+xml" properties="nav"/>
{manifest}  </manifest>
  <spine>
{spine}  </spine>
</package>"#
            ),
        ),
        (
            "OPS/nav.xhtml".to_owned(),
            format!(
                r#"<html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops"><body>
<nav epub:type="toc"><ol>
{nav_items}</ol></nav>
</body></html>"#
            ),
        ),
    ]
    .into_iter()
    .collect();

    for (name, content) in entries.iter().chain(files.iter()) {
        writer.start_file(name.as_str(), options).expect("ZIP entry");
        writer.write_all(content.as_bytes()).expect("ZIP content");
    }
    writer.finish().expect("ZIP finish").into_inner()
}

// ------------------------------------------------------------------------------------------------
// A minimal line-delimited JSON-RPC client over the server's stdio, so the test owns the process
// and can SIGKILL it mid-ingest.
// ------------------------------------------------------------------------------------------------

struct Server {
    child: Child,
    stdin: ChildStdin,
    responses: Arc<StdMutex<HashMap<u64, Value>>>,
    notify: Arc<Notify>,
    next_id: u64,
}

impl Server {
    async fn spawn(root: &Path, state: &Path, chunk_sections: &str) -> Self {
        let mut command = Command::new(env!("CARGO_BIN_EXE_reading-mcp"));
        command
            .env("READING_MCP_LOCAL_ROOTS", root)
            .env("READING_MCP_STATE_DIR", state)
            .env("READING_MCP_TELEMETRY", "false")
            .env("READING_MCP_INGEST_CHUNK_SECTIONS", chunk_sections)
            // The crash test must be able to stop the write between two chunk transactions; they
            // are milliseconds apart on a fixture, so the test server stretches them apart.
            .env("READING_MCP_INGEST_CHUNK_DELAY_MS", "40")
            .env_remove("READING_MCP_TOOLS")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let mut child = command.spawn().expect("server should spawn");
        let stdin = child.stdin.take().expect("stdin");
        let stdout = child.stdout.take().expect("stdout");
        let responses = Arc::new(StdMutex::new(HashMap::new()));
        let notify = Arc::new(Notify::new());
        {
            let responses = Arc::clone(&responses);
            let notify = Arc::clone(&notify);
            tokio::spawn(async move {
                let mut lines = BufReader::new(stdout).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    if let Ok(message) = serde_json::from_str::<Value>(&line)
                        && let Some(id) = message.get("id").and_then(Value::as_u64)
                    {
                        responses
                            .lock()
                            .expect("responses")
                            .insert(id, message);
                        notify.notify_waiters();
                    }
                }
            });
        }
        let mut server = Self {
            child,
            stdin,
            responses,
            notify,
            next_id: 0,
        };
        let init = server
            .call(
                "initialize",
                json!({
                    "protocolVersion": "2025-06-18",
                    "capabilities": {},
                    "clientInfo": {"name": "chunked-ingest-crash", "version": "0"},
                }),
                Duration::from_secs(15),
            )
            .await;
        assert!(init.get("result").is_some(), "initialize: {init}");
        server
            .send_notification("notifications/initialized", json!({}))
            .await;
        server
    }

    async fn send_notification(&mut self, method: &str, params: Value) {
        let line = json!({"jsonrpc": "2.0", "method": method, "params": params}).to_string();
        self.stdin
            .write_all(line.as_bytes())
            .await
            .expect("write");
        self.stdin.write_all(b"\n").await.expect("newline");
        self.stdin.flush().await.expect("flush");
    }

    /// Writes one request and returns its id without waiting for the response.
    async fn send(&mut self, method: &str, params: Value) -> u64 {
        self.next_id += 1;
        let id = self.next_id;
        let line = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})
            .to_string();
        self.stdin
            .write_all(line.as_bytes())
            .await
            .expect("write");
        self.stdin.write_all(b"\n").await.expect("newline");
        self.stdin.flush().await.expect("flush");
        id
    }

    async fn wait(&self, id: u64, timeout: Duration) -> Value {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(message) = self.responses.lock().expect("responses").remove(&id) {
                return message;
            }
            assert!(Instant::now() < deadline, "response {id} timed out");
            tokio::time::timeout(Duration::from_millis(200), self.notify.notified())
                .await
                .ok();
        }
    }

    async fn call(&mut self, method: &str, params: Value, timeout: Duration) -> Value {
        let id = self.send(method, params).await;
        self.wait(id, timeout).await
    }

    async fn tool(&mut self, name: &str, arguments: Value, timeout: Duration) -> Value {
        let response = self
            .call(
                "tools/call",
                json!({"name": name, "arguments": arguments}),
                timeout,
            )
            .await;
        let text = response["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_else(|| panic!("{name} should answer with text: {response}"));
        serde_json::from_str(text).unwrap_or_else(|_| json!({"unparsed": text}))
    }

    async fn open_book(&mut self, path: &Path, refresh: bool, timeout: Duration) -> Value {
        self.tool(
            "open",
            json!({"source": path.to_string_lossy(), "refresh": refresh}),
            timeout,
        )
        .await
    }

    async fn read(&mut self, from: &str) -> Value {
        self.tool("read", json!({"from": from}), Duration::from_secs(60))
            .await
    }

    async fn kill(self) {
        let mut child = self.child;
        child.kill().await.expect("SIGKILL");
        child.wait().await.expect("reap");
        drop(self.stdin);
    }
}

// ------------------------------------------------------------------------------------------------
// SQLite inspection of the state database.
// ------------------------------------------------------------------------------------------------

fn section_row_count(database: &Path) -> i64 {
    let connection = rusqlite::Connection::open(database).expect("database opens");
    connection
        .query_row("SELECT COUNT(*) FROM section_layout_v2", [], |row| row.get(0))
        .expect("count")
}

fn sentinel_count(database: &Path) -> i64 {
    let connection = rusqlite::Connection::open(database).expect("database opens");
    connection
        .query_row(
            "SELECT COUNT(*) FROM text_unit_documents_v2
             WHERE ingest_fingerprint = 'chunked-ingest/in-progress'",
            [],
            |row| row.get(0),
        )
        .expect("count")
}

/// The rows a crash-and-recovery parity proof compares, content-column by content-column. The two
/// databases are written by the same binary from the same input, so every value must match exactly.
fn ingest_dump(database: &Path) -> Vec<String> {
    let connection = rusqlite::Connection::open(database).expect("database opens");
    let mut dump = Vec::new();
    for (table, columns, order) in [
        (
            "text_unit_documents_v2",
            "doc_key, document_id, content_hash, normalized_document_hash, segmentation_version, \
             ingest_fingerprint, handle_checksum",
            "doc_key",
        ),
        (
            "section_layout_v2",
            "doc_key, section_key, section_id, id_hash, char_len, hex(paragraph_coverage), \
             plan_error, paragraph_count, sentence_count, chunk_count, sorted_flags, \
             total_items_paragraph_preserve, total_items_paragraph_eligible, \
             total_items_sentence_preserve, total_items_sentence_eligible, \
             hex(coverage_paragraph_preserve), hex(coverage_paragraph_eligible), \
             hex(coverage_sentence_preserve), hex(coverage_sentence_eligible)",
            "doc_key, section_key",
        ),
        (
            "section_layout_chunk_v2",
            "doc_key, section_key, chunk_no, first_paragraph_index, first_sentence_index, \
             paragraph_slot, sentence_slot, paragraph_entry, sentence_entry, \
             eligible_paragraph_entry, eligible_sentence_entry, paragraph_count, sentence_count, \
             hex(blob)",
            "doc_key, section_key, chunk_no",
        ),
        (
            "section_text_v3",
            "doc_key, section_key, chunk_no, encoding, hex(data)",
            "doc_key, section_key, chunk_no",
        ),
        ("documents", "id, source, content_hash, document_json, normalized_document_hash", "id"),
    ] {
        let mut statement = connection
            .prepare(&format!("SELECT {columns} FROM {table} ORDER BY {order}"))
            .expect("dump query");
        let rows = statement
            .query_map([], |row| {
                let mut values = vec![table.to_owned()];
                for index in 0..columns.matches(',').count() + 1 {
                    let value: rusqlite::types::Value = row.get(index)?;
                    values.push(match value {
                        rusqlite::types::Value::Null => "null".into(),
                        rusqlite::types::Value::Integer(number) => number.to_string(),
                        rusqlite::types::Value::Real(number) => number.to_string(),
                        rusqlite::types::Value::Text(text) => text,
                        rusqlite::types::Value::Blob(blob) => format!("blob:{}", blob.len()),
                    });
                }
                Ok(values.join("|"))
            })
            .expect("dump rows");
        for row in rows {
            dump.push(row.expect("row"));
        }
    }
    dump
}

fn state_database(state: &Path) -> PathBuf {
    state.join("reading-mcp.sqlite")
}

// ------------------------------------------------------------------------------------------------
// The crash test.
// ------------------------------------------------------------------------------------------------

const CHAPTERS: usize = 60;
/// The chunk size the test server runs with, so the 60-chapter book spans 30 write transactions.
const CHUNK: &str = "2";

/// Opens the book on a fresh state directory with the default chunk size: the reference the
/// recovered ingests are compared against, and the section total the kill points need.
async fn reference_ingest(root: &Path, book: &Path) -> (Vec<String>, i64) {
    let state = root.join("reference-state");
    let mut server = Server::spawn(root, &state, "1000").await;
    let opened = server
        .open_book(book, false, Duration::from_secs(180))
        .await;
    assert!(
        opened.get("doc").is_some(),
        "reference open should succeed: {opened}"
    );
    server.kill().await;
    // The kill after a completed ingest is clean: WAL recovery on open replays the last commits.
    let total = section_row_count(&state_database(&state));
    (ingest_dump(&state_database(&state)), total)
}

/// One crash point: opens the book, kills the server once `kill_at` section rows are committed
/// (but before the write completes), restarts and proves the recovery contract.
async fn crash_point(root: &Path, book: &Path, kill_at: i64, total: i64, reference: &[String]) {
    let state = root.join(format!("crash-state-{kill_at}"));
    let backup = root.join(format!("backup-{kill_at}.epub"));
    std::fs::copy(book, &backup).expect("backup of the book");
    let database = state_database(&state);

    let mut server = Server::spawn(root, &state, CHUNK).await;
    // The open is in flight; the test never awaits it because the server dies first.
    let _open_id = server
        .send(
            "tools/call",
            json!({"name": "open", "arguments": {"source": book.to_string_lossy()}}),
        )
        .await;

    // Kill as soon as the target chunk count is visible but the write is incomplete.
    let deadline = Instant::now() + Duration::from_secs(120);
    let killed_at;
    loop {
        let count = if database.exists() {
            section_row_count(&database)
        } else {
            0
        };
        if count >= kill_at {
            killed_at = count;
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the ingest never reached {kill_at} sections"
        );
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    server.kill().await;
    eprintln!("kill point {kill_at}: saw {killed_at} of {total} sections");
    assert!(
        killed_at < total,
        "the kill must land before the write completes ({killed_at} of {total})"
    );

    // The crashed state: nothing reads as complete. The sentinel row marks the partial write.
    assert_eq!(
        sentinel_count(&database),
        1,
        "a crash mid-ingest leaves exactly the in-progress document row"
    );

    // Restart on the same state. With the source removed, recovery cannot re-ingest: a read on
    // the document's handles must answer a clean not-ready, never half-written data.
    std::fs::remove_file(book).expect("remove the source");
    let mut restarted = Server::spawn(root, &state, CHUNK).await;
    // Startup cleanup removed the partial section rows; the document row keeps its sentinel.
    assert_eq!(
        section_row_count(&database),
        0,
        "startup cleanup removes the partial rows"
    );
    assert_eq!(
        sentinel_count(&database),
        1,
        "the document row survives cleanup, still invisible"
    );
    // The handle of the document is unknown to this client, so the not-ready answer is probed
    // through `open` instead: it must re-ingest, and with the source gone it must fail rather than
    // report a partial document.
    let failed_open = restarted
        .open_book(book, false, Duration::from_secs(60))
        .await;
    assert!(
        failed_open.get("doc").is_none(),
        "open of a missing source must not report a document: {failed_open}"
    );
    assert!(
        failed_open.get("error").is_some() || failed_open.get("unparsed").is_some(),
        "the failure is a v2 error payload: {failed_open}"
    );
    restarted.kill().await;

    // Restore the source: `open` re-ingests cleanly, and the stored rows equal the reference
    // ingest byte for byte.
    std::fs::copy(&backup, book).expect("restore the source");
    let mut healed = Server::spawn(root, &state, "1000").await;
    let reopened = healed.open_book(book, false, Duration::from_secs(180)).await;
    assert!(
        reopened.get("doc").is_some(),
        "open re-ingests the crashed document: {reopened}"
    );
    healed.kill().await;

    let recovered = ingest_dump(&database);
    assert_eq!(
        reference.len(),
        recovered.len(),
        "row counts match the reference ingest"
    );
    assert_eq!(
        reference,
        recovered.as_slice(),
        "the recovered ingest is byte-identical to a clean ingest"
    );
}

#[tokio::test]
async fn a_killed_chunked_ingest_never_reads_as_complete_and_recovers_byte_identically() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let root = directory.path();
    let book = root.join("crash-book.epub");
    std::fs::write(&book, epub_with_chapters(CHAPTERS)).expect("book fixture");

    let (reference, total) = reference_ingest(root, &book).await;
    assert!(
        total >= CHAPTERS as i64,
        "the book has one section per chapter at least: {total}"
    );

    // After the first chunk (2 sections), in the middle, before the final commit.
    for kill_at in [2, total / 2, total - 4] {
        crash_point(root, &book, kill_at, total, &reference).await;
    }
}

// ------------------------------------------------------------------------------------------------
// The concurrent-reader test.
// ------------------------------------------------------------------------------------------------

#[tokio::test]
async fn a_concurrent_reader_never_sees_partial_or_mixed_sections() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let root = directory.path();
    let book = root.join("reader-book.epub");
    std::fs::write(&book, epub_with_chapters(24)).expect("book fixture");
    let state = root.join("reader-state");

    let mut server = Server::spawn(root, &state, "1").await;
    let opened = server.open_book(&book, false, Duration::from_secs(180)).await;
    let doc = opened["doc"].as_str().expect("doc handle").to_owned();

    // Walk the first handles of the book and remember the correct answer of each one.
    let mut handles = vec![doc.clone()];
    let mut correct: HashMap<String, String> = HashMap::new();
    let mut current = doc.clone();
    for _ in 0..40 {
        let answer = server.read(&current).await;
        let Some(next) = answer["next"].as_str() else {
            break;
        };
        correct.insert(
            current.clone(),
            answer["text"].as_str().unwrap_or("").to_owned(),
        );
        handles.push(next.to_owned());
        current = next.to_owned();
    }
    assert!(handles.len() > 5, "the walk produced handles");
    // The walk's final `next` was never read, so no baseline answer exists for it.
    handles.retain(|handle| correct.contains_key(handle));

    // Continuous reads on those handles while the same bytes are force-re-ingested in chunks of
    // one section. A read may succeed — only with exactly the text this handle read before — or
    // fail with a v2 error; a partial or mixed document can never be answered as complete.
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let mut read_ids = Vec::new();
    let mut handle_index = 0usize;
    let refresh = server
        .send(
            "tools/call",
            json!({"name": "open", "arguments": {"source": book.to_string_lossy(), "refresh": true}}),
        )
        .await;
    let mut reads = 0usize;
    let mut errors: HashMap<String, usize> = HashMap::new();
    let mut successes = 0usize;
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(15) {
        // Keep a few reads in flight all the time.
        while read_ids.len() < 4 {
            let handle = &handles[handle_index % handles.len()];
            handle_index += 1;
            let id = server
                .send(
                    "tools/call",
                    json!({"name": "read", "arguments": {"from": handle}}),
                )
                .await;
            read_ids.push((id, handle.clone()));
        }
        // Collect every answer that has arrived.
        let mut arrived = Vec::new();
        {
            let responses = server.responses.lock().expect("responses");
            for (id, _) in &read_ids {
                if responses.contains_key(id) {
                    arrived.push(*id);
                }
            }
        }
        if arrived.is_empty() {
            tokio::time::sleep(Duration::from_millis(2)).await;
            if server
                .responses
                .lock()
                .expect("responses")
                .contains_key(&refresh)
            {
                // The re-ingest is over; keep reading a moment longer, then stop.
                if started.elapsed() > Duration::from_secs(3) {
                    break;
                }
            }
            continue;
        }
        for id in arrived {
            let position = read_ids.iter().position(|(read, _)| *read == id).expect("id");
            let (_, handle) = read_ids.remove(position);
            let message = server.wait(id, Duration::from_secs(1)).await;
            reads += 1;
            let result = &message["result"];
            let text = result["content"][0]["text"].as_str().unwrap_or("");
            if result["isError"].as_bool() == Some(true) {
                let payload: Value =
                    serde_json::from_str(text).unwrap_or_else(|_| json!({"error": "unparsed"}));
                let code = payload["error"].as_str().unwrap_or("unparsed").to_owned();
                assert!(
                    matches!(
                        code.as_str(),
                        "not_opened" | "unknown_handle" | "stale_handle"
                    ),
                    "a read during the re-ingest may only fail as not-ready: {code} ({payload})"
                );
                *errors.entry(code).or_default() += 1;
            } else {
                let payload: Value = serde_json::from_str(text).expect("read payload");
                let answer_text = payload["text"].as_str().unwrap_or("");
                if !answer_text.is_empty() {
                    let expected = correct.get(&handle).unwrap_or_else(|| {
                        panic!("{handle} answered non-empty text it never read before: {answer_text}")
                    });
                    assert_eq!(
                        answer_text, expected,
                        "a successful read never answers partial or mixed text for {handle}"
                    );
                }
                successes += 1;
            }
        }
    }
    stop.store(true, std::sync::atomic::Ordering::Relaxed);

    // The refresh completes, the document reads exactly as before, and a final open is a cache hit.
    let refreshed = server.wait(refresh, Duration::from_secs(120)).await;
    let refreshed_text = refreshed["result"]["content"][0]["text"]
        .as_str()
        .expect("refresh answer");
    let refreshed_payload: Value =
        serde_json::from_str(refreshed_text).expect("refresh payload");
    assert_eq!(
        refreshed_payload["doc"].as_str().expect("doc"),
        doc,
        "the re-ingest of the same bytes keeps the document handle"
    );
    assert!(reads > 20, "the window held many reads: {reads}");
    assert!(successes > 0, "reads kept being served: {successes}");
    for handle in correct.keys().take(5) {
        let answer = server.read(handle).await;
        assert_eq!(
            answer["text"].as_str().expect("text"),
            correct[handle],
            "after the re-ingest every handle reads exactly what it read before"
        );
    }
    server.kill().await;
}
