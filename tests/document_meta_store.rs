//! `document_json` keeps only the light metadata entries and `document_meta_v1` the heavy ones, and
//! the light, single-entry and single-section reads see the same document the full read does.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use reading_mcp::application::open_document::{OpenDocumentCommand, OpenDocumentUseCase};
use reading_mcp::application::ports::{
    ApplicationError, DocumentRepository, Parser, RetrievalOptions, RetrievedResource, Retriever,
    SourcePolicy,
};
use reading_mcp::domain::{
    ContentHash, Document, DocumentId, DocumentSource, Location, MediaType, NORMALIZATION_VERSION,
    Section, SectionId,
};
use reading_mcp::infrastructure::{
    SqliteDocumentRepository, SqliteSearchIndex, SqliteTextUnitIndex,
};
use rusqlite::{Connection, params};
use serde_json::{Value, json};
use tempfile::tempdir;

/// A metadata value that is "heavy": longer than the 1024-byte threshold, shaped like the big maps
/// a real book carries.
fn heavy_value() -> String {
    format!("{{\"blocks\":\"{}\"}}", "b".repeat(2000))
}

fn document() -> Document {
    Document {
        id: DocumentId("doc:meta".into()),
        source: DocumentSource("memory:meta.md".into()),
        title: "Meta".into(),
        media_type: MediaType("text/markdown".into()),
        content_hash: ContentHash("sha256:meta".into()),
        metadata: BTreeMap::from([
            ("heavy_map".to_string(), heavy_value()),
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
            children: vec![
                Section {
                    id: SectionId("section://one".into()),
                    parent_id: Some(SectionId("section://part".into())),
                    title: "One".into(),
                    level: 2,
                    content: "Chapter one text.".into(),
                    location: Location::default(),
                    children: vec![],
                },
                Section {
                    id: SectionId("section://two".into()),
                    parent_id: Some(SectionId("section://part".into())),
                    title: "Two".into(),
                    level: 2,
                    content: "Chapter two text.".into(),
                    location: Location::default(),
                    children: vec![],
                },
            ],
        }],
    }
}

struct AllowAllPolicy;

#[async_trait]
impl SourcePolicy for AllowAllPolicy {
    async fn validate(&self, _source: &DocumentSource) -> Result<(), ApplicationError> {
        Ok(())
    }
}

struct StaticRetriever;

#[async_trait]
impl Retriever for StaticRetriever {
    async fn retrieve(
        &self,
        source: &DocumentSource,
        _options: &RetrievalOptions,
    ) -> Result<RetrievedResource, ApplicationError> {
        Ok(RetrievedResource {
            source: source.clone(),
            final_source: source.clone(),
            media_type: MediaType("text/markdown".into()),
            bytes: b"# Meta\n\nPart intro.\n\n## One\n\nChapter one text.\n\n## Two\n\nChapter two text.\n".to_vec(),
            etag: None,
            last_modified: None,
            metadata: BTreeMap::new(),
        })
    }
}

/// Returns one fixed parsed document, so the stored document is exactly the one the test compares.
struct StaticParser(Arc<Document>);

#[async_trait]
impl Parser for StaticParser {
    async fn parse(&self, _resource: RetrievedResource) -> Result<Document, ApplicationError> {
        Ok((*self.0).clone())
    }
}

/// Ingests [`document`] through the open use case over a SQLite repository, text-unit store and
/// search index: the ingest path of the runtime, minus the parser.
async fn open_document(directory: &Path) -> (PathBuf, Document) {
    let database = directory.join("state.sqlite");
    let document = document();
    let repository = Arc::new(SqliteDocumentRepository::open(&database).expect("repository"));
    let text_unit_index: Arc<dyn reading_mcp::application::ports::TextUnitIndex> =
        Arc::new(SqliteTextUnitIndex::open(&database).expect("text unit index"));
    let search_index = Arc::new(SqliteSearchIndex::open(&database).expect("search index"));
    let open = OpenDocumentUseCase::with_text_unit_index(
        Arc::new(AllowAllPolicy),
        Arc::new(StaticRetriever),
        Arc::new(StaticParser(Arc::new(document.clone()))),
        repository,
        text_unit_index,
        search_index,
    );
    open.execute(OpenDocumentCommand {
        source: DocumentSource("memory:meta.md".into()),
        options: RetrievalOptions::default(),
    })
    .await
    .expect("document should open");
    (database, document)
}

fn stored_json_text(database: &Path, document_id: &DocumentId) -> String {
    let connection = Connection::open(database).expect("database should open");
    connection
        .query_row(
            "SELECT document_json FROM documents WHERE id = ?1",
            params![&document_id.0],
            |row| row.get(0),
        )
        .expect("stored document should exist")
}

fn meta_rows(database: &Path, document_id: &DocumentId) -> Vec<(String, String)> {
    let connection = Connection::open(database).expect("database should open");
    let mut statement = connection
        .prepare("SELECT key, value FROM document_meta_v1 WHERE document_id = ?1 ORDER BY key")
        .expect("statement");
    statement
        .query_map(params![&document_id.0], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .expect("rows")
        .map(|row| row.expect("row"))
        .collect()
}

fn update_json(database: &Path, document_id: &DocumentId, edit: impl FnOnce(&mut Value)) {
    let mut stored: Value =
        serde_json::from_str(&stored_json_text(database, document_id)).expect("stored JSON");
    edit(&mut stored);
    let connection = Connection::open(database).expect("database should open");
    connection
        .execute(
            "UPDATE documents SET document_json = ?1 WHERE id = ?2",
            params![stored.to_string(), &document_id.0],
        )
        .expect("document_json should update");
}

/// Every section's `id`, `title`, `level` and `parent_id`, in depth-first order: the tree without
/// the text.
fn tree_shape(document: &Document) -> Vec<(String, String, u8, Option<String>)> {
    fn walk(section: &Section, out: &mut Vec<(String, String, u8, Option<String>)>) {
        out.push((
            section.id.0.clone(),
            section.title.clone(),
            section.level,
            section.parent_id.as_ref().map(|id| id.0.clone()),
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
async fn the_light_row_round_trips() {
    let directory = tempdir().expect("temporary directory");
    let (database, document) = open_document(directory.path()).await;
    let reopened = SqliteDocumentRepository::open(&database).expect("repository should reopen");

    let (full, full_hash) = reopened
        .get_with_hash(&document.id)
        .await
        .expect("document should load")
        .expect("document should exist");
    assert_eq!(
        *full, document,
        "the full read must equal the saved document"
    );
    assert_eq!(full_hash, document.normalized_document_hash());

    let (light, light_hash) = reopened
        .get_light_with_hash(&document.id)
        .await
        .expect("light read should succeed")
        .expect("document should exist");
    assert_eq!(light_hash, full_hash);
    assert_no_content(&light);
    assert_eq!(light.id, document.id);
    assert_eq!(light.source, document.source);
    assert_eq!(light.title, document.title);
    assert_eq!(light.media_type, document.media_type);
    assert_eq!(light.content_hash, document.content_hash);
    assert_eq!(tree_shape(&light), tree_shape(&document));
    assert_eq!(
        light.metadata,
        BTreeMap::from([
            ("light".to_string(), "small".to_string()),
            ("content_encoding".to_string(), "utf-8".to_string()),
        ]),
        "the light read must carry only the light metadata entries"
    );

    assert_eq!(
        reopened
            .load_metadata(&document.id, "heavy_map")
            .await
            .expect("metadata should load"),
        Some(heavy_value())
    );
    assert_eq!(
        reopened
            .load_metadata(&document.id, "light")
            .await
            .expect("metadata should load"),
        Some("small".to_string())
    );
    assert_eq!(
        reopened
            .load_metadata(&document.id, "missing")
            .await
            .expect("metadata should load"),
        None
    );

    assert_eq!(
        reopened
            .load_section_text(&document.id, &SectionId("section://one".into()))
            .await
            .expect("section text should load"),
        Some("Chapter one text.".to_string())
    );
    assert_eq!(
        reopened
            .load_section_text(&document.id, &SectionId("section://part".into()))
            .await
            .expect("section text should load"),
        Some("Part intro.".to_string()),
        "a parent section's text is its own, not its subtree's"
    );
    assert_eq!(
        reopened
            .load_section_text(&document.id, &SectionId("section://unknown".into()))
            .await
            .expect("section text should load"),
        None
    );

    let stored = stored_json_text(&database, &document.id);
    assert!(
        !stored.contains(&heavy_value()),
        "document_json must not carry the heavy value"
    );
    assert!(
        stored.len() < heavy_value().len(),
        "document_json ({} bytes) must be smaller than the heavy value ({} bytes)",
        stored.len(),
        heavy_value().len()
    );
    assert_eq!(
        meta_rows(&database, &document.id),
        vec![("heavy_map".to_string(), heavy_value())]
    );
}

#[tokio::test]
async fn an_old_inline_row_agrees_on_every_read() {
    let directory = tempdir().expect("temporary directory");
    let database = directory.path().join("state.sqlite");
    let document = document();
    let repository = SqliteDocumentRepository::open(&database).expect("repository should open");
    drop(repository);

    // The row shape `main` writes: every metadata entry and every section's text inline, and no
    // `metadata_store` key.
    let stored = json!({
        "normalization_version": NORMALIZATION_VERSION,
        "id": document.id.0,
        "source": document.source.0,
        "title": document.title,
        "media_type": document.media_type.0,
        "content_hash": document.content_hash.0,
        "metadata": document.metadata,
        "root_sections": [{
            "id": "section://part",
            "parent_id": null,
            "title": "Part",
            "level": 1,
            "content": "Part intro.",
            "location": {
                "page": null, "chapter": null, "section_path": [], "anchor": null,
                "paragraph": null, "char_start": null, "char_end": null, "native_location": null
            },
            "children": [],
        }],
    });
    let connection = Connection::open(&database).expect("database should open");
    connection
        .execute(
            "INSERT INTO documents(id, source, content_hash, document_json)
             VALUES (?1, ?2, ?3, ?4)",
            params![
                &document.id.0,
                &document.source.0,
                &document.content_hash.0,
                stored.to_string()
            ],
        )
        .expect("old row should insert");
    drop(connection);

    let reopened = SqliteDocumentRepository::open(&database).expect("repository should reopen");
    let (full, full_hash) = reopened
        .get_with_hash(&document.id)
        .await
        .expect("old row should load")
        .expect("old row should exist");
    assert_eq!(full.metadata, document.metadata);
    assert_eq!(full.root_sections[0].content, "Part intro.");

    let (light, light_hash) = reopened
        .get_light_with_hash(&document.id)
        .await
        .expect("old row should load light")
        .expect("old row should exist");
    assert_eq!(light_hash, full_hash);
    assert_no_content(&light);
    assert_eq!(tree_shape(&light), tree_shape(&full));
    assert_eq!(
        light.metadata,
        BTreeMap::from([
            ("light".to_string(), "small".to_string()),
            ("content_encoding".to_string(), "utf-8".to_string()),
        ]),
        "an old row's light form is its inline metadata split the way a save splits it"
    );

    assert_eq!(
        reopened
            .load_metadata(&document.id, "heavy_map")
            .await
            .expect("metadata should load"),
        Some(heavy_value())
    );
    assert_eq!(
        reopened
            .load_metadata(&document.id, "missing")
            .await
            .expect("metadata should load"),
        None
    );
}

#[tokio::test]
async fn replacing_a_document_removes_its_old_metadata_rows() {
    let directory = tempdir().expect("temporary directory");
    let database = directory.path().join("state.sqlite");
    let repository = SqliteDocumentRepository::open(&database).expect("repository should open");

    let mut first = document();
    first.metadata.insert("heavy_one".into(), heavy_value());
    repository.save(first.clone()).await.expect("first save");
    assert_eq!(meta_rows(&database, &first.id).len(), 2);

    let mut second = document();
    second.metadata = BTreeMap::from([
        ("heavy_two".to_string(), heavy_value()),
        ("light".to_string(), "other".to_string()),
    ]);
    repository.save(second.clone()).await.expect("second save");
    assert_eq!(
        meta_rows(&database, &second.id),
        vec![("heavy_two".to_string(), heavy_value())],
        "the previous heavy entries must be gone"
    );

    let third = Document {
        metadata: BTreeMap::from([("light".to_string(), "only".to_string())]),
        ..document()
    };
    repository.save(third.clone()).await.expect("third save");
    assert!(
        meta_rows(&database, &third.id).is_empty(),
        "a save with no heavy entry must leave no metadata row"
    );
    let stored = stored_json_text(&database, &third.id);
    assert!(
        !stored.contains("metadata_store"),
        "a row whose metadata is all light must not name a metadata store: {stored}"
    );
}

#[tokio::test]
async fn an_unknown_metadata_store_is_stale() {
    let directory = tempdir().expect("temporary directory");
    let (database, document) = open_document(directory.path()).await;
    update_json(&database, &document.id, |stored| {
        stored["metadata_store"] = json!("document_meta_v2");
    });

    let reopened = SqliteDocumentRepository::open(&database).expect("repository should reopen");
    let errors = [
        reopened
            .get_with_hash(&document.id)
            .await
            .expect_err("an unknown metadata store must not load"),
        reopened
            .get_light_with_hash(&document.id)
            .await
            .expect_err("an unknown metadata store must not load light"),
        reopened
            .load_metadata(&document.id, "light")
            .await
            .expect_err("an unknown metadata store must not name an entry"),
    ];
    for error in errors {
        assert!(
            matches!(error, ApplicationError::StaleDocument(_)),
            "expected StaleDocument, got {error:?}"
        );
        assert!(
            error.to_string().contains("unknown store document_meta_v2"),
            "stale message should name the removed store: {error}"
        );
        assert!(
            error
                .to_string()
                .ends_with("explicit source reopen required"),
            "stale message should ask for a reopen: {error}"
        );
    }
}

#[tokio::test]
async fn ocr_metadata_makes_the_light_read_take_the_full_path() {
    let directory = tempdir().expect("temporary directory");
    let (database, document) = open_document(directory.path()).await;

    // An OCR evidence blob with no derivation: `validate_ocr_publication` rejects the document, so
    // both reads must report the same error. The blob is light, so it stays inline.
    update_json(&database, &document.id, |stored| {
        stored["metadata"]["ocr_evidence_blob"] = json!("not-a-derivation");
    });
    let reopened = SqliteDocumentRepository::open(&database).expect("repository should reopen");
    let full_error = reopened
        .get_with_hash(&document.id)
        .await
        .expect_err("an invalid OCR publication must not load");
    let light_error = reopened
        .get_light_with_hash(&document.id)
        .await
        .expect_err("an invalid OCR publication must not load light either");
    assert_eq!(full_error.to_string(), light_error.to_string());

    // The same blob stored as a heavy entry in `document_meta_v1`: the light map does not show it,
    // so the light read must look at the table to notice it.
    update_json(&database, &document.id, |stored| {
        stored["metadata"]
            .as_object_mut()
            .expect("metadata object")
            .remove("ocr_evidence_blob");
    });
    let connection = Connection::open(&database).expect("database should open");
    connection
        .execute(
            "INSERT INTO document_meta_v1(document_id, key, value) VALUES (?1, ?2, ?3)",
            params![&document.id.0, "ocr_evidence_blob", "e".repeat(2000)],
        )
        .expect("heavy OCR row should insert");
    drop(connection);
    let full_error = reopened
        .get_with_hash(&document.id)
        .await
        .expect_err("a heavy invalid OCR publication must not load");
    let light_error = reopened
        .get_light_with_hash(&document.id)
        .await
        .expect_err("a heavy invalid OCR publication must not load light either");
    assert_eq!(full_error.to_string(), light_error.to_string());
}

#[tokio::test]
async fn the_light_read_never_touches_the_document_cache() {
    let directory = tempdir().expect("temporary directory");
    let database = directory.path().join("state.sqlite");
    let document = document();
    let repository =
        SqliteDocumentRepository::open_with_cache_capacity(&database, 8).expect("repository");
    repository
        .save(document.clone())
        .await
        .expect("document should save");

    let (light, _) = repository
        .get_light_with_hash(&document.id)
        .await
        .expect("light read")
        .expect("document should exist");
    let (first, _) = repository
        .get_with_hash(&document.id)
        .await
        .expect("full read")
        .expect("document should exist");
    assert!(
        !Arc::ptr_eq(&light, &first),
        "the light read must not put its document into the cache"
    );
    let (second, _) = repository
        .get_with_hash(&document.id)
        .await
        .expect("full read")
        .expect("document should exist");
    assert!(
        Arc::ptr_eq(&first, &second),
        "the full read must fill the cache, so this one is a hit"
    );

    // With the cache off, the light read works and still fills nothing.
    let uncached =
        SqliteDocumentRepository::open_with_cache_capacity(&database, 0).expect("repository");
    let (light, _) = uncached
        .get_light_with_hash(&document.id)
        .await
        .expect("light read")
        .expect("document should exist");
    assert_no_content(&light);
    let (a, _) = uncached
        .get_with_hash(&document.id)
        .await
        .expect("full read")
        .expect("document should exist");
    let (b, _) = uncached
        .get_with_hash(&document.id)
        .await
        .expect("full read")
        .expect("document should exist");
    assert!(
        !Arc::ptr_eq(&a, &b),
        "a capacity of 0 must never return a cached document"
    );
}
