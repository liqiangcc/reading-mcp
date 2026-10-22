//! The warm of the scan text cache on a document over the cap: nothing is cached, the store ends up
//! in the state its first scan would leave, and the search still answers what the plain scan answers.
//!
//! The cap is read from `READING_MCP_SEARCH_CACHE_MIB` on every scan and every warm, and this is the
//! only test of that variable in this binary, so setting it here cannot race another test.

use reading_mcp::application::ports::{DocumentRepository, Parser, RetrievedResource, SearchIndex};
use reading_mcp::domain::{Document, DocumentSource, MediaType};
use reading_mcp::infrastructure::{ScanOptions, SqliteDocumentRepository, SqliteSearchIndex};
use reading_mcp::parsing::{MarkdownParser, TextParser};

const QUERIES: [&str; 2] = ["皇帝", "不存在的詞組甲乙"];

#[tokio::test]
async fn a_document_over_the_cap_is_never_cached() {
    unsafe {
        std::env::set_var("READING_MCP_SEARCH_CACHE_MIB", "1");
    }

    let directory = tempfile::tempdir().expect("temp directory");
    let database = directory.path().join("prewarm-cap.sqlite");
    let repository = SqliteDocumentRepository::open(&database).expect("repository");
    let index = SqliteSearchIndex::open(&database).expect("index");

    // Over one MiB of stored text in many small sections, so no single section is large enough to be
    // over the cap by itself: the warm measures the whole text, sees that it is over the cap, and
    // marks the store over the cap without reading any text into it.
    let mut text = String::new();
    for section in 0..3_000 {
        text.push_str(&format!("# 第{section}節\n\n"));
        text.push_str("皇帝在朝堂上討論虛擬內存機制與頁表映射，節度使入朝議事，群臣列坐。\n\n");
    }
    assert!(text.len() > 2 * 1024 * 1024, "the document is over the cap");
    let document = markdown_document("memory:prewarm-cap.md", &text).await;
    repository.save(document.clone()).await.expect("save");
    index.index(&document).await.expect("index");

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
        assert_scan_parity(&index, &document, query).await;
        assert_eq!(
            index.scan_text_cache_state(),
            Some((0, true)),
            "a search of an over-cap document leaves it uncached"
        );
    }

    // A second document, on its own store: one section of over a quarter of the cap, so measuring it
    // would read it for nothing.
    let directory = tempfile::tempdir().expect("temp directory");
    let database = directory.path().join("prewarm-cap-one-section.sqlite");
    let repository = SqliteDocumentRepository::open(&database).expect("repository");
    let index = SqliteSearchIndex::open(&database).expect("index");

    // The warm leaves it alone: the store is not even installed, so it holds nothing and is not
    // marked over the cap.
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
    let document = text_document("memory:prewarm-cap-one-section.txt", &text).await;
    repository.save(document.clone()).await.expect("save");
    index.index(&document).await.expect("index");

    assert_eq!(
        index
            .prewarm_blocking(
                &document.id,
                &document.normalized_document_hash(),
                usize::MAX
            )
            .expect("blocking warm"),
        0,
        "the warm leaves a document with a section over a quarter of the cap alone"
    );
    assert_eq!(index.scan_text_cache_state(), None);

    for query in QUERIES {
        assert_scan_parity(&index, &document, query).await;
    }

    unsafe {
        std::env::remove_var("READING_MCP_SEARCH_CACHE_MIB");
    }
}

async fn assert_scan_parity(index: &SqliteSearchIndex, document: &Document, query: &str) {
    let plain = index
        .search_lexical_with_options(&document.id, query, 10, ScanOptions::plain())
        .await
        .unwrap_or_else(|error| panic!("plain search {query:?}: {error}"));
    let fast = index
        .search_lexical_with_options(&document.id, query, 10, ScanOptions::production())
        .await
        .unwrap_or_else(|error| panic!("fast search {query:?}: {error}"));
    assert_eq!(plain, fast, "the fast scan differs for query {query:?}");
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

async fn text_document(source: &str, text: &str) -> Document {
    TextParser
        .parse(RetrievedResource {
            source: DocumentSource(source.into()),
            final_source: DocumentSource(source.into()),
            media_type: MediaType("text/plain".into()),
            bytes: text.as_bytes().to_vec(),
            etag: None,
            last_modified: None,
            metadata: Default::default(),
        })
        .await
        .expect("parse text")
}
