//! `document_meta_v2` stores the heavy metadata entries compressed in zlib format, and every read
//! decodes them back to the exact strings that were saved. A row written in the `document_meta_v1`
//! shape still loads, and a corrupt blob is an error, not a panic.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};

use reading_mcp::application::ports::{ApplicationError, DocumentRepository};
use reading_mcp::domain::{
    ContentHash, Document, DocumentId, DocumentSource, Location, MediaType, Section, SectionId,
};
use reading_mcp::infrastructure::SqliteDocumentRepository;
use rusqlite::{Connection, params};
use serde_json::{Value, json};
use tempfile::tempdir;

/// A JSON-like value of about two megabytes, shaped like `normalized_block_map`: the same long
/// `owner_section_id` and `native_location` strings in every block, which is what compresses well.
fn multi_megabyte_value() -> String {
    let mut value = String::from("{\"blocks\":[");
    for index in 0..16000 {
        if index > 0 {
            value.push(',');
        }
        value.push_str(&format!(
            "{{\"owner_section_id\":\"section://chapter/{}\",\"native_location\":\"OEBPS/text/chapter{}.xhtml#p{}\",\"text\":\"paragraph {} of the chapter body\"}}",
            index / 100,
            index / 100,
            index,
            index
        ));
    }
    value.push_str("]}");
    assert!(value.len() > 2 * 1024 * 1024, "value is {}", value.len());
    value
}

/// A heavy value with non-ASCII text, so a byte-length threshold and a char-length threshold would
/// disagree.
fn non_ascii_value() -> String {
    format!("{{\"blocks\":\"{}\"}}", "章节正文。".repeat(400))
}

/// Exactly one byte over the heavy threshold.
fn just_heavy_value() -> String {
    "x".repeat(1025)
}

fn document() -> Document {
    Document {
        id: DocumentId("doc:v2".into()),
        source: DocumentSource("memory:v2.md".into()),
        title: "V2".into(),
        media_type: MediaType("text/markdown".into()),
        content_hash: ContentHash("sha256:v2".into()),
        metadata: BTreeMap::from([
            ("normalized_block_map".to_string(), multi_megabyte_value()),
            ("epub_structure_map".to_string(), non_ascii_value()),
            (
                "original_source_binding_map".to_string(),
                just_heavy_value(),
            ),
            ("light".to_string(), "small".to_string()),
            ("content_encoding".to_string(), "utf-8".to_string()),
        ]),
        root_sections: vec![Section {
            id: SectionId("section://part".into()),
            parent_id: None,
            title: "Part".into(),
            level: 1,
            content: "Part intro.".into(),
            location: Location::default(),
            children: vec![],
        }],
    }
}

fn stored_json(database: &Path, document_id: &DocumentId) -> String {
    let connection = Connection::open(database).expect("database should open");
    connection
        .query_row(
            "SELECT document_json FROM documents WHERE id = ?1",
            params![&document_id.0],
            |row| row.get(0),
        )
        .expect("stored document should exist")
}

/// Every row of one metadata table of `document_id`: `(key, encoding, bytes)`. A `document_meta_v1`
/// row is reported with an empty encoding.
fn metadata_rows(
    database: &Path,
    table: &str,
    document_id: &DocumentId,
) -> Vec<(String, String, Vec<u8>)> {
    let connection = Connection::open(database).expect("database should open");
    let sql = if table == "document_meta_v1" {
        "SELECT key, '', CAST(value AS BLOB) FROM document_meta_v1
          WHERE document_id = ?1 ORDER BY key"
    } else {
        "SELECT key, encoding, value FROM document_meta_v2 WHERE document_id = ?1 ORDER BY key"
    };
    let mut statement = connection.prepare(sql).expect("statement");
    statement
        .query_map(params![&document_id.0], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Vec<u8>>(2)?,
            ))
        })
        .expect("rows")
        .map(|row| row.expect("row"))
        .collect()
}

fn decode(encoding: &str, blob: &[u8]) -> String {
    let bytes = match encoding {
        "raw" => blob.to_vec(),
        "zlib" => {
            let mut bytes = Vec::new();
            flate2::read::ZlibDecoder::new(blob)
                .read_to_end(&mut bytes)
                .expect("zlib blob");
            bytes
        }
        encoding => panic!("unknown encoding {encoding}"),
    };
    String::from_utf8(bytes).expect("utf-8 value")
}

fn update_json(database: &Path, document_id: &DocumentId, edit: impl FnOnce(&mut Value)) {
    let mut stored: Value =
        serde_json::from_str(&stored_json(database, document_id)).expect("stored JSON");
    edit(&mut stored);
    let connection = Connection::open(database).expect("database should open");
    connection
        .execute(
            "UPDATE documents SET document_json = ?1 WHERE id = ?2",
            params![stored.to_string(), &document_id.0],
        )
        .expect("document_json should update");
}

async fn save(document: Document) -> (tempfile::TempDir, PathBuf) {
    let directory = tempdir().expect("temporary directory");
    let database = directory.path().join("state.sqlite");
    let repository = SqliteDocumentRepository::open(&database).expect("repository");
    repository.save(document).await.expect("save");
    (directory, database)
}

#[tokio::test]
async fn heavy_entries_round_trip_byte_for_byte() {
    let document = document();
    let (_directory, database) = save(document.clone()).await;
    let repository = SqliteDocumentRepository::open(&database).expect("repository");

    let (full, _) = repository
        .get_with_hash(&document.id)
        .await
        .expect("full read")
        .expect("document should exist");
    assert_eq!(
        full.metadata, document.metadata,
        "every entry is byte-identical"
    );
    assert_eq!(
        full.metadata["normalized_block_map"].as_bytes(),
        document.metadata["normalized_block_map"].as_bytes()
    );
    assert_eq!(
        full.metadata["epub_structure_map"].as_bytes(),
        document.metadata["epub_structure_map"].as_bytes()
    );
    assert_eq!(
        full.metadata["original_source_binding_map"].len(),
        1025,
        "a value of exactly 1025 bytes is heavy and round trips"
    );
    for (key, value) in &document.metadata {
        assert_eq!(
            repository
                .load_metadata(&document.id, key)
                .await
                .expect("metadata read"),
            Some(value.clone()),
            "{key} must load exactly"
        );
    }

    let stored = stored_json(&database, &document.id);
    assert!(
        stored.contains("\"metadata_store\":\"document_meta_v2\""),
        "a heavy entry names the v2 store: {stored}"
    );
    let rows = metadata_rows(&database, "document_meta_v2", &document.id);
    assert_eq!(
        rows.iter()
            .map(|(key, _, _)| key.clone())
            .collect::<Vec<_>>(),
        vec![
            "epub_structure_map".to_string(),
            "normalized_block_map".to_string(),
            "original_source_binding_map".to_string(),
        ],
        "every heavy entry is a v2 row and no light entry is"
    );
    for (key, encoding, blob) in &rows {
        assert_eq!(encoding, "zlib", "{key} is compressed");
        assert_eq!(
            decode(encoding, blob),
            document.metadata[key],
            "{key} decompresses to the saved string"
        );
        let saved = document.metadata[key].as_bytes();
        assert!(
            blob.len() < saved.len(),
            "{key} compresses: {} bytes stored for {} bytes of value",
            blob.len(),
            saved.len()
        );
    }
}

#[tokio::test]
async fn light_entries_stay_inline() {
    let document = document();
    let (_directory, database) = save(document.clone()).await;
    let stored = stored_json(&database, &document.id);
    assert!(
        stored.contains("\"light\":\"small\"") && stored.contains("\"content_encoding\":\"utf-8\""),
        "light entries stay in document_json: {stored}"
    );
    let rows = metadata_rows(&database, "document_meta_v2", &document.id);
    assert!(
        rows.iter()
            .all(|(key, _, _)| key != "light" && key != "content_encoding"),
        "no light entry gets a table row"
    );
}

#[tokio::test]
async fn a_corrupt_blob_is_an_error_not_a_panic() {
    let document = document();
    let (_directory, database) = save(document.clone()).await;
    let rows = metadata_rows(&database, "document_meta_v2", &document.id);
    let (_, _, blob) = rows
        .iter()
        .find(|(key, _, _)| key == "normalized_block_map")
        .expect("heavy row");
    let corrupt: Vec<u8> = blob.iter().map(|byte| byte ^ 0xff).collect();
    let connection = Connection::open(&database).expect("database should open");
    connection
        .execute(
            "UPDATE document_meta_v2 SET value = ?1 WHERE document_id = ?2 AND key = ?3",
            params![corrupt, &document.id.0, "normalized_block_map"],
        )
        .expect("blob should update");
    drop(connection);

    let repository = SqliteDocumentRepository::open(&database).expect("repository");
    let error = repository
        .load_metadata(&document.id, "normalized_block_map")
        .await
        .expect_err("a corrupt blob must not load");
    assert!(
        matches!(error, ApplicationError::RepositoryFailed(_)),
        "expected RepositoryFailed, got {error:?}"
    );
    assert!(
        error.to_string().contains("cannot be decoded"),
        "the message should name the broken entry: {error}"
    );
    assert!(
        repository.get_with_hash(&document.id).await.is_err(),
        "the full read fails on the same blob"
    );
    assert_eq!(
        repository
            .load_metadata(&document.id, "light")
            .await
            .expect("a light entry does not read the corrupt row"),
        Some("small".to_string()),
        "the light entry still loads"
    );
}

#[tokio::test]
async fn a_document_meta_v1_row_still_loads_through_every_call() {
    let document = document();
    let (_directory, database) = save(document.clone()).await;
    // Rewrite the saved row into the shape #205-#209 wrote: the heavy entries in
    // `document_meta_v1` as text, the v2 rows gone, and the store key naming v1.
    let rows = metadata_rows(&database, "document_meta_v2", &document.id);
    let connection = Connection::open(&database).expect("database should open");
    for (key, encoding, blob) in &rows {
        let value = decode(encoding, blob);
        connection
            .execute(
                "INSERT INTO document_meta_v1(document_id, key, value) VALUES (?1, ?2, ?3)",
                params![&document.id.0, key, value],
            )
            .expect("v1 row should insert");
    }
    connection
        .execute(
            "DELETE FROM document_meta_v2 WHERE document_id = ?1",
            params![&document.id.0],
        )
        .expect("v2 rows should delete");
    drop(connection);
    update_json(&database, &document.id, |stored| {
        stored["metadata_store"] = json!("document_meta_v1");
    });

    let repository = SqliteDocumentRepository::open(&database).expect("repository");
    let (full, full_hash) = repository
        .get_with_hash(&document.id)
        .await
        .expect("full read of a v1 row")
        .expect("document should exist");
    assert_eq!(full.metadata, document.metadata);
    let (light, light_hash) = repository
        .get_light_with_hash(&document.id)
        .await
        .expect("light read of a v1 row")
        .expect("document should exist");
    assert_eq!(light_hash, full_hash);
    assert_eq!(
        light.metadata,
        BTreeMap::from([
            ("light".to_string(), "small".to_string()),
            ("content_encoding".to_string(), "utf-8".to_string()),
        ]),
        "the light read of a v1 row carries the light entries"
    );
    for (key, value) in &document.metadata {
        assert_eq!(
            repository
                .load_metadata(&document.id, key)
                .await
                .expect("metadata read of a v1 row"),
            Some(value.clone()),
            "{key} loads from the v1 table"
        );
    }
    assert_eq!(
        repository
            .load_metadata(&document.id, "missing")
            .await
            .expect("metadata read"),
        None
    );
}

#[tokio::test]
async fn saving_removes_rows_from_both_tables() {
    let document = document();
    let (_directory, database) = save(document.clone()).await;
    // A leftover v1 row of another version, as a row written before this change would leave.
    let connection = Connection::open(&database).expect("database should open");
    connection
        .execute(
            "INSERT INTO document_meta_v1(document_id, key, value) VALUES (?1, ?2, ?3)",
            params![&document.id.0, "stale_heavy", "s".repeat(2000)],
        )
        .expect("v1 row should insert");
    drop(connection);

    let repository = SqliteDocumentRepository::open(&database).expect("repository");
    repository
        .save(document.clone())
        .await
        .expect("second save");
    assert!(
        metadata_rows(&database, "document_meta_v1", &document.id).is_empty(),
        "a save leaves no row in document_meta_v1"
    );
    assert_eq!(
        metadata_rows(&database, "document_meta_v2", &document.id).len(),
        3,
        "a save writes the heavy entries to document_meta_v2"
    );
    assert_eq!(
        repository
            .load_metadata(&document.id, "stale_heavy")
            .await
            .expect("metadata read"),
        None,
        "the old v1 row is gone"
    );

    let light_only = Document {
        metadata: BTreeMap::from([("light".to_string(), "only".to_string())]),
        ..document
    };
    repository
        .save(light_only.clone())
        .await
        .expect("third save");
    assert!(
        metadata_rows(&database, "document_meta_v2", &light_only.id).is_empty(),
        "a save with no heavy entry leaves no v2 row"
    );
    assert!(
        !stored_json(&database, &light_only.id).contains("metadata_store"),
        "a save with no heavy entry names no metadata store"
    );
}

/// A value of exactly five mebibytes, shaped like `normalized_block_map`: the size the borrowed
/// split in `split_metadata` must compress without ever copying the value (MEM-2b).
fn five_mebibyte_value() -> String {
    let mut value = String::from("{\"blocks\":[");
    let mut index = 0usize;
    while value.len() < 5 * 1024 * 1024 {
        if index > 0 {
            value.push(',');
        }
        value.push_str(&format!(
            "{{\"owner_section_id\":\"section://chapter/{}\",\"native_location\":\"OEBPS/text/chapter{}.xhtml#p{}\",\"text\":\"第{}段的正文内容\"}}",
            index / 100,
            index / 100,
            index,
            index
        ));
        index += 1;
    }
    value.push_str("]}");
    assert!(value.len() >= 5 * 1024 * 1024, "value is {}", value.len());
    value
}

/// The zlib blob this value is stored as, encoded here independently: the level and the 1 MiB
/// feeding chunks `encode_metadata_value` uses, so a change to either — or a copy that changes the
/// bytes — fails the comparison.
fn expected_zlib(value: &str) -> Vec<u8> {
    use std::io::Write;
    let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::new(6));
    for chunk in value.as_bytes().chunks(1024 * 1024) {
        encoder.write_all(chunk).expect("zlib chunk");
    }
    encoder.finish().expect("zlib finish")
}

/// The stored `zlib` blob of `key`, and its encoding.
fn stored_blob(database: &Path, document_id: &DocumentId, key: &str) -> (String, Vec<u8>) {
    let rows = metadata_rows(database, "document_meta_v2", document_id);
    let (_, encoding, blob) = rows
        .into_iter()
        .find(|(row_key, _, _)| row_key == key)
        .expect("the heavy row should exist");
    (encoding, blob)
}

#[tokio::test]
async fn a_five_mebibyte_value_round_trips_and_compresses_to_the_same_bytes() {
    let mut document = document();
    let value = five_mebibyte_value();
    document
        .metadata
        .insert("normalized_block_map".into(), value.clone());
    let (_directory, database) = save(document.clone()).await;

    let repository = SqliteDocumentRepository::open(&database).expect("repository");
    let (full, _) = repository
        .get_with_hash(&document.id)
        .await
        .expect("full read")
        .expect("document should exist");
    assert_eq!(*full, document, "the loaded document is the saved one");
    assert_eq!(
        full.metadata["normalized_block_map"].as_bytes(),
        value.as_bytes(),
        "the five-mebibyte value is byte-identical after the round trip"
    );

    let (encoding, blob) = stored_blob(&database, &document.id, "normalized_block_map");
    assert_eq!(encoding, "zlib");
    assert_eq!(
        blob,
        expected_zlib(&value),
        "the stored blob is the same zlib stream: no copy changed the compressor's input"
    );
    assert_eq!(decode(&encoding, &blob), value);
    assert!(
        !stored_json(&database, &document.id).contains("owner_section_id"),
        "a heavy value stays out of document_json"
    );
}

#[tokio::test]
async fn a_value_of_exactly_the_heavy_threshold_round_trips_and_compresses_to_the_same_bytes() {
    // 1025 bytes: one over `METADATA_HEAVY_BYTES`, so this is the smallest heavy entry there is.
    let value = just_heavy_value();
    assert_eq!(value.len(), 1025);
    let mut document = document();
    document
        .metadata
        .insert("original_source_binding_map".into(), value.clone());
    let (_directory, database) = save(document.clone()).await;

    let repository = SqliteDocumentRepository::open(&database).expect("repository");
    let (full, _) = repository
        .get_with_hash(&document.id)
        .await
        .expect("full read")
        .expect("document should exist");
    assert_eq!(*full, document, "the loaded document is the saved one");

    let (encoding, blob) = stored_blob(&database, &document.id, "original_source_binding_map");
    assert_eq!(encoding, "zlib");
    assert_eq!(blob, expected_zlib(&value), "the stored blob is unchanged");
    assert_eq!(
        repository
            .load_metadata(&document.id, "original_source_binding_map")
            .await
            .expect("metadata read"),
        Some(value),
    );
}
