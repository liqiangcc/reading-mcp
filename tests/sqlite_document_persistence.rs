use std::sync::Arc;

use reading_mcp::application::ports::{ApplicationError, DocumentRepository};
use reading_mcp::domain::{
    ContentHash, Document, DocumentId, DocumentSource, Location, MediaType, NORMALIZATION_VERSION,
    Section, SectionId,
};
use reading_mcp::infrastructure::SqliteDocumentRepository;
use rusqlite::{Connection, params};
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
    assert_eq!(*restored, document);
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

#[tokio::test]
async fn get_with_hash_returns_the_computed_hash_and_decodes_each_call() {
    let directory = tempdir().expect("temporary directory should be created");
    let database = directory.path().join("state.sqlite");
    let document = fixture_document();
    let expected = document.normalized_document_hash();

    let repository = SqliteDocumentRepository::open(&database).expect("repository should open");
    repository
        .save(document.clone())
        .await
        .expect("document should save");
    let (loaded, loaded_hash) = repository
        .get_with_hash(&document.id)
        .await
        .expect("first read should load")
        .expect("document should exist");
    assert_eq!(loaded_hash, expected);
    assert_eq!(*loaded, document);
    drop(repository);

    let reopened = SqliteDocumentRepository::open(&database).expect("repository should reopen");
    let (again, again_hash) = reopened
        .get_with_hash(&document.id)
        .await
        .expect("fresh repository should load")
        .expect("document should exist");
    assert_eq!(again_hash, expected);
    assert_eq!(*again, document);

    // There is no document cache: a second read decodes the row again, so the two results are
    // equal but distinct allocations.
    let (third, third_hash) = reopened
        .get_with_hash(&document.id)
        .await
        .expect("second read should load")
        .expect("document should exist");
    assert_eq!(third_hash, expected);
    assert_eq!(*third, document);
    assert!(!Arc::ptr_eq(&again, &third));
}

#[tokio::test]
async fn legacy_row_without_hash_is_backfilled_on_get_with_hash() {
    let directory = tempdir().expect("temporary directory should be created");
    let seed_database = directory.path().join("seed.sqlite");
    let database = directory.path().join("legacy.sqlite");
    let document = fixture_document();

    let seed = SqliteDocumentRepository::open(&seed_database).expect("seed should open");
    seed.save(document.clone()).await.expect("seed should save");
    drop(seed);
    let seed_connection = Connection::open(&seed_database).expect("seed should open directly");
    let stored_json: String = seed_connection
        .query_row(
            "SELECT document_json FROM documents WHERE id = ?1",
            [&document.id.0],
            |row| row.get(0),
        )
        .expect("seed document should exist");
    drop(seed_connection);

    let legacy = Connection::open(&database).expect("legacy database should open");
    legacy
        .execute_batch(
            "CREATE TABLE documents (
                id TEXT PRIMARY KEY,
                source TEXT NOT NULL,
                content_hash TEXT NOT NULL,
                document_json TEXT NOT NULL
             );",
        )
        .expect("legacy schema should be created");
    legacy
        .execute(
            "INSERT INTO documents(id, source, content_hash, document_json) VALUES (?1, ?2, ?3, ?4)",
            params![
                &document.id.0,
                &document.source.0,
                &document.content_hash.0,
                &stored_json
            ],
        )
        .expect("legacy row should be written");
    drop(legacy);

    let repository = SqliteDocumentRepository::open(&database).expect("legacy should open");
    let expected = document.normalized_document_hash();
    let (restored, hash) = repository
        .get_with_hash(&document.id)
        .await
        .expect("legacy row should load")
        .expect("legacy row should exist");
    assert_eq!(hash, expected);
    assert_eq!(*restored, document);

    let connection = Connection::open(&database).expect("legacy database should reopen");
    let backfilled: Option<String> = connection
        .query_row(
            "SELECT normalized_document_hash FROM documents WHERE id = ?1",
            [&document.id.0],
            |row| row.get(0),
        )
        .expect("backfilled hash should be readable");
    assert_eq!(backfilled, Some(expected.0));
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
