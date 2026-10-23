//! SEC-3: the section tree and the body order live only in their rows.
//!
//! Every expected value is computed from the `Document` that was saved, never from the storage
//! under test, so the rebuilt document is proven equal to the original.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use reading_mcp::application::ports::{ApplicationError, DocumentRepository, TextUnitIndex};
use reading_mcp::domain::{
    ContentHash, Document, DocumentId, DocumentSource, Location, MediaType, Section, SectionId,
};
use reading_mcp::infrastructure::{SqliteDocumentRepository, SqliteTextUnitIndex};
use rusqlite::{Connection, params};
use serde_json::{Value, json};
use tempfile::tempdir;

fn section(
    name: &str,
    parent: Option<&str>,
    level: u8,
    content: &str,
    children: Vec<Section>,
) -> Section {
    Section {
        id: SectionId(format!("section://{name}")),
        parent_id: parent.map(|parent| SectionId(format!("section://{parent}"))),
        title: format!("Title {name}"),
        level,
        content: content.into(),
        location: Location {
            page: Some(u32::from(level)),
            chapter: Some(format!("chapter {name}")),
            section_path: vec![name.into()],
            ..Location::default()
        },
        children,
    }
}

fn document(id: &str, media_type: &str, root_sections: Vec<Section>) -> Document {
    Document {
        id: DocumentId(format!("doc:{id}")),
        source: DocumentSource(format!("memory:{id}")),
        title: format!("Document {id}"),
        media_type: MediaType(media_type.into()),
        content_hash: ContentHash(format!("sha256:{id}")),
        metadata: BTreeMap::new(),
        root_sections,
    }
}

/// Three levels, siblings at two levels and one deep chain.
fn nested_document() -> Document {
    let deep = section(
        "deep-0",
        Some("first"),
        1,
        "Deep body.",
        vec![section(
            "deep-1",
            Some("deep-0"),
            2,
            "Deeper body.",
            Vec::new(),
        )],
    );
    document(
        "nested",
        "text/markdown",
        vec![
            section(
                "first",
                None,
                1,
                "First body.",
                vec![
                    section("second", Some("first"), 2, "Second body.", Vec::new()),
                    deep,
                    section("last", Some("first"), 2, "Last body.", Vec::new()),
                ],
            ),
            section("root-b", None, 1, "Root b body.", Vec::new()),
        ],
    )
}

fn single_document() -> Document {
    document(
        "single",
        "text/markdown",
        vec![section("only", None, 1, "Only body.", Vec::new())],
    )
}

fn wide_document(count: usize) -> Document {
    document(
        "wide",
        "text/markdown",
        (0..count)
            .map(|index| {
                section(
                    &format!("wide-{index}"),
                    None,
                    1,
                    &format!("Body of section {index}."),
                    Vec::new(),
                )
            })
            .collect(),
    )
}

/// Stores `document`'s text first and then the document, exactly as the open use case does.
async fn stored(
    database: &Path,
    document: &Arc<Document>,
) -> (SqliteDocumentRepository, SqliteTextUnitIndex) {
    let text_units = SqliteTextUnitIndex::open(database).expect("text unit index should open");
    let segmentation = Arc::new(
        document
            .try_segmentation()
            .expect("document should segment"),
    );
    text_units
        .replace_document_segmentation(document, segmentation)
        .await
        .expect("section text should store");
    let repository = SqliteDocumentRepository::open(database).expect("repository should open");
    repository
        .save_shared(Arc::clone(document))
        .await
        .expect("document should save");
    (repository, text_units)
}

fn tree_shape(document: &Document) -> Vec<(String, Option<String>, u8, String)> {
    fn walk(section: &Section, out: &mut Vec<(String, Option<String>, u8, String)>) {
        out.push((
            section.id.0.clone(),
            section.parent_id.as_ref().map(|id| id.0.clone()),
            section.level,
            section.title.clone(),
        ));
        for child in &section.children {
            walk(child, out);
        }
    }
    let mut out = Vec::new();
    for section in &document.root_sections {
        walk(section, &mut out);
    }
    out
}

fn dfs(document: &Document) -> Vec<SectionId> {
    fn walk(sections: &[Section], out: &mut Vec<SectionId>) {
        for section in sections {
            out.push(section.id.clone());
            walk(&section.children, out);
        }
    }
    let mut out = Vec::new();
    walk(&document.root_sections, &mut out);
    out
}

fn assert_no_content(document: &Document) {
    fn walk(section: &Section) {
        assert!(
            section.content.is_empty(),
            "light section {} must carry no text",
            section.id.0
        );
        for child in &section.children {
            walk(child);
        }
    }
    for section in &document.root_sections {
        walk(section);
    }
}

#[tokio::test]
async fn the_rebuilt_document_equals_the_saved_one() {
    for (label, document) in [
        ("nested", nested_document()),
        ("single", single_document()),
        ("wide", wide_document(10_000)),
    ] {
        let document = Arc::new(document);
        let directory = tempdir().expect("temporary directory");
        let database = directory.path().join("state.sqlite");
        let (repository, _) = stored(&database, &document).await;

        let (full, hash) = repository
            .get_rebuilt_with_hash(&document.id, true)
            .await
            .expect("full rebuild should load")
            .expect("document should exist");
        assert_eq!(
            *full, *document,
            "{label}: the rebuilt document must equal the saved one"
        );
        assert_eq!(hash, document.normalized_document_hash(), "{label}: hash");
        assert_eq!(dfs(&full), dfs(&document), "{label}: depth-first order");

        let (light, light_hash) = repository
            .get_rebuilt_with_hash(&document.id, false)
            .await
            .expect("tree rebuild should load")
            .expect("document should exist");
        assert_eq!(light_hash, hash, "{label}: the two reads agree on the hash");
        assert_eq!(tree_shape(&light), tree_shape(&document), "{label}: tree");
        assert_no_content(&light);
    }
}

#[tokio::test]
async fn a_stored_row_keeps_no_tree_in_document_json() {
    let document = Arc::new(nested_document());
    let directory = tempdir().expect("temporary directory");
    let database = directory.path().join("state.sqlite");
    let _ = stored(&database, &document).await;

    let connection = Connection::open(&database).expect("database should open");
    let document_json: String = connection
        .query_row(
            "SELECT document_json FROM documents WHERE id = ?1",
            params![&document.id.0],
            |row| row.get(0),
        )
        .expect("the documents row should exist");
    let stored: Value = serde_json::from_str(&document_json).expect("stored JSON");
    assert_eq!(
        stored["root_sections"],
        json!([]),
        "the stored JSON must carry no section tree"
    );
    assert_eq!(
        stored["section_tree_store"],
        json!("document_sections_v1"),
        "the stored JSON must name the rows that carry the tree"
    );
}

#[tokio::test]
async fn a_marker_without_rows_is_an_error_never_an_empty_book() {
    let document = Arc::new(nested_document());
    let directory = tempdir().expect("temporary directory");
    let database = directory.path().join("state.sqlite");
    let (repository, _) = stored(&database, &document).await;

    // The marker says the tree is in the rows; remove the rows and the header still claims them.
    Connection::open(&database)
        .expect("database should open")
        .execute(
            "DELETE FROM document_sections_v1 WHERE document_id = ?1",
            params![&document.id.0],
        )
        .expect("section rows should delete");

    for error in [
        repository
            .get_rebuilt_with_hash(&document.id, false)
            .await
            .expect_err("a treeless row must not load light"),
        repository
            .get_with_hash(&document.id)
            .await
            .expect_err("a treeless row must not load full"),
    ] {
        assert!(
            matches!(error, ApplicationError::StaleDocument(_)),
            "a marker without its rows is stale, got {error:?}"
        );
    }
}

#[tokio::test]
async fn opening_the_text_unit_store_drops_the_old_body_order_table() {
    let directory = tempdir().expect("temporary directory");
    let database = directory.path().join("state.sqlite");
    Connection::open(&database)
        .expect("database should open")
        .execute_batch(
            "CREATE TABLE document_body_order_v1 (
                document_id TEXT NOT NULL,
                order_json TEXT,
                error TEXT,
                UNIQUE(document_id)
             );",
        )
        .expect("old table should create");

    // Idempotent: opening twice is fine.
    let _ = SqliteTextUnitIndex::open(&database).expect("text unit index should open");
    let _ = SqliteTextUnitIndex::open(&database).expect("text unit index should reopen");

    let exists: i64 = Connection::open(&database)
        .expect("database should open")
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master
              WHERE type = 'table' AND name = 'document_body_order_v1'",
            [],
            |row| row.get(0),
        )
        .expect("sqlite_master should read");
    assert_eq!(exists, 0, "the old body-order table must be dropped");
}

#[tokio::test]
async fn a_smaller_resave_leaves_no_orphan_rows_in_the_three_tables() {
    let document = Arc::new(nested_document());
    let directory = tempdir().expect("temporary directory");
    let database = directory.path().join("state.sqlite");
    let (repository, text_units) = stored(&database, &document).await;

    let mut smaller = (*document).clone();
    smaller.root_sections.truncate(1);
    smaller.root_sections[0].children.truncate(1);
    let smaller = Arc::new(smaller);
    let segmentation = Arc::new(smaller.try_segmentation().expect("smaller segments"));
    text_units
        .replace_document_segmentation(&smaller, segmentation)
        .await
        .expect("smaller text should store");
    repository
        .save_shared(Arc::clone(&smaller))
        .await
        .expect("smaller should save");

    let connection = Connection::open(&database).expect("database should open");
    let count = |table: &str| -> i64 {
        connection
            .query_row(
                &format!("SELECT COUNT(*) FROM {table} WHERE document_id = ?1"),
                params![&smaller.id.0],
                |row| row.get(0),
            )
            .expect("count")
    };
    let sections = i64::try_from(dfs(&smaller).len()).expect("count fits");
    assert_eq!(count("document_header_v1"), 1, "one header row");
    assert_eq!(
        count("document_sections_v1"),
        sections,
        "no orphan sections"
    );
    assert_eq!(
        count("document_body_order_rows_v1"),
        sections,
        "no orphan body-order rows"
    );
    assert_eq!(
        repository
            .body_order_len(&smaller.id)
            .await
            .expect("body order length"),
        Some(usize::try_from(sections).expect("count fits"))
    );
}
