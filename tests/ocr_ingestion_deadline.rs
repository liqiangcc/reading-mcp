//! Virtual-time tests exercise the real OpenDocument -> BudgetedParser -> save path.
use async_trait::async_trait;
use reading_mcp::{
    application::{
        open_document::{OpenDocumentCommand, OpenDocumentUseCase},
        ports::{
            ApplicationError, DocumentRepository, Parser, RetrievalOptions, RetrievedResource,
            Retriever, SearchHit, SearchIndex, SourcePolicy,
        },
    },
    domain::{Document, DocumentId, DocumentSource, MediaType},
    infrastructure::{BudgetedParser, ResourceBudget},
    parsing::MarkdownParser,
};
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

struct Fixture {
    delay: u64,
    media: &'static str,
}
#[async_trait]
impl SourcePolicy for Fixture {
    async fn validate(&self, _: &DocumentSource) -> Result<(), ApplicationError> {
        Ok(())
    }
}
#[async_trait]
impl Retriever for Fixture {
    async fn retrieve(
        &self,
        source: &DocumentSource,
        _: &RetrievalOptions,
    ) -> Result<RetrievedResource, ApplicationError> {
        tokio::time::sleep(Duration::from_secs(self.delay)).await;
        Ok(RetrievedResource {
            source: source.clone(),
            final_source: source.clone(),
            media_type: MediaType(self.media.into()),
            bytes: b"# Synthetic fixture\n\nA complete paragraph for budget testing.".to_vec(),
            etag: None,
            last_modified: None,
            metadata: Default::default(),
        })
    }
}
struct DelayedParser(u64);
#[async_trait]
impl Parser for DelayedParser {
    async fn parse(&self, resource: RetrievedResource) -> Result<Document, ApplicationError> {
        tokio::time::sleep(Duration::from_secs(self.0)).await;
        MarkdownParser.parse(resource).await
    }
}
struct Repository {
    delay: u64,
    started: AtomicUsize,
    saved: Mutex<Option<Document>>,
}
#[async_trait]
impl DocumentRepository for Repository {
    async fn save(&self, document: Document) -> Result<(), ApplicationError> {
        self.started.fetch_add(1, Ordering::SeqCst);
        tokio::time::sleep(Duration::from_secs(self.delay)).await;
        *self.saved.lock().unwrap() = Some(document);
        Ok(())
    }
    async fn get(&self, _: &DocumentId) -> Result<Option<Document>, ApplicationError> {
        Ok(self.saved.lock().unwrap().clone())
    }
}
#[derive(Default)]
struct Index(AtomicUsize);
#[async_trait]
impl SearchIndex for Index {
    async fn index(&self, _: &Document) -> Result<(), ApplicationError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
    async fn search(
        &self,
        _: &DocumentId,
        _: &str,
        _: usize,
    ) -> Result<Vec<SearchHit>, ApplicationError> {
        Ok(vec![])
    }
}

async fn exercise(
    retrieval: u64,
    parsing: u64,
    publication: u64,
    enabled: bool,
    media: &'static str,
) -> (bool, u64, usize, bool, usize) {
    let fixture = Arc::new(Fixture {
        delay: retrieval,
        media,
    });
    let repository = Arc::new(Repository {
        delay: publication,
        started: AtomicUsize::new(0),
        saved: Mutex::new(None),
    });
    let index = Arc::new(Index::default());
    let parser = Arc::new(
        BudgetedParser::new(Arc::new(DelayedParser(parsing)), ResourceBudget::default())
            .with_ocr_budget(enabled),
    );
    let application = OpenDocumentUseCase::new(
        fixture.clone(),
        fixture,
        parser,
        repository.clone(),
        index.clone(),
    )
    .with_ocr_budget(enabled);
    let started = tokio::time::Instant::now();
    let result = application
        .execute(OpenDocumentCommand {
            source: DocumentSource("file:///public-synthetic.pdf".into()),
            options: RetrievalOptions::default(),
        })
        .await;
    if let Err(error) = &result {
        if enabled && media.starts_with("application/pdf") {
            assert_eq!(error, &ApplicationError::OcrTimeout);
        } else {
            assert!(
                matches!(error, ApplicationError::ResourceLimitExceeded(_)),
                "unexpected failure: {error}"
            );
        }
    }
    let saved = repository.saved.lock().unwrap().is_some();
    (
        result.is_ok(),
        started.elapsed().as_secs(),
        repository.started.load(Ordering::SeqCst),
        saved,
        index.0.load(Ordering::SeqCst),
    )
}

#[tokio::test(start_paused = true)]
async fn parse_and_publication_share_sixty_seconds_instead_of_resetting_each_stage() {
    // Retrieval 20 + parse 40 + publication 25 exceeds the shared deadline at 80.
    // The delayed save is cancelled before mutation, and indexing never begins.
    assert_eq!(
        exercise(20, 40, 25, true, "application/pdf").await,
        (false, 80, 1, false, 0)
    );
}

#[tokio::test(start_paused = true)]
async fn earlier_whole_open_deadline_includes_retrieval() {
    assert_eq!(
        exercise(50, 50, 0, true, "application/pdf").await,
        (false, 90, 0, false, 0)
    );
}

#[tokio::test(start_paused = true)]
async fn enabled_pdf_can_parse_past_thirty_seconds_and_publish_within_shared_budget() {
    assert_eq!(
        exercise(20, 40, 10, true, "application/pdf; charset=binary").await,
        (true, 70, 1, true, 1)
    );
}

#[tokio::test(start_paused = true)]
async fn disabled_pdf_and_non_pdf_keep_existing_thirty_second_parse_limit() {
    assert_eq!(
        exercise(0, 40, 0, false, "application/pdf").await,
        (false, 30, 0, false, 0)
    );
    assert_eq!(
        exercise(0, 40, 0, true, "text/markdown").await,
        (false, 30, 0, false, 0)
    );
}
