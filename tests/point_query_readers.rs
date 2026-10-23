//! The hot readers answer from the SEC-1 point-query rows (SEC-2).
//!
//! Every test here stores one document twice: in a repository that can serve the whole decoded
//! document, and behind a double that refuses the document loads the readers used to make. A
//! reader that still loaded a document fails the test instead of silently answering from a tree,
//! and every answer of the row-only run must equal the full run byte for byte.
//!
//! `RowOnlyRepository` refuses all three document reads (`get`, `get_with_hash`,
//! `get_light_with_hash`); `LightOnlyRepository` refuses the two full reads and keeps the light one,
//! for the readers whose work is itself a whole-document walk (the lexical search scan).

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex, RwLock};

use async_trait::async_trait;
use reading_mcp::application::cite_reading::CiteReadingUseCase;
use reading_mcp::application::get_context::{
    ContextContainerKind, ContextRelation, ContextTarget, ContextUnit, GetContextCommand,
    GetContextUseCase, GetStructuredContextCommand, StructuralContextKind,
};
use reading_mcp::application::get_text_units::{
    GetTextUnitsCommand, GetTextUnitsUseCase, RequestedTextUnitKind, TextUnitCoveragePolicy,
    TextUnitDirection,
};
use reading_mcp::application::open_document::OpenDocumentUseCase;
use reading_mcp::application::open_reading::{OpenReadingCommand, OpenReadingUseCase};
use reading_mcp::application::outline_reading::{OutlineReadingCommand, OutlineReadingUseCase};
use reading_mcp::application::ports::{
    ApplicationError, BodyOrderWindow, DocumentHeader, DocumentRepository, Parser,
    RetrievalOptions, RetrievedResource, Retriever, SearchIndex, SectionRow, SectionTextUnits,
    SourcePolicy, TextUnitIndex,
};
use reading_mcp::application::read_document::{
    ContinueReadCommand, ReadDocumentUseCase, ReadExactTargetCommand, ReadSectionCommand,
};
use reading_mcp::application::read_reading::{ReadReadingCommand, ReadReadingUseCase};
use reading_mcp::application::search_document::{SearchDocumentCommand, SearchDocumentUseCase};
use reading_mcp::application::search_reading::{SearchReadingCommand, SearchReadingUseCase};
use reading_mcp::application::short_handle::{
    DocumentHandle, SectionHandle, handle_checksum, parse_handle,
};
use reading_mcp::application::text_unit_store::store_text_units;
use reading_mcp::domain::{
    ContentHash, Document, DocumentId, DocumentSource, Location, MediaType, NormalizedDocumentHash,
    ParagraphTextUnitSet, Section, SectionId, SentenceTextUnitSet, TextLocator, TextUnit,
};
use reading_mcp::infrastructure::{
    InMemoryDocumentRepository, InMemorySearchIndex, InMemoryTextUnitIndex, NoopSearchIndex,
    SqliteDocumentRepository,
};

// ------------------------------------------------------------------------------------------------
// The two doubles
// ------------------------------------------------------------------------------------------------

/// A repository that answers only the point queries: every document read panics.
struct RowOnlyRepository {
    inner: Arc<dyn DocumentRepository>,
}

impl RowOnlyRepository {
    fn new(inner: Arc<dyn DocumentRepository>) -> Self {
        Self { inner }
    }
}

#[async_trait]
impl DocumentRepository for RowOnlyRepository {
    async fn save(&self, document: Document) -> Result<(), ApplicationError> {
        self.inner.save(document).await
    }

    async fn save_shared(&self, document: Arc<Document>) -> Result<(), ApplicationError> {
        self.inner.save_shared(document).await
    }

    async fn get(&self, id: &DocumentId) -> Result<Option<Arc<Document>>, ApplicationError> {
        panic!("a point-query reader must not load the document ({})", id.0);
    }

    async fn get_with_hash(
        &self,
        id: &DocumentId,
    ) -> Result<Option<(Arc<Document>, NormalizedDocumentHash)>, ApplicationError> {
        panic!("a point-query reader must not load the document ({})", id.0);
    }

    async fn get_light_with_hash(
        &self,
        id: &DocumentId,
    ) -> Result<Option<(Arc<Document>, NormalizedDocumentHash)>, ApplicationError> {
        panic!(
            "a point-query reader must not load the light document ({})",
            id.0
        );
    }

    async fn load_metadata(
        &self,
        id: &DocumentId,
        key: &str,
    ) -> Result<Option<String>, ApplicationError> {
        self.inner.load_metadata(id, key).await
    }

    async fn load_section_text(
        &self,
        id: &DocumentId,
        section_id: &SectionId,
    ) -> Result<Option<String>, ApplicationError> {
        self.inner.load_section_text(id, section_id).await
    }

    async fn load_section_char_len(
        &self,
        id: &DocumentId,
        section_id: &SectionId,
    ) -> Result<Option<usize>, ApplicationError> {
        self.inner.load_section_char_len(id, section_id).await
    }

    async fn document_header(
        &self,
        id: &DocumentId,
    ) -> Result<Option<DocumentHeader>, ApplicationError> {
        self.inner.document_header(id).await
    }

    async fn section_by_id(
        &self,
        id: &DocumentId,
        section_id: &SectionId,
    ) -> Result<Option<SectionRow>, ApplicationError> {
        self.inner.section_by_id(id, section_id).await
    }

    async fn section_by_key(
        &self,
        id: &DocumentId,
        section_key: i64,
    ) -> Result<Option<SectionRow>, ApplicationError> {
        self.inner.section_by_key(id, section_key).await
    }

    async fn sections_in_key_range(
        &self,
        id: &DocumentId,
        from_key: i64,
        to_key_inclusive: i64,
        limit: usize,
    ) -> Result<Vec<SectionRow>, ApplicationError> {
        self.inner
            .sections_in_key_range(id, from_key, to_key_inclusive, limit)
            .await
    }

    async fn section_children(
        &self,
        id: &DocumentId,
        parent_key: Option<i64>,
        limit: usize,
    ) -> Result<Vec<SectionRow>, ApplicationError> {
        self.inner.section_children(id, parent_key, limit).await
    }

    async fn section_ancestors(
        &self,
        id: &DocumentId,
        section_key: i64,
    ) -> Result<Vec<SectionRow>, ApplicationError> {
        self.inner.section_ancestors(id, section_key).await
    }

    async fn body_order_window(
        &self,
        id: &DocumentId,
        section_key: i64,
        before: usize,
        after: usize,
    ) -> Result<Option<BodyOrderWindow>, ApplicationError> {
        self.inner
            .body_order_window(id, section_key, before, after)
            .await
    }

    async fn body_order_at(
        &self,
        id: &DocumentId,
        ord: usize,
    ) -> Result<Option<i64>, ApplicationError> {
        self.inner.body_order_at(id, ord).await
    }

    async fn body_order_len(&self, id: &DocumentId) -> Result<Option<usize>, ApplicationError> {
        self.inner.body_order_len(id).await
    }
}

/// A repository that serves the light document and the point queries, and panics on the two full
/// reads. The lexical search scan walks the tree by contract, so its tests use this one.
struct LightOnlyRepository {
    inner: Arc<dyn DocumentRepository>,
}

impl LightOnlyRepository {
    fn new(inner: Arc<dyn DocumentRepository>) -> Self {
        Self { inner }
    }
}

#[async_trait]
impl DocumentRepository for LightOnlyRepository {
    async fn save(&self, document: Document) -> Result<(), ApplicationError> {
        self.inner.save(document).await
    }

    async fn save_shared(&self, document: Arc<Document>) -> Result<(), ApplicationError> {
        self.inner.save_shared(document).await
    }

    async fn get(&self, id: &DocumentId) -> Result<Option<Arc<Document>>, ApplicationError> {
        panic!("a light reader must not load the full document ({})", id.0);
    }

    async fn get_with_hash(
        &self,
        id: &DocumentId,
    ) -> Result<Option<(Arc<Document>, NormalizedDocumentHash)>, ApplicationError> {
        panic!("a light reader must not load the full document ({})", id.0);
    }

    async fn get_light_with_hash(
        &self,
        id: &DocumentId,
    ) -> Result<Option<(Arc<Document>, NormalizedDocumentHash)>, ApplicationError> {
        self.inner.get_light_with_hash(id).await
    }

    async fn load_metadata(
        &self,
        id: &DocumentId,
        key: &str,
    ) -> Result<Option<String>, ApplicationError> {
        self.inner.load_metadata(id, key).await
    }

    async fn load_section_text(
        &self,
        id: &DocumentId,
        section_id: &SectionId,
    ) -> Result<Option<String>, ApplicationError> {
        self.inner.load_section_text(id, section_id).await
    }

    async fn load_section_char_len(
        &self,
        id: &DocumentId,
        section_id: &SectionId,
    ) -> Result<Option<usize>, ApplicationError> {
        self.inner.load_section_char_len(id, section_id).await
    }

    async fn document_header(
        &self,
        id: &DocumentId,
    ) -> Result<Option<DocumentHeader>, ApplicationError> {
        self.inner.document_header(id).await
    }

    async fn section_by_id(
        &self,
        id: &DocumentId,
        section_id: &SectionId,
    ) -> Result<Option<SectionRow>, ApplicationError> {
        self.inner.section_by_id(id, section_id).await
    }

    async fn section_by_key(
        &self,
        id: &DocumentId,
        section_key: i64,
    ) -> Result<Option<SectionRow>, ApplicationError> {
        self.inner.section_by_key(id, section_key).await
    }

    async fn sections_in_key_range(
        &self,
        id: &DocumentId,
        from_key: i64,
        to_key_inclusive: i64,
        limit: usize,
    ) -> Result<Vec<SectionRow>, ApplicationError> {
        self.inner
            .sections_in_key_range(id, from_key, to_key_inclusive, limit)
            .await
    }

    async fn section_children(
        &self,
        id: &DocumentId,
        parent_key: Option<i64>,
        limit: usize,
    ) -> Result<Vec<SectionRow>, ApplicationError> {
        self.inner.section_children(id, parent_key, limit).await
    }

    async fn section_ancestors(
        &self,
        id: &DocumentId,
        section_key: i64,
    ) -> Result<Vec<SectionRow>, ApplicationError> {
        self.inner.section_ancestors(id, section_key).await
    }

    async fn body_order_window(
        &self,
        id: &DocumentId,
        section_key: i64,
        before: usize,
        after: usize,
    ) -> Result<Option<BodyOrderWindow>, ApplicationError> {
        self.inner
            .body_order_window(id, section_key, before, after)
            .await
    }

    async fn body_order_at(
        &self,
        id: &DocumentId,
        ord: usize,
    ) -> Result<Option<i64>, ApplicationError> {
        self.inner.body_order_at(id, ord).await
    }

    async fn body_order_len(&self, id: &DocumentId) -> Result<Option<usize>, ApplicationError> {
        self.inner.body_order_len(id).await
    }
}

// ------------------------------------------------------------------------------------------------
// A text-unit index that also answers the v2 short-handle keys
// ------------------------------------------------------------------------------------------------

/// [`InMemoryTextUnitIndex`] plus the `document_key` / `section_key` mapping, in document order.
#[derive(Default)]
struct KeyedTextUnitIndex {
    inner: InMemoryTextUnitIndex,
    sections: RwLock<HashMap<DocumentId, Vec<SectionId>>>,
}

impl KeyedTextUnitIndex {
    async fn store(&self, document: &Document) {
        store_text_units(&self.inner, document)
            .await
            .expect("units should persist");
        self.record_sections(document);
    }

    fn record_sections(&self, document: &Document) {
        let mut ordered = Vec::new();
        collect_ids(&document.root_sections, &mut ordered);
        self.sections
            .write()
            .expect("lock")
            .insert(document.id.clone(), ordered);
    }

    fn section_ids(&self, document_id: &DocumentId) -> Option<Vec<SectionId>> {
        self.sections
            .read()
            .expect("lock")
            .get(document_id)
            .cloned()
    }
}

fn collect_ids(sections: &[Section], output: &mut Vec<SectionId>) {
    for section in sections {
        output.push(section.id.clone());
        collect_ids(&section.children, output);
    }
}

#[async_trait]
impl TextUnitIndex for KeyedTextUnitIndex {
    async fn replace_document(
        &self,
        document_id: &DocumentId,
        units: &[TextUnit],
    ) -> Result<(), ApplicationError> {
        self.inner.replace_document(document_id, units).await
    }

    async fn list_document(
        &self,
        document_id: &DocumentId,
    ) -> Result<Vec<TextUnit>, ApplicationError> {
        self.inner.list_document(document_id).await
    }

    async fn replace_document_sets(
        &self,
        document: &Arc<Document>,
        paragraphs: Arc<ParagraphTextUnitSet>,
        sentences: Arc<SentenceTextUnitSet>,
    ) -> Result<(), ApplicationError> {
        self.record_sections(document);
        self.inner
            .replace_document_sets(document, paragraphs, sentences)
            .await
    }

    async fn section_units(
        &self,
        document_id: &DocumentId,
        section_id: &SectionId,
    ) -> Result<Option<SectionTextUnits>, ApplicationError> {
        self.inner.section_units(document_id, section_id).await
    }

    async fn document_key(
        &self,
        document_id: &DocumentId,
    ) -> Result<Option<i64>, ApplicationError> {
        Ok(self.section_ids(document_id).map(|_| 1))
    }

    async fn document_id_for_key(
        &self,
        doc_key: i64,
    ) -> Result<Option<DocumentId>, ApplicationError> {
        if doc_key != 1 {
            return Ok(None);
        }
        Ok(self.sections.read().expect("lock").keys().next().cloned())
    }

    async fn section_key(
        &self,
        document_id: &DocumentId,
        section_id: &SectionId,
    ) -> Result<Option<i64>, ApplicationError> {
        let Some(sections) = self.section_ids(document_id) else {
            return Ok(None);
        };
        Ok(sections
            .iter()
            .position(|candidate| candidate == section_id)
            .and_then(|index| i64::try_from(index).ok()))
    }

    async fn section_id_for_key(
        &self,
        document_id: &DocumentId,
        section_key: i64,
    ) -> Result<Option<SectionId>, ApplicationError> {
        let Some(sections) = self.section_ids(document_id) else {
            return Ok(None);
        };
        Ok(usize::try_from(section_key)
            .ok()
            .and_then(|index| sections.get(index).cloned()))
    }
}

// ------------------------------------------------------------------------------------------------
// Fixtures
// ------------------------------------------------------------------------------------------------

fn document_fixture() -> Document {
    Document {
        id: DocumentId("doc:point-queries".into()),
        source: DocumentSource("memory:point-queries".into()),
        title: "Point queries".into(),
        media_type: MediaType("text/markdown".into()),
        content_hash: ContentHash("sha256:point-queries".into()),
        metadata: BTreeMap::new(),
        root_sections: vec![section(
            "root",
            None,
            "Root",
            1,
            "Root overview sentence.",
            vec![
                section(
                    "topic-a",
                    Some("root"),
                    "Topic A",
                    2,
                    "Alpha one. Alpha two.\n\nAlpha three.",
                    vec![section(
                        "child",
                        Some("topic-a"),
                        "Topic A Child",
                        3,
                        "Child text one. Child text two.",
                        vec![section(
                            "grandchild",
                            Some("child"),
                            "Topic A Grandchild",
                            4,
                            "Grandchild text one. Grandchild text two.",
                            Vec::new(),
                        )],
                    )],
                ),
                section(
                    "topic-b",
                    Some("root"),
                    "Topic B",
                    2,
                    "Beta one. Beta two.",
                    Vec::new(),
                ),
                section("empty", Some("root"), "Empty Topic", 2, "", Vec::new()),
            ],
        )],
    }
}

fn section(
    name: &str,
    parent: Option<&str>,
    title: &str,
    level: u8,
    content: &str,
    children: Vec<Section>,
) -> Section {
    let mut section_path = vec![title.to_owned()];
    if let Some(parent_title) = parent {
        section_path.insert(0, parent_title.to_owned());
    }
    Section {
        id: SectionId(format!("section://{name}")),
        parent_id: parent.map(|parent| SectionId(format!("section://{parent}"))),
        title: title.into(),
        level,
        content: content.into(),
        location: Location {
            section_path,
            ..Location::default()
        },
        children,
    }
}

fn section_id(name: &str) -> SectionId {
    SectionId(format!("section://{name}"))
}

/// The stored document behind every repository view the tests compare.
struct Stored {
    full: Arc<dyn DocumentRepository>,
    rows: Arc<dyn DocumentRepository>,
    light: Arc<dyn DocumentRepository>,
    index: Arc<KeyedTextUnitIndex>,
    document: Document,
}

async fn stored() -> Stored {
    let document = document_fixture();
    let full: Arc<dyn DocumentRepository> = Arc::new(InMemoryDocumentRepository::default());
    full.save(document.clone()).await.expect("save");
    let rows: Arc<dyn DocumentRepository> = Arc::new(RowOnlyRepository::new(Arc::clone(&full)));
    let light: Arc<dyn DocumentRepository> = Arc::new(LightOnlyRepository::new(Arc::clone(&full)));
    let index = Arc::new(KeyedTextUnitIndex::default());
    index.store(&document).await;
    Stored {
        full,
        rows,
        light,
        index,
        document,
    }
}

impl Stored {
    fn units(&self) -> Arc<dyn TextUnitIndex> {
        let units: Arc<dyn TextUnitIndex> = self.index.clone();
        units
    }

    async fn document_handle(&self) -> DocumentHandle {
        DocumentHandle {
            doc_key: self
                .index
                .document_key(&self.document.id)
                .await
                .expect("document key")
                .expect("stored document"),
            checksum: handle_checksum(&self.document.id, &self.document.normalized_document_hash()),
        }
    }

    async fn section_handle(&self, name: &str) -> SectionHandle {
        SectionHandle {
            document: self.document_handle().await,
            section_key: self
                .index
                .section_key(&self.document.id, &section_id(name))
                .await
                .expect("section key")
                .expect("stored section"),
        }
    }
}

// ------------------------------------------------------------------------------------------------
// v2 read
// ------------------------------------------------------------------------------------------------

#[tokio::test]
async fn v2_read_answers_from_the_rows() {
    let stored = stored().await;
    let handle = stored.document_handle().await;
    let section = stored.section_handle("topic-a").await;
    let full = ReadReadingUseCase::new(Arc::clone(&stored.full), stored.units());
    let rows = ReadReadingUseCase::new(Arc::clone(&stored.rows), stored.units());

    // A backward cursor names a stream position in `topic-a`.
    let cursor = {
        let probe = rows
            .execute(ReadReadingCommand {
                from: section.to_string(),
                unit: RequestedTextUnitKind::Sentence,
                max_chars: None,
                direction: Some(TextUnitDirection::Backward),
                back: 0,
            })
            .await
            .expect("backward probe");
        probe.next.expect("a backward cursor")
    };
    assert!(cursor.starts_with("b:"), "backward next is a b: cursor");

    for (label, command) in [
        (
            "sentence step",
            ReadReadingCommand {
                from: section.to_string(),
                unit: RequestedTextUnitKind::Sentence,
                max_chars: None,
                direction: None,
                back: 0,
            },
        ),
        (
            "back 2",
            ReadReadingCommand {
                from: section.to_string(),
                unit: RequestedTextUnitKind::Sentence,
                max_chars: None,
                direction: None,
                back: 2,
            },
        ),
        (
            "batch max_chars",
            ReadReadingCommand {
                from: section.to_string(),
                unit: RequestedTextUnitKind::Sentence,
                max_chars: Some(16),
                direction: None,
                back: 0,
            },
        ),
        (
            "backward cursor",
            ReadReadingCommand {
                from: cursor.clone(),
                unit: RequestedTextUnitKind::Sentence,
                max_chars: Some(16),
                direction: None,
                back: 0,
            },
        ),
        (
            "document handle",
            ReadReadingCommand {
                from: handle.to_string(),
                unit: RequestedTextUnitKind::Sentence,
                max_chars: None,
                direction: None,
                back: 0,
            },
        ),
        (
            "paragraph stream",
            ReadReadingCommand {
                from: section.to_string(),
                unit: RequestedTextUnitKind::Paragraph,
                max_chars: None,
                direction: None,
                back: 0,
            },
        ),
    ] {
        let expected = full.execute(command.clone()).await.expect("full read");
        let actual = rows.execute(command).await.expect("row read");
        assert_eq!(actual, expected, "{label}");
        assert!(!actual.items.is_empty(), "{label} returned no items");
    }
}

/// A section whose units are not stored counts as zero items, and a `back` walk crosses it without
/// inventing an item: `topic-b` sits after `empty` in body order.
#[tokio::test]
async fn v2_read_walks_across_a_section_with_no_stored_units() {
    let stored = stored().await;
    let section = stored.section_handle("empty").await;
    let full = ReadReadingUseCase::new(Arc::clone(&stored.full), stored.units());
    let rows = ReadReadingUseCase::new(Arc::clone(&stored.rows), stored.units());

    let command = ReadReadingCommand {
        from: section.to_string(),
        unit: RequestedTextUnitKind::Sentence,
        max_chars: None,
        direction: Some(TextUnitDirection::Backward),
        back: 1,
    };
    let expected = full.execute(command.clone()).await.expect("full read");
    let actual = rows.execute(command).await.expect("row read");
    assert_eq!(actual, expected);
    // The empty section has no item of its own, so the answer is the previous section's item.
    assert_eq!(actual.items.len(), 2);
    assert_eq!(actual.anchor, None);
}

// ------------------------------------------------------------------------------------------------
// v2 open, outline, search, cite
// ------------------------------------------------------------------------------------------------

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
            bytes: b"# Point queries\n".to_vec(),
            etag: None,
            last_modified: None,
            metadata: BTreeMap::new(),
        })
    }
}

struct StaticParser;

#[async_trait]
impl Parser for StaticParser {
    async fn parse(&self, resource: RetrievedResource) -> Result<Document, ApplicationError> {
        let mut document = document_fixture();
        document.content_hash = ContentHash::of_bytes(&resource.bytes);
        document.id = DocumentId::derive(&resource.final_source, &document.content_hash);
        document.source = resource.final_source;
        Ok(document)
    }
}

#[tokio::test]
async fn v2_open_answers_from_the_header_and_the_root_rows() {
    let document = document_fixture();
    let full: Arc<dyn DocumentRepository> = Arc::new(InMemoryDocumentRepository::default());
    let rows: Arc<dyn DocumentRepository> = Arc::new(RowOnlyRepository::new(Arc::clone(&full)));
    let index = Arc::new(KeyedTextUnitIndex::default());
    let keyed: Arc<KeyedTextUnitIndex> = Arc::clone(&index);
    let units: Arc<dyn TextUnitIndex> = keyed;

    let open_full = Arc::new(OpenDocumentUseCase::with_text_unit_index(
        Arc::new(AllowAllPolicy),
        Arc::new(StaticRetriever),
        Arc::new(StaticParser),
        Arc::clone(&full),
        Arc::clone(&units),
        Arc::new(NoopSearchIndex),
    ));
    let open_rows = Arc::new(OpenDocumentUseCase::with_text_unit_index(
        Arc::new(AllowAllPolicy),
        Arc::new(StaticRetriever),
        Arc::new(StaticParser),
        Arc::clone(&full),
        Arc::clone(&units),
        Arc::new(NoopSearchIndex),
    ));
    let full_use_case = OpenReadingUseCase::new(open_full, Arc::clone(&full), Arc::clone(&units));
    let row_use_case = OpenReadingUseCase::new(open_rows, rows, units);

    let command = OpenReadingCommand {
        source: document.source.clone(),
        refresh: true,
    };
    let expected = full_use_case
        .execute(command.clone())
        .await
        .expect("full open");
    let actual = row_use_case.execute(command).await.expect("row open");
    assert_eq!(actual, expected);
    assert_eq!(actual.top.len(), 1);
    assert_eq!(actual.top[0].1, "Root");
    assert_eq!(actual.more_top, 0);
}

#[tokio::test]
async fn v2_outline_answers_from_the_child_rows() {
    let stored = stored().await;
    let handle = stored.document_handle().await;
    let section = stored.section_handle("topic-a").await;
    let full = OutlineReadingUseCase::new(Arc::clone(&stored.full), stored.units());
    let rows = OutlineReadingUseCase::new(Arc::clone(&stored.rows), stored.units());

    for (label, at, depth, limit) in [
        ("document depth 1", handle.to_string(), 1, 100),
        ("document depth 2", handle.to_string(), 2, 100),
        ("section depth 1", section.to_string(), 1, 100),
        ("section depth 2", section.to_string(), 2, 100),
        ("paged", handle.to_string(), 2, 2),
    ] {
        let command = OutlineReadingCommand {
            at: at.clone(),
            depth,
            limit,
            cursor: None,
        };
        let expected = full.execute(command.clone()).await.expect("full outline");
        let actual = rows.execute(command).await.expect("row outline");
        assert_eq!(actual, expected, "{label}");
        if let Some(cursor) = actual.next.clone() {
            let command = OutlineReadingCommand {
                at,
                depth,
                limit,
                cursor: Some(cursor),
            };
            let expected = full
                .execute(command.clone())
                .await
                .expect("full continuation");
            let actual = rows.execute(command).await.expect("row continuation");
            assert_eq!(actual, expected, "{label} continuation");
        }
    }

    let items = rows
        .execute(OutlineReadingCommand {
            at: handle.to_string(),
            depth: 3,
            limit: 100,
            cursor: None,
        })
        .await
        .expect("row outline");
    assert!(
        items
            .items
            .iter()
            .any(|item| item.title == "Topic A Grandchild"),
        "depth 3 lists the third level"
    );
}

#[tokio::test]
async fn v2_search_and_cite_answer_from_the_header() {
    let stored = stored().await;
    let handle = stored.document_handle().await;
    let section = stored.section_handle("topic-a").await;
    let search_index: Arc<dyn SearchIndex> = Arc::new(InMemorySearchIndex::default());
    search_index.index(&stored.document).await.expect("index");

    let full_search = SearchReadingUseCase::new(
        Arc::clone(&stored.full),
        stored.units(),
        Arc::new(
            SearchDocumentUseCase::new(Arc::clone(&search_index), Arc::clone(&stored.full))
                .with_text_unit_index(stored.units()),
        ),
    );
    // The scan walks the tree by contract, so the search use case behind both runs is the light
    // one; the handle resolution is what must come from the header.
    let row_search = SearchReadingUseCase::new(
        Arc::clone(&stored.rows),
        stored.units(),
        Arc::new(
            SearchDocumentUseCase::new(Arc::clone(&search_index), Arc::clone(&stored.light))
                .with_text_unit_index(stored.units()),
        ),
    );
    let command = SearchReadingCommand {
        doc: handle.to_string(),
        query: "Alpha three".into(),
        limit: 5,
    };
    let expected = full_search
        .execute(command.clone())
        .await
        .expect("full search");
    let actual = row_search.execute(command).await.expect("row search");
    assert_eq!(actual, expected);
    assert!(!actual.hits.is_empty());

    let full_cite = CiteReadingUseCase::new(Arc::clone(&stored.full), stored.units());
    let row_cite = CiteReadingUseCase::new(Arc::clone(&stored.rows), stored.units());
    for at in [
        handle.to_string(),
        section.to_string(),
        format!("{}.p1", section),
    ] {
        let expected = full_cite
            .execute(&[at.clone()])
            .await
            .expect("full cite")
            .items;
        let actual = row_cite
            .execute(&[at.clone()])
            .await
            .expect("row cite")
            .items;
        assert_eq!(actual, expected, "{at}");
    }
    // A document handle renders the first section in body order: position 0, one point query.
    let locator = row_cite
        .source_locator(&handle.to_string())
        .await
        .expect_err("a markdown document has no original page");
    assert_eq!(locator.code.as_str(), "invalid_argument");
}

// ------------------------------------------------------------------------------------------------
// v1 readers
// ------------------------------------------------------------------------------------------------

fn text_unit_command(document: &Document, name: &str) -> GetTextUnitsCommand {
    GetTextUnitsCommand {
        document_id: document.id.clone(),
        section_id: section_id(name),
        requested_kind: RequestedTextUnitKind::Sentence,
        direction: TextUnitDirection::Forward,
        coverage_policy: TextUnitCoveragePolicy::PreserveSource,
        max_items: 8,
        max_chars: None,
        cursor: None,
    }
}

#[tokio::test]
async fn get_text_units_answers_from_the_header_and_one_row() {
    let stored = stored().await;
    let document = &stored.document;
    let full = GetTextUnitsUseCase::new(Arc::clone(&stored.full), stored.units());
    let rows = GetTextUnitsUseCase::new(Arc::clone(&stored.rows), stored.units());

    for name in ["root", "topic-a", "child", "grandchild", "topic-b"] {
        let command = text_unit_command(document, name);
        let expected = full.execute(command.clone()).await.expect("full units");
        let actual = rows.execute(command).await.expect("row units");
        assert_eq!(actual, expected, "{name}");
        assert!(!actual.items.is_empty(), "{name} returned no items");
    }

    // A section-kind anchor resolves against the same rows and fails the same way.
    let topic_a = document
        .find_section(&section_id("topic-a"))
        .expect("topic A");
    let anchor = TextLocator::for_section(document, topic_a);
    let expected = full
        .execute_from_anchor(text_unit_command(document, "topic-a"), anchor.clone())
        .await
        .expect_err("a section anchor is not in a text-unit stream");
    let actual = rows
        .execute_from_anchor(text_unit_command(document, "topic-a"), anchor)
        .await
        .expect_err("a section anchor is not in a text-unit stream");
    assert_eq!(actual, expected);
}

#[tokio::test]
async fn get_context_serves_every_relation_from_the_rows() {
    let stored = stored().await;
    let document = &stored.document;
    let full =
        GetContextUseCase::new(Arc::clone(&stored.full)).with_text_unit_index(stored.units());
    let rows =
        GetContextUseCase::new(Arc::clone(&stored.rows)).with_text_unit_index(stored.units());

    let neighbour = GetContextCommand {
        document_id: document.id.clone(),
        section_id: section_id("topic-a"),
        before: 1,
        after: 2,
        max_chars: None,
    };
    let expected = full
        .execute(neighbour.clone())
        .await
        .expect("full neighbour");
    let actual = rows.execute(neighbour).await.expect("row neighbour");
    assert_eq!(actual, expected);
    assert_eq!(actual.items.len(), 4);

    for (label, target, relation) in [
        (
            "section container",
            ContextTarget::Section(section_id("child")),
            ContextRelation::Container {
                kind: ContextContainerKind::Section,
            },
        ),
        (
            "owner section",
            ContextTarget::Section(section_id("topic-a")),
            ContextRelation::Structural {
                kind: StructuralContextKind::OwnerSection,
            },
        ),
        (
            "ancestors",
            ContextTarget::Section(section_id("grandchild")),
            ContextRelation::Structural {
                kind: StructuralContextKind::Ancestors,
            },
        ),
        (
            "siblings",
            ContextTarget::Section(section_id("topic-b")),
            ContextRelation::Structural {
                kind: StructuralContextKind::Siblings,
            },
        ),
        (
            "children",
            ContextTarget::Section(section_id("child")),
            ContextRelation::Structural {
                kind: StructuralContextKind::Children,
            },
        ),
    ] {
        let command = GetStructuredContextCommand {
            document_id: document.id.clone(),
            target,
            relation,
            max_chars: None,
        };
        let expected = full
            .execute_structured(command.clone())
            .await
            .expect("full structured context");
        let actual = rows
            .execute_structured(command)
            .await
            .expect("row structured context");
        assert_eq!(actual, expected, "{label}");
    }

    // A paragraph locator anchor resolves through `resolve_text_locator_at`.
    let topic_a = document
        .find_section(&section_id("topic-a"))
        .expect("topic A");
    let paragraph = document
        .paragraph_text_units()
        .units
        .into_iter()
        .find(|unit| unit.owner_section_id == topic_a.id && unit.paragraph_index == 2)
        .expect("second paragraph");
    let command = GetStructuredContextCommand {
        document_id: document.id.clone(),
        target: ContextTarget::Locator(TextLocator::for_paragraph(document, topic_a, &paragraph)),
        relation: ContextRelation::Neighbor {
            unit: ContextUnit::Paragraph,
            before: 1,
            after: 1,
        },
        max_chars: None,
    };
    let expected = full
        .execute_structured(command.clone())
        .await
        .expect("full paragraph neighbours");
    let actual = rows
        .execute_structured(command)
        .await
        .expect("row paragraph neighbours");
    assert_eq!(actual, expected);
}

#[tokio::test]
async fn read_document_renders_a_subtree_from_the_key_range() {
    let stored = stored().await;
    let document = &stored.document;
    let full =
        ReadDocumentUseCase::new(Arc::clone(&stored.full)).with_text_unit_index(stored.units());
    let rows =
        ReadDocumentUseCase::new(Arc::clone(&stored.rows)).with_text_unit_index(stored.units());

    for name in ["root", "topic-a", "child", "grandchild", "topic-b", "empty"] {
        let command = ReadSectionCommand {
            document_id: document.id.clone(),
            section_id: section_id(name),
            max_chars: None,
        };
        let expected = full.execute(command.clone()).await.expect("full read");
        let actual = rows.execute(command).await.expect("row read");
        assert_eq!(actual, expected, "section {name}");

        let command = ReadSectionCommand {
            document_id: document.id.clone(),
            section_id: section_id(name),
            max_chars: Some(12),
        };
        let expected = full.execute(command.clone()).await.expect("full read");
        let actual = rows.execute(command).await.expect("row read");
        assert_eq!(actual, expected, "section {name} with max_chars");
        if let Some(cursor) = actual.next_cursor.clone() {
            let continuation = ContinueReadCommand {
                document_id: document.id.clone(),
                section_id: section_id(name),
                cursor,
                max_chars: Some(12),
            };
            let expected = full
                .continue_read(continuation.clone())
                .await
                .expect("full continuation");
            let actual = rows
                .continue_read(continuation)
                .await
                .expect("row continuation");
            assert_eq!(actual, expected, "section {name} continuation");
        }
    }

    // The exact read resolves its locator against the rows too.
    let topic_a = document
        .find_section(&section_id("topic-a"))
        .expect("topic A");
    let command = ReadExactTargetCommand {
        document_id: document.id.clone(),
        target_locator: TextLocator::for_section(document, topic_a),
        max_chars: Some(7),
    };
    let expected = full.read_exact(command.clone()).await.expect("full exact");
    let actual = rows.read_exact(command).await.expect("row exact");
    assert_eq!(actual, expected);
    assert_eq!(actual.content, "Alpha o");
}

/// The trap: a parent section renders its whole subtree, so the key-range read must carry the
/// third level. A `Section` built from one row would silently truncate here.
#[tokio::test]
async fn a_parent_section_renders_its_three_level_subtree() {
    let stored = stored().await;
    let document = &stored.document;
    let rows =
        ReadDocumentUseCase::new(Arc::clone(&stored.rows)).with_text_unit_index(stored.units());
    let rendered = rows
        .execute(ReadSectionCommand {
            document_id: document.id.clone(),
            section_id: section_id("root"),
            max_chars: None,
        })
        .await
        .expect("root read");
    for expected in [
        "Root overview sentence.",
        "Alpha one. Alpha two.",
        "Child text one. Child text two.",
        "Grandchild text one. Grandchild text two.",
        "Beta one. Beta two.",
    ] {
        assert!(
            rendered.content.contains(expected),
            "the subtree render must carry {expected:?}: {}",
            rendered.content
        );
    }
    assert_eq!(
        rendered.stream.total_chars,
        rendered.content.chars().count()
    );
}

#[tokio::test]
async fn search_document_answers_from_the_light_document_and_one_row() {
    let stored = stored().await;
    let document = &stored.document;
    let search_index: Arc<dyn SearchIndex> = Arc::new(InMemorySearchIndex::default());
    search_index.index(document).await.expect("index");
    let full = SearchDocumentUseCase::new(Arc::clone(&search_index), Arc::clone(&stored.full))
        .with_text_unit_index(stored.units());
    let light = SearchDocumentUseCase::new(search_index, Arc::clone(&stored.light))
        .with_text_unit_index(stored.units());

    for query in ["Topic B", "Alpha three", "Grandchild"] {
        let command = SearchDocumentCommand {
            document_id: document.id.clone(),
            query: query.into(),
            limit: 5,
        };
        let expected = full.execute(command.clone()).await.expect("full search");
        let actual = light.execute(command).await.expect("light search");
        assert_eq!(actual, expected, "{query}");
    }
}

// ------------------------------------------------------------------------------------------------
// Body order
// ------------------------------------------------------------------------------------------------

const INVALID_MAP: &str = r#"{"schema_version":"epub-structure-reconciliation/v1","sections":[{"section_id":"section://second","source_order":0},{"section_id":"section://first","source_order":5},{"section_id":"section://nested","source_order":2}]}"#;

/// An EPUB whose stored body order cannot be computed.
fn broken_body_order_document() -> Document {
    let mut document = Document {
        id: DocumentId("doc:broken-order".into()),
        source: DocumentSource("memory:broken-order".into()),
        title: "Broken order".into(),
        media_type: MediaType("application/epub+zip".into()),
        content_hash: ContentHash("sha256:broken-order".into()),
        metadata: BTreeMap::new(),
        root_sections: vec![
            section(
                "first",
                None,
                "First",
                1,
                "First text.",
                vec![section(
                    "nested",
                    Some("first"),
                    "Nested",
                    2,
                    "Nested text.",
                    vec![],
                )],
            ),
            section("second", None, "Second", 1, "Second text.", vec![]),
        ],
    };
    document
        .metadata
        .insert("epub_structure_map".into(), INVALID_MAP.into());
    document
}

#[tokio::test]
async fn a_failed_stored_body_order_reports_the_same_error() {
    let document = broken_body_order_document();
    let full: Arc<dyn DocumentRepository> = Arc::new(InMemoryDocumentRepository::default());
    full.save(document.clone()).await.expect("save");
    let rows: Arc<dyn DocumentRepository> = Arc::new(RowOnlyRepository::new(Arc::clone(&full)));

    let stored_error = full
        .body_order(&document.id)
        .await
        .expect("body order read")
        .expect("a stored answer")
        .expect_err("the structure map is rejected");
    let expected_detail = stored_error.to_string();

    // Only the handle mapping is needed: the read fails on the body order before it reads a stream.
    let index = Arc::new(KeyedTextUnitIndex::default());
    index.record_sections(&document);
    let keyed: Arc<KeyedTextUnitIndex> = Arc::clone(&index);
    let units: Arc<dyn TextUnitIndex> = keyed;
    let handle = DocumentHandle {
        doc_key: 1,
        checksum: handle_checksum(&document.id, &document.normalized_document_hash()),
    };
    let command = ReadReadingCommand {
        from: handle.to_string(),
        unit: RequestedTextUnitKind::Sentence,
        max_chars: None,
        direction: None,
        back: 0,
    };
    let full_read = ReadReadingUseCase::new(Arc::clone(&full), Arc::clone(&units));
    let row_read = ReadReadingUseCase::new(Arc::clone(&rows), units);
    let expected = full_read
        .execute(command.clone())
        .await
        .expect_err("the body order is rejected");
    let actual = row_read
        .execute(command)
        .await
        .expect_err("the body order is rejected");
    assert_eq!(actual, expected, "the same variant, code and message");
    assert!(
        actual.detail.contains(&expected_detail),
        "the stored error must surface unchanged: {} vs {expected_detail}",
        actual.detail
    );
}

// ------------------------------------------------------------------------------------------------
// An old-shape document: no header row, no section rows, no body-order rows
// ------------------------------------------------------------------------------------------------

async fn open_sqlite_repository() -> (tempfile::TempDir, SqliteDocumentRepository) {
    let directory = tempfile::tempdir().expect("temp directory");
    let path = directory.path().join("point-queries.sqlite");
    let repository = SqliteDocumentRepository::open(&path).expect("repository should open");
    (directory, repository)
}

#[tokio::test]
async fn an_old_shape_document_answers_every_migrated_reader_identically() {
    let (directory, repository) = open_sqlite_repository().await;
    let document = document_fixture();
    repository.save(document.clone()).await.expect("save");
    let stored: Arc<dyn DocumentRepository> = Arc::new(repository);
    let index = Arc::new(KeyedTextUnitIndex::default());
    index.store(&document).await;
    let keyed: Arc<KeyedTextUnitIndex> = Arc::clone(&index);
    let units: Arc<dyn TextUnitIndex> = keyed;
    let handle = DocumentHandle {
        doc_key: 1,
        checksum: handle_checksum(&document.id, &document.normalized_document_hash()),
    };
    let section = SectionHandle {
        document: handle.clone(),
        section_key: 1,
    };

    // Every migrated reader, as one list of answers.
    async fn answers(
        repository: &Arc<dyn DocumentRepository>,
        units: &Arc<dyn TextUnitIndex>,
        document: &Document,
        handle: &DocumentHandle,
        section: &SectionHandle,
    ) -> Vec<String> {
        let read = ReadReadingUseCase::new(Arc::clone(repository), Arc::clone(units));
        let outline = OutlineReadingUseCase::new(Arc::clone(repository), Arc::clone(units));
        let cite = CiteReadingUseCase::new(Arc::clone(repository), Arc::clone(units));
        let text_units = GetTextUnitsUseCase::new(Arc::clone(repository), Arc::clone(units));
        let context =
            GetContextUseCase::new(Arc::clone(repository)).with_text_unit_index(Arc::clone(units));
        let read_document = ReadDocumentUseCase::new(Arc::clone(repository))
            .with_text_unit_index(Arc::clone(units));
        let mut out = Vec::new();
        out.push(format!(
            "{:?}",
            read.execute(ReadReadingCommand {
                from: section.to_string(),
                unit: RequestedTextUnitKind::Sentence,
                max_chars: None,
                direction: None,
                back: 2,
            })
            .await
        ));
        out.push(format!(
            "{:?}",
            outline
                .execute(OutlineReadingCommand {
                    at: handle.to_string(),
                    depth: 2,
                    limit: 100,
                    cursor: None,
                })
                .await
        ));
        out.push(format!("{:?}", cite.execute(&[section.to_string()]).await));
        out.push(format!(
            "{:?}",
            text_units
                .execute(text_unit_command(document, "topic-a"))
                .await
        ));
        out.push(format!(
            "{:?}",
            context
                .execute(GetContextCommand {
                    document_id: document.id.clone(),
                    section_id: section_id("topic-a"),
                    before: 1,
                    after: 1,
                    max_chars: None,
                })
                .await
        ));
        out.push(format!(
            "{:?}",
            context
                .execute_structured(GetStructuredContextCommand {
                    document_id: document.id.clone(),
                    target: ContextTarget::Section(section_id("grandchild")),
                    relation: ContextRelation::Structural {
                        kind: StructuralContextKind::Ancestors,
                    },
                    max_chars: None,
                })
                .await
        ));
        out.push(format!(
            "{:?}",
            read_document
                .execute(ReadSectionCommand {
                    document_id: document.id.clone(),
                    section_id: section_id("topic-a"),
                    max_chars: Some(12),
                })
                .await
        ));
        out
    }

    let before = answers(&stored, &units, &document, &handle, &section).await;

    // Delete the rows by hand: the document is now shaped like one stored before SEC-1.
    let database = directory.path().join("point-queries.sqlite");
    let connection = rusqlite::Connection::open(&database).expect("database should open");
    for table in [
        "document_header_v1",
        "document_sections_v1",
        "document_body_order_rows_v1",
    ] {
        connection
            .execute(
                &format!("DELETE FROM {table} WHERE document_id = ?1"),
                rusqlite::params![document.id.0],
            )
            .expect("rows should delete");
    }
    drop(connection);

    let after = answers(&stored, &units, &document, &handle, &section).await;
    assert_eq!(
        after, before,
        "the fallback answers every reader identically"
    );
    assert!(
        before.iter().all(|answer| !answer.contains("Err")),
        "no reader may fail: {before:?}"
    );
}

// ------------------------------------------------------------------------------------------------
// Cost shape
// ------------------------------------------------------------------------------------------------

/// A repository that counts every call and answers from an in-memory document.
struct CountingRepository {
    inner: Arc<dyn DocumentRepository>,
    calls: Mutex<Vec<&'static str>>,
}

impl CountingRepository {
    fn new(inner: Arc<dyn DocumentRepository>) -> Self {
        Self {
            inner,
            calls: Mutex::new(Vec::new()),
        }
    }

    fn record(&self, call: &'static str) {
        self.calls.lock().expect("lock").push(call);
    }

    fn count(&self) -> usize {
        self.calls.lock().expect("lock").len()
    }
}

#[async_trait]
impl DocumentRepository for CountingRepository {
    async fn save(&self, document: Document) -> Result<(), ApplicationError> {
        self.record("save");
        self.inner.save(document).await
    }

    async fn save_shared(&self, document: Arc<Document>) -> Result<(), ApplicationError> {
        self.record("save_shared");
        self.inner.save_shared(document).await
    }

    async fn get(&self, id: &DocumentId) -> Result<Option<Arc<Document>>, ApplicationError> {
        self.record("get");
        self.inner.get(id).await
    }

    async fn get_with_hash(
        &self,
        id: &DocumentId,
    ) -> Result<Option<(Arc<Document>, NormalizedDocumentHash)>, ApplicationError> {
        self.record("get_with_hash");
        self.inner.get_with_hash(id).await
    }

    async fn get_light_with_hash(
        &self,
        id: &DocumentId,
    ) -> Result<Option<(Arc<Document>, NormalizedDocumentHash)>, ApplicationError> {
        self.record("get_light_with_hash");
        self.inner.get_light_with_hash(id).await
    }

    async fn load_section_text(
        &self,
        id: &DocumentId,
        section_id: &SectionId,
    ) -> Result<Option<String>, ApplicationError> {
        self.record("load_section_text");
        self.inner.load_section_text(id, section_id).await
    }

    async fn load_section_char_len(
        &self,
        id: &DocumentId,
        section_id: &SectionId,
    ) -> Result<Option<usize>, ApplicationError> {
        self.record("load_section_char_len");
        self.inner.load_section_char_len(id, section_id).await
    }

    async fn document_header(
        &self,
        id: &DocumentId,
    ) -> Result<Option<DocumentHeader>, ApplicationError> {
        self.record("document_header");
        self.inner.document_header(id).await
    }

    async fn section_by_id(
        &self,
        id: &DocumentId,
        section_id: &SectionId,
    ) -> Result<Option<SectionRow>, ApplicationError> {
        self.record("section_by_id");
        self.inner.section_by_id(id, section_id).await
    }

    async fn section_by_key(
        &self,
        id: &DocumentId,
        section_key: i64,
    ) -> Result<Option<SectionRow>, ApplicationError> {
        self.record("section_by_key");
        self.inner.section_by_key(id, section_key).await
    }

    async fn sections_in_key_range(
        &self,
        id: &DocumentId,
        from_key: i64,
        to_key_inclusive: i64,
        limit: usize,
    ) -> Result<Vec<SectionRow>, ApplicationError> {
        self.record("sections_in_key_range");
        self.inner
            .sections_in_key_range(id, from_key, to_key_inclusive, limit)
            .await
    }

    async fn section_children(
        &self,
        id: &DocumentId,
        parent_key: Option<i64>,
        limit: usize,
    ) -> Result<Vec<SectionRow>, ApplicationError> {
        self.record("section_children");
        self.inner.section_children(id, parent_key, limit).await
    }

    async fn section_ancestors(
        &self,
        id: &DocumentId,
        section_key: i64,
    ) -> Result<Vec<SectionRow>, ApplicationError> {
        self.record("section_ancestors");
        self.inner.section_ancestors(id, section_key).await
    }

    async fn body_order_window(
        &self,
        id: &DocumentId,
        section_key: i64,
        before: usize,
        after: usize,
    ) -> Result<Option<BodyOrderWindow>, ApplicationError> {
        self.record("body_order_window");
        self.inner
            .body_order_window(id, section_key, before, after)
            .await
    }

    async fn body_order_at(
        &self,
        id: &DocumentId,
        ord: usize,
    ) -> Result<Option<i64>, ApplicationError> {
        self.record("body_order_at");
        self.inner.body_order_at(id, ord).await
    }

    async fn body_order_len(&self, id: &DocumentId) -> Result<Option<usize>, ApplicationError> {
        self.record("body_order_len");
        self.inner.body_order_len(id).await
    }
}

/// `roots` root sections with `per_root` children each: `roots * (per_root + 1)` sections.
fn wide_document(name: &str, roots: usize, per_root: usize) -> Document {
    let root_sections = (0..roots)
        .map(|root| {
            let children = (0..per_root)
                .map(|child| {
                    section(
                        &format!("{name}-{root}-{child}"),
                        Some(&format!("{name}-{root}")),
                        2,
                        "Child",
                        2,
                        "Child text one. Child text two.",
                        Vec::new(),
                    )
                })
                .collect();
            section(
                &format!("{name}-{root}"),
                None,
                "Root",
                1,
                "Root text one.",
                children,
            )
        })
        .collect();
    Document {
        id: DocumentId(format!("doc:{name}")),
        source: DocumentSource(format!("memory:{name}")),
        title: name.into(),
        media_type: MediaType("text/markdown".into()),
        content_hash: ContentHash(format!("sha256:{name}")),
        metadata: BTreeMap::new(),
        root_sections,
    }
}

/// The number of repository calls one `read`-path call makes, for a small and a large book.
async fn read_path_calls(roots: usize, per_root: usize) -> usize {
    let document = wide_document(&format!("wide-{roots}-{per_root}"), roots, per_root);
    let inner: Arc<dyn DocumentRepository> = Arc::new(InMemoryDocumentRepository::default());
    inner.save(document.clone()).await.expect("save");
    let counting = Arc::new(CountingRepository::new(Arc::clone(&inner)));
    let counted: Arc<dyn DocumentRepository> = Arc::clone(&counting);
    let index = Arc::new(KeyedTextUnitIndex::default());
    index.store(&document).await;
    let keyed: Arc<KeyedTextUnitIndex> = Arc::clone(&index);
    let units: Arc<dyn TextUnitIndex> = keyed;
    let handle = DocumentHandle {
        doc_key: 1,
        checksum: handle_checksum(&document.id, &document.normalized_document_hash()),
    };
    let read = ReadReadingUseCase::new(Arc::clone(&counted), Arc::clone(&units));
    let before = counting.count();
    let result = read
        .execute(ReadReadingCommand {
            from: handle.to_string(),
            unit: RequestedTextUnitKind::Sentence,
            max_chars: None,
            direction: None,
            back: 2,
        })
        .await
        .expect("read");
    assert!(!result.items.is_empty());
    counting.count() - before
}

#[tokio::test]
async fn the_read_path_cost_does_not_grow_with_the_section_count() {
    let small = read_path_calls(10, 49).await; // 500 sections
    let large = read_path_calls(200, 99).await; // 20,000 sections
    assert_eq!(
        small, large,
        "a read of 500 sections and a read of 20,000 sections must make the same number of \
         repository calls"
    );
}

#[tokio::test]
async fn every_query_a_migrated_reader_issues_uses_an_index() {
    let document = wide_document("indexed", 200, 99);
    assert_eq!(document.section_count(), 20_000);
    let (directory, repository) = open_sqlite_repository().await;
    repository.save(document.clone()).await.expect("save");
    let database = directory.path().join("point-queries.sqlite");
    let connection = rusqlite::Connection::open(&database).expect("database should open");

    let queries = [
        (
            "document_header",
            "SELECT document_id, source, title, media_type, content_hash,
                    normalized_document_hash, section_count, root_count
               FROM document_header_v1 WHERE document_id = ?1",
        ),
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
            "section_ancestors",
            "SELECT section_key, section_id, parent_key, level, title, location_json, subtree_end_key
               FROM document_sections_v1 WHERE document_id = ?1 AND section_key = ?2",
        ),
        (
            "body_order_window",
            "SELECT ord, section_key FROM document_body_order_rows_v1
              WHERE document_id = ?1 AND section_key = ?2",
        ),
        (
            "body_order_at",
            "SELECT section_key FROM document_body_order_rows_v1
              WHERE document_id = ?1 AND ord = ?2",
        ),
        (
            "body_order_len",
            "SELECT COUNT(*) FROM document_body_order_rows_v1 WHERE document_id = ?1",
        ),
    ];

    for (label, sql) in queries {
        let mut statement = connection
            .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
            .expect("query should prepare");
        // The plan does not depend on the bound values, so every parameter is NULL.
        let params = vec![rusqlite::types::Value::Null; statement.parameter_count()];
        let plan: Vec<String> = statement
            .query_map(rusqlite::params_from_iter(params), |row| {
                row.get::<_, String>(3)
            })
            .expect("plan should run")
            .collect::<Result<Vec<_>, _>>()
            .expect("plan rows should decode");
        let plan = plan.join(" | ");
        let upper = plan.to_ascii_uppercase();
        assert!(
            !upper.contains("SCAN DOCUMENT_SECTIONS_V1"),
            "{label} must not scan document_sections_v1: {plan}"
        );
        assert!(
            !upper.contains("SCAN DOCUMENT_BODY_ORDER_ROWS_V1"),
            "{label} must not scan document_body_order_rows_v1: {plan}"
        );
        assert!(upper.contains("USING"), "{label} must use an index: {plan}");
    }
}

// ------------------------------------------------------------------------------------------------
// The handle parser the fixtures rely on
// ------------------------------------------------------------------------------------------------

#[tokio::test]
async fn the_fixture_handles_parse_back() {
    let stored = stored().await;
    let handle = stored.document_handle().await;
    let parsed = parse_handle(&handle.to_string()).expect("a document handle parses");
    assert_eq!(parsed.document(), &handle);
}
