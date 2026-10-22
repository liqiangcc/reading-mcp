//! `READING_MCP_SCAN_PREWARM` turns the warm off: nothing is cached, and the search answers exactly
//! what it answers with the warm on and what the plain scan answers.
//!
//! The knob is read from the environment on every warm, and this is the only test of that variable in
//! this binary, so setting it here cannot race another test.

use reading_mcp::application::ports::{
    DocumentRepository, LexicalSearchHit, Parser, RetrievedResource, SearchIndex,
};
use reading_mcp::domain::{Document, DocumentSource, MediaType, Section};
use reading_mcp::infrastructure::{ScanOptions, SqliteDocumentRepository, SqliteSearchIndex};
use reading_mcp::parsing::MarkdownParser;

const QUERIES: [&str; 4] = ["皇帝", "寶玉", "節度使", "不存在的詞組甲乙"];

#[tokio::test]
async fn the_knob_off_disables_the_warm_and_changes_no_hit() {
    let directory = tempfile::tempdir().expect("temp directory");
    let database = directory.path().join("prewarm-knob.sqlite");
    let repository = SqliteDocumentRepository::open(&database).expect("repository");
    let index = SqliteSearchIndex::open(&database).expect("index");
    let document = multi_section_document().await;
    repository.save(document.clone()).await.expect("save");
    index.index(&document).await.expect("index");
    let hash = document.normalized_document_hash();
    let sections = sections_with_text(&document);
    assert!(
        sections > 1,
        "the fixture holds {sections} sections with text"
    );

    // Off: neither the background warm nor the blocking one touches the store.
    unsafe {
        std::env::set_var("READING_MCP_SCAN_PREWARM", "0");
    }
    index.prewarm(&document.id, &hash);
    assert_eq!(
        index
            .prewarm_blocking(&document.id, &hash, usize::MAX)
            .expect("blocking warm"),
        0,
        "the knob off must warm nothing"
    );
    assert_eq!(
        index.scan_text_cache_state(),
        None,
        "the knob off must not even install a store"
    );
    let mut off_hits: Vec<Vec<LexicalSearchHit>> = Vec::new();
    for query in QUERIES {
        off_hits.push(production_hits(&index, &document, query).await);
    }
    // The first search built the store on its own, as it does without any warm.
    assert!(
        index.scan_text_cache_state().is_some(),
        "a search still fills the store itself"
    );

    // On: the same document in a fresh store, warmed before the search.
    let directory = tempfile::tempdir().expect("temp directory");
    let database = directory.path().join("prewarm-knob-on.sqlite");
    let repository = SqliteDocumentRepository::open(&database).expect("repository");
    let index = SqliteSearchIndex::open(&database).expect("index");
    repository.save(document.clone()).await.expect("save");
    index.index(&document).await.expect("index");

    unsafe {
        std::env::set_var("READING_MCP_SCAN_PREWARM", "1");
    }
    assert_eq!(
        index
            .prewarm_blocking(&document.id, &hash, usize::MAX)
            .expect("blocking warm"),
        sections,
        "the knob on warms every section with text"
    );
    assert_eq!(index.scan_text_cache_state(), Some((sections, false)));

    for (query, expected) in QUERIES.iter().zip(off_hits) {
        let on = production_hits(&index, &document, query).await;
        assert_eq!(on, expected, "the warm changed the hits for {query:?}");
        let plain = index
            .search_lexical_with_options(&document.id, query, 10, ScanOptions::plain())
            .await
            .unwrap_or_else(|error| panic!("plain search {query:?}: {error}"));
        assert_eq!(plain, on, "the fast scan differs for {query:?}");
    }

    unsafe {
        std::env::remove_var("READING_MCP_SCAN_PREWARM");
    }
}

async fn production_hits(
    index: &SqliteSearchIndex,
    document: &Document,
    query: &str,
) -> Vec<LexicalSearchHit> {
    index
        .search_lexical_with_options(&document.id, query, 10, ScanOptions::production())
        .await
        .unwrap_or_else(|error| panic!("production search {query:?}: {error}"))
}

fn sections_with_text(document: &Document) -> usize {
    fn count(section: &Section) -> usize {
        let own = usize::from(!section.content.is_empty());
        own + section.children.iter().map(count).sum::<usize>()
    }
    document.root_sections.iter().map(count).sum()
}

async fn multi_section_document() -> Document {
    let text = "\
# 第一章\n\n\
皇帝在朝堂上議事，群臣列坐，節度使入朝。\n\n\
# 第二章\n\n\
節度使入朝議事，皇帝遠在京城，群臣列坐。\n\n\
# 第三章\n\n\
寶玉在大觀園裡讀書，黛玉在旁邊看花。\n\n\
# 第四章\n\n\
記憶體與頁表映射，進程只能看到自己的地址空間。\n\n\
# 第五章\n\n\
The winter light fell on the old house by the river.\n";
    MarkdownParser
        .parse(RetrievedResource {
            source: DocumentSource("memory:prewarm-knob.md".into()),
            final_source: DocumentSource("memory:prewarm-knob.md".into()),
            media_type: MediaType("text/markdown".into()),
            bytes: text.as_bytes().to_vec(),
            etag: None,
            last_modified: None,
            metadata: Default::default(),
        })
        .await
        .expect("parse markdown")
}
