//! DISK-1: `section_text_v3` stores each chunk as its raw UTF-8 bytes or as zlib (RFC 1950), and
//! every read path — the whole document, one section, one slice, the scan — decodes it back to
//! exactly the text that was saved.

use std::collections::BTreeMap;
use std::path::Path;

use reading_mcp::application::get_text_units::EffectiveTextUnitKind;
use reading_mcp::application::ports::{
    ApplicationError, DocumentRepository, OwnedStreamUnit, SearchIndex, TextUnitIndex,
};
use reading_mcp::application::text_unit_store::store_text_units;
use reading_mcp::domain::{
    ContentHash, Document, DocumentId, DocumentSource, Location, MediaType, Section, SectionId,
};
use reading_mcp::infrastructure::{
    ScanOptions, SqliteDocumentRepository, SqliteSearchIndex, SqliteTextUnitIndex,
};
use rusqlite::{Connection, params};
use tempfile::tempdir;

/// `CHUNK_CHARS` of `src/infrastructure/section_layout.rs`: the number of Unicode scalars one
/// stored text chunk holds. It is crate-private, so the tests pin the same number.
const CHUNK_CHARS: usize = 16_384;

fn document(id: &str, sections: &[(&str, String)]) -> Document {
    Document {
        id: DocumentId(id.into()),
        source: DocumentSource("memory:textzip".into()),
        title: "Text zip".into(),
        media_type: MediaType("text/plain".into()),
        content_hash: ContentHash(format!("sha256:{id}")),
        metadata: BTreeMap::new(),
        root_sections: sections
            .iter()
            .map(|(section_id, content)| Section {
                id: SectionId((*section_id).into()),
                parent_id: None,
                title: (*section_id).into(),
                level: 1,
                content: content.clone(),
                location: Location::default(),
                children: vec![],
            })
            .collect(),
    }
}

/// A deterministic, random-looking ASCII string: zlib's output for it is not smaller enough, so a
/// short one is stored raw.
fn random_looking(len: usize) -> String {
    let mut state = 0x2545_f491_4f6c_dd1du64;
    (0..len)
        .map(|_| {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            char::from(0x21 + ((state >> 33) % 94) as u8)
        })
        .collect()
}

/// Opens one state database, stores the document's text units (which writes `section_text_v3`) and
/// then saves the document row, which is what a runtime open does.
async fn store_document(
    database: &Path,
    document: &Document,
) -> (
    SqliteDocumentRepository,
    SqliteTextUnitIndex,
    SqliteSearchIndex,
) {
    let repository = SqliteDocumentRepository::open(database).expect("repository should open");
    let index = SqliteTextUnitIndex::open(database).expect("text unit index should open");
    let search = SqliteSearchIndex::open(database).expect("search index should open");
    store_text_units(&index, document)
        .await
        .expect("units should store");
    repository
        .save(document.clone())
        .await
        .expect("document should save");
    search
        .index(document)
        .await
        .expect("search index should fill");
    (repository, index, search)
}

fn stored_encodings(database: &Path, document_id: &DocumentId) -> Vec<(i64, i64, i64)> {
    let connection = Connection::open(database).expect("database should open");
    let mut statement = connection
        .prepare(
            "SELECT t.section_key, t.chunk_no, t.encoding
               FROM section_text_v3 t
               JOIN text_unit_documents_v2 d ON d.doc_key = t.doc_key
              WHERE d.document_id = ?1
              ORDER BY t.section_key, t.chunk_no",
        )
        .expect("query should prepare");
    statement
        .query_map(params![&document_id.0], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })
        .expect("rows should read")
        .collect::<Result<Vec<_>, _>>()
        .expect("rows should decode")
}

/// The text of `content[range]`, by Unicode scalar offsets.
fn substring(content: &str, start: usize, end: usize) -> String {
    content.chars().skip(start).take(end - start).collect()
}

fn all_sections(document: &Document) -> Vec<&Section> {
    fn collect<'a>(sections: &'a [Section], out: &mut Vec<&'a Section>) {
        for section in sections {
            out.push(section);
            collect(&section.children, out);
        }
    }
    let mut out = Vec::new();
    collect(&document.root_sections, &mut out);
    out
}

#[tokio::test]
async fn every_kind_of_text_round_trips_through_every_reader() {
    let directory = tempdir().expect("temporary directory should be created");
    let database = directory.path().join("state.sqlite");
    let sections = vec![
        // CJK, more than one chunk (3-byte scalars).
        ("section://cjk", "宋史記載。".repeat(4000)),
        // ASCII, more than one chunk, with a word the scan can find.
        (
            "section://ascii",
            format!(
                "{}the xylophone plays. ",
                "The quick brown fox. ".repeat(900)
            ),
        ),
        // 4-byte scalars on both sides of a chunk boundary.
        (
            "section://emoji",
            format!(
                "{}\u{1F600}\u{1F600}{}",
                "a".repeat(CHUNK_CHARS - 1),
                "b".repeat(50)
            ),
        ),
        // A section with no text at all.
        ("section://empty", String::new()),
        // One chunk.
        ("section://one-chunk", "One chunk of text.".into()),
        // Many chunks of 3-byte scalars.
        ("section://many-chunks", "文".repeat(CHUNK_CHARS * 2 + 100)),
        // Random-looking text that zlib cannot make 5% smaller: stored raw.
        ("section://raw", random_looking(256)),
        // Exactly one chunk, and one scalar more than one chunk.
        ("section://exact", "y".repeat(CHUNK_CHARS)),
        ("section://exact-plus-one", "z".repeat(CHUNK_CHARS + 1)),
    ];
    let document = document("doc:textzip", &sections);
    let (repository, index, search) = store_document(&database, &document).await;

    // `load_document_section_text`: the whole document comes back byte for byte.
    let loaded = repository
        .get(&document.id)
        .await
        .expect("document should load")
        .expect("document should exist");
    assert_eq!(loaded.root_sections.len(), sections.len());
    for (stored, (_, content)) in loaded.root_sections.iter().zip(&sections) {
        assert_eq!(
            &stored.content, content,
            "section {} must round trip",
            stored.id.0
        );
    }

    // The raw and the zlib encodings are both exercised, and every section is stored in one chunk
    // per `CHUNK_CHARS` scalars.
    let encodings = stored_encodings(&database, &document.id);
    let raw = encodings
        .iter()
        .filter(|(_, _, encoding)| *encoding == 0)
        .count();
    assert!(
        raw > 0,
        "random-looking text must be stored raw, got {encodings:?}"
    );
    assert!(
        encodings.iter().any(|(_, _, encoding)| *encoding == 1),
        "compressible text must be stored compressed, got {encodings:?}"
    );
    let chunks = |section_key: i64| {
        encodings
            .iter()
            .filter(|(key, _, _)| *key == section_key)
            .count()
    };
    let key = |index: usize| i64::try_from(index).expect("section index fits");
    assert_eq!(chunks(key(0)), 2, "CJK section chunks");
    assert_eq!(chunks(key(4)), 1, "one-chunk section chunks");
    assert_eq!(chunks(key(5)), 3, "many-chunk section chunks");
    assert_eq!(chunks(key(7)), 1, "exactly CHUNK_CHARS is one chunk");
    assert_eq!(chunks(key(8)), 2, "CHUNK_CHARS + 1 scalars is two chunks");

    for section in all_sections(&document) {
        let section_id = &section.id;
        // `load_section_text` (the light single-section read).
        let one = repository
            .load_section_text(&document.id, section_id)
            .await
            .expect("section text should load");
        assert_eq!(
            one.as_deref(),
            Some(section.content.as_str()),
            "load_section_text of {}",
            section_id.0
        );

        // `load_section_text_unchecked` through the section units, and `load_text_slice` through
        // every unit of it.
        let units = index
            .section_units(&document.id, section_id)
            .await
            .expect("section units should load")
            .expect("section units should exist");
        for paragraph in &units.paragraphs {
            let range = paragraph.normalized_range;
            let expected = substring(&section.content, range.start(), range.end());
            assert_eq!(
                paragraph.text, expected,
                "paragraph {} of {}",
                paragraph.paragraph_index, section_id.0
            );
            let unit = index
                .section_unit(
                    &document.id,
                    section_id,
                    EffectiveTextUnitKind::Paragraph,
                    paragraph.paragraph_index,
                    None,
                )
                .await
                .expect("section unit should load")
                .expect("section unit should exist");
            match unit {
                OwnedStreamUnit::Paragraph(unit) => assert_eq!(
                    unit.text, expected,
                    "sliced paragraph {} of {}",
                    paragraph.paragraph_index, section_id.0
                ),
                OwnedStreamUnit::Sentence(_) => panic!("expected a paragraph unit"),
            }
        }
    }

    // The scan loader reads the same text: a hit's snippet holds the query.
    let cjk = search
        .search_lexical_with_options(&document.id, "宋史記載", 5, ScanOptions::production())
        .await
        .expect("CJK search");
    assert!(!cjk.is_empty(), "the CJK text must be searchable");
    assert!(
        cjk.iter().any(|hit| hit.snippet.contains("宋史記載")),
        "the CJK snippet must hold the query: {cjk:?}"
    );
    let ascii = search
        .search_lexical_with_options(&document.id, "xylophone", 5, ScanOptions::production())
        .await
        .expect("ASCII search");
    assert!(
        ascii.iter().any(|hit| hit.snippet.contains("xylophone")),
        "the ASCII snippet must hold the query: {ascii:?}"
    );
}

#[tokio::test]
async fn slicing_a_section_inside_and_across_chunks_equals_the_original_substring() {
    let directory = tempdir().expect("temporary directory should be created");
    let database = directory.path().join("state.sqlite");
    let first = "甲".repeat(CHUNK_CHARS);
    let second = "乙".repeat(CHUNK_CHARS + 10);
    let third = "丙".repeat(20);
    let content = format!("{first}\n\n{second}\n\n{third}");
    let sections = vec![("section://root", content.clone())];
    let document = document("doc:slices", &sections);
    let (_, index, _) = store_document(&database, &document).await;
    let section_id = document.root_sections[0].id.clone();

    let units = index
        .section_units(&document.id, &section_id)
        .await
        .expect("section units should load")
        .expect("section units should exist");
    assert_eq!(units.paragraphs.len(), 3, "three paragraphs");

    let mut inside_one_chunk = 0;
    let mut across_two_chunks = 0;
    for paragraph in &units.paragraphs {
        let range = paragraph.normalized_range;
        let expected = substring(&content, range.start(), range.end());
        assert_eq!(paragraph.text, expected);
        let unit = index
            .section_unit(
                &document.id,
                &section_id,
                EffectiveTextUnitKind::Paragraph,
                paragraph.paragraph_index,
                None,
            )
            .await
            .expect("section unit should load")
            .expect("section unit should exist");
        let OwnedStreamUnit::Paragraph(unit) = unit else {
            panic!("expected a paragraph unit");
        };
        assert_eq!(
            unit.text,
            expected,
            "slice [{}, {})",
            range.start(),
            range.end()
        );
        let first_chunk = range.start() / CHUNK_CHARS;
        let last_chunk = (range.end() - 1) / CHUNK_CHARS;
        if first_chunk == last_chunk {
            inside_one_chunk += 1;
        } else {
            across_two_chunks += 1;
        }
    }
    assert!(inside_one_chunk >= 1, "a slice must stay inside one chunk");
    assert!(
        across_two_chunks >= 1,
        "a slice must cross two chunks, got {inside_one_chunk}/{across_two_chunks}"
    );
}

#[tokio::test]
async fn a_corrupt_chunk_is_an_error_not_a_panic() {
    let directory = tempdir().expect("temporary directory should be created");
    let database = directory.path().join("state.sqlite");
    let sections = vec![("section://root", "宋史記載。".repeat(4000))];
    let document = document("doc:corrupt", &sections);
    let (repository, index, _) = store_document(&database, &document).await;

    // Flip bytes of the first compressed chunk: it is no longer valid zlib data.
    let connection = Connection::open(&database).expect("database should open");
    connection
        .execute(
            "UPDATE section_text_v3 SET encoding = 1, data = x'00010203'
              WHERE section_key = 0 AND chunk_no = 0",
            [],
        )
        .expect("chunk should corrupt");
    drop(connection);

    let error = repository
        .get(&document.id)
        .await
        .expect_err("a corrupt chunk must not load");
    assert!(
        matches!(error, ApplicationError::StaleDocument(_)),
        "expected StaleDocument, got {error:?}"
    );
    assert!(
        error
            .to_string()
            .contains("stored section text chunk cannot be read"),
        "the message must name the broken chunk: {error}"
    );

    let section_id = document.root_sections[0].id.clone();
    let error = index
        .section_units(&document.id, &section_id)
        .await
        .expect_err("a corrupt chunk must not scan");
    assert!(
        error
            .to_string()
            .contains("stored section text chunk cannot be read"),
        "the message must name the broken chunk: {error}"
    );
}

#[tokio::test]
async fn a_database_that_keeps_only_section_text_v2_rows_is_stale_and_drops_the_table() {
    let directory = tempdir().expect("temporary directory should be created");
    let database = directory.path().join("state.sqlite");
    let sections = vec![("section://root", "Old shape text.".to_string())];
    let document = document("doc:v2", &sections);
    let (repository, _, _) = store_document(&database, &document).await;
    drop(repository);

    // Rewrite the row and the text into the v4 shape: `content_store = "section_text_v2"` and one
    // uncompressed text chunk per section.
    let connection = Connection::open(&database).expect("database should open");
    let stored: String = connection
        .query_row(
            "SELECT document_json FROM documents WHERE id = ?1",
            params![&document.id.0],
            |row| row.get(0),
        )
        .expect("document row should exist");
    let mut stored: serde_json::Value = serde_json::from_str(&stored).expect("JSON");
    stored["content_store"] = serde_json::json!("section_text_v2");
    connection
        .execute(
            "UPDATE documents SET document_json = ?1 WHERE id = ?2",
            params![stored.to_string(), &document.id.0],
        )
        .expect("document row should update");
    connection
        .execute_batch(
            "CREATE TABLE section_text_v2 (
                doc_key INTEGER NOT NULL,
                section_key INTEGER NOT NULL,
                chunk_no INTEGER NOT NULL,
                text TEXT NOT NULL,
                UNIQUE(doc_key, section_key, chunk_no)
             );
             INSERT INTO section_text_v2(doc_key, section_key, chunk_no, text)
                SELECT doc_key, section_key, chunk_no, 'Old shape text.' FROM section_text_v3;",
        )
        .expect("old table should be written");
    drop(connection);

    // Opening the text unit index drops the old table, as it drops every superseded one.
    let index = SqliteTextUnitIndex::open(&database).expect("index should open");
    drop(index);
    let connection = Connection::open(&database).expect("database should open");
    let old_table: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'section_text_v2'",
            [],
            |row| row.get(0),
        )
        .expect("table count");
    assert_eq!(old_table, 0, "the old text table must be dropped");

    // The row still names the old store, so it needs an explicit source reopen.
    let repository = SqliteDocumentRepository::open(&database).expect("repository should open");
    let error = repository
        .get(&document.id)
        .await
        .expect_err("a v2 row must not load");
    assert!(
        matches!(error, ApplicationError::StaleDocument(_)),
        "expected StaleDocument, got {error:?}"
    );
    assert!(
        error.to_string().contains("unknown store section_text_v2"),
        "the message must name the old store: {error}"
    );
    assert!(
        error
            .to_string()
            .ends_with("explicit source reopen required"),
        "the message must ask for a reopen: {error}"
    );
}
