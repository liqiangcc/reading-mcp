//! The per-document scan text cache: it holds one document, and re-storing a document drops it.

use reading_mcp::application::ports::{
    DocumentRepository, LexicalSearchHit, Parser, RetrievedResource, SearchIndex,
};
use reading_mcp::domain::{Document, DocumentSource, MediaType};
use reading_mcp::infrastructure::{ScanOptions, SqliteDocumentRepository, SqliteSearchIndex};
use reading_mcp::parsing::TextParser;

#[tokio::test]
async fn a_second_document_evicts_the_first() {
    let directory = tempfile::tempdir().expect("temp directory");
    let database = directory.path().join("scan-cache.sqlite");
    let repository = SqliteDocumentRepository::open(&database).expect("repository");
    let index = SqliteSearchIndex::open(&database).expect("index");

    let first = text_document("memory:first.txt", "皇帝在朝堂上議事。節度使入朝。").await;
    let second = text_document("memory:second.txt", "寶玉在大觀園裡讀書。皇帝遠在京城。").await;
    for document in [&first, &second] {
        repository.save(document.clone()).await.expect("save");
        index.index(document).await.expect("index");
    }

    let expected = search(&index, &first, "皇帝").await;
    assert!(!expected.is_empty(), "the first document holds the query");

    // The second document's search replaces the cached text of the first one.
    let other = search(&index, &second, "皇帝").await;
    assert!(!other.is_empty(), "the second document holds the query");
    assert_ne!(expected, other, "the two documents answer differently");

    let again = search(&index, &first, "皇帝").await;
    assert_eq!(
        expected, again,
        "a search after another document's search returns the same hits"
    );
}

#[tokio::test]
async fn re_ingesting_a_document_invalidates_the_cached_text() {
    let directory = tempfile::tempdir().expect("temp directory");
    let database = directory.path().join("scan-cache-reingest.sqlite");
    let repository = SqliteDocumentRepository::open(&database).expect("repository");
    let index = SqliteSearchIndex::open(&database).expect("index");

    let source = "memory:reingest.txt";
    let before = text_document(source, "皇帝在朝堂上議事。群臣列坐。").await;
    repository.save(before.clone()).await.expect("save");
    index.index(&before).await.expect("index");
    assert!(!search(&index, &before, "皇帝").await.is_empty());

    // The same document identity, re-stored with different text: the stored segmentation is replaced,
    // so a search must read the new text and not the text the cache holds.
    let mut after = text_document(source, "節度使入朝議事。群臣列坐。").await;
    after.id = before.id.clone();
    after.content_hash = before.content_hash.clone();
    repository.save(after.clone()).await.expect("re-save");
    index.index(&after).await.expect("re-index");

    assert!(
        search(&index, &after, "皇帝").await.is_empty(),
        "the replaced text no longer holds the old query"
    );
    assert!(
        !search(&index, &after, "節度使").await.is_empty(),
        "a search reads the text the re-ingest stored"
    );
}

async fn search(
    index: &SqliteSearchIndex,
    document: &Document,
    query: &str,
) -> Vec<LexicalSearchHit> {
    let fast = index
        .search_lexical_with_options(&document.id, query, 10, ScanOptions::production())
        .await
        .unwrap_or_else(|error| panic!("fast search {query:?}: {error}"));
    let plain = index
        .search_lexical_with_options(&document.id, query, 10, ScanOptions::plain())
        .await
        .unwrap_or_else(|error| panic!("plain search {query:?}: {error}"));
    assert_eq!(plain, fast, "the fast scan differs for query {query:?}");
    fast
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
