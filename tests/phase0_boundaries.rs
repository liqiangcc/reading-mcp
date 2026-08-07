use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use reading_mcp::application::open_document::{OpenDocumentCommand, OpenDocumentUseCase};
use reading_mcp::application::ports::{
    ApplicationError, DocumentRepository, Parser, RetrievalOptions, RetrievedResource, Retriever,
    SearchHit, SearchIndex, SourcePolicy,
};
use reading_mcp::domain::{
    ContentHash, Document, DocumentId, DocumentSource, Location, MediaType, Section, SectionId,
};
use reading_mcp::mcp::contracts::OpenDocumentRequest;
use schemars::schema_for;

struct AllowAllPolicy;

#[async_trait]
impl SourcePolicy for AllowAllPolicy {
    async fn validate(&self, _source: &DocumentSource) -> Result<(), ApplicationError> {
        Ok(())
    }
}

struct FakeRetriever;

#[async_trait]
impl Retriever for FakeRetriever {
    async fn retrieve(
        &self,
        source: &DocumentSource,
        _options: &RetrievalOptions,
    ) -> Result<RetrievedResource, ApplicationError> {
        Ok(RetrievedResource {
            source: source.clone(),
            final_source: source.clone(),
            media_type: MediaType("text/markdown".into()),
            bytes: b"# Virtual Memory\nPage tables".to_vec(),
            etag: None,
            last_modified: None,
            metadata: BTreeMap::new(),
        })
    }
}

struct FakeParser;

#[async_trait]
impl Parser for FakeParser {
    async fn parse(&self, resource: RetrievedResource) -> Result<Document, ApplicationError> {
        Ok(Document {
            id: DocumentId("doc:test".into()),
            source: resource.final_source,
            title: "Operating Systems".into(),
            media_type: resource.media_type,
            content_hash: ContentHash("sha256:test".into()),
            metadata: BTreeMap::new(),
            root_sections: vec![Section {
                id: SectionId("section://virtual-memory".into()),
                parent_id: None,
                title: "Virtual Memory".into(),
                level: 1,
                content: "Page tables".into(),
                location: Location::default(),
                children: vec![],
            }],
        })
    }
}

#[derive(Default)]
struct FakeRepository {
    saved: Mutex<Option<Document>>,
}

#[async_trait]
impl DocumentRepository for FakeRepository {
    async fn save(&self, document: Document) -> Result<(), ApplicationError> {
        *self.saved.lock().expect("repository mutex poisoned") = Some(document);
        Ok(())
    }

    async fn get(&self, id: &DocumentId) -> Result<Option<Document>, ApplicationError> {
        Ok(self
            .saved
            .lock()
            .expect("repository mutex poisoned")
            .as_ref()
            .filter(|document| &document.id == id)
            .cloned())
    }
}

#[derive(Default)]
struct FakeSearchIndex {
    indexed: Mutex<Vec<DocumentId>>,
}

#[async_trait]
impl SearchIndex for FakeSearchIndex {
    async fn index(&self, document: &Document) -> Result<(), ApplicationError> {
        self.indexed
            .lock()
            .expect("index mutex poisoned")
            .push(document.id.clone());
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

#[tokio::test]
async fn open_document_runs_only_against_abstract_ports() {
    let repository = Arc::new(FakeRepository::default());
    let index = Arc::new(FakeSearchIndex::default());

    let use_case = OpenDocumentUseCase::new(
        Arc::new(AllowAllPolicy),
        Arc::new(FakeRetriever),
        Arc::new(FakeParser),
        repository.clone(),
        index.clone(),
    );

    let result = use_case
        .execute(OpenDocumentCommand {
            source: DocumentSource("https://example.com/os.md".into()),
            options: RetrievalOptions::default(),
        })
        .await
        .expect("open_document should succeed with fakes");

    assert_eq!(result.document_id, DocumentId("doc:test".into()));
    assert_eq!(result.section_count, 1);
    assert!(repository
        .saved
        .lock()
        .expect("repository mutex poisoned")
        .is_some());
    assert_eq!(
        index.indexed.lock().expect("index mutex poisoned").as_slice(),
        &[DocumentId("doc:test".into())]
    );
}

#[test]
fn mcp_contract_schema_is_sdk_independent() {
    let schema = schema_for!(OpenDocumentRequest);
    let json = serde_json::to_value(schema).expect("schema should serialize");

    assert_eq!(
        json.pointer("/properties/source/type").and_then(|value| value.as_str()),
        Some("string")
    );
}
