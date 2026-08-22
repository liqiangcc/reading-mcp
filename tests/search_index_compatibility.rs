use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use reading_mcp::application::ports::{
    ApplicationError, DocumentRepository, SearchHit, SearchIndex,
};
use reading_mcp::application::search_document::{
    SearchCandidateKind, SearchDocumentCommand, SearchDocumentUseCase,
};
use reading_mcp::domain::{
    ContentHash, Document, DocumentId, DocumentSource, Location, MediaType, Section, SectionId,
};
use reading_mcp::infrastructure::InMemoryDocumentRepository;

#[derive(Default)]
struct LegacySearchIndex;

#[async_trait]
impl SearchIndex for LegacySearchIndex {
    async fn index(&self, _document: &Document) -> Result<(), ApplicationError> {
        Ok(())
    }

    async fn search(
        &self,
        document_id: &DocumentId,
        _query: &str,
        _limit: usize,
    ) -> Result<Vec<SearchHit>, ApplicationError> {
        if document_id.0 != "doc:legacy-search" {
            return Err(ApplicationError::DocumentNotFound);
        }
        Ok(vec![SearchHit {
            section_id: SectionId("section://topic".into()),
            title: "Topic".into(),
            source: DocumentSource("memory:legacy-search".into()),
            snippet: "legacy preview".into(),
            score: 1.0,
            location: Location {
                section_path: vec!["Topic".into()],
                native_location: Some("legacy-search-unit:1".into()),
                ..Location::default()
            },
        }])
    }
}

#[tokio::test]
async fn legacy_search_adapter_keeps_section_level_handoff_without_precise_port() {
    let document = Document {
        id: DocumentId("doc:legacy-search".into()),
        source: DocumentSource("memory:legacy-search".into()),
        title: "Legacy".into(),
        media_type: MediaType("text/plain".into()),
        content_hash: ContentHash("sha256:legacy".into()),
        metadata: BTreeMap::new(),
        root_sections: vec![Section {
            id: SectionId("section://topic".into()),
            parent_id: None,
            title: "Topic".into(),
            level: 1,
            content: "Canonical body.".into(),
            location: Location {
                section_path: vec!["Topic".into()],
                ..Location::default()
            },
            children: vec![],
        }],
    };
    let repository = Arc::new(InMemoryDocumentRepository::default());
    repository.save(document.clone()).await.expect("save");

    let result = SearchDocumentUseCase::new(Arc::new(LegacySearchIndex), repository)
        .execute(SearchDocumentCommand {
            document_id: document.id,
            query: "legacy".into(),
            limit: 10,
        })
        .await
        .expect("legacy adapter should keep working");

    assert_eq!(
        result.tokenizer_version,
        "legacy-search-tokenizer/unversioned"
    );
    assert_eq!(result.hits.len(), 1);
    assert_eq!(result.hits[0].candidate_kind, SearchCandidateKind::Section);
    assert_eq!(
        result.hits[0].text_locator.owner_section_id.0,
        "section://topic"
    );
    assert!(result.hits[0].text_locator.normalized_range.is_none());
    assert_eq!(
        result.hits[0].location.native_location.as_deref(),
        Some("legacy-search-unit:1")
    );
}
