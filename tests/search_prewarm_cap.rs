//! The warm of the scan text cache on a document over the cap: nothing is cached and it stops at
//! once, and the search still answers exactly what the plain scan answers.
//!
//! The cap is read from `READING_MCP_SEARCH_CACHE_MIB` on every scan and every warm, and this is the
//! only test of that variable in this binary, so setting it here cannot race another test.

use reading_mcp::application::ports::{DocumentRepository, Parser, RetrievedResource, SearchIndex};
use reading_mcp::domain::{Document, DocumentSource, MediaType};
use reading_mcp::infrastructure::{ScanOptions, SqliteDocumentRepository, SqliteSearchIndex};
use reading_mcp::parsing::TextParser;

const QUERIES: [&str; 2] = ["皇帝", "不存在的詞組甲乙"];

#[tokio::test]
async fn a_document_over_the_cap_is_not_warmed_and_still_answers_the_plain_scan() {
    // The cap is in MiB, so one MiB is the smallest cap a document can overflow.
    unsafe {
        std::env::set_var("READING_MCP_SEARCH_CACHE_MIB", "1");
    }

    let directory = tempfile::tempdir().expect("temp directory");
    let database = directory.path().join("prewarm-cap.sqlite");
    let repository = SqliteDocumentRepository::open(&database).expect("repository");
    let index = SqliteSearchIndex::open(&database).expect("index");

    // Well over one MiB of stored text, in few enough sentences to stay quick to segment.
    let mut text = String::new();
    for paragraph in 0..4_000 {
        for _ in 0..6 {
            text.push_str(&format!(
                "第{paragraph}段：皇帝在朝堂上討論虛擬內存機制與頁表映射，節度使入朝議事，"
            ));
        }
        text.push_str("。\n\n");
    }
    assert!(text.len() > 2 * 1024 * 1024, "the document is over the cap");
    let document = document(&text).await;
    repository.save(document.clone()).await.expect("save");
    index.index(&document).await.expect("index");

    // The warm stops as soon as the document outgrows the cap, and holds nothing.
    index
        .prewarm_blocking(
            &document.id,
            &document.normalized_document_hash(),
            usize::MAX,
        )
        .expect("blocking warm");
    assert_eq!(
        index.scan_text_cache_state(),
        Some((0, true)),
        "a document over the cap is not cached at all"
    );

    for query in QUERIES {
        let plain = index
            .search_lexical_with_options(&document.id, query, 10, ScanOptions::plain())
            .await
            .unwrap_or_else(|error| panic!("plain search {query:?}: {error}"));
        let fast = index
            .search_lexical_with_options(&document.id, query, 10, ScanOptions::production())
            .await
            .unwrap_or_else(|error| panic!("fast search {query:?}: {error}"));
        assert_eq!(plain, fast, "the fast scan differs for query {query:?}");
        assert_eq!(
            index.scan_text_cache_state(),
            Some((0, true)),
            "a search of an over-cap document leaves it uncached"
        );
    }

    unsafe {
        std::env::remove_var("READING_MCP_SEARCH_CACHE_MIB");
    }
}

async fn document(text: &str) -> Document {
    TextParser
        .parse(RetrievedResource {
            source: DocumentSource("memory:prewarm-cap.txt".into()),
            final_source: DocumentSource("memory:prewarm-cap.txt".into()),
            media_type: MediaType("text/plain".into()),
            bytes: text.as_bytes().to_vec(),
            etag: None,
            last_modified: None,
            metadata: Default::default(),
        })
        .await
        .expect("parse text")
}
