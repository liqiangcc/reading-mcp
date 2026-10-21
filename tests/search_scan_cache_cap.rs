//! The scan text cache cap: a document over it is not cached and still returns the same hits.
//!
//! The cap is read from `READING_MCP_SEARCH_CACHE_MIB` on every scan, and this is the only test of
//! its binary, so setting the variable here cannot race another test.

use reading_mcp::application::ports::{DocumentRepository, Parser, RetrievedResource, SearchIndex};
use reading_mcp::domain::{Document, DocumentSource, MediaType};
use reading_mcp::infrastructure::{ScanOptions, SqliteDocumentRepository, SqliteSearchIndex};
use reading_mcp::parsing::TextParser;

const QUERIES: [&str; 3] = ["皇帝", "節度使", "不存在的詞組甲乙"];

#[tokio::test]
async fn a_document_over_the_cap_is_not_cached_and_still_returns_the_same_hits() {
    // The cap is in MiB, so one MiB is the smallest cap a document can overflow.
    unsafe {
        std::env::set_var("READING_MCP_SEARCH_CACHE_MIB", "1");
    }

    let directory = tempfile::tempdir().expect("temp directory");
    let database = directory.path().join("scan-cache-cap.sqlite");
    let repository = SqliteDocumentRepository::open(&database).expect("repository");
    let index = SqliteSearchIndex::open(&database).expect("index");

    // Well over one MiB of stored text.
    let mut text = String::new();
    for paragraph in 0..24_000 {
        text.push_str(&format!(
            "第{paragraph}段：皇帝在朝堂上討論虛擬內存機制與頁表映射，節度使入朝議事。\n\n"
        ));
    }
    assert!(text.len() > 2 * 1024 * 1024, "the document is over the cap");
    let document = document(&text).await;
    repository.save(document.clone()).await.expect("save");
    index.index(&document).await.expect("index");

    for query in QUERIES {
        let plain = index
            .search_lexical_with_options(&document.id, query, 10, ScanOptions::plain())
            .await
            .unwrap_or_else(|error| panic!("plain search {query:?}: {error}"));
        // Twice: an uncached document must not answer the second search differently from the first.
        for run in 1..=2 {
            let fast = index
                .search_lexical_with_options(&document.id, query, 10, ScanOptions::production())
                .await
                .unwrap_or_else(|error| panic!("fast search {query:?}: {error}"));
            assert_eq!(
                plain, fast,
                "the fast scan differs for query {query:?} on run {run}"
            );
        }
    }

    unsafe {
        std::env::remove_var("READING_MCP_SEARCH_CACHE_MIB");
    }
}

async fn document(text: &str) -> Document {
    TextParser
        .parse(RetrievedResource {
            source: DocumentSource("memory:cap.txt".into()),
            final_source: DocumentSource("memory:cap.txt".into()),
            media_type: MediaType("text/plain".into()),
            bytes: text.as_bytes().to_vec(),
            etag: None,
            last_modified: None,
            metadata: Default::default(),
        })
        .await
        .expect("parse text")
}
