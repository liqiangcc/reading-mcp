use reading_mcp::application::ports::{ApplicationError, DocumentRepository};
use reading_mcp::domain::{
    ContentHash, Document, DocumentId, DocumentSource, MediaType, Section, SectionId,
};
use reading_mcp::infrastructure::SqliteDocumentRepository;
use rusqlite::{Connection, params};
use serde_json::Value;

#[tokio::test]
async fn sqlite_document_repository_rejects_unversioned_and_stale_normalization() {
    let directory = tempfile::tempdir().expect("temporary state directory should be created");
    let database = directory.path().join("reading-mcp.sqlite");
    let document = fixture_document();

    let repository =
        SqliteDocumentRepository::open(&database).expect("SQLite repository should open");
    repository
        .save(document.clone())
        .await
        .expect("current document should save");
    drop(repository);

    let current =
        SqliteDocumentRepository::open(&database).expect("SQLite repository should reopen");
    assert_eq!(
        current
            .get(&document.id)
            .await
            .expect("current normalization should load")
            .expect("current document should exist"),
        document
    );
    drop(current);

    rewrite_normalization_version(&database, &document.id, None);
    let repository = SqliteDocumentRepository::open(&database)
        .expect("SQLite repository should reopen after legacy rewrite");
    let error = repository
        .get(&document.id)
        .await
        .expect_err("unversioned persisted canonical facts must fail closed");
    assert!(matches!(error, ApplicationError::StaleDocument(_)));
    assert!(error.to_string().contains("normalization version"));
    drop(repository);

    let repository =
        SqliteDocumentRepository::open(&database).expect("SQLite repository should reopen");
    repository
        .save(document.clone())
        .await
        .expect("current document should restore");
    drop(repository);

    rewrite_normalization_version(
        &database,
        &document.id,
        Some("reading-mcp-normalization/v6"),
    );
    let repository = SqliteDocumentRepository::open(&database)
        .expect("SQLite repository should reopen after stale rewrite");
    let error = repository
        .get(&document.id)
        .await
        .expect_err("v6 persisted canonical facts must not be reinterpreted as v7");
    assert!(matches!(error, ApplicationError::StaleDocument(_)));
    assert!(error.to_string().contains("reading-mcp-normalization/v6"));
}

fn rewrite_normalization_version(
    database: &std::path::Path,
    document_id: &DocumentId,
    version: Option<&str>,
) {
    let connection = Connection::open(database).expect("SQLite database should open directly");
    let json: String = connection
        .query_row(
            "SELECT document_json FROM documents WHERE id = ?1",
            params![&document_id.0],
            |row| row.get(0),
        )
        .expect("persisted document JSON should exist");
    let mut value: Value = serde_json::from_str(&json).expect("document JSON should decode");
    let object = value
        .as_object_mut()
        .expect("persisted document JSON should be an object");
    match version {
        Some(version) => {
            object.insert(
                "normalization_version".into(),
                Value::String(version.into()),
            );
        }
        None => {
            object.remove("normalization_version");
        }
    }
    connection
        .execute(
            "UPDATE documents SET document_json = ?1 WHERE id = ?2",
            params![value.to_string(), &document_id.0],
        )
        .expect("persisted document JSON should update");
}

fn fixture_document() -> Document {
    Document {
        id: DocumentId("doc:normalization-migration".into()),
        source: DocumentSource("memory:normalization-migration.md".into()),
        title: "Normalization migration".into(),
        media_type: MediaType("text/markdown".into()),
        content_hash: ContentHash("sha256:normalization-migration".into()),
        metadata: Default::default(),
        root_sections: vec![Section {
            id: SectionId("section://one".into()),
            parent_id: None,
            title: "One".into(),
            level: 1,
            content: "body".into(),
            location: Default::default(),
            children: Vec::new(),
        }],
    }
}
