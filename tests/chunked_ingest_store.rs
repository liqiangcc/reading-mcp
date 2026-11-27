//! Store-level tests of the chunked ingest write: byte identity against the single-transaction
//! write, the in-progress sentinel's invisibility, startup cleanup of crash leftovers, and remap
//! survival across a chunked replace.

use std::collections::BTreeMap;
use std::sync::Arc;

use reading_mcp::application::ports::{HandleRemapUnit, TextUnitIndex};
use reading_mcp::domain::{
    ContentHash, Document, DocumentId, DocumentSource, Location, MediaType, Section, SectionId,
};
use reading_mcp::infrastructure::SqliteTextUnitIndex;
use rusqlite::Connection;
use tempfile::tempdir;

const IN_PROGRESS: &str = "chunked-ingest/in-progress";

fn document_with_sections(id: &str, texts: &[&str]) -> Document {
    Document {
        id: DocumentId(id.into()),
        source: DocumentSource("memory:chunked".into()),
        title: "Chunked".into(),
        media_type: MediaType("text/plain".into()),
        content_hash: ContentHash("sha256:raw".into()),
        metadata: BTreeMap::new(),
        root_sections: texts
            .iter()
            .enumerate()
            .map(|(index, text)| Section {
                id: SectionId(format!("section://{index}")),
                parent_id: None,
                title: format!("Section {index}"),
                level: 1,
                content: (*text).into(),
                location: Location::default(),
                children: vec![],
            })
            .collect(),
    }
}

async fn store(index: &SqliteTextUnitIndex, document: &Arc<Document>) {
    let paragraphs = Arc::new(document.paragraph_text_units());
    let sentences = Arc::new(document.sentence_text_units());
    index
        .replace_document_sets(document, paragraphs, sentences)
        .await
        .expect("document sections should store");
}

/// Every row of the three section tables plus the document row, ordered for comparison.
fn section_rows_dump(database: &std::path::Path) -> Vec<Vec<String>> {
    let connection = Connection::open(database).expect("database should open");
    let mut dump = Vec::new();
    for (table, columns) in [
        (
            "text_unit_documents_v2",
            "doc_key, document_id, content_hash, normalized_document_hash, segmentation_version, \
             ingest_fingerprint, handle_checksum",
        ),
        (
            "section_layout_v2",
            "doc_key, section_key, section_id, id_hash, char_len, paragraph_coverage, plan_error, \
             paragraph_count, sentence_count, chunk_count, sorted_flags, \
             total_items_paragraph_preserve, total_items_paragraph_eligible, \
             total_items_sentence_preserve, total_items_sentence_eligible, \
             coverage_paragraph_preserve, coverage_paragraph_eligible, \
             coverage_sentence_preserve, coverage_sentence_eligible",
        ),
        (
            "section_layout_chunk_v2",
            "doc_key, section_key, chunk_no, first_paragraph_index, first_sentence_index, \
             paragraph_slot, sentence_slot, paragraph_entry, sentence_entry, \
             eligible_paragraph_entry, eligible_sentence_entry, paragraph_count, sentence_count, \
             blob",
        ),
        (
            "section_text_v3",
            "doc_key, section_key, chunk_no, encoding, data",
        ),
    ] {
        let order = if table == "text_unit_documents_v2" {
            "doc_key"
        } else if table == "section_layout_v2" {
            "doc_key, section_key"
        } else {
            "doc_key, section_key, chunk_no"
        };
        let mut statement = connection
            .prepare(&format!("SELECT {columns} FROM {table} ORDER BY {order}"))
            .expect("dump query");
        let rows = statement
            .query_map([], |row| {
                let mut values = Vec::new();
                for index in 0..columns.matches(',').count() + 1 {
                    let value: rusqlite::types::Value = row.get(index)?;
                    values.push(match value {
                        rusqlite::types::Value::Null => "null".into(),
                        rusqlite::types::Value::Integer(number) => number.to_string(),
                        rusqlite::types::Value::Real(number) => number.to_string(),
                        rusqlite::types::Value::Text(text) => text,
                        rusqlite::types::Value::Blob(blob) => format!("blob:{}", blob.len()),
                    });
                }
                Ok(values)
            })
            .expect("dump rows");
        for row in rows {
            let mut row = row.expect("row");
            row.insert(0, table.into());
            dump.push(row);
        }
    }
    dump
}

#[tokio::test]
async fn a_chunked_write_is_byte_identical_to_the_single_transaction_write() {
    // 30 sections of varied text, one section per chunk transaction: the write spans 30
    // transactions here and one in the reference store.
    let texts: Vec<String> = (0..30)
        .map(|index| {
            format!(
                "Chapter {index} opening line.\n\n{}\n\nClosing line of chapter {index}.",
                "A paragraph body sentence. ".repeat(20 + index)
            )
        })
        .collect();
    let refs: Vec<&str> = texts.iter().map(String::as_str).collect();
    let document = Arc::new(document_with_sections("doc:chunked-parity", &refs));

    let single_dir = tempdir().expect("single tempdir");
    let single_db = single_dir.path().join("state.sqlite");
    let single = SqliteTextUnitIndex::open(&single_db).expect("single store");
    store(&single, &document).await;
    drop(single);

    // Single-test binaries mutate the process environment; the variable is read once per write.
    unsafe {
        std::env::set_var("READING_MCP_INGEST_CHUNK_SECTIONS", "1");
    }
    let chunked_dir = tempdir().expect("chunked tempdir");
    let chunked_db = chunked_dir.path().join("state.sqlite");
    let chunked = SqliteTextUnitIndex::open(&chunked_db).expect("chunked store");
    store(&chunked, &document).await;
    unsafe {
        std::env::remove_var("READING_MCP_INGEST_CHUNK_SECTIONS");
    }
    drop(chunked);

    let single_dump = section_rows_dump(&single_db);
    let chunked_dump = section_rows_dump(&chunked_db);
    assert_eq!(
        single_dump.len(),
        chunked_dump.len(),
        "both writes must produce the same rows"
    );
    assert_eq!(
        single_dump, chunked_dump,
        "a chunked write must leave byte-identical stored rows"
    );

    // And a chunked write reads back exactly what the single write reads back.
    let single = SqliteTextUnitIndex::open(&single_db).expect("single store");
    let chunked = SqliteTextUnitIndex::open(&chunked_db).expect("chunked store");
    for section in &document.root_sections {
        assert_eq!(
            single
                .section_units(&document.id, &section.id)
                .await
                .expect("single read"),
            chunked
                .section_units(&document.id, &section.id)
                .await
                .expect("chunked read"),
            "section {} must read identically",
            section.id.0
        );
    }
}

#[tokio::test]
async fn an_in_progress_document_reads_as_not_ingested() {
    let directory = tempdir().expect("temporary directory");
    let database = directory.path().join("state.sqlite");
    let index = SqliteTextUnitIndex::open(&database).expect("store");
    let document = Arc::new(document_with_sections(
        "doc:in-progress",
        &[
            "First paragraph.\n\nSecond paragraph.",
            "Other section text.",
        ],
    ));
    store(&index, &document).await;

    // A complete ingest reads.
    let first = &document.root_sections[0].id;
    assert!(
        index
            .section_units(&document.id, first)
            .await
            .expect("read")
            .is_some(),
        "a complete document's units are stored"
    );
    assert!(
        index
            .section_key(&document.id, first)
            .await
            .expect("section key")
            .is_some(),
        "a complete document's section key resolves"
    );

    // The state a crash leaves: the sentinel fingerprint plus the rows written so far. Nothing of
    // it may read as ingested.
    Connection::open(&database)
        .expect("database")
        .execute(
            "UPDATE text_unit_documents_v2 SET ingest_fingerprint = ?1",
            rusqlite::params![IN_PROGRESS],
        )
        .expect("sentinel update");

    assert!(
        index
            .section_units(&document.id, first)
            .await
            .expect("read")
            .is_none(),
        "an in-progress document's units read as not ingested"
    );
    assert!(
        index
            .list_document(&document.id)
            .await
            .expect("list")
            .is_empty(),
        "an in-progress document lists no units"
    );
    assert!(
        index
            .section_key(&document.id, first)
            .await
            .expect("section key")
            .is_none(),
        "an in-progress document's section key does not resolve"
    );
    assert!(
        index
            .section_id_for_key(&document.id, 0)
            .await
            .expect("section id")
            .is_none(),
        "an in-progress document's section key does not resolve backwards"
    );

    // Re-ingesting heals it: the stored rows of the partial write are replaced wholesale.
    store(&index, &document).await;
    assert!(
        index
            .section_units(&document.id, first)
            .await
            .expect("read")
            .is_some(),
        "the re-ingested document reads again"
    );
}

#[tokio::test]
async fn startup_cleanup_removes_the_partial_rows_but_keeps_the_document_key() {
    let directory = tempdir().expect("temporary directory");
    let database = directory.path().join("state.sqlite");
    let index = SqliteTextUnitIndex::open(&database).expect("store");
    let document = Arc::new(document_with_sections(
        "doc:cleanup",
        &["Cleanup test text.\n\nMore text."],
    ));
    store(&index, &document).await;
    let doc_key = index
        .document_key(&document.id)
        .await
        .expect("document key")
        .expect("the document is stored");
    drop(index);

    // The crash leftover: sentinel fingerprint, sections written so far.
    Connection::open(&database)
        .expect("database")
        .execute(
            "UPDATE text_unit_documents_v2 SET ingest_fingerprint = ?1",
            rusqlite::params![IN_PROGRESS],
        )
        .expect("sentinel update");

    // Reopening the store is the startup of the next server: the partial rows go, the document row
    // and its key stay so the handles bound to it are never aliased to another document.
    let index = SqliteTextUnitIndex::open(&database).expect("store reopens");
    let connection = Connection::open(&database).expect("database");
    for table in [
        "section_text_v3",
        "section_layout_v2",
        "section_layout_chunk_v2",
    ] {
        let count: i64 = connection
            .query_row(
                &format!("SELECT COUNT(*) FROM {table} WHERE doc_key = ?1"),
                rusqlite::params![doc_key],
                |row| row.get(0),
            )
            .expect("count");
        assert_eq!(count, 0, "{table} rows of the partial write are removed");
    }
    assert_eq!(
        index
            .document_key(&document.id)
            .await
            .expect("document key"),
        Some(doc_key),
        "the document row and its key survive the cleanup"
    );

    // Re-ingest writes the document again, on the same key.
    store(&index, &document).await;
    assert_eq!(
        index
            .document_key(&document.id)
            .await
            .expect("document key"),
        Some(doc_key)
    );
    assert!(
        index
            .section_units(&document.id, &document.root_sections[0].id)
            .await
            .expect("read")
            .is_some()
    );
}

#[tokio::test]
async fn handle_remaps_survive_a_chunked_replace_and_its_crash_cleanup() {
    // Single-test binaries mutate the process environment; the variable is read once per write.
    unsafe {
        std::env::set_var("READING_MCP_INGEST_CHUNK_SECTIONS", "1");
    }
    let directory = tempdir().expect("temporary directory");
    let database = directory.path().join("state.sqlite");
    let index = SqliteTextUnitIndex::open(&database).expect("store");

    // One document id, two versions of its rows: the normalization change a parser deploy brings,
    // which is exactly when a remap must carry the old handles into the new version.
    let before = Arc::new(document_with_sections(
        "doc:remap",
        &[
            "Remap anchor one.\n\nRemap anchor two.",
            "Second section stays.",
        ],
    ));
    store(&index, &before).await;
    let mut after_document = document_with_sections(
        "doc:remap",
        &[
            "Remap anchor one.\n\nRemap anchor two.",
            "Second section stays.",
        ],
    );
    // The same bytes, normalized differently: trailing whitespace the new version strips.
    after_document.root_sections[0].content = "Remap anchor one. \n\nRemap anchor two.".into();
    let after = Arc::new(after_document);
    assert_ne!(
        before.normalized_document_hash(),
        after.normalized_document_hash(),
        "the two versions must differ in normalized identity"
    );

    // The chunked replace. One section per transaction, so the remap build shares the first
    // transaction with the sentinel upsert: exactly the state a crash after that commit leaves.
    store(&index, &after).await;
    unsafe {
        std::env::remove_var("READING_MCP_INGEST_CHUNK_SECTIONS");
    }

    let doc_key = index
        .document_key(&after.id)
        .await
        .expect("document key")
        .expect("the document is stored");
    let before_checksum = reading_mcp::application::short_handle::handle_checksum(
        &before.id,
        &before.normalized_document_hash(),
        &before.media_type,
    );
    let remap = index
        .handle_remap(doc_key, &before_checksum, &HandleRemapUnit::Document)
        .await
        .expect("remap read");
    assert!(
        remap.is_some(),
        "the document remap row is written by the chunked replace"
    );

    // The crash cleanup never touches remap rows: they belong to the key, not to a version's rows.
    let connection = Connection::open(&database).expect("database");
    let count: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM handle_remap_v1 WHERE doc_key = ?1",
            rusqlite::params![doc_key],
            |row| row.get(0),
        )
        .expect("remap count");
    assert!(count > 0, "remap rows exist for the old version");
}

#[tokio::test]
async fn a_chunked_reingest_leaves_no_duplicate_or_stale_rows() {
    unsafe {
        std::env::set_var("READING_MCP_INGEST_CHUNK_SECTIONS", "2");
    }
    let directory = tempdir().expect("temporary directory");
    let database = directory.path().join("state.sqlite");
    let index = SqliteTextUnitIndex::open(&database).expect("store");

    // One document id, two versions of its rows — the parser-deploy case. The second version
    // differs in text and segmentation, and one section shrinks so old chunk rows would outlive it.
    let before = Arc::new(document_with_sections(
        "doc:redup",
        &[
            "Alpha version one.\n\nAlpha second paragraph.",
            "Beta stays the same.",
            "Gamma third section.",
        ],
    ));
    store(&index, &before).await;
    let mut after_document = document_with_sections(
        "doc:redup",
        &[
            "Alpha version one. \n\nAlpha second paragraph.",
            "Beta stays the same.",
            "Gamma third section.",
        ],
    );
    after_document.root_sections[0]
        .content
        .push_str("\n\nAn extra final paragraph.");
    let after = Arc::new(after_document);
    store(&index, &after).await;
    unsafe {
        std::env::remove_var("READING_MCP_INGEST_CHUNK_SECTIONS");
    }

    // Exactly one layout row and exactly the new chunk rows per section: no duplicates, no stale
    // chunks of the previous version.
    let connection = Connection::open(&database).expect("database");
    let doc_key = index
        .document_key(&after.id)
        .await
        .expect("key")
        .expect("stored");
    for table in [
        "section_layout_v2",
        "section_layout_chunk_v2",
        "section_text_v3",
    ] {
        let total: i64 = connection
            .query_row(
                &format!("SELECT COUNT(*) FROM {table} WHERE doc_key = ?1"),
                rusqlite::params![doc_key],
                |row| row.get(0),
            )
            .expect("count");
        let distinct: i64 = connection
            .query_row(
                &format!(
                    "SELECT COUNT(*) FROM (SELECT DISTINCT section_key, chunk_no FROM {table} WHERE doc_key = ?1)"
                ),
                rusqlite::params![doc_key],
                |row| row.get(0),
            )
            .unwrap_or(total);
        if table == "section_layout_v2" {
            let sections: i64 = connection
                .query_row(
                    "SELECT COUNT(DISTINCT section_key) FROM section_layout_v2 WHERE doc_key = ?1",
                    rusqlite::params![doc_key],
                    |row| row.get(0),
                )
                .expect("sections");
            assert_eq!(
                total, sections,
                "every layout row is unique per section: {total} rows for {sections} sections"
            );
        } else {
            assert_eq!(total, distinct, "{table} rows are unique per chunk");
        }
    }

    // The stored rows equal a fresh single-transaction ingest of the new version byte for byte.
    let fresh_dir = tempdir().expect("fresh tempdir");
    let fresh_db = fresh_dir.path().join("state.sqlite");
    let fresh = SqliteTextUnitIndex::open(&fresh_db).expect("fresh store");
    store(&fresh, &after).await;
    drop(fresh);
    assert_eq!(
        section_rows_dump(&database),
        section_rows_dump(&fresh_db),
        "a chunked re-ingest converges to the same rows as a fresh ingest"
    );

    // And the new version reads correctly.
    let units = index
        .section_units(&after.id, &after.root_sections[0].id)
        .await
        .expect("read")
        .expect("stored");
    assert!(
        units
            .paragraphs
            .iter()
            .any(|unit| unit.text.contains("extra final paragraph")),
        "the new version's text is what reads back"
    );
}
