use reading_mcp::application::ports::{ApplicationError, DocumentRepository, SearchIndex};
use reading_mcp::domain::{
    ContentHash, Document, DocumentId, DocumentSource, Location, MediaType, Section, SectionId,
};
use reading_mcp::infrastructure::{
    SqliteDocumentRepository, SqliteSearchIndex, SqliteTextUnitIndex,
};
use rusqlite::{Connection, params};
use std::collections::BTreeMap;

#[tokio::test]
async fn lexical_v4_state_is_dropped_and_search_scans_the_stored_text() {
    let directory = tempfile::tempdir().expect("temp directory");
    let database = directory.path().join("lexical-migration.sqlite");
    seed_v4_lexical_state(&database);

    {
        let repository = SqliteDocumentRepository::open(&database).expect("repository");
        let index = SqliteSearchIndex::open(&database).expect("open should drop derived state");
        let error = index
            .search_lexical(&DocumentId("doc:legacy".into()), "legacy", 10)
            .await
            .expect_err("legacy document rows must be discarded");
        assert!(matches!(error, ApplicationError::DocumentNotFound));

        let document = fixture();
        repository
            .save(document.clone())
            .await
            .expect("save document");
        index
            .index(&document)
            .await
            .expect("store the document's units");
        let hits = index
            .search_lexical(&document.id, "current", 10)
            .await
            .expect("search the stored text");
        assert!(!hits.is_empty());
        assert_eq!(index.index_version(), "lexical-scan/v1");
    }

    let connection = Connection::open(&database).expect("inspect migrated metadata");
    let remaining: i64 = connection
        .query_row(
            "SELECT count(*) FROM sqlite_master
             WHERE name LIKE 'lexical!_search!_%' ESCAPE '!'",
            [],
            |row| row.get(0),
        )
        .expect("legacy table probe");
    assert_eq!(remaining, 0, "every lexical_search_% table must be dropped");
}

#[test]
fn opening_the_text_unit_index_drops_the_legacy_search_tables() {
    let directory = tempfile::tempdir().expect("temp directory");
    let database = directory.path().join("legacy-fts.sqlite");
    {
        let connection = Connection::open(&database).expect("seed database");
        connection
            .execute_batch(
                "CREATE TABLE lexical_search_meta (
                    key TEXT PRIMARY KEY,
                    value TEXT NOT NULL
                 );
                 CREATE TABLE lexical_search_candidates_v5 (
                    rowid INTEGER PRIMARY KEY,
                    doc_key INTEGER NOT NULL
                 );
                 CREATE VIRTUAL TABLE lexical_search_fts_v5 USING fts5(
                    lexemes,
                    doc,
                    content='',
                    contentless_delete=1
                 );",
            )
            .expect("legacy search schema");
    }

    let _index = SqliteTextUnitIndex::open(&database).expect("open text unit index");
    let connection = Connection::open(&database).expect("inspect");
    let remaining: i64 = connection
        .query_row(
            "SELECT count(*) FROM sqlite_master
             WHERE name LIKE 'lexical!_search!_%' ESCAPE '!'",
            [],
            |row| row.get(0),
        )
        .expect("legacy table probe");
    assert_eq!(remaining, 0, "every lexical_search_% table must be dropped");
}

fn seed_v4_lexical_state(path: &std::path::Path) {
    let connection = Connection::open(path).expect("seed database");
    connection
        .execute_batch(
            "CREATE TABLE lexical_search_meta (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
             );
             CREATE VIRTUAL TABLE lexical_search_units_v2 USING fts5(
                document_id UNINDEXED,
                candidate_kind UNINDEXED,
                section_id UNINDEXED,
                title UNINDEXED,
                source UNINDEXED,
                snippet UNINDEXED,
                location_json UNINDEXED,
                locator_json UNINDEXED,
                tokenizer_version UNINDEXED,
                source_order UNINDEXED,
                lexemes
             );
             CREATE TABLE lexical_search_documents_v2 (
                document_id TEXT PRIMARY KEY,
                normalized_document_hash TEXT NOT NULL,
                index_version TEXT NOT NULL,
                tokenizer_version TEXT NOT NULL
             );",
        )
        .expect("legacy schema");
    connection
        .execute(
            "INSERT INTO lexical_search_meta(key, value) VALUES (?1, ?2)",
            params!["lexical_search_index_version", "lexical-search-index/v4"],
        )
        .expect("legacy index meta");
    connection
        .execute(
            "INSERT INTO lexical_search_meta(key, value) VALUES (?1, ?2)",
            params!["lexical_tokenizer_version", "lexical-tokenizer/v1"],
        )
        .expect("legacy tokenizer meta");
    connection
        .execute(
            "INSERT INTO lexical_search_documents_v2(
                document_id, normalized_document_hash, index_version, tokenizer_version
             ) VALUES (?1, ?2, ?3, ?4)",
            params![
                "doc:legacy",
                "sha256:legacy",
                "lexical-search-index/v4",
                "lexical-tokenizer/v1"
            ],
        )
        .expect("legacy document row");
    connection
        .execute(
            "INSERT INTO lexical_search_units_v2(
                document_id, candidate_kind, section_id, title, source, snippet,
                location_json, locator_json, tokenizer_version, source_order, lexemes
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                "doc:legacy",
                "section",
                "section://legacy",
                "Legacy",
                "memory:legacy",
                "legacy",
                "{}",
                "{}",
                "lexical-tokenizer/v1",
                "0",
                "x6c6567616379"
            ],
        )
        .expect("legacy FTS row");
}

fn fixture() -> Document {
    Document {
        id: DocumentId("doc:current-v3".into()),
        source: DocumentSource("memory:current-v3".into()),
        title: "Current".into(),
        media_type: MediaType("text/plain".into()),
        content_hash: ContentHash("sha256:current-v3".into()),
        metadata: BTreeMap::new(),
        root_sections: vec![Section {
            id: SectionId("section://current".into()),
            parent_id: None,
            title: "Current".into(),
            level: 1,
            content: "Current lexical identity.".into(),
            location: Location::default(),
            children: vec![],
        }],
    }
}
