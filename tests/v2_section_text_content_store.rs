//! V2-4b PR-2, §5: ports the one v1-driven test of `tests/section_text_content_store.rs` —
//! `stdio_responses_are_identical_after_a_restart` — to the v2 tools.
//!
//! The v1 session called `open_document`, `get_document_structure`, `get_text_units`,
//! `read_document`, `search_document` and `get_context` and compared the six raw tool results
//! byte for byte across two fresh processes. The v2 session calls `open`, `outline`, `read` (the
//! text-unit stream), `read` (the single-item read), `search` and `cite` (the section locator,
//! which reads the same stored row `get_context` did) and compares the same six results the same
//! way. The six direct-store tests of the v1 file never touched a tool and stay there unchanged.

#[path = "support/v2_mcp.rs"]
mod v2_mcp;

use std::io::{Cursor, Write};
use std::path::Path;

use serde_json::json;
use zip::write::SimpleFileOptions;

use v2_mcp::{call, payload, start_client};

#[tokio::test]
async fn v2_stdio_responses_are_identical_after_a_restart() {
    let directory = tempfile::tempdir().expect("temporary directory should be created");
    let epub = directory.path().join("book.epub");
    std::fs::write(&epub, build_epub()).expect("EPUB fixture should be written");
    let state = directory.path().join("state");

    let first = run_stdio_session(directory.path(), &state, &epub).await;
    let second = run_stdio_session(directory.path(), &state, &epub).await;
    assert_eq!(
        first, second,
        "the same calls must answer identically from a fresh process"
    );
    assert_eq!(first.len(), 6, "every call must be compared");
}

async fn run_stdio_session(root: &Path, state: &Path, epub: &Path) -> Vec<(String, Vec<u8>)> {
    let client = start_client(root, state).await;

    let mut responses = Vec::new();
    let opened = call(&client, "open", json!({ "source": epub.to_string_lossy() })).await;
    let doc = payload(&opened)["doc"]
        .as_str()
        .expect("doc handle")
        .to_owned();
    responses.push((String::from("open"), json_bytes(&opened)));

    let structure = call(
        &client,
        "outline",
        json!({ "at": doc, "depth": 8, "limit": 100 }),
    )
    .await;
    let outline = payload(&structure);
    let rows = outline["items"].as_array().expect("outline items");
    // The v1 root's first child, else the root: the section the text stream reads.
    let section = rows.get(1).unwrap_or(&rows[0])[0]
        .as_str()
        .expect("section handle")
        .to_owned();
    responses.push((String::from("outline"), json_bytes(&structure)));

    let units = call(
        &client,
        "read",
        json!({ "from": section, "max_chars": 4000 }),
    )
    .await;
    responses.push((String::from("read"), json_bytes(&units)));

    let read = call(&client, "read", json!({ "from": section })).await;
    responses.push((String::from("read_section"), json_bytes(&read)));

    let search = call(
        &client,
        "search",
        json!({ "doc": doc, "query": "memory", "limit": 5 }),
    )
    .await;
    responses.push((String::from("search"), json_bytes(&search)));

    let cite = call(&client, "cite", json!({ "at": [section] })).await;
    responses.push((String::from("cite"), json_bytes(&cite)));

    client.cancel().await.expect("server should stop");
    responses
}

fn json_bytes<T: serde::Serialize>(value: &T) -> Vec<u8> {
    serde_json::to_vec(value).expect("response should serialize")
}

fn build_epub() -> Vec<u8> {
    build_zip(&[
        ("mimetype", "application/epub+zip"),
        (
            "META-INF/container.xml",
            r#"<?xml version="1.0"?>
<container xmlns="urn:oasis:names:tc:opendocument:xmlns:container">
  <rootfiles><rootfile full-path="OPS/package.opf" media-type="application/oebps-package+xml"/></rootfiles>
</container>"#,
        ),
        (
            "OPS/package.opf",
            r#"<?xml version="1.0"?>
<package xmlns="http://www.idpf.org/2007/opf" version="3.0">
  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:title>Operating Systems EPUB</dc:title></metadata>
  <manifest>
    <item id="one" href="chapter-one.xhtml" media-type="application/xhtml+xml"/>
    <item id="two" href="chapter-two.xhtml" media-type="application/xhtml+xml"/>
  </manifest>
  <spine><itemref idref="one"/><itemref idref="two"/></spine>
</package>"#,
        ),
        (
            "OPS/chapter-one.xhtml",
            r#"<!doctype html><html><head><title>Virtual Memory</title></head><body><main>
<h1 id="vm">Virtual Memory</h1><p>Orbital memory in EPUB. Page tables map addresses.</p>
<h2 id="tables">Page Tables</h2><p>Page tables hold one entry per page.</p>
</main></body></html>"#,
        ),
        (
            "OPS/chapter-two.xhtml",
            r#"<!doctype html><html><head><title>Processes</title></head><body><main>
<h1 id="processes">Processes</h1><p>A process owns an address space and its memory.</p>
</main></body></html>"#,
        ),
    ])
}

fn build_zip(entries: &[(&str, &str)]) -> Vec<u8> {
    let cursor = Cursor::new(Vec::new());
    let mut writer = zip::ZipWriter::new(cursor);
    let options = SimpleFileOptions::default();
    for (name, content) in entries {
        writer
            .start_file(*name, options)
            .expect("ZIP fixture entry should start");
        writer
            .write_all(content.as_bytes())
            .expect("ZIP fixture entry should write");
    }
    writer
        .finish()
        .expect("ZIP fixture should finish")
        .into_inner()
}
