//! The background warm of the per-document scan text cache (SEARCH-PREWARM): it fills the store with
//! exactly the section texts a first scan would read, stops when it must, and never changes a hit.

use std::time::{Duration, Instant};

use reading_mcp::application::ports::{
    DocumentRepository, LexicalSearchHit, Parser, RetrievedResource, SearchIndex,
};
use reading_mcp::domain::{Document, DocumentSource, MediaType, Section};
use reading_mcp::infrastructure::{ScanOptions, SqliteDocumentRepository, SqliteSearchIndex};
use reading_mcp::parsing::MarkdownParser;

const QUERIES: [&str; 4] = ["皇帝", "寶玉", "節度使", "不存在的詞組甲乙"];

#[tokio::test]
async fn the_warm_fills_every_section_and_the_search_still_matches_the_plain_scan() {
    let directory = tempfile::tempdir().expect("temp directory");
    let database = directory.path().join("prewarm.sqlite");
    let repository = SqliteDocumentRepository::open(&database).expect("repository");
    let index = SqliteSearchIndex::open(&database).expect("index");
    let document = multi_section_document().await;
    store_and_index(&repository, &index, &document).await;

    let sections = sections_with_text(&document);
    assert!(
        sections > 1,
        "the fixture holds {sections} sections with text"
    );

    index.prewarm(&document.id, &document.normalized_document_hash());

    // No fixed sleep: poll the store until it holds every section, and fail if it never does.
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some((held, over_cap)) = index.scan_text_cache_state() {
            assert!(!over_cap, "a small document must not be over the cap");
            if held == sections {
                break;
            }
        }
        assert!(
            Instant::now() < deadline,
            "the warm did not fill the store within 10 s (state {:?})",
            index.scan_text_cache_state()
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }

    for query in QUERIES {
        assert_scan_parity(&index, &document, query).await;
    }
}

#[tokio::test]
async fn the_warm_stores_exactly_the_sections_a_first_scan_would() {
    let directory = tempfile::tempdir().expect("temp directory");
    let database = directory.path().join("prewarm-exact.sqlite");
    let repository = SqliteDocumentRepository::open(&database).expect("repository");
    let index = SqliteSearchIndex::open(&database).expect("index");
    let document = multi_section_document().await;
    store_and_index(&repository, &index, &document).await;

    let sections = sections_with_text(&document);
    let warmed = index
        .prewarm_blocking(
            &document.id,
            &document.normalized_document_hash(),
            usize::MAX,
        )
        .expect("blocking warm");
    assert_eq!(warmed, sections, "the warm read every section with text");
    assert_eq!(
        index.scan_text_cache_state(),
        Some((sections, false)),
        "the store holds every section and is not over the cap"
    );

    // The store is what a first scan would have built: every query answers as the plain scan does.
    for query in QUERIES {
        assert_scan_parity(&index, &document, query).await;
    }
}

#[tokio::test]
async fn a_search_issued_while_the_warm_runs_returns_the_same_hits() {
    let directory = tempfile::tempdir().expect("temp directory");
    let database = directory.path().join("prewarm-race.sqlite");
    let repository = SqliteDocumentRepository::open(&database).expect("repository");
    let index = SqliteSearchIndex::open(&database).expect("index");
    let document = large_multi_section_document().await;
    store_and_index(&repository, &index, &document).await;

    let expected: Vec<Vec<LexicalSearchHit>> = {
        let mut expected = Vec::new();
        for query in QUERIES {
            expected.push(
                index
                    .search_lexical_with_options(&document.id, query, 10, ScanOptions::plain())
                    .await
                    .unwrap_or_else(|error| panic!("plain search {query:?}: {error}")),
            );
        }
        expected
    };

    // The warm starts and the searches run against the store while it is being filled.
    index.prewarm(&document.id, &document.normalized_document_hash());
    for (query, plain) in QUERIES.iter().zip(expected) {
        let fast = index
            .search_lexical_with_options(&document.id, query, 10, ScanOptions::production())
            .await
            .unwrap_or_else(|error| panic!("fast search {query:?}: {error}"));
        assert_eq!(
            fast, plain,
            "the search during the warm differs for {query:?}"
        );
    }
}

#[tokio::test]
async fn a_second_document_in_the_slot_leaves_the_old_warm_nothing_to_fill() {
    let directory = tempfile::tempdir().expect("temp directory");
    let database = directory.path().join("prewarm-replace.sqlite");
    let repository = SqliteDocumentRepository::open(&database).expect("repository");
    let index = SqliteSearchIndex::open(&database).expect("index");
    let first = multi_section_document().await;
    let second = other_document().await;
    store_and_index(&repository, &index, &first).await;
    store_and_index(&repository, &index, &second).await;

    // A warm stopped after one section, so the store holds part of the first document.
    let first_hash = first.normalized_document_hash();
    assert_eq!(
        index
            .prewarm_blocking(&first.id, &first_hash, 1)
            .expect("warm"),
        1
    );
    assert_eq!(index.scan_text_cache_state(), Some((1, false)));

    // A search of the second document takes the slot, as it does today.
    let second_sections = sections_with_text(&second);
    assert!(!second_sections.is_empty());
    let second_hits = index
        .search_lexical_with_options(&second.id, "寶玉", 10, ScanOptions::production())
        .await
        .expect("search the second document");
    assert!(
        !second_hits.is_empty(),
        "the second document holds the query"
    );
    assert_eq!(
        index.scan_text_cache_state(),
        Some((second_sections, false)),
        "the second document's store holds exactly its own sections"
    );

    // Warming the first document again must not touch the second document's store.
    assert_eq!(
        index
            .prewarm_blocking(&first.id, &first_hash, usize::MAX)
            .expect("second warm"),
        0,
        "a warm never takes the slot from another document"
    );
    assert_eq!(
        index.scan_text_cache_state(),
        Some((second_sections, false)),
        "the old warm left the new store untouched"
    );

    // And the first document still answers exactly as the plain scan does.
    for query in QUERIES {
        assert_scan_parity(&index, &first, query).await;
    }
}

#[tokio::test]
async fn a_document_whose_units_are_not_stored_is_not_warmed() {
    let directory = tempfile::tempdir().expect("temp directory");
    let database = directory.path().join("prewarm-unstored.sqlite");
    let repository = SqliteDocumentRepository::open(&database).expect("repository");
    let index = SqliteSearchIndex::open(&database).expect("index");
    let document = multi_section_document().await;
    // Saved but never indexed: the store has no units for it.
    repository.save(document.clone()).await.expect("save");

    index.prewarm(&document.id, &document.normalized_document_hash());
    assert_eq!(
        index
            .prewarm_blocking(
                &document.id,
                &document.normalized_document_hash(),
                usize::MAX
            )
            .expect("warm"),
        0
    );
    assert_eq!(
        index.scan_text_cache_state(),
        None,
        "nothing is cached for a document with no stored units"
    );
}

async fn store_and_index(
    repository: &SqliteDocumentRepository,
    index: &SqliteSearchIndex,
    document: &Document,
) {
    repository.save(document.clone()).await.expect("save");
    index.index(document).await.expect("index");
}

/// The production scan and the plain scan of R2c step 3 must answer `query` identically.
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

/// The sections a scan reads: the ones that hold text.
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
    markdown_document("memory:prewarm.md", text).await
}

/// The same shape, but big enough that the warm takes more than one step to finish.
async fn large_multi_section_document() -> Document {
    let mut text = String::new();
    for section in 0..2_000 {
        text.push_str(&format!("# 第{section}節\n\n"));
        text.push_str(
            "皇帝在朝堂上議事，群臣列坐，節度使入朝，寶玉在大觀園裡讀書，\
             記憶體與頁表映射，進程只能看到自己的地址空間。\n\n",
        );
    }
    markdown_document("memory:prewarm-large.md", &text).await
}

async fn other_document() -> Document {
    let text = "\
# 甲\n\n\
寶玉在大觀園裡讀書，黛玉在旁邊看花。\n\n\
# 乙\n\n\
The winter light fell on the old house by the river.\n";
    markdown_document("memory:prewarm-other.md", text).await
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

/// A search index that counts the `prewarm` calls it receives and answers nothing.
#[derive(Default)]
struct CountingSearchIndex {
    prewarms: std::sync::atomic::AtomicUsize,
}

#[async_trait::async_trait]
impl SearchIndex for CountingSearchIndex {
    async fn index(
        &self,
        _document: &Document,
    ) -> Result<(), reading_mcp::application::ports::ApplicationError> {
        Ok(())
    }

    async fn search(
        &self,
        _document_id: &reading_mcp::domain::DocumentId,
        _query: &str,
        _limit: usize,
    ) -> Result<
        Vec<reading_mcp::application::ports::SearchHit>,
        reading_mcp::application::ports::ApplicationError,
    > {
        Ok(vec![])
    }

    fn prewarm(
        &self,
        _document_id: &reading_mcp::domain::DocumentId,
        _normalized_document_hash: &reading_mcp::domain::NormalizedDocumentHash,
    ) {
        self.prewarms
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }
}

#[test]
fn the_observed_search_index_forwards_prewarm_to_its_inner_index() {
    let inner = std::sync::Arc::new(CountingSearchIndex::default());
    let observed = reading_mcp::infrastructure::ObservedSearchIndex::new(inner.clone());
    let document = Document {
        id: reading_mcp::domain::DocumentId("doc:observed-prewarm".into()),
        source: DocumentSource("memory:observed-prewarm.md".into()),
        title: "Observed prewarm".into(),
        media_type: MediaType("text/markdown".into()),
        content_hash: reading_mcp::domain::ContentHash("sha256:observed-prewarm".into()),
        metadata: Default::default(),
        root_sections: vec![],
    };
    SearchIndex::prewarm(
        &observed,
        &document.id,
        &document.normalized_document_hash(),
    );
    assert_eq!(
        inner.prewarms.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "a no-op forward here would silently disable the warm in the runtime"
    );
}
