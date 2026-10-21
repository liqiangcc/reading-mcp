//! The stored body order: what `save` writes, and what `body_order` reads back (R1c-3c).
//!
//! The expected value of every test is what the default `DocumentRepository::body_order` returns for
//! the same document — the full computation `section_body_order` performs — so the stored row is
//! proven to carry exactly that result.

use std::collections::BTreeMap;
use std::sync::Arc;

use reading_mcp::application::ports::{ApplicationError, DocumentRepository};
use reading_mcp::domain::{
    ContentHash, Document, DocumentId, DocumentSource, MediaType, Section, SectionId,
};
use reading_mcp::infrastructure::{InMemoryDocumentRepository, SqliteDocumentRepository};

const VALID_MAP: &str = r#"{"schema_version":"epub-structure-reconciliation/v1","sections":[{"section_id":"section://second","source_order":0},{"section_id":"section://first","source_order":1},{"section_id":"section://nested","source_order":2}]}"#;

/// The same two sections, in the other order.
const REORDERED_MAP: &str = r#"{"schema_version":"epub-structure-reconciliation/v1","sections":[{"section_id":"section://nested","source_order":0},{"section_id":"section://first","source_order":1},{"section_id":"section://second","source_order":2}]}"#;

/// A map whose source order is not contiguous, which `section_body_order` rejects.
const INVALID_MAP: &str = r#"{"schema_version":"epub-structure-reconciliation/v1","sections":[{"section_id":"section://second","source_order":0},{"section_id":"section://first","source_order":5},{"section_id":"section://nested","source_order":2}]}"#;

fn epub_document(structure_map: &str) -> Document {
    let mut metadata = BTreeMap::new();
    metadata.insert("epub_structure_map".to_string(), structure_map.to_string());
    Document {
        id: DocumentId("doc:body-order".into()),
        source: DocumentSource("memory:body-order".into()),
        title: "Body order".into(),
        media_type: MediaType("application/epub+zip".into()),
        content_hash: ContentHash("sha256:body-order".into()),
        metadata,
        root_sections: vec![
            section(
                "first",
                None,
                "First",
                vec![section("nested", Some("first"), "Nested", Vec::new())],
            ),
            section("second", None, "Second", Vec::new()),
        ],
    }
}

fn section(name: &str, parent: Option<&str>, title: &str, children: Vec<Section>) -> Section {
    Section {
        id: SectionId(format!("section://{name}")),
        parent_id: parent.map(|parent| SectionId(format!("section://{parent}"))),
        title: title.into(),
        level: 1,
        content: format!("{title} body."),
        location: Default::default(),
        children,
    }
}

/// What the full computation reports for `document`.
async fn computed(document: &Document) -> Result<Vec<SectionId>, ApplicationError> {
    let repository: Arc<dyn DocumentRepository> = Arc::new(InMemoryDocumentRepository::default());
    repository.save(document.clone()).await.expect("save");
    repository
        .body_order(&document.id)
        .await
        .expect("the default body order never fails")
        .expect("the default body order always answers")
}

async fn stored_body_order(document: &Document) -> Result<Vec<SectionId>, ApplicationError> {
    let directory = tempfile::tempdir().expect("temporary directory");
    let repository: Arc<dyn DocumentRepository> = Arc::new(
        SqliteDocumentRepository::open(directory.path().join("state.sqlite"))
            .expect("repository should open"),
    );
    repository.save(document.clone()).await.expect("save");
    repository
        .body_order(&document.id)
        .await
        .expect("body order read")
        .expect("the stored document has a body order")
}

#[tokio::test]
async fn stored_body_order_equals_the_computed_order() {
    let document = epub_document(VALID_MAP);
    let expected = computed(&document).await.expect("computed order");
    assert_eq!(
        expected,
        vec![
            SectionId("section://second".into()),
            SectionId("section://first".into()),
            SectionId("section://nested".into()),
        ],
        "the spine facts, not the tree, define the order"
    );
    assert_eq!(stored_body_order(&document).await, Ok(expected));
}

#[tokio::test]
async fn stored_body_order_error_equals_the_computed_error() {
    let document = epub_document(INVALID_MAP);
    let expected = computed(&document).await.expect_err("computed error");
    assert!(matches!(expected, ApplicationError::RepositoryFailed(_)));
    assert_eq!(
        stored_body_order(&document)
            .await
            .expect_err("stored error"),
        expected
    );
}

#[tokio::test]
async fn a_document_saved_before_the_table_existed_falls_back_to_the_computation() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let database = directory.path().join("state.sqlite");
    let document = epub_document(VALID_MAP);
    let repository = SqliteDocumentRepository::open(&database).expect("repository should open");
    repository.save(document.clone()).await.expect("save");

    // Simulate a row written by a binary that had no `document_body_order_v1`.
    rusqlite::Connection::open(&database)
        .expect("database should open")
        .execute(
            "DELETE FROM document_body_order_v1 WHERE document_id = ?1",
            rusqlite::params![&document.id.0],
        )
        .expect("the stored body order row should be deleted");

    let expected = computed(&document).await.expect("computed order");
    let stored = repository
        .body_order(&document.id)
        .await
        .expect("body order read")
        .expect("the stored document has a body order");
    assert_eq!(stored, Ok(expected));

    // The read path never writes: the row is still absent.
    let rows: i64 = rusqlite::Connection::open(&database)
        .expect("database should open")
        .query_row(
            "SELECT COUNT(*) FROM document_body_order_v1 WHERE document_id = ?1",
            rusqlite::params![&document.id.0],
            |row| row.get(0),
        )
        .expect("count");
    assert_eq!(rows, 0);
}

#[tokio::test]
async fn a_re_save_replaces_the_stored_body_order() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let database = directory.path().join("state.sqlite");
    let repository = SqliteDocumentRepository::open(&database).expect("repository should open");

    let first = epub_document(VALID_MAP);
    repository.save(first.clone()).await.expect("first save");
    let second = epub_document(REORDERED_MAP);
    repository.save(second.clone()).await.expect("second save");

    let rows: i64 = rusqlite::Connection::open(&database)
        .expect("database should open")
        .query_row(
            "SELECT COUNT(*) FROM document_body_order_v1 WHERE document_id = ?1",
            rusqlite::params![&second.id.0],
            |row| row.get(0),
        )
        .expect("count");
    assert_eq!(rows, 1, "one row per document, replaced on every save");

    let expected = computed(&second).await.expect("computed order");
    assert_ne!(expected, computed(&first).await.expect("computed order"));
    let stored = repository
        .body_order(&second.id)
        .await
        .expect("body order read")
        .expect("the stored document has a body order");
    assert_eq!(stored, Ok(expected));
}

#[tokio::test]
async fn a_saved_error_is_read_back_as_the_same_error() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let database = directory.path().join("state.sqlite");
    let repository = SqliteDocumentRepository::open(&database).expect("repository should open");
    let document = epub_document(INVALID_MAP);
    repository.save(document.clone()).await.expect("save");

    let error: Option<String> = rusqlite::Connection::open(&database)
        .expect("database should open")
        .query_row(
            "SELECT error FROM document_body_order_v1 WHERE document_id = ?1",
            rusqlite::params![&document.id.0],
            |row| row.get(0),
        )
        .expect("the error column should be readable");
    let error = error.expect("an invalid map stores the error text");
    assert!(!error.is_empty());
    assert_eq!(
        repository
            .body_order(&document.id)
            .await
            .expect("body order read")
            .expect("the stored document has a body order"),
        Err(ApplicationError::RepositoryFailed(error))
    );
}

#[tokio::test]
async fn a_non_epub_document_stores_its_canonical_order() {
    let mut document = epub_document(VALID_MAP);
    document.media_type = MediaType("text/markdown".into());
    document.metadata.clear();
    let expected = computed(&document).await.expect("computed order");
    assert_eq!(
        expected,
        vec![
            SectionId("section://first".into()),
            SectionId("section://nested".into()),
            SectionId("section://second".into()),
        ]
    );
    assert_eq!(stored_body_order(&document).await, Ok(expected));
}
