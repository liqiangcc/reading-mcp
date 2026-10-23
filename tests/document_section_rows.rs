//! The section tree and the body order as rows (SEC-1).
//!
//! Every expected value is computed here from the `Document` itself with an independent walk of
//! `root_sections`, never from the repository calls under test, so the rows are proven to carry
//! exactly the tree and the body order the document holds.

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use reading_mcp::application::ports::{
    ApplicationError, BodyOrderWindow, DocumentHeader, DocumentRepository, SectionRow,
};
use reading_mcp::domain::{
    ContentHash, Document, DocumentId, DocumentSource, Location, MediaType, Section, SectionId,
};
use reading_mcp::infrastructure::{InMemoryDocumentRepository, ObservedDocumentRepository};

fn section(id: &str, parent: Option<&str>, level: u8, children: Vec<Section>) -> Section {
    Section {
        id: SectionId(format!("section://{id}")),
        parent_id: parent.map(|parent| SectionId(format!("section://{parent}"))),
        title: format!("Title {id}"),
        level,
        content: format!("Body of {id}."),
        location: Location {
            page: Some(level as u32),
            chapter: Some(format!("chapter {id}")),
            section_path: vec![id.to_string()],
            ..Default::default()
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
    let mut deep = section("deep-3", Some("deep-2"), 4, vec![]);
    deep = section("deep-2", Some("deep-1"), 3, vec![deep]);
    deep = section("deep-1", Some("deep-0"), 2, vec![deep]);
    deep = section("deep-0", Some("first"), 1, vec![deep]);
    document(
        "nested",
        "text/markdown",
        vec![
            section(
                "first",
                None,
                1,
                vec![
                    section(
                        "second",
                        Some("first"),
                        2,
                        vec![section("third", Some("second"), 3, vec![])],
                    ),
                    deep,
                    section("last", Some("first"), 2, vec![]),
                ],
            ),
            section(
                "root-b",
                None,
                1,
                vec![section("b-1", Some("root-b"), 2, vec![])],
            ),
        ],
    )
}

fn single_section_document() -> Document {
    document(
        "single",
        "text/plain",
        vec![section("only", None, 1, vec![])],
    )
}

/// `roots` roots of 99 children each.
fn many_section_document(roots: usize) -> Document {
    let roots = (0..roots)
        .map(|root| {
            let children = (0..99)
                .map(|child| {
                    section(
                        &format!("r{root}-c{child}"),
                        Some(&format!("r{root}")),
                        2,
                        vec![],
                    )
                })
                .collect();
            section(&format!("r{root}"), None, 1, children)
        })
        .collect();
    document("many", "text/markdown", roots)
}

const VALID_MAP: &str = r#"{"schema_version":"epub-structure-reconciliation/v1","sections":[{"section_id":"section://second","source_order":0},{"section_id":"section://first","source_order":1},{"section_id":"section://nested","source_order":2}]}"#;

/// A map whose source order is not contiguous, which `body_order_ids` rejects.
const INVALID_MAP: &str = r#"{"schema_version":"epub-structure-reconciliation/v1","sections":[{"section_id":"section://second","source_order":0},{"section_id":"section://first","source_order":5},{"section_id":"section://nested","source_order":2}]}"#;

/// An EPUB whose structure map puts `second` before `first`, with `nested` under `first`.
fn epub_document(structure_map: &str) -> Document {
    let mut document = document(
        "epub",
        "application/epub+zip",
        vec![
            section(
                "first",
                None,
                1,
                vec![section("nested", Some("first"), 2, vec![])],
            ),
            section("second", None, 1, vec![]),
        ],
    );
    document
        .metadata
        .insert("epub_structure_map".into(), structure_map.into());
    document
}

/// A PDF whose sections are pages: the body order is depth-first document order.
fn pdf_document() -> Document {
    document(
        "pdf",
        "application/pdf",
        vec![
            section("page-1", None, 1, vec![]),
            section(
                "page-2",
                None,
                1,
                vec![section("page-2-a", Some("page-2"), 2, vec![])],
            ),
            section("page-3", None, 1, vec![]),
        ],
    )
}

/// The depth-first walk of `document` as `(id, parent id, level, title, location)`.
fn dfs(document: &Document) -> Vec<(String, Option<String>, u8, String, Location)> {
    fn walk(
        section: &Section,
        parent: Option<&str>,
        out: &mut Vec<(String, Option<String>, u8, String, Location)>,
    ) {
        out.push((
            section.id.0.clone(),
            parent.map(str::to_owned),
            section.level,
            section.title.clone(),
            section.location.clone(),
        ));
        for child in &section.children {
            walk(child, Some(&section.id.0), out);
        }
    }
    let mut out = Vec::new();
    for root in &document.root_sections {
        walk(root, None, &mut out);
    }
    out
}

fn subtree_size(section: &Section) -> usize {
    1 + section.children.iter().map(subtree_size).sum::<usize>()
}

/// The section with `id`, from the document.
fn find<'a>(document: &'a Document, id: &str) -> &'a Section {
    document
        .find_section(&SectionId(format!("section://{id}")))
        .unwrap_or_else(|| panic!("document holds section {id}"))
}

async fn open_repository() -> (
    tempfile::TempDir,
    reading_mcp::infrastructure::SqliteDocumentRepository,
) {
    let directory = tempfile::tempdir().expect("temp directory");
    let path = directory.path().join("section-rows.sqlite");
    let repository = reading_mcp::infrastructure::SqliteDocumentRepository::open(&path)
        .expect("repository should open");
    (directory, repository)
}

/// The stored rows, in key order, as the flattening of the document must be.
async fn rows(repository: &dyn DocumentRepository, document: &Document) -> Vec<SectionRow> {
    repository
        .sections_in_key_range(&document.id, 0, i64::MAX, usize::MAX)
        .await
        .expect("sections_in_key_range should succeed")
}

async fn assert_rows_reproduce_the_tree(document: &Document) {
    let (_directory, repository) = open_repository().await;
    repository.save(document.clone()).await.expect("save");

    let expected = dfs(document);
    let stored = rows(&repository, document).await;
    assert_eq!(stored.len(), expected.len(), "one row per section");

    let actual: Vec<(String, Option<String>, u8, String, Location)> = stored
        .iter()
        .map(|row| {
            let parent = row
                .parent_key
                .map(|key| stored[usize::try_from(key).expect("key fits")].id.0.clone());
            (
                row.id.0.clone(),
                parent,
                row.level,
                row.title.clone(),
                row.location.clone(),
            )
        })
        .collect();
    assert_eq!(actual, expected, "rows in depth-first order equal the tree");

    for (index, row) in stored.iter().enumerate() {
        let key = i64::try_from(index).expect("key fits");
        assert_eq!(row.section_key, key, "keys are the depth-first index");
        let size = subtree_size(find(document, &row.id.0["section://".len()..]));
        assert_eq!(
            row.subtree_end_key,
            key + i64::try_from(size).expect("size fits") - 1,
            "subtree_end_key closes the subtree of {}",
            row.id.0
        );

        let by_id = repository
            .section_by_id(&document.id, &row.id)
            .await
            .expect("section_by_id should succeed")
            .expect("section_by_id finds the row");
        assert_eq!(&by_id, row, "section_by_id agrees with the key range");
        let by_key = repository
            .section_by_key(&document.id, row.section_key)
            .await
            .expect("section_by_key should succeed")
            .expect("section_by_key finds the row");
        assert_eq!(&by_key, row, "section_by_key agrees with the key range");

        let section = find(document, &row.id.0["section://".len()..]);
        let children: Vec<SectionId> = repository
            .section_children(&document.id, Some(row.section_key), usize::MAX)
            .await
            .expect("section_children should succeed")
            .into_iter()
            .map(|child| child.id)
            .collect();
        assert_eq!(
            children,
            section
                .children
                .iter()
                .map(|child| child.id.clone())
                .collect::<Vec<_>>(),
            "children of {}",
            row.id.0
        );

        let mut expected_ancestors = Vec::new();
        let mut current = section.parent_id.clone();
        while let Some(parent) = current {
            let parent_section = find(document, &parent.0["section://".len()..]);
            current = parent_section.parent_id.clone();
            expected_ancestors.push(parent);
        }
        expected_ancestors.reverse();
        let ancestors: Vec<SectionId> = repository
            .section_ancestors(&document.id, row.section_key)
            .await
            .expect("section_ancestors should succeed")
            .into_iter()
            .map(|ancestor| ancestor.id)
            .collect();
        assert_eq!(ancestors, expected_ancestors, "ancestors of {}", row.id.0);
    }

    let root_ids: Vec<SectionId> = repository
        .section_children(&document.id, None, usize::MAX)
        .await
        .expect("root children should succeed")
        .into_iter()
        .map(|root| root.id)
        .collect();
    assert_eq!(
        root_ids,
        document
            .root_sections
            .iter()
            .map(|root| root.id.clone())
            .collect::<Vec<_>>(),
        "root sections"
    );

    let header = repository
        .document_header(&document.id)
        .await
        .expect("document_header should succeed")
        .expect("document_header finds the document");
    assert_eq!(
        header,
        DocumentHeader {
            id: document.id.clone(),
            source: document.source.clone(),
            title: document.title.clone(),
            media_type: document.media_type.clone(),
            content_hash: document.content_hash.clone(),
            normalized_document_hash: Some(document.normalized_document_hash()),
            section_count: expected.len(),
            root_count: document.root_sections.len(),
        }
    );
}

#[tokio::test]
async fn the_rows_reproduce_a_nested_tree() {
    assert_rows_reproduce_the_tree(&nested_document()).await;
}

#[tokio::test]
async fn the_rows_reproduce_a_single_section_document() {
    assert_rows_reproduce_the_tree(&single_section_document()).await;
}

#[tokio::test]
async fn the_rows_reproduce_a_ten_thousand_section_document() {
    let document = many_section_document(100);
    assert_eq!(document.section_count(), 10_000);
    assert_rows_reproduce_the_tree(&document).await;
}

/// The body-order window, position and length, against the order `body_order` reports.
async fn assert_body_order_rows(document: &Document) {
    let (_directory, repository) = open_repository().await;
    repository.save(document.clone()).await.expect("save");
    let order = repository
        .body_order(&document.id)
        .await
        .expect("body_order should succeed")
        .expect("the document has a body order")
        .expect("the body order is an order");
    let stored = rows(&repository, document).await;
    let key_of = |section_id: &SectionId| -> i64 {
        stored
            .iter()
            .find(|row| &row.id == section_id)
            .expect("the order names stored sections")
            .section_key
    };

    assert_eq!(
        repository
            .body_order_len(&document.id)
            .await
            .expect("body_order_len should succeed"),
        Some(order.len())
    );
    for (ord, section_id) in order.iter().enumerate() {
        assert_eq!(
            repository
                .body_order_at(&document.id, ord)
                .await
                .expect("body_order_at should succeed"),
            Some(key_of(section_id)),
            "position {ord}"
        );
    }
    assert_eq!(
        repository
            .body_order_at(&document.id, order.len())
            .await
            .expect("a position past the end should succeed"),
        None
    );

    for (ord, section_id) in order.iter().enumerate() {
        let section_key = key_of(section_id);
        let before = 2;
        let after = 1;
        let start = ord.saturating_sub(before);
        let end = (ord + after).min(order.len() - 1);
        let expected: Vec<(usize, i64)> = order[start..=end]
            .iter()
            .enumerate()
            .map(|(offset, id)| (start + offset, key_of(id)))
            .collect();
        assert_eq!(
            repository
                .body_order_window(&document.id, section_key, before, after)
                .await
                .expect("body_order_window should succeed"),
            Some(BodyOrderWindow {
                ord,
                len: order.len(),
                window: expected,
            }),
            "window around {}",
            section_id.0
        );
    }
}

#[tokio::test]
async fn the_body_order_rows_match_an_epub_structure_map() {
    assert_body_order_rows(&epub_document(VALID_MAP)).await;
}

#[tokio::test]
async fn the_body_order_rows_match_a_pdf_page_document() {
    assert_body_order_rows(&pdf_document()).await;
}

#[tokio::test]
async fn a_failed_body_order_writes_no_rows_and_reports_the_same_error() {
    let document = epub_document(INVALID_MAP);
    let (directory, repository) = open_repository().await;
    repository.save(document.clone()).await.expect("save");

    let expected = repository
        .body_order(&document.id)
        .await
        .expect("body_order should succeed")
        .expect("the document has a body order");
    let ApplicationError::RepositoryFailed(expected_message) =
        expected.expect_err("the structure map is rejected")
    else {
        panic!("the rejected body order is a repository failure");
    };

    for call in [
        repository
            .body_order_window(&document.id, 0, 1, 1)
            .await
            .map(|_| ()),
        repository.body_order_at(&document.id, 0).await.map(|_| ()),
        repository.body_order_len(&document.id).await.map(|_| ()),
    ] {
        match call {
            Err(ApplicationError::RepositoryFailed(message)) => {
                assert_eq!(&message, &expected_message, "same error as body_order");
            }
            other => panic!("expected the body order error, got {other:?}"),
        }
    }

    let database = directory.path().join("section-rows.sqlite");
    let connection = rusqlite::Connection::open(&database).expect("database should open");
    let body_order_rows: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM document_body_order_rows_v1 WHERE document_id = ?1",
            rusqlite::params![document.id.0],
            |row| row.get(0),
        )
        .expect("count should succeed");
    assert_eq!(body_order_rows, 0, "a failed body order writes no rows");
    let section_rows: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM document_sections_v1 WHERE document_id = ?1",
            rusqlite::params![document.id.0],
            |row| row.get(0),
        )
        .expect("count should succeed");
    assert_eq!(section_rows, 3, "the tree is still stored");
}

/// The new calls, in a form comparable across repositories.
#[derive(Debug, PartialEq)]
struct Answers {
    header: Option<DocumentHeader>,
    by_id: Vec<Option<SectionRow>>,
    by_key: Vec<Option<SectionRow>>,
    ancestors: Vec<Vec<SectionRow>>,
    windows: Vec<Option<BodyOrderWindow>>,
    at: Vec<Option<i64>>,
    len: Option<usize>,
}

async fn answers(repository: &dyn DocumentRepository, document: &Document) -> Answers {
    let stored = rows(repository, document).await;
    let mut by_id = Vec::new();
    let mut by_key = Vec::new();
    let mut ancestors = Vec::new();
    for row in &stored {
        by_id.push(
            repository
                .section_by_id(&document.id, &row.id)
                .await
                .expect("section_by_id"),
        );
        by_key.push(
            repository
                .section_by_key(&document.id, row.section_key)
                .await
                .expect("section_by_key"),
        );
        ancestors.push(
            repository
                .section_ancestors(&document.id, row.section_key)
                .await
                .expect("section_ancestors"),
        );
    }
    Answers {
        header: repository
            .document_header(&document.id)
            .await
            .expect("document_header"),
        by_id,
        by_key,
        ancestors,
        windows: vec![
            repository
                .body_order_window(&document.id, 0, 1, 1)
                .await
                .expect("body_order_window"),
            repository
                .body_order_window(&document.id, 1, 0, 3)
                .await
                .expect("body_order_window"),
        ],
        at: vec![
            repository
                .body_order_at(&document.id, 0)
                .await
                .expect("body_order_at"),
            repository
                .body_order_at(&document.id, 2)
                .await
                .expect("body_order_at"),
        ],
        len: repository
            .body_order_len(&document.id)
            .await
            .expect("body_order_len"),
    }
}

#[tokio::test]
async fn an_old_document_without_rows_answers_through_the_default_path() {
    let document = nested_document();
    let (directory, repository) = open_repository().await;
    repository.save(document.clone()).await.expect("save");

    // The shape of a document saved before this PR: the `documents` row exists and the three new
    // tables hold nothing for it.
    let database = directory.path().join("section-rows.sqlite");
    {
        let connection = rusqlite::Connection::open(&database).expect("database should open");
        for table in [
            "document_header_v1",
            "document_sections_v1",
            "document_body_order_rows_v1",
        ] {
            connection
                .execute(&format!("DELETE FROM {table}"), [])
                .expect("delete should succeed");
        }
    }

    let memory = InMemoryDocumentRepository::default();
    memory.save(document.clone()).await.expect("save");
    let expected = answers(&memory, &document).await;
    let actual = answers(&repository, &document).await;
    assert_eq!(actual, expected);
}

#[tokio::test]
async fn replacing_a_document_with_fewer_sections_leaves_no_orphan_rows() {
    let document = nested_document();
    let (directory, repository) = open_repository().await;
    repository.save(document.clone()).await.expect("save");
    let large_count = rows(&repository, &document).await.len();

    let mut smaller = document.clone();
    smaller.root_sections.truncate(1);
    smaller.root_sections[0].children.truncate(1);
    repository.save(smaller.clone()).await.expect("resave");
    let smaller_count = dfs(&smaller).len();
    assert!(smaller_count < large_count);

    assert_eq!(rows(&repository, &smaller).await.len(), smaller_count);
    let database = directory.path().join("section-rows.sqlite");
    let connection = rusqlite::Connection::open(&database).expect("database should open");
    for table in ["document_sections_v1", "document_body_order_rows_v1"] {
        let count: i64 = connection
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .expect("count should succeed");
        assert_eq!(
            usize::try_from(count).expect("count fits"),
            smaller_count,
            "no orphan rows in {table}"
        );
    }
    assert_eq!(
        repository
            .body_order_len(&smaller.id)
            .await
            .expect("body_order_len should succeed"),
        Some(smaller_count)
    );
}

/// A store that counts the calls it forwards, so a wrapper default cannot hide behind its own
/// implementation.
#[derive(Default)]
struct CountingStore {
    calls: std::sync::Mutex<Vec<&'static str>>,
    document: Option<Document>,
}

impl CountingStore {
    fn record(&self, call: &'static str) {
        self.calls.lock().expect("lock").push(call);
    }

    fn calls(&self) -> Vec<&'static str> {
        self.calls.lock().expect("lock").clone()
    }
}

#[async_trait]
impl DocumentRepository for CountingStore {
    async fn save(&self, _document: Document) -> Result<(), ApplicationError> {
        panic!("the wrapper must not save");
    }

    async fn get(&self, _id: &DocumentId) -> Result<Option<Arc<Document>>, ApplicationError> {
        self.record("get");
        Ok(self.document.clone().map(Arc::new))
    }

    async fn document_header(
        &self,
        _id: &DocumentId,
    ) -> Result<Option<DocumentHeader>, ApplicationError> {
        self.record("document_header");
        let Some(document) = &self.document else {
            return Ok(None);
        };
        Ok(Some(DocumentHeader {
            id: document.id.clone(),
            source: document.source.clone(),
            title: document.title.clone(),
            media_type: document.media_type.clone(),
            content_hash: document.content_hash.clone(),
            normalized_document_hash: Some(document.normalized_document_hash()),
            section_count: document.section_count(),
            root_count: document.root_sections.len(),
        }))
    }

    async fn section_by_id(
        &self,
        _id: &DocumentId,
        _section_id: &SectionId,
    ) -> Result<Option<SectionRow>, ApplicationError> {
        self.record("section_by_id");
        Ok(None)
    }

    async fn section_by_key(
        &self,
        _id: &DocumentId,
        _section_key: i64,
    ) -> Result<Option<SectionRow>, ApplicationError> {
        self.record("section_by_key");
        Ok(None)
    }

    async fn sections_in_key_range(
        &self,
        _id: &DocumentId,
        _from_key: i64,
        _to_key_inclusive: i64,
        _limit: usize,
    ) -> Result<Vec<SectionRow>, ApplicationError> {
        self.record("sections_in_key_range");
        Ok(Vec::new())
    }

    async fn section_children(
        &self,
        _id: &DocumentId,
        _parent_key: Option<i64>,
        _limit: usize,
    ) -> Result<Vec<SectionRow>, ApplicationError> {
        self.record("section_children");
        Ok(Vec::new())
    }

    async fn section_ancestors(
        &self,
        _id: &DocumentId,
        _section_key: i64,
    ) -> Result<Vec<SectionRow>, ApplicationError> {
        self.record("section_ancestors");
        Ok(Vec::new())
    }

    async fn body_order_window(
        &self,
        _id: &DocumentId,
        _section_key: i64,
        _before: usize,
        _after: usize,
    ) -> Result<Option<BodyOrderWindow>, ApplicationError> {
        self.record("body_order_window");
        Ok(None)
    }

    async fn body_order_at(
        &self,
        _id: &DocumentId,
        _ord: usize,
    ) -> Result<Option<i64>, ApplicationError> {
        self.record("body_order_at");
        Ok(None)
    }

    async fn body_order_len(&self, _id: &DocumentId) -> Result<Option<usize>, ApplicationError> {
        self.record("body_order_len");
        Ok(None)
    }
}

#[tokio::test]
async fn the_observed_wrapper_forwards_every_section_row_call() {
    let document = nested_document();
    let store = Arc::new(CountingStore {
        calls: std::sync::Mutex::new(Vec::new()),
        document: Some(document.clone()),
    });
    let observed = ObservedDocumentRepository::new(store.clone());
    let id = &document.id;

    let _ = observed.document_header(id).await.expect("document_header");
    let _ = observed
        .section_by_id(id, &SectionId("section://first".into()))
        .await
        .expect("section_by_id");
    let _ = observed
        .section_by_key(id, 0)
        .await
        .expect("section_by_key");
    let _ = observed
        .sections_in_key_range(id, 0, 10, 10)
        .await
        .expect("sections_in_key_range");
    let _ = observed
        .section_children(id, None, 10)
        .await
        .expect("section_children");
    let _ = observed
        .section_ancestors(id, 1)
        .await
        .expect("section_ancestors");
    let _ = observed
        .body_order_window(id, 0, 1, 1)
        .await
        .expect("body_order_window");
    let _ = observed.body_order_at(id, 0).await.expect("body_order_at");
    let _ = observed.body_order_len(id).await.expect("body_order_len");

    assert_eq!(
        store.calls(),
        vec![
            "document_header",
            "section_by_id",
            "section_by_key",
            "sections_in_key_range",
            "section_children",
            "section_ancestors",
            "body_order_window",
            "body_order_at",
            "body_order_len",
        ],
        "every call reaches the store instead of a wrapper default"
    );
    assert!(
        !store.calls().contains(&"get"),
        "no call loads the whole document"
    );
}

#[tokio::test]
async fn the_section_row_queries_use_indexes_and_never_scan_the_table() {
    let document = many_section_document(200);
    assert_eq!(document.section_count(), 20_000);
    let (directory, repository) = open_repository().await;
    repository.save(document.clone()).await.expect("save");
    let database = directory.path().join("section-rows.sqlite");
    let connection = rusqlite::Connection::open(&database).expect("database should open");

    let queries = [
        (
            "section_by_id",
            "SELECT section_key, section_id, parent_key, level, title, location_json, subtree_end_key
               FROM document_sections_v1 WHERE document_id = ?1 AND section_id = ?2",
        ),
        (
            "section_by_key",
            "SELECT section_key, section_id, parent_key, level, title, location_json, subtree_end_key
               FROM document_sections_v1 WHERE document_id = ?1 AND section_key = ?2",
        ),
        (
            "sections_in_key_range",
            "SELECT section_key, section_id, parent_key, level, title, location_json, subtree_end_key
               FROM document_sections_v1 WHERE document_id = ?1 AND section_key BETWEEN ?2 AND ?3
               ORDER BY section_key LIMIT ?4",
        ),
        (
            "section_children (roots)",
            "SELECT section_key, section_id, parent_key, level, title, location_json, subtree_end_key
               FROM document_sections_v1 WHERE document_id = ?1 AND parent_key IS NULL
               ORDER BY section_key LIMIT ?2",
        ),
        (
            "section_children",
            "SELECT section_key, section_id, parent_key, level, title, location_json, subtree_end_key
               FROM document_sections_v1 WHERE document_id = ?1 AND parent_key = ?2
               ORDER BY section_key LIMIT ?3",
        ),
        (
            "body_order_window",
            "SELECT ord, section_key FROM document_body_order_rows_v1
              WHERE document_id = ?1 AND section_key = ?2",
        ),
    ];

    for (label, sql) in queries {
        let plan: Vec<String> = connection
            .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
            .expect("query should prepare")
            .query_map([], |row| row.get::<_, String>(3))
            .expect("plan should run")
            .collect::<Result<Vec<_>, _>>()
            .expect("plan rows should decode");
        let plan = plan.join(" | ");
        assert!(
            !plan
                .to_ascii_uppercase()
                .contains("SCAN DOCUMENT_SECTIONS_V1"),
            "{label} must not scan document_sections_v1: {plan}"
        );
        assert!(
            plan.to_ascii_uppercase().contains("USING"),
            "{label} must use an index: {plan}"
        );
    }
}
