//! The hot readers hold only the light document (R1c-3c).
//!
//! Every test here builds one document and stores it twice: in a repository that can serve the full
//! decoded document, and behind [`LightOnlyRepository`], whose `get` and `get_with_hash` panic. A
//! reader that asked for the full document would fail the test instead of silently reading an empty
//! section text, and every answer of the light run must equal the full run byte for byte.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, RwLock};

use async_trait::async_trait;
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
use reading_mcp::application::ports::{
    ApplicationError, DocumentRepository, Parser, RetrievalOptions, RetrievedResource, Retriever,
    SearchIndex, SectionTextUnits, SourcePolicy, TextUnitIndex,
};
use reading_mcp::application::read_document::{
    ContinueReadCommand, ReadDocumentUseCase, ReadExactTargetCommand, ReadSectionCommand,
};
use reading_mcp::application::read_reading::{ReadReadingCommand, ReadReadingUseCase};
use reading_mcp::application::search_document::{
    SearchCandidateKind, SearchDocumentCommand, SearchDocumentUseCase,
};
use reading_mcp::application::short_handle::{DocumentHandle, SectionHandle, handle_checksum};
use reading_mcp::application::text_unit_store::store_text_units;
use reading_mcp::domain::{
    ContentHash, Document, DocumentId, DocumentSource, Location, MediaType, NormalizedDocumentHash,
    ParagraphTextUnitSet, Section, SectionId, SentenceTextUnitSet, TextLocator, TextUnit,
};
use reading_mcp::infrastructure::{
    InMemoryDocumentRepository, InMemorySearchIndex, InMemoryTextUnitIndex, NoopSearchIndex,
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
}

fn clear_section_text(sections: &mut [Section]) {
    for section in sections {
        section.content.clear();
        clear_section_text(&mut section.children);
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
        Ok(self.section_ids(document_id).map(|_| 0))
    }

    async fn document_id_for_key(
        &self,
        doc_key: i64,
    ) -> Result<Option<DocumentId>, ApplicationError> {
        if doc_key != 0 {
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
        let units: Arc<dyn TextUnitIndex> = Arc::clone(&self.index);
        units
    }
}

// ------------------------------------------------------------------------------------------------
// v1 read_document
// ------------------------------------------------------------------------------------------------

#[tokio::test]
async fn read_document_renders_a_subtree_from_the_light_document() {
    let stored = stored().await;
    let document = &stored.document;
    let full =
        ReadDocumentUseCase::new(Arc::clone(&stored.full)).with_text_unit_index(stored.units());
    let light =
        ReadDocumentUseCase::new(Arc::clone(&stored.light)).with_text_unit_index(stored.units());

    for name in ["root", "topic-a", "child", "topic-b", "empty"] {
        let command = ReadSectionCommand {
            document_id: document.id.clone(),
            section_id: section_id(name),
            max_chars: None,
        };
        let expected = full.execute(command.clone()).await.expect("full read");
        let actual = light.execute(command).await.expect("light read");
        assert_eq!(actual, expected, "section {name}");

        // A small budget exercises the continuation cursor over the same rendered subtree.
        let command = ReadSectionCommand {
            document_id: document.id.clone(),
            section_id: section_id(name),
            max_chars: Some(12),
        };
        let expected = full.execute(command.clone()).await.expect("full read");
        let actual = light.execute(command).await.expect("light read");
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
            let actual = light
                .continue_read(continuation)
                .await
                .expect("light continuation");
            assert_eq!(actual, expected, "section {name} continuation");
        }
    }

    // The parent's rendering really carries its child's text, so the comparison above cannot pass
    // on two equally empty renderings.
    let parent = light
        .execute(ReadSectionCommand {
            document_id: document.id.clone(),
            section_id: section_id("root"),
            max_chars: None,
        })
        .await
        .expect("root read");
    assert!(parent.content.contains("Root overview sentence."));
    assert!(parent.content.contains("Child text one. Child text two."));
    assert!(parent.content.contains("Beta one. Beta two."));
    assert_eq!(parent.stream.total_chars, parent.content.chars().count());
}

#[tokio::test]
async fn read_document_exact_reads_one_section_from_the_light_document() {
    let stored = stored().await;
    let document = &stored.document;
    let full =
        ReadDocumentUseCase::new(Arc::clone(&stored.full)).with_text_unit_index(stored.units());
    let light =
        ReadDocumentUseCase::new(Arc::clone(&stored.light)).with_text_unit_index(stored.units());

    let target = document
        .find_section(&section_id("topic-a"))
        .expect("topic A");
    let command = ReadExactTargetCommand {
        document_id: document.id.clone(),
        target_locator: TextLocator::for_section(document, target),
        max_chars: Some(7),
    };
    let expected = full.read_exact(command.clone()).await.expect("full");
    let actual = light.read_exact(command).await.expect("light");
    assert_eq!(actual, expected);
    assert_eq!(actual.content, "Alpha o");
}

// ------------------------------------------------------------------------------------------------
// v1 get_context
// ------------------------------------------------------------------------------------------------

#[tokio::test]
async fn get_context_serves_every_relation_from_the_light_document() {
    let stored = stored().await;
    let document = &stored.document;
    let full =
        GetContextUseCase::new(Arc::clone(&stored.full)).with_text_unit_index(stored.units());
    let light =
        GetContextUseCase::new(Arc::clone(&stored.light)).with_text_unit_index(stored.units());

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
    let actual = light.execute(neighbour).await.expect("light neighbour");
    assert_eq!(actual, expected);
    // The anchor section's own text and its neighbours' text are all in the answer.
    assert!(actual.content.contains("Alpha one."));
    assert!(actual.content.contains("Root overview sentence."));
    assert!(actual.content.contains("Beta one."));
    assert_eq!(actual.items.len(), 4);

    for (label, target, relation) in [
        (
            "section container",
            ContextTarget::Section(section_id("topic-a")),
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
            ContextTarget::Section(section_id("child")),
            ContextRelation::Structural {
                kind: StructuralContextKind::Ancestors,
            },
        ),
        (
            "siblings",
            ContextTarget::Section(section_id("child")),
            ContextRelation::Structural {
                kind: StructuralContextKind::Siblings,
            },
        ),
        (
            "children",
            ContextTarget::Section(section_id("topic-a")),
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
        let actual = light
            .execute_structured(command)
            .await
            .expect("light structured context");
        assert_eq!(actual, expected, "{label}");
    }

    // A paragraph locator anchor exercises `resolve_text_locator` on the light document.
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
    let actual = light
        .execute_structured(command)
        .await
        .expect("light paragraph neighbours");
    assert_eq!(actual, expected);
    assert_eq!(
        actual
            .items
            .iter()
            .map(|item| item.content.as_deref())
            .collect::<Vec<_>>(),
        vec![Some("Alpha one. Alpha two."), Some("Alpha three.")]
    );
}

#[tokio::test]
async fn get_context_section_container_reads_only_the_container_text() {
    let stored = stored().await;
    let document = &stored.document;
    let full =
        GetContextUseCase::new(Arc::clone(&stored.full)).with_text_unit_index(stored.units());
    let light =
        GetContextUseCase::new(Arc::clone(&stored.light)).with_text_unit_index(stored.units());
    let command = GetStructuredContextCommand {
        document_id: document.id.clone(),
        target: ContextTarget::Section(section_id("child")),
        relation: ContextRelation::Container {
            kind: ContextContainerKind::Section,
        },
        max_chars: None,
    };
    let expected = full
        .execute_structured(command.clone())
        .await
        .expect("full container");
    let actual = light
        .execute_structured(command)
        .await
        .expect("light container");
    assert_eq!(actual, expected);
    assert_eq!(
        actual.content,
        "### Topic A Child\n\nChild text one. Child text two."
    );
}

// ------------------------------------------------------------------------------------------------
// v1 get_text_units
// ------------------------------------------------------------------------------------------------

#[tokio::test]
async fn get_text_units_reads_the_light_document_even_for_a_section_anchor() {
    let stored = stored().await;
    let document = &stored.document;
    let full = GetTextUnitsUseCase::new(Arc::clone(&stored.full), stored.units());
    let light = GetTextUnitsUseCase::new(Arc::clone(&stored.light), stored.units());

    let topic_a = document
        .find_section(&section_id("topic-a"))
        .expect("topic A");
    let section_locator = TextLocator::for_section(document, topic_a);
    let expected = full
        .execute_from_anchor(
            command(document, RequestedTextUnitKind::Sentence),
            section_locator.clone(),
        )
        .await
        .expect_err("a section anchor is not part of a text-unit stream");
    let actual = light
        .execute_from_anchor(
            command(document, RequestedTextUnitKind::Sentence),
            section_locator,
        )
        .await
        .expect_err("a section anchor is not part of a text-unit stream");
    assert_eq!(actual, expected);

    // A paragraph anchor is the supported shape, and it must be identical too.
    let paragraph = document
        .paragraph_text_units()
        .units
        .into_iter()
        .find(|unit| unit.owner_section_id == topic_a.id && unit.paragraph_index == 1)
        .expect("first paragraph");
    let paragraph_locator = TextLocator::for_paragraph(document, topic_a, &paragraph);
    let expected = full
        .execute_from_anchor(
            command(document, RequestedTextUnitKind::Sentence),
            paragraph_locator.clone(),
        )
        .await
        .expect("full anchored read");
    let actual = light
        .execute_from_anchor(
            command(document, RequestedTextUnitKind::Sentence),
            paragraph_locator,
        )
        .await
        .expect("light anchored read");
    assert_eq!(actual, expected);
    assert!(!actual.items.is_empty());
}

fn command(document: &Document, kind: RequestedTextUnitKind) -> GetTextUnitsCommand {
    GetTextUnitsCommand {
        document_id: document.id.clone(),
        section_id: section_id("topic-a"),
        requested_kind: kind,
        direction: TextUnitDirection::Forward,
        coverage_policy: TextUnitCoveragePolicy::PreserveSource,
        max_items: 8,
        max_chars: None,
        cursor: None,
    }
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
