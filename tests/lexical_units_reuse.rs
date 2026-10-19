use std::sync::Arc;

use reading_mcp::application::ports::{DocumentRepository, Parser, RetrievedResource, SearchIndex};
use reading_mcp::domain::{Document, DocumentSource, MediaType};
use reading_mcp::infrastructure::{
    InMemoryDocumentRepository, InMemorySearchIndex, SqliteDocumentRepository, SqliteSearchIndex,
};
use reading_mcp::parsing::TextParser;

const QUERIES: [&str; 4] = ["beta", "epsilon", "zeta theta", "lambda"];

fn resource() -> RetrievedResource {
    RetrievedResource {
        source: DocumentSource("memory:lexical-units.txt".into()),
        final_source: DocumentSource("memory:lexical-units.txt".into()),
        media_type: MediaType("text/plain".into()),
        bytes: b"Alpha beta gamma. Delta epsilon.\n\nZeta eta theta. Iota kappa lambda.\n".to_vec(),
        etag: None,
        last_modified: None,
        metadata: Default::default(),
    }
}

async fn parsed_document() -> Document {
    TextParser.parse(resource()).await.expect("parse document")
}

#[tokio::test]
async fn in_memory_index_reuses_caller_units() {
    let document = parsed_document().await;
    let paragraphs = document
        .try_paragraph_text_units()
        .expect("paragraph units");
    let sentences = document.try_sentence_text_units().expect("sentence units");

    let rebuilt = InMemorySearchIndex::default();
    let reused = InMemorySearchIndex::default();
    rebuilt.index(&document).await.expect("index");
    reused
        .index_with_units(&document, &paragraphs, &sentences)
        .await
        .expect("index with units");

    for query in QUERIES {
        assert_eq!(
            rebuilt
                .search_lexical(&document.id, query, 10)
                .await
                .expect("rebuilt search"),
            reused
                .search_lexical(&document.id, query, 10)
                .await
                .expect("reused search"),
            "in-memory results differ for {query:?}"
        );
    }
}

#[tokio::test]
async fn sqlite_index_reuses_caller_units() {
    let document = parsed_document().await;
    let paragraphs = document
        .try_paragraph_text_units()
        .expect("paragraph units");
    let sentences = document.try_sentence_text_units().expect("sentence units");

    let directory = tempfile::tempdir().expect("temp directory");
    let rebuilt =
        SqliteSearchIndex::open(directory.path().join("rebuilt.sqlite")).expect("rebuilt index");
    let reused =
        SqliteSearchIndex::open(directory.path().join("reused.sqlite")).expect("reused index");
    rebuilt.index(&document).await.expect("index");
    reused
        .index_with_units(&document, &paragraphs, &sentences)
        .await
        .expect("index with units");

    for query in QUERIES {
        assert_eq!(
            rebuilt
                .search_lexical(&document.id, query, 10)
                .await
                .expect("rebuilt search"),
            reused
                .search_lexical(&document.id, query, 10)
                .await
                .expect("reused search"),
            "sqlite results differ for {query:?}"
        );
    }
}

#[tokio::test]
async fn in_memory_repository_save_shared_reuses_the_arc() {
    let document = parsed_document().await;
    let repository = InMemoryDocumentRepository::default();
    let shared = Arc::new(document);
    repository
        .save_shared(Arc::clone(&shared))
        .await
        .expect("save shared");

    let stored = repository
        .get(&shared.id)
        .await
        .expect("get")
        .expect("stored document");
    assert!(Arc::ptr_eq(&stored, &shared));
}

#[tokio::test]
async fn sqlite_repository_save_shared_round_trips() {
    let document = parsed_document().await;
    let directory = tempfile::tempdir().expect("temp directory");
    let repository = SqliteDocumentRepository::open(directory.path().join("repository.sqlite"))
        .expect("repository");
    let shared = Arc::new(document);
    repository
        .save_shared(Arc::clone(&shared))
        .await
        .expect("save shared");

    let stored = repository
        .get(&shared.id)
        .await
        .expect("get")
        .expect("stored document");
    assert_eq!(*stored, *shared);
}
