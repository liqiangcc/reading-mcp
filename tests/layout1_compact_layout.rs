//! LAYOUT-1: the compact `section_layout_v2` row.
//!
//! The header keeps its paragraph coverage and its four stream coverages as varint blobs instead of
//! JSON objects, no longer repeats the section id inside the paragraph coverage, and looks a section
//! up by a 64-bit hash of its id. These tests pin the three things a reader can observe:
//!
//! - the stored blobs decode to exactly the coverage the planner reports, and cost a fraction of the
//!   JSON they replaced;
//! - a database written by the previous store version (`text-unit-store/v11`) is dropped and
//!   rebuilt on open, reads as "not stored" until its document re-ingests, and then answers every
//!   read identically to a store that was never migrated;
//! - every statement the store runs against either layout table still seeks an index, with the
//!   index named here, so no read turns into a scan.

use std::collections::BTreeMap;

use reading_mcp::application::get_text_units::{RequestedTextUnitKind, TextUnitCoveragePolicy};
use reading_mcp::application::ports::{
    Parser, RetrievedResource, TextUnitIndex, TextUnitStreamKey,
};
use reading_mcp::application::text_unit_store::store_text_units;
use reading_mcp::domain::{Document, DocumentSource, MediaType, SectionId};
use reading_mcp::infrastructure::{InMemoryTextUnitIndex, SqliteTextUnitIndex};
use reading_mcp::parsing::MarkdownParser;
use rusqlite::types::Value;

const MARKDOWN: &str = "# Alpha\n\nAlpha first. Alpha second. Alpha third.\n\n- List item one. List item two.\n\n> Quote alpha. Quote beta.\n\n# Beta\n\nBeta first. Beta second.\n\n```text\ncode line one. code line two.\n```\n\n# Gamma\n\nGamma one. Gamma two. Gamma three.\n";

/// The `section_layout_v2` and `section_layout_chunk_v2` tables exactly as `text-unit-store/v11`
/// created them: JSON coverages, no `id_hash`, and a UNIQUE index over the long section ids.
const V11_LAYOUT_TABLES: &str = "
    DROP TABLE IF EXISTS section_layout_chunk_v2;
    DROP TABLE IF EXISTS section_layout_v2;
    CREATE TABLE section_layout_v2 (
        doc_key INTEGER NOT NULL,
        section_key INTEGER NOT NULL,
        section_id TEXT NOT NULL,
        char_len INTEGER NOT NULL,
        paragraph_coverage_json TEXT,
        plan_error TEXT,
        paragraph_count INTEGER NOT NULL,
        sentence_count INTEGER NOT NULL,
        chunk_count INTEGER NOT NULL,
        sorted_flags INTEGER NOT NULL,
        total_items_paragraph_preserve INTEGER NOT NULL,
        total_items_paragraph_eligible INTEGER NOT NULL,
        total_items_sentence_preserve INTEGER NOT NULL,
        total_items_sentence_eligible INTEGER NOT NULL,
        coverage_paragraph_preserve_json TEXT NOT NULL,
        coverage_paragraph_eligible_json TEXT NOT NULL,
        coverage_sentence_preserve_json TEXT NOT NULL,
        coverage_sentence_eligible_json TEXT NOT NULL,
        UNIQUE(doc_key, section_id)
    );
    CREATE INDEX idx_section_layout_v2_section_key
        ON section_layout_v2(doc_key, section_key);
    CREATE TABLE section_layout_chunk_v2 (
        doc_key INTEGER NOT NULL,
        section_key INTEGER NOT NULL,
        chunk_no INTEGER NOT NULL,
        first_paragraph_index INTEGER NOT NULL,
        first_sentence_index INTEGER NOT NULL,
        paragraph_slot INTEGER NOT NULL,
        sentence_slot INTEGER NOT NULL,
        paragraph_entry INTEGER NOT NULL,
        sentence_entry INTEGER NOT NULL,
        eligible_paragraph_entry INTEGER NOT NULL,
        eligible_sentence_entry INTEGER NOT NULL,
        paragraph_count INTEGER NOT NULL,
        sentence_count INTEGER NOT NULL,
        blob BLOB NOT NULL,
        UNIQUE(doc_key, section_key, chunk_no)
    );
    CREATE INDEX idx_section_layout_chunk_v2_paragraph_entry
        ON section_layout_chunk_v2(doc_key, section_key, paragraph_entry);
    CREATE INDEX idx_section_layout_chunk_v2_sentence_entry
        ON section_layout_chunk_v2(doc_key, section_key, sentence_entry);
    CREATE INDEX idx_section_layout_chunk_v2_eligible_paragraph_entry
        ON section_layout_chunk_v2(doc_key, section_key, eligible_paragraph_entry);
    CREATE INDEX idx_section_layout_chunk_v2_eligible_sentence_entry
        ON section_layout_chunk_v2(doc_key, section_key, eligible_sentence_entry);
    CREATE INDEX idx_section_layout_chunk_v2_first
        ON section_layout_chunk_v2(
            doc_key, section_key, first_paragraph_index, first_sentence_index
        );";

/// One JSON stream coverage, as `text-unit-store/v11` stored it.
const V11_STREAM_COVERAGE_JSON: &str = r#"{"owner_chars":0,"section_separator_chars":0,"sentence_separator_chars":0,"paragraph_count":0,"sentence_eligible_paragraphs":0,"non_prose_paragraphs":0,"coarse_structural_paragraphs":0,"represented_paragraphs":0,"represented_sentences":0,"coarse_non_prose_items":0,"coarse_structural_items":0,"intentionally_skipped":0,"unsupported_gaps":0,"source_complete":true}"#;

/// One `section_layout_v2` row's stored coverages, as the compact blobs they are.
struct StoredCoverage {
    section_id: String,
    paragraph_coverage: Option<Vec<u8>>,
    paragraph_preserve: Vec<u8>,
    paragraph_eligible: Vec<u8>,
    sentence_preserve: Vec<u8>,
    sentence_eligible: Vec<u8>,
}

#[tokio::test]
async fn the_stored_coverages_decode_to_the_planner_coverage_and_cost_a_fraction_of_the_json() {
    let directory = tempfile::tempdir().expect("temporary directory should be created");
    let database = directory.path().join("state.sqlite");
    let index = SqliteTextUnitIndex::open(&database).expect("index should open");
    let memory = InMemoryTextUnitIndex::default();
    let document = parse_markdown().await;
    store_text_units(&index, &document)
        .await
        .expect("units should persist");
    store_text_units(&memory, &document)
        .await
        .expect("units should persist in memory");

    let connection = rusqlite::Connection::open(&database).expect("database should open");
    let doc_key: i64 = connection
        .query_row(
            "SELECT doc_key FROM text_unit_documents_v2 WHERE document_id = ?1",
            rusqlite::params![&document.id.0],
            |row| row.get(0),
        )
        .expect("doc_key should exist");
    let rows = connection
        .prepare(
            "SELECT section_id, paragraph_coverage, coverage_paragraph_preserve,
                    coverage_paragraph_eligible, coverage_sentence_preserve,
                    coverage_sentence_eligible
               FROM section_layout_v2
              WHERE doc_key = ?1
              ORDER BY section_key",
        )
        .expect("statement should prepare")
        .query_map(rusqlite::params![doc_key], |row| {
            Ok(StoredCoverage {
                section_id: row.get(0)?,
                paragraph_coverage: row.get(1)?,
                paragraph_preserve: row.get(2)?,
                paragraph_eligible: row.get(3)?,
                sentence_preserve: row.get(4)?,
                sentence_eligible: row.get(5)?,
            })
        })
        .expect("rows should read")
        .collect::<Result<Vec<_>, _>>()
        .expect("rows should decode");
    drop(connection);
    assert!(!rows.is_empty(), "the document must have layout rows");

    for row in &rows {
        // A section without coverage has no row value; the ones that have one keep a version byte, a
        // flag and eight varints. The owner id those coverages repeat is the row's own `section_id`,
        // so it is not written: a coverage that carried it would be longer than this bound by the
        // id's length alone. `section_layout`'s unit tests pin the elision itself.
        if let Some(paragraph_coverage) = row.paragraph_coverage.as_ref() {
            assert!(
                paragraph_coverage.len() <= 34,
                "the paragraph coverage of {} is a version byte, a flag and eight varints: {} bytes",
                row.section_id,
                paragraph_coverage.len()
            );
        }
        for (name, blob) in [
            ("paragraph preserve", &row.paragraph_preserve),
            ("paragraph eligible", &row.paragraph_eligible),
            ("sentence preserve", &row.sentence_preserve),
            ("sentence eligible", &row.sentence_eligible),
        ] {
            assert!(
                blob.len() <= 24,
                "the {name} coverage of {} is a version byte and varints: {} bytes",
                row.section_id,
                blob.len()
            );
        }
    }

    // The bytes above are the ones every read decodes: each summary equals the in-memory planner's,
    // coverage included.
    assert_eq!(
        summaries(&index, &document).await,
        summaries(&memory, &document).await,
        "the stored coverages must decode to the planner's"
    );
}

#[tokio::test]
async fn a_v11_layout_store_is_dropped_on_open_and_answers_identically_after_re_ingest() {
    let directory = tempfile::tempdir().expect("temporary directory should be created");
    let database = directory.path().join("state.sqlite");
    let document = parse_markdown().await;

    // The store as this version writes it, for the answers every read must keep.
    let index = SqliteTextUnitIndex::open(&database).expect("index should open");
    store_text_units(&index, &document)
        .await
        .expect("units should persist");
    let expected_layouts = index
        .stored_section_layouts(&document.id)
        .await
        .expect("layouts should load");
    let expected_summaries = summaries(&index, &document).await;
    drop(index);

    // Rewrite the layout tables as `text-unit-store/v11` wrote them, with a row of the old shape
    // that the store must not read.
    let connection = rusqlite::Connection::open(&database).expect("database should open");
    connection
        .execute_batch(V11_LAYOUT_TABLES)
        .expect("v11 tables should replace the v12 ones");
    let doc_key: i64 = connection
        .query_row(
            "SELECT doc_key FROM text_unit_documents_v2 WHERE document_id = ?1",
            rusqlite::params![&document.id.0],
            |row| row.get(0),
        )
        .expect("doc_key should exist");
    connection
        .execute(
            "INSERT INTO section_layout_v2(
                doc_key, section_key, section_id, char_len, paragraph_coverage_json, plan_error,
                paragraph_count, sentence_count, chunk_count, sorted_flags,
                total_items_paragraph_preserve, total_items_paragraph_eligible,
                total_items_sentence_preserve, total_items_sentence_eligible,
                coverage_paragraph_preserve_json, coverage_paragraph_eligible_json,
                coverage_sentence_preserve_json, coverage_sentence_eligible_json)
             VALUES (?1, 0, 'section://v11/root', 0, NULL, NULL, 0, 0, 0, 0,
                     0, 0, 0, 0, ?2, ?2, ?2, ?2)",
            rusqlite::params![doc_key, V11_STREAM_COVERAGE_JSON],
        )
        .expect("a v11 header row should insert");
    connection
        .execute(
            "INSERT INTO section_layout_chunk_v2(
                doc_key, section_key, chunk_no, first_paragraph_index, first_sentence_index,
                paragraph_slot, sentence_slot, paragraph_entry, sentence_entry,
                eligible_paragraph_entry, eligible_sentence_entry, paragraph_count,
                sentence_count, blob)
             VALUES (?1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, x'01')",
            rusqlite::params![doc_key],
        )
        .expect("a v11 chunk row should insert");
    drop(connection);

    // Opening drops both tables and builds the compact shape; the v11 rows are gone.
    let index = SqliteTextUnitIndex::open(&database).expect("an old database must open");
    let connection = rusqlite::Connection::open(&database).expect("database should open");
    let columns: Vec<String> = connection
        .prepare("PRAGMA table_info(section_layout_v2)")
        .expect("statement should prepare")
        .query_map([], |row| row.get::<_, String>(1))
        .expect("columns should read")
        .collect::<Result<Vec<_>, _>>()
        .expect("columns should decode");
    assert!(
        columns.iter().any(|column| column == "id_hash"),
        "the compact header must have id_hash: {columns:?}"
    );
    assert!(
        columns.iter().any(|column| column == "paragraph_coverage"),
        "the compact header must store the coverage as a blob: {columns:?}"
    );
    assert!(
        !columns
            .iter()
            .any(|column| column == "paragraph_coverage_json"),
        "the JSON column must be gone: {columns:?}"
    );
    for table in ["section_layout_v2", "section_layout_chunk_v2"] {
        let rows: i64 = connection
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .expect("row count should read");
        assert_eq!(rows, 0, "{table} still holds rows of the previous version");
    }
    drop(connection);

    // Nothing is stored for the document until it re-ingests, which is what makes the caller open it
    // again instead of answering from rows of another layout version.
    let section_id = section_ids(&document)[0].clone();
    let stale_key = TextUnitStreamKey {
        document_id: &document.id,
        section_id: &section_id,
        kind: RequestedTextUnitKind::Sentence,
        policy: TextUnitCoveragePolicy::PreserveSource,
    };
    assert!(
        index
            .section_stream_summary(stale_key)
            .await
            .expect("summary lookup should succeed")
            .is_none(),
        "a dropped layout must read as not stored"
    );
    assert!(
        index
            .stored_section_layouts(&document.id)
            .await
            .expect("layouts should load")
            .is_empty(),
        "a dropped layout must have no rows"
    );

    // Re-ingesting the same document answers exactly what the never-migrated store answered.
    store_text_units(&index, &document)
        .await
        .expect("units should re-ingest");
    assert_eq!(
        index
            .stored_section_layouts(&document.id)
            .await
            .expect("layouts should load"),
        expected_layouts,
        "the re-ingested layouts must be byte-identical"
    );
    assert_eq!(
        summaries(&index, &document).await,
        expected_summaries,
        "every stream summary must be identical after the migration"
    );
}

/// One layout statement, the values its placeholders are bound to, and the indexes it may seek.
///
/// A statement may have more than one acceptable index: the planner picks between two covering
/// indexes over `doc_key` by statistics it does not have, and either seek is one row.
struct PlanCase {
    name: &'static str,
    sql: String,
    values: Vec<Value>,
    expected_indexes: &'static [&'static str],
}

/// Every statement the store runs against `section_layout_v2` or `section_layout_chunk_v2`, with the
/// index it must seek. A read that loses its index becomes a scan of the document, so this pins the
/// O(page) claim the layout's design rests on.
#[tokio::test]
async fn every_layout_read_query_seeks_an_index() {
    let directory = tempfile::tempdir().expect("temporary directory should be created");
    let database = directory.path().join("state.sqlite");
    let index = SqliteTextUnitIndex::open(&database).expect("index should open");
    let document = parse_markdown().await;
    store_text_units(&index, &document)
        .await
        .expect("units should persist");
    drop(index);

    let connection = rusqlite::Connection::open(&database).expect("database should open");
    let doc_key: i64 = connection
        .query_row(
            "SELECT doc_key FROM text_unit_documents_v2 WHERE document_id = ?1",
            rusqlite::params![&document.id.0],
            |row| row.get(0),
        )
        .expect("doc_key should exist");
    let (section_key, section_id): (i64, String) = connection
        .query_row(
            "SELECT section_key, section_id FROM section_layout_v2
              WHERE doc_key = ?1 ORDER BY section_key LIMIT 1",
            rusqlite::params![doc_key],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("a section should exist");
    let id_hash = {
        // The same FNV-1a 64 the store hashes section ids with, so the plan is the one a real
        // lookup produces.
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for byte in section_id.as_bytes() {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        hash as i64
    };
    let chunk_columns = "chunk_no, first_paragraph_index, first_sentence_index, paragraph_slot,
            sentence_slot, paragraph_entry, sentence_entry, eligible_paragraph_entry,
            eligible_sentence_entry, paragraph_count, sentence_count, blob";
    let header_columns = "section_key, char_len, paragraph_coverage, plan_error,
            paragraph_count, sentence_count, chunk_count,
            total_items_paragraph_preserve, total_items_paragraph_eligible,
            total_items_sentence_preserve, total_items_sentence_eligible,
            coverage_paragraph_preserve, coverage_paragraph_eligible,
            coverage_sentence_preserve, coverage_sentence_eligible";

    let statements: Vec<PlanCase> = vec![
        PlanCase {
            name: "char_len by key",
            sql: "SELECT char_len FROM section_layout_v2 WHERE doc_key = ?1 AND section_key = ?2"
                .to_string(),
            values: vec![Value::Integer(doc_key), Value::Integer(section_key)],
            expected_indexes: &["idx_section_layout_v2_section_key"],
        },
        PlanCase {
            name: "char_lens by document",
            sql: "SELECT char_len FROM section_layout_v2 WHERE doc_key = ?1 ORDER BY section_key"
                .to_string(),
            values: vec![Value::Integer(doc_key)],
            expected_indexes: &["idx_section_layout_v2_section_key"],
        },
        PlanCase {
            name: "stored section keys",
            sql: "SELECT l.section_id, l.section_key FROM section_layout_v2 l
              WHERE l.doc_key = ?1 ORDER BY l.section_key"
                .to_string(),
            values: vec![Value::Integer(doc_key)],
            expected_indexes: &["idx_section_layout_v2_section_key"],
        },
        PlanCase {
            name: "stored sections",
            sql: "SELECT section_key, section_id, char_len FROM section_layout_v2
              WHERE doc_key = ?1 ORDER BY section_key ASC"
                .to_string(),
            values: vec![Value::Integer(doc_key)],
            expected_indexes: &["idx_section_layout_v2_section_key"],
        },
        PlanCase {
            name: "document exists with a layout",
            sql: "SELECT doc_key FROM text_unit_documents_v2 WHERE document_id = ?1
               AND EXISTS (SELECT 1 FROM section_layout_v2
                            WHERE section_layout_v2.doc_key = text_unit_documents_v2.doc_key)"
                .to_string(),
            values: vec![Value::Text(document.id.0.clone())],
            expected_indexes: &[
                "idx_section_layout_v2_section_key",
                "idx_section_layout_v2_id",
            ],
        },
        PlanCase {
            name: "header by section id",
            sql: format!(
                "SELECT section_id, {header_columns} FROM section_layout_v2
                  WHERE doc_key = ?1 AND id_hash = ?2"
            ),
            values: vec![Value::Integer(doc_key), Value::Integer(id_hash)],
            expected_indexes: &["idx_section_layout_v2_id"],
        },
        PlanCase {
            name: "section keys and ids",
            sql: "SELECT section_key, section_id FROM section_layout_v2
              WHERE doc_key = ?1 ORDER BY section_key ASC"
                .to_string(),
            values: vec![Value::Integer(doc_key)],
            expected_indexes: &["idx_section_layout_v2_section_key"],
        },
        PlanCase {
            name: "section key of id",
            sql: "SELECT l.section_id, l.section_key FROM section_layout_v2 l
               JOIN text_unit_documents_v2 d ON d.doc_key = l.doc_key
              WHERE d.document_id = ?1 AND l.id_hash = ?2"
                .to_string(),
            values: vec![Value::Text(document.id.0.clone()), Value::Integer(id_hash)],
            expected_indexes: &["idx_section_layout_v2_id"],
        },
        PlanCase {
            name: "section id of key",
            sql: "SELECT l.section_id FROM section_layout_v2 l
               JOIN text_unit_documents_v2 d ON d.doc_key = l.doc_key
              WHERE d.document_id = ?1 AND l.section_key = ?2"
                .to_string(),
            values: vec![
                Value::Text(document.id.0.clone()),
                Value::Integer(section_key),
            ],
            expected_indexes: &["idx_section_layout_v2_section_key"],
        },
        PlanCase {
            name: "chunk at paragraph",
            sql: format!(
                "SELECT {chunk_columns} FROM section_layout_chunk_v2
                  WHERE doc_key = ?1 AND section_key = ?2
                    AND first_paragraph_index <= ?3 AND paragraph_count > 0
                  ORDER BY first_paragraph_index DESC, chunk_no DESC LIMIT 1"
            ),
            values: vec![
                Value::Integer(doc_key),
                Value::Integer(section_key),
                Value::Integer(1),
            ],
            expected_indexes: &["idx_section_layout_chunk_v2_first"],
        },
        PlanCase {
            name: "chunk at sentence",
            sql: format!(
                "SELECT {chunk_columns} FROM section_layout_chunk_v2
                  WHERE doc_key = ?1 AND section_key = ?2 AND first_paragraph_index <= ?3
                    AND (first_paragraph_index < ?3 OR first_sentence_index <= ?4)
                  ORDER BY first_paragraph_index DESC, first_sentence_index DESC, chunk_no DESC
                  LIMIT 1"
            ),
            values: vec![
                Value::Integer(doc_key),
                Value::Integer(section_key),
                Value::Integer(1),
                Value::Integer(1),
            ],
            expected_indexes: &["idx_section_layout_chunk_v2_first"],
        },
        PlanCase {
            name: "chunks from",
            sql: format!(
                "SELECT {chunk_columns} FROM section_layout_chunk_v2
                  WHERE doc_key = ?1 AND section_key = ?2 AND chunk_no >= ?3
                  ORDER BY chunk_no ASC"
            ),
            values: vec![
                Value::Integer(doc_key),
                Value::Integer(section_key),
                Value::Integer(0),
            ],
            expected_indexes: &["sqlite_autoindex_section_layout_chunk_v2_1"],
        },
        PlanCase {
            name: "chunk at no",
            sql: format!(
                "SELECT {chunk_columns} FROM section_layout_chunk_v2
                  WHERE doc_key = ?1 AND section_key = ?2 AND chunk_no = ?3"
            ),
            values: vec![
                Value::Integer(doc_key),
                Value::Integer(section_key),
                Value::Integer(0),
            ],
            expected_indexes: &["sqlite_autoindex_section_layout_chunk_v2_1"],
        },
        PlanCase {
            name: "stream position paragraph preserve",
            sql: format!(
                "SELECT {chunk_columns} FROM section_layout_chunk_v2
                  WHERE doc_key = ?1 AND section_key = ?2
                    AND paragraph_entry <= ?3 AND paragraph_count > 0
                  ORDER BY paragraph_entry DESC, chunk_no DESC LIMIT 1"
            ),
            values: vec![
                Value::Integer(doc_key),
                Value::Integer(section_key),
                Value::Integer(0),
            ],
            expected_indexes: &["idx_section_layout_chunk_v2_paragraph_entry"],
        },
        PlanCase {
            name: "stream position paragraph eligible",
            sql: format!(
                "SELECT {chunk_columns} FROM section_layout_chunk_v2
                  WHERE doc_key = ?1 AND section_key = ?2
                    AND eligible_paragraph_entry <= ?3 AND paragraph_count > 0
                  ORDER BY eligible_paragraph_entry DESC, chunk_no DESC LIMIT 1"
            ),
            values: vec![
                Value::Integer(doc_key),
                Value::Integer(section_key),
                Value::Integer(0),
            ],
            expected_indexes: &["idx_section_layout_chunk_v2_eligible_paragraph_entry"],
        },
        PlanCase {
            name: "stream position sentence preserve",
            sql: format!(
                "SELECT {chunk_columns} FROM section_layout_chunk_v2
                  WHERE doc_key = ?1 AND section_key = ?2
                    AND sentence_entry <= ?3
                    AND (sentence_count > 0 OR paragraph_count > 0)
                  ORDER BY sentence_entry DESC, chunk_no DESC LIMIT 1"
            ),
            values: vec![
                Value::Integer(doc_key),
                Value::Integer(section_key),
                Value::Integer(0),
            ],
            expected_indexes: &["idx_section_layout_chunk_v2_sentence_entry"],
        },
        PlanCase {
            name: "stream position sentence eligible",
            sql: format!(
                "SELECT {chunk_columns} FROM section_layout_chunk_v2
                  WHERE doc_key = ?1 AND section_key = ?2
                    AND eligible_sentence_entry <= ?3
                    AND (sentence_count > 0 OR paragraph_count > 0)
                  ORDER BY eligible_sentence_entry DESC, chunk_no DESC LIMIT 1"
            ),
            values: vec![
                Value::Integer(doc_key),
                Value::Integer(section_key),
                Value::Integer(0),
            ],
            expected_indexes: &["idx_section_layout_chunk_v2_eligible_sentence_entry"],
        },
    ];

    for case in statements {
        let plan = connection
            .prepare(&format!("EXPLAIN QUERY PLAN {}", case.sql))
            .expect("statement should prepare")
            .query_map(rusqlite::params_from_iter(case.values), |row| {
                row.get::<_, String>(3)
            })
            .expect("plan should read")
            .collect::<Result<Vec<_>, _>>()
            .expect("plan should decode");
        let details = plan.join(" | ");
        assert!(
            case.expected_indexes
                .iter()
                .any(|index| plan.iter().any(|detail| detail.contains(index))),
            "{} does not use {}: {details}",
            case.name,
            case.expected_indexes.join(" or ")
        );
        assert!(
            !details.contains("SCAN section_layout"),
            "{} scans a layout table: {details}",
            case.name
        );
    }
}

async fn summaries(index: &dyn TextUnitIndex, document: &Document) -> Vec<(String, usize, String)> {
    let mut summaries = Vec::new();
    for section_id in section_ids(document) {
        for kind in [
            RequestedTextUnitKind::Paragraph,
            RequestedTextUnitKind::Sentence,
        ] {
            for policy in [
                TextUnitCoveragePolicy::PreserveSource,
                TextUnitCoveragePolicy::EligibleOnly,
            ] {
                let key = TextUnitStreamKey {
                    document_id: &document.id,
                    section_id: &section_id,
                    kind,
                    policy,
                };
                let summary = index
                    .section_stream_summary(key)
                    .await
                    .expect("summary should load")
                    .expect("the stream should be stored");
                summaries.push((
                    format!("{} {kind:?} {policy:?}", section_id.0),
                    summary.total_items,
                    format!("{:?}", summary.coverage),
                ));
            }
        }
    }
    summaries
}

/// Every section id of the document, in depth-first order.
fn section_ids(document: &Document) -> Vec<SectionId> {
    fn collect(roots: &[reading_mcp::domain::Section], ids: &mut Vec<SectionId>) {
        for section in roots {
            ids.push(section.id.clone());
            collect(&section.children, ids);
        }
    }
    let mut ids = Vec::new();
    collect(&document.root_sections, &mut ids);
    ids
}

async fn parse_markdown() -> Document {
    MarkdownParser
        .parse(RetrievedResource {
            source: DocumentSource("memory:sections.md".into()),
            final_source: DocumentSource("memory:sections.md".into()),
            media_type: MediaType("text/markdown".into()),
            bytes: MARKDOWN.as_bytes().to_vec(),
            etag: None,
            last_modified: None,
            metadata: BTreeMap::new(),
        })
        .await
        .expect("markdown should parse")
}
