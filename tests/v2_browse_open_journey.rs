//! V2-4b PR-1, §2 row 3: re-covers `tests/directory_navigation_stdio.rs` — browse a nested source
//! workspace, then open a document found by browsing — as the `browse` → `open` stdio journey.
//!
//! The v1 test listed roots with `list_directory` and documents with `list_documents`; v2 `browse`
//! is those two use cases behind one tool: no `path` lists the documents under the roots, a `path`
//! lists that directory's `[name, kind, size]` entries.

#[path = "support/v2_mcp.rs"]
mod v2_mcp;

use std::path::Path;

use serde_json::{Value, json};

use v2_mcp::{call, flat_text, payload, start_client, v2_open};

#[tokio::test]
async fn browse_a_nested_workspace_then_open_the_discovered_document() {
    let root = tempfile::tempdir().expect("source workspace should be created");
    let papers = root.path().join("papers");
    let paper = papers.join("kafka-2011-distributed-messaging");
    let revision = paper.join("kafka-2011-netdb11");
    tokio::fs::create_dir_all(&revision)
        .await
        .expect("nested source workspace should be created");
    tokio::fs::write(revision.join("paper.md"), "# Kafka\n\nA source document.\n")
        .await
        .expect("document fixture should be written");
    tokio::fs::write(revision.join("source.json"), b"{}")
        .await
        .expect("metadata fixture should be written");

    let client = start_client(root.path(), &root.path().join("state")).await;

    // No path: the documents under the roots, not the root directory's children. The workspace
    // has none at its top level, so the listing is empty.
    let roots = payload(&call(&client, "browse", json!({})).await);
    assert_eq!(
        roots
            .as_array()
            .expect("browse answers with an array")
            .len(),
        0,
        "the roots hold no top-level document: {roots}"
    );

    // The journey walks down by browsing each directory it discovers.
    let entries = browse_directory(&client, root.path()).await;
    assert!(
        entries.contains(&("papers".to_owned(), "dir".to_owned())),
        "the root lists the papers directory: {entries:?}"
    );

    let entries = browse_directory(&client, &papers).await;
    assert!(
        entries.contains(&(
            "kafka-2011-distributed-messaging".to_owned(),
            "dir".to_owned()
        )),
        "papers lists the paper directory: {entries:?}"
    );

    let entries = browse_directory(&client, &paper).await;
    assert!(
        entries.contains(&("kafka-2011-netdb11".to_owned(), "dir".to_owned())),
        "the paper lists its revision: {entries:?}"
    );

    let listing = payload(
        &call(
            &client,
            "browse",
            json!({ "path": revision.to_string_lossy() }),
        )
        .await,
    );
    let entries = listing.as_array().expect("browse answers with an array");
    assert!(
        entries.iter().all(|entry| entry[1] == "doc"),
        "a leaf directory lists documents only: {listing}"
    );
    let paper_entry = entries
        .iter()
        .find(|entry| entry[0] == "paper.md")
        .unwrap_or_else(|| panic!("browse lists paper.md: {listing}"));
    assert_eq!(
        paper_entry[2].as_u64(),
        Some(std::fs::metadata(revision.join("paper.md")).unwrap().len()),
        "a document entry reports its size"
    );

    // The browsed name resolves against the browsed directory, and `open` reads it.
    let discovered: &Path = &revision.join("paper.md");
    let opened = v2_open(&client, discovered).await;
    assert_eq!(opened["format"], "markdown", "open reports the format");
    assert_eq!(opened["title"], "Kafka", "open reports the heading title");
    assert_eq!(
        opened["sections"].as_u64(),
        Some(1),
        "the fixture has one section: {opened}"
    );
    let doc = opened["doc"].as_str().expect("doc handle");
    let read = payload(&call(&client, "read", json!({ "from": doc })).await);
    assert_eq!(
        flat_text(&read),
        "A source document.",
        "the browsed document reads"
    );

    client.cancel().await.expect("client should close cleanly");
}

/// The `[name, kind]` pairs of one directory listing.
async fn browse_directory(client: &v2_mcp::Client, path: &Path) -> Vec<(String, String)> {
    let listing: Value =
        payload(&call(client, "browse", json!({ "path": path.to_string_lossy() })).await);
    listing
        .as_array()
        .expect("browse answers with an array")
        .iter()
        .map(|entry| {
            (
                entry[0].as_str().expect("name").to_owned(),
                entry[1].as_str().expect("kind").to_owned(),
            )
        })
        .collect()
}
