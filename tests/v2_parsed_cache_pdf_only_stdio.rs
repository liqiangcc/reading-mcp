//! V2-4b PR-2, §5: ports `tests/parsed_cache_pdf_only_stdio.rs` — opening Markdown writes no
//! parsed-cache entry — to the v2 `open` tool.
//!
//! Only the call changes: `open_document{force_refresh:false}` is `open{refresh:false}`, and the
//! `media_type` field is the v2 `format`. The cache directories and the reopen equality are the
//! same storage assertions as v1.

#[path = "support/v2_mcp.rs"]
mod v2_mcp;

use serde_json::json;

use v2_mcp::{call, payload, start_client};

#[tokio::test]
async fn v2_opening_markdown_through_stdio_writes_no_parsed_cache_entry() {
    let directory = tempfile::tempdir().expect("temporary directory should be created");
    let document_path = directory.path().join("operating-systems.md");
    tokio::fs::write(
        &document_path,
        "# Operating Systems\n\n## Virtual Memory\n\nPage tables map memory.\n",
    )
    .await
    .expect("Markdown fixture should be written");
    let state = directory.path().join("state");

    let client = start_client(directory.path(), &state).await;

    let opened = payload(
        &call(
            &client,
            "open",
            json!({
                "source": document_path.to_string_lossy(),
                "refresh": false
            }),
        )
        .await,
    );
    assert_eq!(opened["format"], "markdown", "{opened}");

    for directory_name in ["parsed", "parsed-v2"] {
        let entries = std::fs::read_dir(state.join("cache").join(directory_name))
            .map(|entries| {
                entries
                    .flatten()
                    .map(|entry| entry.path())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        assert!(
            entries.is_empty(),
            "opening Markdown must not write {directory_name} entries: {entries:?}"
        );
    }

    let reopened = payload(
        &call(
            &client,
            "open",
            json!({
                "source": document_path.to_string_lossy(),
                "refresh": false
            }),
        )
        .await,
    );
    assert_eq!(reopened, opened, "reopening answers the same document");

    client.cancel().await.unwrap();
}
