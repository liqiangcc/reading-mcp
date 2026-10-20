//! `open_document` must ingest from one segmentation: never a whole-document unit set.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use reading_mcp::application::open_document::{OpenDocumentCommand, OpenDocumentUseCase};
use reading_mcp::application::ports::{
    ApplicationError, Parser, RetrievalOptions, RetrievedResource, Retriever, SectionTextUnits,
    SourcePolicy, TextUnitIndex,
};
use reading_mcp::domain::{
    ContentHash, Document, DocumentId, DocumentSegmentation, DocumentSource, Location, MediaType,
    ParagraphTextUnitSet, Section, SectionId, SentenceTextUnitSet, TextUnit,
};
use reading_mcp::infrastructure::{InMemoryDocumentRepository, NoopSearchIndex};

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
            bytes: b"# Sample\n\nAlpha beta. Gamma delta.\n\n- Item one. Item two.\n".to_vec(),
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
                content: "Alpha beta. Gamma delta.".into(),
                location: Location::default(),
                children: vec![],
            }],
        })
    }
}

#[derive(Default)]
struct CountingTextUnits {
    segmentation_calls: AtomicUsize,
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
        _document: &Arc<Document>,
        _paragraphs: Arc<ParagraphTextUnitSet>,
        _sentences: Arc<SentenceTextUnitSet>,
    ) -> Result<(), ApplicationError> {
        self.set_calls.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    async fn replace_document_segmentation(
        &self,
        _document: &Arc<Document>,
        _segmentation: Arc<DocumentSegmentation>,
    ) -> Result<(), ApplicationError> {
        self.segmentation_calls.fetch_add(1, Ordering::SeqCst);
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

#[tokio::test]
async fn open_document_ingests_from_one_segmentation_and_never_from_unit_sets() {
    let text_units = Arc::new(CountingTextUnits::default());
    let use_case = OpenDocumentUseCase::with_text_unit_index(
        Arc::new(AllowAllPolicy),
        Arc::new(StaticRetriever),
        Arc::new(StaticParser),
        Arc::new(InMemoryDocumentRepository::default()),
        text_units.clone(),
        Arc::new(NoopSearchIndex),
    );

    let result = use_case
        .execute(OpenDocumentCommand {
            source: DocumentSource("/books/sample.md".into()),
            options: RetrievalOptions::default(),
        })
        .await
        .expect("open should succeed");

    assert_eq!(text_units.segmentation_calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        text_units.set_calls.load(Ordering::SeqCst),
        0,
        "open_document must not build whole-document unit sets"
    );
    assert_eq!(result.section_count, 1);
    assert_eq!(
        result
            .reading_profile
            .canonical_text_coverage
            .paragraph_count,
        1
    );
}
