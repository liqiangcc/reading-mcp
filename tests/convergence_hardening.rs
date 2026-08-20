use std::collections::BTreeMap;
use std::sync::Arc;

use reading_mcp::application::get_context::{GetContextCommand, GetContextUseCase};
use reading_mcp::application::get_document_structure::GetDocumentStructureUseCase;
use reading_mcp::application::ports::{
    ApplicationError, DocumentRepository, SearchIndex,
};
use reading_mcp::application::read_document::{ReadDocumentUseCase, ReadSectionCommand};
use reading_mcp::domain::{
    ContentHash, Document, DocumentId, DocumentSource, Location, MediaType, Section, SectionId,
};
use reading_mcp::infrastructure::{
    AdaptiveSearchIndex, InMemoryDocumentRepository, InMemorySearchIndex,
};

#[tokio::test]
async fn default_read_and_context_responses_are_server_bounded() {
    let repository: Arc<dyn DocumentRepository> =
        Arc::new(InMemoryDocumentRepository::default());
    let document = large_document();
    repository.save(document.clone()).await.unwrap();

    let read = ReadDocumentUseCase::new(repository.clone())
        .execute(ReadSectionCommand {
            document_id: document.id.clone(),
            section_id: SectionId("section://large".into()),
            max_chars: None,
        })
        .await
        .unwrap();
    assert!(read.truncated);
    assert_eq!(read.content.chars().count(), 40_000);

    let context = GetContextUseCase::new(repository)
        .execute(GetContextCommand {
            document_id: document.id,
            section_id: SectionId("section://large".into()),
            before: 0,
            after: 0,
            max_chars: None,
        })
        .await
        .unwrap();
    assert!(context.truncated);
    assert_eq!(context.content.chars().count(), 24_000);
}

#[tokio::test]
async fn oversized_structure_is_rejected_before_becoming_an_mcp_payload() {
    let repository: Arc<dyn DocumentRepository> =
        Arc::new(InMemoryDocumentRepository::default());
    let document = Document {
        id: DocumentId("doc:wide".into()),
        source: DocumentSource("memory:wide".into()),
        title: "Wide".into(),
        media_type: MediaType("text/plain".into()),
        content_hash: ContentHash("sha256:wide".into()),
        metadata: BTreeMap::new(),
        root_sections: (0..2_001)
            .map(|index| Section {
                id: SectionId(format!("section://{index}")),
                parent_id: None,
                title: format!("Section {index}"),
                level: 1,
                content: String::new(),
                location: Location::default(),
                children: vec![],
            })
            .collect(),
    };
    repository.save(document.clone()).await.unwrap();

    let error = GetDocumentStructureUseCase::new(repository)
        .execute(document.id, None)
        .await
        .expect_err("oversized structures must be rejected");
    assert!(matches!(error, ApplicationError::ResourceLimitExceeded(_)));
}

#[tokio::test]
async fn adaptive_search_recalls_cjk_natural_language_queries() {
    let repository: Arc<dyn DocumentRepository> =
        Arc::new(InMemoryDocumentRepository::default());
    let inner: Arc<dyn SearchIndex> = Arc::new(InMemorySearchIndex::default());
    let adaptive = AdaptiveSearchIndex::new(inner, repository.clone());
    let document = Document {
        id: DocumentId("doc:cjk".into()),
        source: DocumentSource("memory:cjk".into()),
        title: "操作系统".into(),
        media_type: MediaType("text/markdown".into()),
        content_hash: ContentHash("sha256:cjk".into()),
        metadata: BTreeMap::new(),
        root_sections: vec![Section {
            id: SectionId("section://virtual-memory".into()),
            parent_id: None,
            title: "虚拟内存".into(),
            level: 1,
            content: "页面置换算法用于在物理内存不足时选择需要淘汰的页面。".into(),
            location: Location::default(),
            children: vec![],
        }],
    };
    repository.save(document.clone()).await.unwrap();
    adaptive.index(&document).await.unwrap();

    let hits = adaptive
        .search(&document.id, "什么是虚拟内存的页面置换算法", 10)
        .await
        .unwrap();
    assert!(!hits.is_empty());
    assert_eq!(hits[0].section_id.0, "section://virtual-memory");
    assert!(hits[0].snippet.contains("页面置换算法"));
}

fn large_document() -> Document {
    Document {
        id: DocumentId("doc:large".into()),
        source: DocumentSource("memory:large".into()),
        title: "Large".into(),
        media_type: MediaType("text/plain".into()),
        content_hash: ContentHash("sha256:large".into()),
        metadata: BTreeMap::new(),
        root_sections: vec![Section {
            id: SectionId("section://large".into()),
            parent_id: None,
            title: "Large".into(),
            level: 1,
            content: "x".repeat(100_000),
            location: Location::default(),
            children: vec![],
        }],
    }
}
