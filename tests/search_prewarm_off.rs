//! `READING_MCP_SEARCH_PREWARM_BUDGET_MS=0` turns the warm off, and it must restore exactly the
//! behaviour of a server without a warm: an open starts no background read at all.
//!
//! This is the only test of that variable in this binary, so setting it here cannot race another
//! test.

use std::time::Duration;

use reading_mcp::application::ports::{DocumentRepository, Parser, RetrievedResource, SearchIndex};
use reading_mcp::domain::{Document, DocumentSource, MediaType};
use reading_mcp::infrastructure::{SqliteDocumentRepository, SqliteSearchIndex};
use reading_mcp::parsing::MarkdownParser;

#[tokio::test]
async fn a_zero_budget_starts_no_warm_at_all() {
    unsafe {
        std::env::set_var("READING_MCP_SEARCH_PREWARM_BUDGET_MS", "0");
    }

    let directory = tempfile::tempdir().expect("temp directory");
    let database = directory.path().join("prewarm-off.sqlite");
    let repository = SqliteDocumentRepository::open(&database).expect("repository");
    let index = SqliteSearchIndex::open(&database).expect("index");
    let document = markdown_document(
        "memory:prewarm-off.md",
        "# 第一章\n\n皇帝在朝堂上議事。\n\n# 第二章\n\n節度使入朝議事。\n",
    )
    .await;
    repository.save(document.clone()).await.expect("save");
    index.index(&document).await.expect("index");

    index.prewarm(&document.id, &document.normalized_document_hash());
    // Longer than the default 250 ms budget and than this document's whole warm: with the warm on,
    // the store would hold both sections long before this.
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(
        index.scan_text_cache_state(),
        None,
        "a zero budget must not start the warm"
    );

    unsafe {
        std::env::remove_var("READING_MCP_SEARCH_PREWARM_BUDGET_MS");
    }
}

async fn markdown_document(source: &str, text: &str) -> Document {
    MarkdownParser
        .parse(RetrievedResource {
            source: DocumentSource(source.into()),
            final_source: DocumentSource(source.into()),
            media_type: MediaType("text/markdown".into()),
            bytes: text.as_bytes().to_vec(),
            etag: None,
            last_modified: None,
            metadata: Default::default(),
        })
        .await
        .expect("parse markdown")
}
