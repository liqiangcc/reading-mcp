//! STORAGE-1: a store-version bump must leave no dead open result or unreachable document behind.
//!
//! The state here is the host shape observed on 2026-10-24: one document with an open result of the
//! current store version next to one of an older version, a document no open result references any
//! more with all of its rows still present, and a document saved before SEC-1 (no header row) that
//! a current open result references and that must stay readable through the port defaults.

use std::collections::BTreeMap;

use reading_mcp::application::open_document::OpenDocumentResult;
use reading_mcp::application::ports::{DocumentRepository, OpenResultStore};
use reading_mcp::application::reading_profile::{
    CanonicalTextCoverage, LexicalSearchCapability, ReadingCapabilities,
    ReadingCapabilityAvailability, ReadingProfile, ReliabilitySummary, SegmentedReadingCapability,
    SentenceFirstCapability, SimpleReadingCapability, StructureCapability,
};
use reading_mcp::application::text_unit_stream::TEXT_UNIT_STORE_VERSION;
use reading_mcp::domain::{
    ContentHash, Document, DocumentId, DocumentSource, Location, MediaType, NormalizedDocumentHash,
    Section, SectionId,
};
use reading_mcp::infrastructure::{
    SqliteDocumentRepository, SqliteOpenResultStore, SqliteTextUnitIndex,
};
use rusqlite::{Connection, params};
use tempfile::tempdir;

const ORPHAN_DOC_KEY: i64 = 42;

fn document(id: &str) -> Document {
    Document {
        id: DocumentId(format!("doc:{id}")),
        source: DocumentSource(format!("memory:{id}.md")),
        title: format!("Document {id}"),
        media_type: MediaType("text/markdown".into()),
        content_hash: ContentHash(format!("sha256:{id}")),
        metadata: BTreeMap::new(),
        root_sections: vec![Section {
            id: SectionId(format!("section://{id}/root")),
            parent_id: None,
            title: "Root".into(),
            level: 1,
            content: format!("Body of {id}."),
            location: Location {
                section_path: vec!["Root".into()],
                ..Location::default()
            },
            children: vec![],
        }],
    }
}

fn current_fingerprint() -> String {
    format!("open-result/v1|test|text-unit-stream/v1|{TEXT_UNIT_STORE_VERSION}")
}

fn older_fingerprint() -> String {
    "open-result/v1|test|text-unit-stream/v1|text-unit-store/v8".into()
}

/// The legacy `open_results_v1` shape: no uniqueness on `document_id`, so a state directory that
/// went through an older version can hold two rows of the same document.
fn create_legacy_open_results(connection: &Connection) {
    connection
        .execute_batch(
            "DROP TABLE IF EXISTS open_results_v1;
             CREATE TABLE open_results_v1 (
                document_id TEXT NOT NULL,
                fingerprint TEXT NOT NULL,
                result_json TEXT NOT NULL,
                UNIQUE(document_id, fingerprint)
             );",
        )
        .expect("legacy open results table should be created");
}

fn insert_open_result(connection: &Connection, document_id: &str, fingerprint: &str) {
    connection
        .execute(
            "INSERT INTO open_results_v1(document_id, fingerprint, result_json)
             VALUES (?1, ?2, '{}')",
            params![document_id, fingerprint],
        )
        .expect("open result row should be inserted");
}

fn count(connection: &Connection, sql: &str, value: &str) -> i64 {
    connection
        .query_row(sql, params![value], |row| row.get(0))
        .expect("count should be readable")
}

fn seed_orphan_rows(connection: &Connection, orphan: &Document) {
    connection
        .execute(
            "INSERT INTO text_unit_documents_v2(
                doc_key, document_id, content_hash, normalized_document_hash, segmentation_version)
             VALUES (?1, ?2, ?3, 'nd:orphan', 'text-segmentation/v3')",
            params![ORPHAN_DOC_KEY, orphan.id.0, orphan.content_hash.0],
        )
        .expect("text unit document row should be inserted");
    connection
        .execute(
            "INSERT INTO section_text_v3(doc_key, section_key, chunk_no, encoding, data)
             VALUES (?1, 0, 0, 0, x'00')",
            params![ORPHAN_DOC_KEY],
        )
        .expect("section text row should be inserted");
    connection
        .execute(
            "INSERT INTO section_layout_v2(
                doc_key, section_key, section_id, char_len, paragraph_coverage_json, plan_error,
                paragraph_count, sentence_count, chunk_count, sorted_flags,
                total_items_paragraph_preserve, total_items_paragraph_eligible,
                total_items_sentence_preserve, total_items_sentence_eligible,
                coverage_paragraph_preserve_json, coverage_paragraph_eligible_json,
                coverage_sentence_preserve_json, coverage_sentence_eligible_json)
             VALUES (?1, 0, 'section://orphan/root', 0, NULL, NULL, 0, 0, 0, 0,
                     0, 0, 0, 0, '[]', '[]', '[]', '[]')",
            params![ORPHAN_DOC_KEY],
        )
        .expect("section layout row should be inserted");
    connection
        .execute(
            "INSERT INTO section_layout_chunk_v2(
                doc_key, section_key, chunk_no, first_paragraph_index, first_sentence_index,
                paragraph_slot, sentence_slot, paragraph_entry, sentence_entry,
                eligible_paragraph_entry, eligible_sentence_entry, paragraph_count,
                sentence_count, blob)
             VALUES (?1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, x'00')",
            params![ORPHAN_DOC_KEY],
        )
        .expect("section layout chunk row should be inserted");
}

fn sample_result(id: &str) -> OpenDocumentResult {
    OpenDocumentResult {
        document_id: DocumentId(id.into()),
        source: DocumentSource("/books/sample.md".into()),
        title: "Sample".into(),
        media_type: MediaType("text/markdown".into()),
        content_hash: ContentHash("sha256:abc".into()),
        normalized_document_hash: NormalizedDocumentHash("nd:abc".into()),
        normalized_document_hash_version: "normalized-document-hash/v1".into(),
        normalization_version: "normalization/v1".into(),
        normalized_text_coordinate_space: "normalized-text/v1".into(),
        section_count: 1,
        reading_profile: ReadingProfile {
            schema_version: "reading-profile/v1".into(),
            capabilities: ReadingCapabilities {
                structural_navigation: StructureCapability {
                    availability: ReadingCapabilityAvailability::Available,
                    section_count: 1,
                },
                paragraph_enumeration: SegmentedReadingCapability {
                    availability: ReadingCapabilityAvailability::Available,
                    segmentation_version: "text-segmentation/v1".into(),
                },
                sentence_first_enumeration: SentenceFirstCapability {
                    availability: ReadingCapabilityAvailability::Available,
                    segmentation_version: "text-segmentation/v1".into(),
                    source_preserving_coarse_regions: true,
                },
                exact_locator_read: SimpleReadingCapability {
                    availability: ReadingCapabilityAvailability::Available,
                },
                locator_context: SimpleReadingCapability {
                    availability: ReadingCapabilityAvailability::Available,
                },
                lexical_search: LexicalSearchCapability {
                    availability: ReadingCapabilityAvailability::Unavailable,
                    precise_candidates: false,
                },
            },
            canonical_text_coverage: CanonicalTextCoverage::default(),
            reliability: ReliabilitySummary::not_applicable(),
        },
    }
}

#[tokio::test]
async fn the_prune_removes_the_old_result_and_the_unreachable_document_and_keeps_the_reachable_ones()
 {
    let directory = tempdir().expect("temporary directory should be created");
    let database = directory.path().join("state.sqlite");
    let current = document("current");
    let orphan = document("orphan");
    let legacy = document("legacy");
    let current_fingerprint = current_fingerprint();
    let older_fingerprint = older_fingerprint();

    {
        let repository = SqliteDocumentRepository::open(&database).expect("repository should open");
        repository
            .save(current.clone())
            .await
            .expect("current document should save");
        repository
            .save(orphan.clone())
            .await
            .expect("unreachable document should save");
        repository
            .save(legacy.clone())
            .await
            .expect("pre-SEC-1 document should save");
    }
    // The text-unit store creates the per-`doc_key` tables this test seeds.
    drop(SqliteTextUnitIndex::open(&database).expect("text unit index should open"));

    let connection = Connection::open(&database).expect("database should open directly");
    create_legacy_open_results(&connection);
    insert_open_result(&connection, &current.id.0, &current_fingerprint);
    insert_open_result(&connection, &current.id.0, &older_fingerprint);
    insert_open_result(&connection, &legacy.id.0, &current_fingerprint);
    // The pre-SEC-1 document keeps only its `documents` row: no header, section or body-order rows.
    for table in [
        "document_header_v1",
        "document_sections_v1",
        "document_body_order_rows_v1",
    ] {
        connection
            .execute(
                &format!("DELETE FROM {table} WHERE document_id = ?1"),
                params![legacy.id.0],
            )
            .expect("pre-SEC-1 rows should be removed");
    }
    seed_orphan_rows(&connection, &orphan);
    let current_json_before: String = connection
        .query_row(
            "SELECT document_json FROM documents WHERE id = ?1",
            params![current.id.0],
            |row| row.get(0),
        )
        .expect("current document json should exist");
    drop(connection);

    let repository = SqliteDocumentRepository::open(&database).expect("repository should reopen");
    let legacy_header_before = repository
        .document_header(&legacy.id)
        .await
        .expect("header read should succeed")
        .expect("pre-SEC-1 header should fall back to the document");

    let store = SqliteOpenResultStore::open(&database).expect("open result store should open");
    let report = store
        .prune_stale_and_unreachable(64)
        .expect("prune should succeed");
    assert_eq!(report.open_results_removed, 1, "one older-version result");
    assert_eq!(report.documents_removed, 1, "one unreachable document");
    assert!(report.rows_removed > 1, "the document rows are removed too");

    // The current document is untouched: its one current result and its bytes stay.
    let connection = Connection::open(&database).expect("database should reopen directly");
    assert_eq!(
        count(
            &connection,
            "SELECT COUNT(*) FROM open_results_v1 WHERE document_id = ?1",
            &current.id.0,
        ),
        1
    );
    let stored_fingerprint: String = connection
        .query_row(
            "SELECT fingerprint FROM open_results_v1 WHERE document_id = ?1",
            params![current.id.0],
            |row| row.get(0),
        )
        .expect("current result should exist");
    assert_eq!(stored_fingerprint, current_fingerprint);
    let current_json_after: String = connection
        .query_row(
            "SELECT document_json FROM documents WHERE id = ?1",
            params![current.id.0],
            |row| row.get(0),
        )
        .expect("current document should still exist");
    assert_eq!(current_json_after, current_json_before);

    // The old open result is gone.
    let stale_rows: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM open_results_v1
              WHERE document_id = ?1 AND fingerprint <> ?2",
            params![current.id.0, current_fingerprint],
            |row| row.get(0),
        )
        .expect("stale result count should be readable");
    assert_eq!(stale_rows, 0);

    // The unreachable document and every row keyed by it or its `doc_key` are gone.
    assert_eq!(
        count(
            &connection,
            "SELECT COUNT(*) FROM documents WHERE id = ?1",
            &orphan.id.0,
        ),
        0
    );
    for table in [
        "document_header_v1",
        "document_sections_v1",
        "document_body_order_rows_v1",
        "document_meta_v2",
        "text_unit_documents_v2",
        "text_units",
    ] {
        assert_eq!(
            count(
                &connection,
                &format!("SELECT COUNT(*) FROM {table} WHERE document_id = ?1"),
                &orphan.id.0,
            ),
            0,
            "{table} still holds an orphan row"
        );
    }
    for table in [
        "section_text_v3",
        "section_layout_v2",
        "section_layout_chunk_v2",
    ] {
        let rows: i64 = connection
            .query_row(
                &format!("SELECT COUNT(*) FROM {table} WHERE doc_key = ?1"),
                params![ORPHAN_DOC_KEY],
                |row| row.get(0),
            )
            .expect("doc_key count should be readable");
        assert_eq!(rows, 0, "{table} still holds an orphan row");
    }
    drop(connection);

    // The pre-SEC-1 document is untouched and still answers every read through the fallback.
    assert_eq!(
        repository
            .document_header(&legacy.id)
            .await
            .expect("header read should succeed")
            .expect("pre-SEC-1 document should still be reachable"),
        legacy_header_before
    );
    let loaded = repository
        .get(&legacy.id)
        .await
        .expect("document read should succeed")
        .expect("pre-SEC-1 document should still load");
    assert_eq!(*loaded, legacy);
    let section = repository
        .section_by_key(&legacy.id, 0)
        .await
        .expect("section read should succeed")
        .expect("pre-SEC-1 section should still answer");
    assert_eq!(section.id, SectionId("section://legacy/root".into()));
    let text = repository
        .load_section_text(&legacy.id, &SectionId("section://legacy/root".into()))
        .await
        .expect("section text read should succeed");
    assert_eq!(text.as_deref(), Some("Body of legacy."));
    let order = repository
        .body_order(&legacy.id)
        .await
        .expect("body order read should succeed")
        .expect("pre-SEC-1 body order should still answer")
        .expect("body order should be valid");
    assert_eq!(order, vec![SectionId("section://legacy/root".into())]);
}

#[tokio::test]
async fn pruning_twice_changes_nothing() {
    let directory = tempdir().expect("temporary directory should be created");
    let database = directory.path().join("state.sqlite");
    let orphan = document("orphan");
    {
        let repository = SqliteDocumentRepository::open(&database).expect("repository should open");
        repository
            .save(orphan)
            .await
            .expect("unreachable document should save");
    }
    let store = SqliteOpenResultStore::open(&database).expect("open result store should open");

    let first = store
        .prune_stale_and_unreachable(64)
        .expect("first prune should succeed");
    assert_eq!(first.documents_removed, 1);
    let second = store
        .prune_stale_and_unreachable(64)
        .expect("second prune should succeed");
    assert_eq!(second.documents_removed, 0);
    assert_eq!(second.open_results_removed, 0);
    assert_eq!(second.rows_removed, 0);
}

#[tokio::test]
async fn the_startup_sweep_removes_at_most_its_bound_per_call() {
    let directory = tempdir().expect("temporary directory should be created");
    let database = directory.path().join("state.sqlite");
    {
        let repository = SqliteDocumentRepository::open(&database).expect("repository should open");
        for id in ["orphan-a", "orphan-b", "orphan-c"] {
            repository
                .save(document(id))
                .await
                .expect("unreachable document should save");
        }
    }
    let store = SqliteOpenResultStore::open(&database).expect("open result store should open");

    let first = store
        .prune_stale_and_unreachable(2)
        .expect("bounded prune should succeed");
    assert_eq!(first.documents_removed, 2);
    let second = store
        .prune_stale_and_unreachable(2)
        .expect("second bounded prune should succeed");
    assert_eq!(second.documents_removed, 1);
    let third = store
        .prune_stale_and_unreachable(2)
        .expect("third bounded prune should succeed");
    assert_eq!(third.documents_removed, 0);
}

#[tokio::test]
async fn put_keeps_one_open_result_per_document() {
    let directory = tempdir().expect("temporary directory should be created");
    let database = directory.path().join("state.sqlite");
    let current_fingerprint = current_fingerprint();
    let older_fingerprint = older_fingerprint();
    {
        let connection = Connection::open(&database).expect("database should open directly");
        create_legacy_open_results(&connection);
        insert_open_result(&connection, "doc:put", &older_fingerprint);
        insert_open_result(&connection, "doc:put", &current_fingerprint);
    }

    let store = SqliteOpenResultStore::open(&database).expect("open result store should open");
    store
        .put(&sample_result("doc:put"), &current_fingerprint)
        .await
        .expect("put should succeed");

    let connection = Connection::open(&database).expect("database should reopen directly");
    assert_eq!(
        count(
            &connection,
            "SELECT COUNT(*) FROM open_results_v1 WHERE document_id = ?1",
            "doc:put",
        ),
        1
    );
    let stored_fingerprint: String = connection
        .query_row(
            "SELECT fingerprint FROM open_results_v1 WHERE document_id = ?1",
            params!["doc:put"],
            |row| row.get(0),
        )
        .expect("one result should exist");
    assert_eq!(stored_fingerprint, current_fingerprint);
}
