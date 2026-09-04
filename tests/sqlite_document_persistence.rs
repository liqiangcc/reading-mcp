use reading_mcp::application::ports::{ApplicationError, DocumentRepository};
use reading_mcp::domain::{
    ContentHash, Document, DocumentId, DocumentSource, Location, MediaType, NORMALIZATION_VERSION,
    Section, SectionId,
};
use reading_mcp::infrastructure::SqliteDocumentRepository;
use rusqlite::Connection;
use serde_json::Value;
use tempfile::tempdir;

#[tokio::test]
async fn current_normalization_version_document_survives_sqlite_reopen() {
    let directory = tempdir().expect("temporary directory should be created");
    let database = directory.path().join("state.sqlite");
    let document = fixture_document();

    let repository = SqliteDocumentRepository::open(&database).expect("repository should open");
    repository
        .save(document.clone())
        .await
        .expect("current document should save");
    drop(repository);

    let connection = Connection::open(&database).expect("database should open directly");
    let stored_json: String = connection
        .query_row(
            "SELECT document_json FROM documents WHERE id = ?1",
            [&document.id.0],
            |row| row.get(0),
        )
        .expect("stored document should exist");
    let stored: Value = serde_json::from_str(&stored_json).expect("stored document should be JSON");
    assert_eq!(
        stored.get("normalization_version"),
        Some(&Value::String(NORMALIZATION_VERSION.into()))
    );
    drop(connection);

    let reopened = SqliteDocumentRepository::open(&database).expect("repository should reopen");
    let restored = reopened
        .get(&document.id)
        .await
        .expect("current document should load")
        .expect("current document should exist");
    assert_eq!(restored, document);
}

#[tokio::test]
async fn legacy_row_without_normalization_version_fails_closed() {
    let directory = tempdir().expect("temporary directory should be created");
    let database = directory.path().join("state.sqlite");
    let document = fixture_document();

    let repository = SqliteDocumentRepository::open(&database).expect("repository should open");
    repository
        .save(document.clone())
        .await
        .expect("document should save");
    drop(repository);

    let connection = Connection::open(&database).expect("database should open directly");
    let stored_json: String = connection
        .query_row(
            "SELECT document_json FROM documents WHERE id = ?1",
            [&document.id.0],
            |row| row.get(0),
        )
        .expect("stored document should exist");
    let mut stored: Value =
        serde_json::from_str(&stored_json).expect("stored document should be JSON");
    stored
        .as_object_mut()
        .expect("stored document should be an object")
        .remove("normalization_version");
    connection
        .execute(
            "UPDATE documents SET document_json = ?1 WHERE id = ?2",
            [
                &serde_json::to_string(&stored).expect("legacy document should serialize"),
                &document.id.0,
            ],
        )
        .expect("legacy document should be written");
    drop(connection);

    let reopened = SqliteDocumentRepository::open(&database).expect("repository should reopen");
    let error = reopened
        .get(&document.id)
        .await
        .expect_err("legacy row must fail closed");
    assert!(matches!(error, ApplicationError::StaleDocument(_)));
    let message = error.to_string();
    assert!(message.contains(NORMALIZATION_VERSION));
    assert!(message.contains("explicit source reopen required"));
}

fn fixture_document() -> Document {
    Document {
        id: DocumentId("doc:sqlite-persistence".into()),
        source: DocumentSource("memory:sqlite-persistence.md".into()),
        title: "SQLite persistence".into(),
        media_type: MediaType("text/markdown".into()),
        content_hash: ContentHash("sha256:sqlite-persistence".into()),
        metadata: Default::default(),
        root_sections: vec![Section {
            id: SectionId("section://root".into()),
            parent_id: None,
            title: "Root".into(),
            level: 1,
            content: "Persisted content.".into(),
            location: Location {
                section_path: vec!["Root".into()],
                ..Location::default()
            },
            children: vec![],
        }],
    }
}
