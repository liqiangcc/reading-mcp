//! The hot readers hold only the light document (R1c-3c).
//!
//! Every test here builds one document and stores it twice: in a repository that can serve the full
//! decoded document, and behind [`LightOnlyRepository`], whose `get` and `get_with_hash` panic. A
//! reader that asked for the full document would fail the test instead of silently reading an empty
//! section text, and every answer of the light run must equal the full run byte for byte.

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use reading_mcp::application::get_text_units::RequestedTextUnitKind;
use reading_mcp::application::open_document::OpenDocumentUseCase;
use reading_mcp::application::open_reading::{OpenReadingCommand, OpenReadingUseCase};
use reading_mcp::application::ports::{
    ApplicationError, BodyOrderWindow, DocumentHeader, DocumentRepository, Parser,
    RetrievalOptions, RetrievedResource, Retriever, SearchIndex, SectionRow, SourcePolicy,
    TextUnitIndex,
};
use reading_mcp::application::read_reading::{ReadReadingCommand, ReadReadingUseCase};
use reading_mcp::application::search_document::{
    SearchCandidateKind, SearchDocumentCommand, SearchDocumentUseCase,
};
use reading_mcp::application::short_handle::{
    DocumentHandle, SectionHandle, handle_checksum, paragraph_handle,
};
use reading_mcp::domain::{
    ContentHash, Document, DocumentId, DocumentSource, Location, MediaType, NormalizedDocumentHash,
    Section, SectionId,
};
#[path = "support/keyed_index.rs"]
mod keyed_index;
use keyed_index::KeyedTextUnitIndex;

use reading_mcp::infrastructure::{
    InMemoryDocumentRepository, InMemorySearchIndex, NoopSearchIndex,
};

// ------------------------------------------------------------------------------------------------
// The light-only repository
// ------------------------------------------------------------------------------------------------

/// A repository that serves the light document and nothing else: every full-document read panics.
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
        panic!(
            "a light reader must not load the full document ({}): use get_light_with_hash",
            id.0
        );
    }

    async fn get_with_hash(
        &self,
        id: &DocumentId,
    ) -> Result<Option<(Arc<Document>, NormalizedDocumentHash)>, ApplicationError> {
        panic!(
            "a light reader must not load the full document ({}): use get_light_with_hash",
            id.0
        );
    }

    async fn get_light_with_hash(
        &self,
        id: &DocumentId,
    ) -> Result<Option<(Arc<Document>, NormalizedDocumentHash)>, ApplicationError> {
        let Some((document, hash)) = self.inner.get_light_with_hash(id).await? else {
            return Ok(None);
        };
        // The in-memory repository has no light read of its own, so the light form is made here.
        let mut light = (*document).clone();
        clear_section_text(&mut light.root_sections);
        Ok(Some((Arc::new(light), hash)))
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

    async fn body_order(
        &self,
        id: &DocumentId,
    ) -> Result<Option<Result<Vec<SectionId>, ApplicationError>>, ApplicationError> {
        self.inner.body_order(id).await
    }

    // The SEC-1 point queries are answered from the inner repository, which computes them from the
    // document it keeps: a reader that asks for a row never loads the full document itself.
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

fn clear_section_text(sections: &mut [Section]) {
    for section in sections {
        section.content.clear();
        clear_section_text(&mut section.children);
    }
}

// ------------------------------------------------------------------------------------------------
// Fixtures
// ------------------------------------------------------------------------------------------------

fn document_fixture() -> Document {
    Document {
        id: DocumentId("doc:light-readers".into()),
        source: DocumentSource("memory:light-readers".into()),
        title: "Light readers".into(),
        media_type: MediaType("text/markdown".into()),
        content_hash: ContentHash("sha256:light-readers".into()),
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
                        Vec::new(),
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

/// The stored document behind both a full and a light-only repository view.
struct Stored {
    full: Arc<dyn DocumentRepository>,
    light: Arc<dyn DocumentRepository>,
    index: Arc<KeyedTextUnitIndex>,
    document: Document,
}

async fn stored() -> Stored {
    let document = document_fixture();
    let full: Arc<dyn DocumentRepository> = Arc::new(InMemoryDocumentRepository::default());
    full.save(document.clone()).await.expect("save");
    let light: Arc<dyn DocumentRepository> = Arc::new(LightOnlyRepository::new(Arc::clone(&full)));
    let index = Arc::new(KeyedTextUnitIndex::default());
    index.store(&document).await;
    Stored {
        full,
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
}

// ------------------------------------------------------------------------------------------------
// v2 read on the light document
// ------------------------------------------------------------------------------------------------

async fn section_handle(stored: &Stored, name: &str) -> SectionHandle {
    SectionHandle {
        document: document_handle(stored).await,
        section_key: stored
            .index
            .section_key(&stored.document.id, &section_id(name))
            .await
            .expect("section key")
            .expect("stored section"),
    }
}

fn read_command(from: String, max_chars: Option<usize>) -> ReadReadingCommand {
    ReadReadingCommand {
        from,
        unit: RequestedTextUnitKind::Sentence,
        max_chars,
        direction: None,
        back: 0,
    }
}

#[tokio::test]
async fn v2_reads_from_every_anchor_match_on_the_light_document() {
    let stored = stored().await;
    let full = ReadReadingUseCase::new(Arc::clone(&stored.full), stored.units());
    let light = ReadReadingUseCase::new(Arc::clone(&stored.light), stored.units());

    let mut anchors = vec![document_handle(&stored).await.to_string()];
    for name in ["root", "topic-a", "child", "topic-b", "empty"] {
        anchors.push(section_handle(&stored, name).await.to_string());
    }
    for from in anchors {
        let command = read_command(from.clone(), None);
        let expected = full.execute(command.clone()).await;
        let actual = light.execute(command).await;
        assert_eq!(actual, expected, "from {from}");

        // A small budget exercises the `next` continuation over the same stream.
        let command = read_command(from.clone(), Some(12));
        let expected = full.execute(command.clone()).await;
        let actual = light.execute(command).await;
        assert_eq!(actual, expected, "from {from} with max_chars");
        if let Some(next) = actual.ok().and_then(|result| result.next) {
            let expected = full.execute(read_command(next.clone(), Some(12))).await;
            let actual = light.execute(read_command(next, Some(12))).await;
            assert_eq!(actual, expected, "from {from} continuation");
        }
    }
}

#[tokio::test]
async fn v2_read_at_a_paragraph_handle_matches_on_the_light_document() {
    let stored = stored().await;
    let full = ReadReadingUseCase::new(Arc::clone(&stored.full), stored.units());
    let light = ReadReadingUseCase::new(Arc::clone(&stored.light), stored.units());

    let section = section_handle(&stored, "topic-a").await;
    let command = read_command(paragraph_handle(&section, 1), None);
    let expected = full.execute(command.clone()).await.expect("full");
    let actual = light.execute(command).await.expect("light");
    assert_eq!(actual, expected);
    assert_eq!(actual.items[0].text, "Alpha one.");
}

// ------------------------------------------------------------------------------------------------
// v1 search_document
// ------------------------------------------------------------------------------------------------

#[tokio::test]
async fn search_document_loads_the_document_once_and_light() {
    let stored = stored().await;
    let document = &stored.document;
    let search_index: Arc<dyn SearchIndex> = Arc::new(InMemorySearchIndex::default());
    search_index.index(document).await.expect("index");
    let full = SearchDocumentUseCase::new(Arc::clone(&search_index), Arc::clone(&stored.full))
        .with_text_unit_index(stored.units());
    let light = SearchDocumentUseCase::new(search_index, Arc::clone(&stored.light))
        .with_text_unit_index(stored.units());

    let command = SearchDocumentCommand {
        document_id: document.id.clone(),
        query: "Topic B".into(),
        limit: 5,
    };
    let expected = full.execute(command.clone()).await.expect("full search");
    let actual = light.execute(command).await.expect("light search");
    assert_eq!(actual, expected);
    assert!(
        actual
            .hits
            .iter()
            .any(|hit| hit.candidate_kind == SearchCandidateKind::Section),
        "the query must produce a section-kind hit, got {:?}",
        actual.hits
    );

    // A paragraph query resolves a paragraph locator, which needs no section length at all.
    let command = SearchDocumentCommand {
        document_id: document.id.clone(),
        query: "Alpha three".into(),
        limit: 5,
    };
    let expected = full.execute(command.clone()).await.expect("full search");
    let actual = light.execute(command).await.expect("light search");
    assert_eq!(actual, expected);
}

// ------------------------------------------------------------------------------------------------
// v2 read and open
// ------------------------------------------------------------------------------------------------

async fn document_handle(stored: &Stored) -> DocumentHandle {
    DocumentHandle {
        doc_key: stored
            .index
            .document_key(&stored.document.id)
            .await
            .expect("document key")
            .expect("stored document"),
        checksum: handle_checksum(
            &stored.document.id,
            &stored.document.normalized_document_hash(),
            &stored.document.media_type,
        ),
    }
}

#[tokio::test]
async fn v2_read_never_loads_the_full_document() {
    let stored = stored().await;
    let document = &stored.document;
    let handle = document_handle(&stored).await;
    let section = SectionHandle {
        document: handle.clone(),
        section_key: stored
            .index
            .section_key(&document.id, &section_id("topic-a"))
            .await
            .expect("section key")
            .expect("stored section"),
    };
    let full = ReadReadingUseCase::new(Arc::clone(&stored.full), stored.units());
    let light = ReadReadingUseCase::new(Arc::clone(&stored.light), stored.units());

    for (label, command) in [
        (
            "section handle",
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
            "batch",
            ReadReadingCommand {
                from: section.to_string(),
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
    ] {
        let expected = full.execute(command.clone()).await.expect("full read");
        let actual = light.execute(command).await.expect("light read");
        assert_eq!(actual, expected, "{label}");
        assert!(!actual.items.is_empty(), "{label} returned no items");
    }
}

#[tokio::test]
async fn v2_open_never_loads_the_full_document() {
    let document = document_fixture();
    let full: Arc<dyn DocumentRepository> = Arc::new(InMemoryDocumentRepository::default());
    let light: Arc<dyn DocumentRepository> = Arc::new(LightOnlyRepository::new(Arc::clone(&full)));
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
    let open_light = Arc::new(OpenDocumentUseCase::with_text_unit_index(
        Arc::new(AllowAllPolicy),
        Arc::new(StaticRetriever),
        Arc::new(StaticParser),
        Arc::clone(&full),
        Arc::clone(&units),
        Arc::new(NoopSearchIndex),
    ));
    let full_use_case = OpenReadingUseCase::new(open_full, Arc::clone(&full), Arc::clone(&units));
    let light_use_case = OpenReadingUseCase::new(open_light, light, units);

    let command = OpenReadingCommand {
        source: document.source.clone(),
        refresh: true,
    };
    let expected = full_use_case
        .execute(command.clone())
        .await
        .expect("full open");
    let actual = light_use_case.execute(command).await.expect("light open");
    assert_eq!(actual, expected);
    assert_eq!(actual.sections, 5);
    assert_eq!(actual.top.len(), 1);
    assert_eq!(actual.top[0].1, "Root");
}

// ------------------------------------------------------------------------------------------------
// The ingest path the v2 `open` test needs
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
            bytes: b"# Light readers\n".to_vec(),
            etag: None,
            last_modified: None,
            metadata: BTreeMap::new(),
        })
    }
}

/// Parses the fixture bytes into the same document the tests build by hand, so `open` sees exactly
/// the document the readers then read.
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
