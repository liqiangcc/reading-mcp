//! SEC-3c: the section row stores a compact location and indexes the id and parent lookups by
//! hashes, and every `Location` still comes back exactly as it was saved.
//!
//! Each expectation is the `Location` the test put in the document, compared as a whole struct, so
//! a field that is present, absent or at a default is proven to round-trip, not just the fields the
//! encoding happens to name.

use std::collections::BTreeMap;

use reading_mcp::application::ports::{DocumentRepository, SectionRow};
use reading_mcp::domain::{
    ContentHash, Document, DocumentId, DocumentSource, Location, MediaType, Section, SectionId,
};
use reading_mcp::infrastructure::SqliteDocumentRepository;

async fn open_repository() -> (tempfile::TempDir, SqliteDocumentRepository) {
    let directory = tempfile::tempdir().expect("temp directory");
    let path = directory.path().join("section-locations.sqlite");
    let repository = SqliteDocumentRepository::open(&path).expect("repository should open");
    (directory, repository)
}

/// One root section whose title is `title` and whose location is `location`.
fn root(index: usize, title: &str, location: Location) -> Section {
    Section {
        id: SectionId(format!("section://root-{index}")),
        parent_id: None,
        title: title.to_string(),
        level: 1,
        content: format!("Body of root {index}."),
        location,
        children: Vec::new(),
    }
}

fn document(root_sections: Vec<Section>) -> Document {
    Document {
        id: DocumentId("doc:section-locations".into()),
        source: DocumentSource("memory:section-locations".into()),
        title: "Section locations".into(),
        media_type: MediaType("text/markdown".into()),
        content_hash: ContentHash("sha256:section-locations".into()),
        metadata: BTreeMap::new(),
        root_sections,
    }
}

/// Every stored row of `document`, in key order.
async fn stored_rows(repository: &dyn DocumentRepository, document: &Document) -> Vec<SectionRow> {
    repository
        .sections_in_key_range(&document.id, 0, i64::MAX, usize::MAX)
        .await
        .expect("sections_in_key_range should succeed")
}

/// The whole stored `Location` and title of every section, read through all three point queries.
async fn assert_locations_round_trip(document: &Document) {
    let (_directory, repository) = open_repository().await;
    repository.save(document.clone()).await.expect("save");

    let stored = stored_rows(&repository, document).await;
    let expected: Vec<&Section> = document.root_sections.iter().collect();
    assert_eq!(stored.len(), expected.len(), "one row per section");
    for (row, section) in stored.iter().zip(&expected) {
        assert_eq!(row.title, section.title, "the title round-trips");
        assert_eq!(
            row.location, section.location,
            "the whole Location round-trips for {}",
            section.id.0
        );
        let by_id = repository
            .section_by_id(&document.id, &section.id)
            .await
            .expect("section_by_id should succeed")
            .expect("section_by_id finds the row");
        assert_eq!(
            by_id.location, section.location,
            "section_by_id returns the whole Location"
        );
        let by_key = repository
            .section_by_key(&document.id, row.section_key)
            .await
            .expect("section_by_key should succeed")
            .expect("section_by_key finds the row");
        assert_eq!(
            by_key.location, section.location,
            "section_by_key returns the whole Location"
        );
    }
}

#[tokio::test]
async fn every_location_field_present_absent_and_mixed_round_trips() {
    let all_set = Location {
        page: Some(7),
        chapter: Some("chapter-3".into()),
        section_path: vec!["part".into(), "chapter-3".into(), "the section".into()],
        anchor: Some("id0042".into()),
        paragraph: Some(11),
        char_start: Some(120),
        char_end: Some(480),
        native_location: Some("epub:OEBPS/part-3.html#id0042".into()),
    };
    let all_absent = Location::default();
    let mixed = Location {
        page: Some(0),
        chapter: None,
        section_path: vec!["only".into()],
        anchor: Some(String::new()),
        paragraph: Some(0),
        char_start: Some(0),
        char_end: None,
        native_location: Some(String::new()),
    };
    let document = document(vec![
        root(0, "All set", all_set),
        root(1, "All absent", all_absent),
        root(2, "Mixed", mixed),
    ]);
    assert_locations_round_trip(&document).await;
}

#[tokio::test]
async fn non_ascii_and_empty_section_paths_round_trip() {
    let non_ascii = Location {
        page: Some(1),
        chapter: Some("spine-2".into()),
        section_path: vec![
            String::new(),
            "第一章 宋史".into(),
            "🙂/😀".into(),
            "preamble".into(),
        ],
        anchor: Some("id00038".into()),
        paragraph: Some(3),
        char_start: Some(1),
        char_end: Some(2),
        native_location: Some("epub:OEBPS/第一章.html#id00038".into()),
    };
    let empty_path = Location {
        section_path: vec![String::new()],
        ..Default::default()
    };
    let document = document(vec![
        root(0, "A non-ASCII path", non_ascii),
        root(1, "An empty segment", empty_path),
    ]);
    assert_locations_round_trip(&document).await;
}

#[tokio::test]
async fn a_path_equal_to_the_title_round_trips_through_the_elision() {
    // 403 of 宋史's 543 rows look like this: the title is the whole one-element path.
    let title = "帝性孝友節儉，質任自然，不事矯飾。受禪之初，頗好微行，或諫其輕出。";
    let elided = Location {
        page: None,
        chapter: Some("spine-2".into()),
        section_path: vec![title.to_string()],
        anchor: Some("id00038".into()),
        paragraph: None,
        char_start: None,
        char_end: None,
        native_location: Some("epub:OEBPS/6621_24183-0-0.txt.html#id00038".into()),
    };
    // The same title in a path that is not exactly `[title]` is written out, not rebuilt.
    let written = Location {
        section_path: vec!["document".into()],
        ..elided.clone()
    };
    let document = document(vec![
        root(0, title, elided),
        root(1, title, written),
        root(2, "", Location::default()),
    ]);
    assert_locations_round_trip(&document).await;
}

#[tokio::test]
async fn maximum_numbers_round_trip() {
    let maxima = Location {
        page: Some(u32::MAX),
        chapter: Some("spine-65535".into()),
        section_path: vec!["maximum".into()],
        anchor: Some("id-max".into()),
        paragraph: Some(u32::MAX),
        char_start: Some(usize::MAX),
        char_end: Some(usize::MAX - 1),
        native_location: Some("pdf:page-max".into()),
    };
    let document = document(vec![root(0, "Maximums", maxima)]);
    assert_locations_round_trip(&document).await;
}

/// `roots` roots of 99 children each, so the document holds 20,000 sections.
fn wide_document(roots: usize) -> Document {
    let roots = (0..roots)
        .map(|root| {
            let children = (0..99)
                .map(|child| Section {
                    id: SectionId(format!("section://r{root}-c{child}")),
                    parent_id: Some(SectionId(format!("section://r{root}"))),
                    title: format!("Child {child}"),
                    level: 2,
                    content: format!("Body {root}.{child}."),
                    location: Location {
                        page: Some(child as u32),
                        section_path: vec![format!("root {root}"), format!("child {child}")],
                        ..Default::default()
                    },
                    children: Vec::new(),
                })
                .collect();
            Section {
                id: SectionId(format!("section://r{root}")),
                parent_id: None,
                title: format!("Root {root}"),
                level: 1,
                content: format!("Body {root}."),
                location: Location {
                    page: Some(0),
                    section_path: vec![format!("root {root}")],
                    ..Default::default()
                },
                children,
            }
        })
        .collect();
    document(roots)
}

#[tokio::test]
async fn section_by_id_is_flat_on_a_20000_section_document() {
    let document = wide_document(200);
    assert_eq!(document.section_count(), 20_000);
    let (directory, repository) = open_repository().await;
    repository.save(document.clone()).await.expect("save");
    let database = directory.path().join("section-locations.sqlite");
    let connection = rusqlite::Connection::open(&database).expect("database should open");

    // The production `section_by_id` query: the id is found through the document and section
    // hashes, then compared.
    let sql =
        "SELECT section_key, section_id, parent_key, level, title, location_json, subtree_end_key
                 FROM document_sections_v1 WHERE doc_hash = ?1 AND id_hash = ?2";

    let plan: Vec<String> = connection
        .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
        .expect("plan should prepare")
        .query_map(
            rusqlite::params![rusqlite::types::Value::Null, rusqlite::types::Value::Null],
            |row| row.get::<_, String>(3),
        )
        .expect("plan should run")
        .collect::<Result<Vec<_>, _>>()
        .expect("plan rows should decode");
    let plan = plan.join(" | ");
    assert!(
        !plan
            .to_ascii_uppercase()
            .contains("SCAN DOCUMENT_SECTIONS_V1"),
        "section_by_id must not scan document_sections_v1: {plan}"
    );
    assert!(
        plan.to_ascii_uppercase()
            .contains("IDX_DOCUMENT_SECTIONS_V1_ID"),
        "section_by_id must use the id hash index: {plan}"
    );

    // The first and the last section cost the same statement steps: the lookup is one index seek
    // and one row read, not a walk over the document's 20,000 rows.
    let (first_doc, first_hash, first_key): (i64, i64, i64) = connection
        .query_row(
            "SELECT doc_hash, id_hash, section_key FROM document_sections_v1
              WHERE document_id = ?1 ORDER BY section_key LIMIT 1",
            rusqlite::params![document.id.0],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("first hash");
    let (last_doc, last_hash, last_key): (i64, i64, i64) = connection
        .query_row(
            "SELECT doc_hash, id_hash, section_key FROM document_sections_v1
              WHERE document_id = ?1 ORDER BY section_key DESC LIMIT 1",
            rusqlite::params![document.id.0],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("last hash");
    assert_eq!(first_doc, last_doc, "one document, one doc hash");
    assert_ne!(first_key, last_key);

    let mut statement = connection.prepare(sql).expect("lookup should prepare");
    let steps = |statement: &mut rusqlite::Statement<'_>, doc: i64, hash: i64| {
        let rows = statement
            .query_map(rusqlite::params![doc, hash], |row| row.get::<_, i64>(0))
            .expect("lookup should run")
            .collect::<Result<Vec<_>, _>>()
            .expect("lookup rows should decode");
        assert_eq!(rows.len(), 1, "one row per section id");
        statement.get_status(rusqlite::StatementStatus::VmStep)
    };
    // `VmStep` is cumulative for the prepared statement, so the second run's cost is the delta.
    let first_steps = steps(&mut statement, first_doc, first_hash);
    let after_second = steps(&mut statement, last_doc, last_hash);
    let last_steps = after_second - first_steps;
    assert_eq!(
        first_steps, last_steps,
        "the first and last section cost the same steps"
    );
    assert!(
        first_steps <= 50,
        "section_by_id takes {first_steps} VM steps, not a scan"
    );
}
