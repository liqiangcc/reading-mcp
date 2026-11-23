//! V2-4b PR-1, §2 row 1: re-covers `tests/acceptance_matrix.rs` — the stdio smoke over
//! txt/md/html/pdf/epub, `open` → structure → read → search — with the seven v2 tools:
//! `open` → `outline` → `read` → `search`.
//!
//! The v1 test pinned the search hit's `section://` id; the v2 equivalent is the hit's sentence
//! handle, whose section is named in `search.sections` and which `read` opens at the match.

#[path = "support/v2_mcp.rs"]
mod v2_mcp;

use std::path::Path;

use v2_mcp::{
    build_pdf, call, epub_book, flat_text, payload, section_of, start_client, v2_open, walk,
};

#[tokio::test]
async fn v2_acceptance_matrix_covers_txt_md_html_pdf_and_epub() {
    let directory = tempfile::tempdir().expect("acceptance directory should be created");
    tokio::fs::write(
        directory.path().join("notes.txt"),
        "Orbital memory appears in plain text.\n",
    )
    .await
    .expect("text fixture should be written");
    tokio::fs::write(
        directory.path().join("guide.md"),
        "# Markdown Guide\n\n## Memory\n\nOrbital memory appears in Markdown.\n",
    )
    .await
    .expect("Markdown fixture should be written");
    tokio::fs::write(
        directory.path().join("guide.html"),
        "<!doctype html><html><head><title>HTML Guide</title></head><body><main><h1 id=\"memory\">Memory</h1><p>Orbital memory appears in HTML.</p></main></body></html>",
    )
    .await
    .expect("HTML fixture should be written");
    tokio::fs::write(
        directory.path().join("guide.pdf"),
        build_pdf("Orbital memory appears in PDF."),
    )
    .await
    .expect("PDF fixture should be written");
    tokio::fs::write(directory.path().join("book.epub"), epub_book())
        .await
        .expect("EPUB fixture should be written");

    let client = start_client(directory.path(), &directory.path().join("state")).await;

    for (name, format) in [
        ("notes.txt", "text"),
        ("guide.md", "markdown"),
        ("guide.html", "html"),
        ("guide.pdf", "pdf"),
        ("book.epub", "epub"),
    ] {
        let path = directory.path().join(name);
        assert_flow(&client, &path, format, "Orbital memory appears in").await;
    }

    client.cancel().await.expect("client should close cleanly");
}

/// open → outline → read → search, exactly the v1 acceptance flow per format.
async fn assert_flow(client: &v2_mcp::Client, path: &Path, format: &str, expected: &str) {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .expect("a fixture file name");

    // open: the format and a document handle.
    let opened = v2_open(client, path).await;
    assert_eq!(opened["format"], format, "{name}: open format");
    let doc = opened["doc"]
        .as_str()
        .expect("open returns a document handle")
        .to_owned();
    assert!(doc.starts_with('d'), "{name}: doc handle is short: {doc}");
    assert!(
        opened["sections"].as_u64().expect("sections") >= 1,
        "{name}: open reports at least one section: {opened}"
    );

    // outline: the section tree, in preorder, from the document handle.
    let outline = payload(
        &call(
            client,
            "outline",
            serde_json::json!({ "at": doc, "depth": 8, "limit": 100 }),
        )
        .await,
    );
    let sections = outline["items"]
        .as_array()
        .unwrap_or_else(|| panic!("{name}: outline items: {outline}"));
    assert!(!sections.is_empty(), "{name}: outline lists a section");
    for section in sections {
        let parts = section.as_array().expect("an outline row is an array");
        assert!(
            parts[0].as_str().expect("handle").starts_with(&doc),
            "{name}: outline handles belong to the document: {section}"
        );
        assert!(
            !parts[1].as_str().expect("title").is_empty(),
            "{name}: a listed section has a title: {section}"
        );
    }

    // read: the whole book in reading order carries the format's text. The EPUB spine order is
    // deliberately not the navigation order, so the walk (not only the first section) is the
    // assertion that every format reads.
    let walked = walk(client, &doc, serde_json::json!({})).await;
    assert!(
        walked.iter().any(|(_, text)| text.contains(expected)),
        "{name}: no read item carries {expected:?}: {walked:?}"
    );

    // search: the same query as the v1 matrix, answered with readable sentence handles.
    let found = payload(
        &call(
            client,
            "search",
            serde_json::json!({ "doc": doc, "query": "orbital memory", "limit": 5 }),
        )
        .await,
    );
    let hits = found["hits"]
        .as_array()
        .unwrap_or_else(|| panic!("{name}: search hits: {found}"));
    assert!(
        !hits.is_empty(),
        "{name}: search finds the sentence: {found}"
    );
    assert_eq!(
        found["total"].as_u64(),
        Some(hits.len() as u64),
        "{name}: total counts the hits"
    );
    let named = found["sections"]
        .as_object()
        .unwrap_or_else(|| panic!("{name}: search names its sections: {found}"));
    assert!(!named.is_empty(), "{name}: search names a hit section");
    for hit in hits {
        let handle = hit[0].as_str().expect("a sentence handle");
        assert!(
            named.contains_key(&section_of(handle)),
            "{name}: the hit section is named in sections: {found}"
        );
        let at = payload(&call(client, "read", serde_json::json!({ "from": handle })).await);
        let text = flat_text(&at);
        assert!(
            text.to_ascii_lowercase().contains("orbital memory"),
            "{name}: the hit handle reads at the match, got {text:?}"
        );
    }
}
