use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use reading_mcp::application::open_document::{
    OpenDocumentCommand, OpenDocumentResult, OpenDocumentUseCase,
};
use reading_mcp::application::ports::{
    ApplicationError, OpenResultStore, Parser, RetrievalOptions, RetrievedResource, Retriever,
    SearchHit, SearchIndex, SectionTextUnits, SourcePolicy, TextUnitIndex,
};
use reading_mcp::application::reading_profile::{
    CanonicalTextCoverage, LexicalSearchCapability, ReadingCapabilities,
    ReadingCapabilityAvailability, ReadingProfile, ReliabilitySummary, SegmentedReadingCapability,
    SentenceFirstCapability, SimpleReadingCapability, StructureCapability,
};
use reading_mcp::domain::{
    ContentHash, Document, DocumentId, DocumentSource, Location, MediaType, NormalizedDocumentHash,
    ParagraphTextUnitSet, Section, SectionId, SentenceTextUnitSet, TextUnit,
};
use reading_mcp::infrastructure::{InMemoryDocumentRepository, SqliteOpenResultStore};
use tempfile::tempdir;

struct AllowAllPolicy;

#[async_trait]
impl SourcePolicy for AllowAllPolicy {
    async fn validate(&self, _source: &DocumentSource) -> Result<(), ApplicationError> {
        Ok(())
    }
}

/// Returns whatever bytes the test last set, so a test can change the content at the same source.
struct ByteRetriever {
    bytes: Mutex<Vec<u8>>,
}

#[async_trait]
impl Retriever for ByteRetriever {
    async fn retrieve(
        &self,
        source: &DocumentSource,
        _options: &RetrievalOptions,
    ) -> Result<RetrievedResource, ApplicationError> {
        Ok(RetrievedResource {
            source: source.clone(),
            final_source: source.clone(),
            media_type: MediaType("text/markdown".into()),
            bytes: self.bytes.lock().expect("bytes mutex poisoned").clone(),
            etag: None,
            last_modified: None,
            metadata: BTreeMap::new(),
        })
    }
}

#[derive(Default)]
struct CountingParser {
    calls: AtomicUsize,
    fail: AtomicBool,
}

#[async_trait]
impl Parser for CountingParser {
    async fn parse(&self, resource: RetrievedResource) -> Result<Document, ApplicationError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.fail.load(Ordering::SeqCst) {
            return Err(ApplicationError::ParseFailed(
                "injected parse failure".into(),
            ));
        }
        let content_hash = ContentHash::of_bytes(&resource.bytes);
        let id = DocumentId::derive(&resource.final_source, &content_hash);
        Ok(Document {
            id,
            source: resource.final_source,
            title: "Sample".into(),
            media_type: resource.media_type,
            content_hash,
            metadata: BTreeMap::new(),
            root_sections: vec![Section {
                id: SectionId("section://sample".into()),
                parent_id: None,
                title: "Sample".into(),
                level: 1,
                content: "Page tables".into(),
                location: Location::default(),
                children: vec![],
            }],
        })
    }
}

#[derive(Default)]
struct CountingTextUnits {
    set_calls: AtomicUsize,
}

#[async_trait]
impl TextUnitIndex for CountingTextUnits {
    async fn replace_document(
        &self,
        _document_id: &DocumentId,
        _units: &[TextUnit],
    ) -> Result<(), ApplicationError> {
        Ok(())
    }

    async fn list_document(
        &self,
        _document_id: &DocumentId,
    ) -> Result<Vec<TextUnit>, ApplicationError> {
        Ok(vec![])
    }

    async fn replace_document_sets(
        &self,
        _document_id: &DocumentId,
        _paragraphs: Arc<ParagraphTextUnitSet>,
        _sentences: Arc<SentenceTextUnitSet>,
    ) -> Result<(), ApplicationError> {
        self.set_calls.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    async fn section_units(
        &self,
        _document_id: &DocumentId,
        _section_id: &SectionId,
    ) -> Result<Option<SectionTextUnits>, ApplicationError> {
        Ok(None)
    }
}

#[derive(Default)]
struct CountingSearch {
    index_calls: AtomicUsize,
}

#[async_trait]
impl SearchIndex for CountingSearch {
    async fn index(&self, _document: &Document) -> Result<(), ApplicationError> {
        Ok(())
    }

    async fn index_with_units(
        &self,
        _document: Arc<Document>,
        _paragraphs: Arc<ParagraphTextUnitSet>,
        _sentences: Arc<SentenceTextUnitSet>,
    ) -> Result<(), ApplicationError> {
        self.index_calls.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    async fn search(
        &self,
        _document_id: &DocumentId,
        _query: &str,
        _limit: usize,
    ) -> Result<Vec<SearchHit>, ApplicationError> {
        Ok(vec![])
    }
}

struct Harness {
    retriever: Arc<ByteRetriever>,
    parser: Arc<CountingParser>,
    text_units: Arc<CountingTextUnits>,
    search: Arc<CountingSearch>,
}

impl Harness {
    fn new(bytes: &[u8]) -> Self {
        Self {
            retriever: Arc::new(ByteRetriever {
                bytes: Mutex::new(bytes.to_vec()),
            }),
            parser: Arc::new(CountingParser::default()),
            text_units: Arc::new(CountingTextUnits::default()),
            search: Arc::new(CountingSearch::default()),
        }
    }

    fn use_case(&self, store: Option<(Arc<dyn OpenResultStore>, &str)>) -> OpenDocumentUseCase {
        let use_case = OpenDocumentUseCase::with_text_unit_index(
            Arc::new(AllowAllPolicy),
            self.retriever.clone(),
            self.parser.clone(),
            Arc::new(InMemoryDocumentRepository::default()),
            self.text_units.clone(),
            self.search.clone(),
        );
        match store {
            Some((store, fingerprint)) => {
                use_case.with_open_result_store(store, fingerprint.to_owned())
            }
            None => use_case,
        }
    }
}

fn command(source: &str) -> OpenDocumentCommand {
    OpenDocumentCommand {
        source: DocumentSource(source.into()),
        options: RetrievalOptions::default(),
    }
}

fn sample_result(id: &str) -> OpenDocumentResult {
    OpenDocumentResult {
        document_id: DocumentId(id.into()),
        source: DocumentSource("/books/sample.md".into()),
        title: "Sample".into(),
        media_type: MediaType("text/markdown".into()),
        content_hash: ContentHash("sha256:abc".into()),
        normalized_document_hash: NormalizedDocumentHash("nd:abc".into()),
        normalized_document_hash_version: "normalized-document-hash/v1".into(),
        normalization_version: "normalization/v1".into(),
        normalized_text_coordinate_space: "normalized-text/v1".into(),
        section_count: 2,
        reading_profile: ReadingProfile {
            schema_version: "reading-profile/v1".into(),
            capabilities: ReadingCapabilities {
                structural_navigation: StructureCapability {
                    availability: ReadingCapabilityAvailability::Available,
                    section_count: 2,
                },
                paragraph_enumeration: SegmentedReadingCapability {
                    availability: ReadingCapabilityAvailability::Available,
                    segmentation_version: "text-segmentation/v1".into(),
                },
                sentence_first_enumeration: SentenceFirstCapability {
                    availability: ReadingCapabilityAvailability::Available,
                    segmentation_version: "text-segmentation/v1".into(),
                    source_preserving_coarse_regions: true,
                },
                exact_locator_read: SimpleReadingCapability {
                    availability: ReadingCapabilityAvailability::Available,
                },
                locator_context: SimpleReadingCapability {
                    availability: ReadingCapabilityAvailability::Available,
                },
                lexical_search: LexicalSearchCapability {
                    availability: ReadingCapabilityAvailability::Unavailable,
                    precise_candidates: false,
                },
            },
            canonical_text_coverage: CanonicalTextCoverage::default(),
            reliability: ReliabilitySummary::not_applicable(),
        },
    }
}

#[tokio::test]
async fn reopening_same_bytes_returns_the_stored_result_without_reparsing() {
    let directory = tempdir().expect("temp directory should be created");
    let store: Arc<dyn OpenResultStore> = Arc::new(
        SqliteOpenResultStore::open(directory.path().join("reading-mcp.sqlite"))
            .expect("store should open"),
    );
    let harness = Harness::new(b"# Sample\nPage tables");

    let first = harness
        .use_case(Some((store.clone(), "fp-a")))
        .execute(command("/books/sample.md"))
        .await
        .expect("first open should succeed");
    let second = harness
        .use_case(Some((store.clone(), "fp-a")))
        .execute(command("/books/sample.md"))
        .await
        .expect("reopen should succeed");

    assert_eq!(harness.parser.calls.load(Ordering::SeqCst), 1);
    assert_eq!(harness.text_units.set_calls.load(Ordering::SeqCst), 1);
    assert_eq!(harness.search.index_calls.load(Ordering::SeqCst), 1);
    assert_eq!(second, first);
}

#[tokio::test]
async fn changed_bytes_at_the_same_source_are_parsed_again() {
    let directory = tempdir().expect("temp directory should be created");
    let store: Arc<dyn OpenResultStore> = Arc::new(
        SqliteOpenResultStore::open(directory.path().join("reading-mcp.sqlite"))
            .expect("store should open"),
    );
    let harness = Harness::new(b"# Sample\nPage tables");

    let first = harness
        .use_case(Some((store.clone(), "fp-a")))
        .execute(command("/books/sample.md"))
        .await
        .expect("first open should succeed");
    *harness
        .retriever
        .bytes
        .lock()
        .expect("bytes mutex poisoned") = b"# Sample\nPage tables, revised".to_vec();
    let second = harness
        .use_case(Some((store.clone(), "fp-a")))
        .execute(command("/books/sample.md"))
        .await
        .expect("second open should succeed");

    assert_eq!(harness.parser.calls.load(Ordering::SeqCst), 2);
    assert_ne!(second.document_id, first.document_id);
}

#[tokio::test]
async fn a_different_ingest_fingerprint_is_parsed_again() {
    let directory = tempdir().expect("temp directory should be created");
    let store: Arc<dyn OpenResultStore> = Arc::new(
        SqliteOpenResultStore::open(directory.path().join("reading-mcp.sqlite"))
            .expect("store should open"),
    );
    let harness = Harness::new(b"# Sample\nPage tables");

    harness
        .use_case(Some((store.clone(), "fp-a")))
        .execute(command("/books/sample.md"))
        .await
        .expect("first open should succeed");
    harness
        .use_case(Some((store.clone(), "fp-b")))
        .execute(command("/books/sample.md"))
        .await
        .expect("open under another fingerprint should succeed");

    assert_eq!(harness.parser.calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn without_a_store_every_open_parses() {
    let harness = Harness::new(b"# Sample\nPage tables");

    harness
        .use_case(None)
        .execute(command("/books/sample.md"))
        .await
        .expect("first open should succeed");
    harness
        .use_case(None)
        .execute(command("/books/sample.md"))
        .await
        .expect("second open should succeed");

    assert_eq!(harness.parser.calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn a_failed_parse_stores_nothing() {
    let directory = tempdir().expect("temp directory should be created");
    let store: Arc<dyn OpenResultStore> = Arc::new(
        SqliteOpenResultStore::open(directory.path().join("reading-mcp.sqlite"))
            .expect("store should open"),
    );
    let harness = Harness::new(b"# Sample\nPage tables");

    harness.parser.fail.store(true, Ordering::SeqCst);
    let failed = harness
        .use_case(Some((store.clone(), "fp-a")))
        .execute(command("/books/sample.md"))
        .await;
    assert!(failed.is_err());

    harness.parser.fail.store(false, Ordering::SeqCst);
    harness
        .use_case(Some((store.clone(), "fp-a")))
        .execute(command("/books/sample.md"))
        .await
        .expect("open after the failure should succeed");

    assert_eq!(harness.parser.calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn sqlite_store_round_trips_and_checks_the_fingerprint() {
    let directory = tempdir().expect("temp directory should be created");
    let store = SqliteOpenResultStore::open(directory.path().join("reading-mcp.sqlite"))
        .expect("store should open");
    let result = sample_result("doc:round-trip");
    let id = DocumentId("doc:round-trip".into());

    store
        .put(&result, "fp-a")
        .await
        .expect("put should succeed");

    assert_eq!(
        store.get(&id, "fp-a").await.expect("get should succeed"),
        Some(result)
    );
    assert_eq!(
        store.get(&id, "fp-b").await.expect("get should succeed"),
        None
    );
}

#[tokio::test]
async fn sqlite_store_treats_an_undecodable_row_as_a_miss() {
    let directory = tempdir().expect("temp directory should be created");
    let path = directory.path().join("reading-mcp.sqlite");
    let store = SqliteOpenResultStore::open(&path).expect("store should open");
    let id = DocumentId("doc:corrupt".into());
    store
        .put(&sample_result("doc:corrupt"), "fp-a")
        .await
        .expect("put should succeed");

    rusqlite::Connection::open(&path)
        .expect("database should open")
        .execute("UPDATE open_results_v1 SET result_json = '{'", [])
        .expect("corruption should be written");

    assert_eq!(
        store.get(&id, "fp-a").await.expect("get should not fail"),
        None
    );
}

#[tokio::test]
async fn sqlite_store_remove_deletes_the_result() {
    let directory = tempdir().expect("temp directory should be created");
    let store = SqliteOpenResultStore::open(directory.path().join("reading-mcp.sqlite"))
        .expect("store should open");
    let id = DocumentId("doc:removed".into());
    store
        .put(&sample_result("doc:removed"), "fp-a")
        .await
        .expect("put should succeed");

    store.remove(&id).await.expect("remove should succeed");

    assert_eq!(
        store.get(&id, "fp-a").await.expect("get should succeed"),
        None
    );
}

#[tokio::test]
async fn sqlite_store_result_survives_reopening_the_database() {
    let directory = tempdir().expect("temp directory should be created");
    let path = directory.path().join("reading-mcp.sqlite");
    let result = sample_result("doc:durable");
    let id = DocumentId("doc:durable".into());
    {
        let store = SqliteOpenResultStore::open(&path).expect("store should open");
        store
            .put(&result, "fp-a")
            .await
            .expect("put should succeed");
    }

    let reopened = SqliteOpenResultStore::open(&path).expect("store should reopen");

    assert_eq!(
        reopened.get(&id, "fp-a").await.expect("get should succeed"),
        Some(result)
    );
}
