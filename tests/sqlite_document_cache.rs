use std::sync::Arc;

use reading_mcp::application::ports::DocumentRepository;
use reading_mcp::domain::{ContentHash, Document, DocumentId, DocumentSource, MediaType};
use reading_mcp::infrastructure::SqliteDocumentRepository;

fn document(n: usize, title: &str) -> Document {
    Document {
        id: DocumentId(format!("doc-{n}")),
        source: DocumentSource(format!("memory:doc-{n}.txt")),
        title: title.into(),
        media_type: MediaType("text/plain".into()),
        content_hash: ContentHash(format!("sha256:{n}")),
        metadata: Default::default(),
        root_sections: vec![],
    }
}

fn id(n: usize) -> DocumentId {
    DocumentId(format!("doc-{n}"))
}

async fn get(repository: &SqliteDocumentRepository, n: usize) -> Arc<Document> {
    repository.get(&id(n)).await.expect("get").expect("saved")
}

#[tokio::test]
async fn repeated_get_returns_the_cached_document() {
    let directory = tempfile::tempdir().expect("tempdir");
    let repository =
        SqliteDocumentRepository::open(directory.path().join("state.sqlite")).expect("open");
    repository.save(document(1, "v1")).await.expect("save");
    let first = get(&repository, 1).await;
    let second = get(&repository, 1).await;
    assert!(Arc::ptr_eq(&first, &second));
}

#[tokio::test]
async fn save_through_the_same_repository_invalidates_the_entry() {
    let directory = tempfile::tempdir().expect("tempdir");
    let repository =
        SqliteDocumentRepository::open(directory.path().join("state.sqlite")).expect("open");
    repository.save(document(1, "v1")).await.expect("save v1");
    assert_eq!(get(&repository, 1).await.title, "v1");
    repository.save(document(1, "v2")).await.expect("save v2");
    assert_eq!(get(&repository, 1).await.title, "v2");
}

#[tokio::test]
async fn write_from_another_connection_invalidates_the_cache() {
    let directory = tempfile::tempdir().expect("tempdir");
    let path = directory.path().join("state.sqlite");
    let reader = SqliteDocumentRepository::open(&path).expect("open reader");
    let writer = SqliteDocumentRepository::open(&path).expect("open writer");
    reader.save(document(1, "v1")).await.expect("save v1");
    assert_eq!(get(&reader, 1).await.title, "v1");
    writer
        .save(document(1, "v2"))
        .await
        .expect("save v2 through another connection");
    assert_eq!(get(&reader, 1).await.title, "v2");
}

#[tokio::test]
async fn least_recently_used_document_is_evicted_after_eight() {
    let directory = tempfile::tempdir().expect("tempdir");
    let repository =
        SqliteDocumentRepository::open(directory.path().join("state.sqlite")).expect("open");
    for n in 0..9 {
        repository.save(document(n, "v1")).await.expect("save");
    }
    let first = get(&repository, 0).await;
    for n in 1..9 {
        get(&repository, n).await;
    }
    let again = get(&repository, 0).await;
    assert!(
        !Arc::ptr_eq(&first, &again),
        "doc-0 should have been evicted by doc-1..doc-8"
    );
    assert_eq!(*first, *again);
}
