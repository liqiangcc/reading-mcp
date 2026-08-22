use std::sync::Arc;

use reading_mcp::application::open_document::{OpenDocumentCommand, OpenDocumentUseCase};
use reading_mcp::application::ports::RetrievalOptions;
use reading_mcp::application::read_document::{ReadDocumentUseCase, ReadExactTargetCommand};
use reading_mcp::application::search_document::{
    SearchCandidateKind, SearchDocumentCommand, SearchDocumentUseCase,
};
use reading_mcp::domain::DocumentSource;
use reading_mcp::infrastructure::{InMemoryDocumentRepository, InMemorySearchIndex};
use reading_mcp::parsing::ParserRouter;
use reading_mcp::retrieval::{FileRetriever, LocalFileSourcePolicy};
use tempfile::tempdir;

#[tokio::test]
async fn paragraph_like_search_unit_hands_off_only_the_truthful_owner_section_locator() {
    let directory = tempdir().expect("temp directory");
    let path = directory.path().join("book.md");
    tokio::fs::write(
        &path,
        "# Book\n\nOverview.\n\n## Topic\n\nFirst paragraph gives context.\n\nNeedle phrase lives in a narrower retrieval unit.\n\n### Child\n\nChild-only text.\n",
    )
    .await
    .expect("fixture");

    let repository = Arc::new(InMemoryDocumentRepository::default());
    let index = Arc::new(InMemorySearchIndex::default());
    let opened = OpenDocumentUseCase::new(
        Arc::new(LocalFileSourcePolicy::allow_roots([directory.path()])),
        Arc::new(FileRetriever),
        Arc::new(ParserRouter::phase1()),
        repository.clone(),
        index.clone(),
    )
    .execute(OpenDocumentCommand {
        source: DocumentSource(path.to_string_lossy().into_owned()),
        options: RetrievalOptions::default(),
    })
    .await
    .expect("open");

    let searched = SearchDocumentUseCase::new(index, repository.clone())
        .execute(SearchDocumentCommand {
            document_id: opened.document_id.clone(),
            query: "needle phrase".into(),
            limit: 10,
        })
        .await
        .expect("search");
    let hit = searched.hits.first().expect("hit");

    assert_eq!(hit.candidate_kind, SearchCandidateKind::Section);
    assert_eq!(hit.section_id.0, "section://book/topic");
    assert_eq!(hit.text_locator.owner_section_id, hit.section_id);
    assert_eq!(hit.text_locator.document_id, opened.document_id);
    assert_eq!(hit.text_locator.content_hash.0, opened.content_hash.0);
    assert_eq!(
        hit.text_locator.normalized_document_hash.0,
        opened.normalized_document_hash.0
    );
    assert!(hit.text_locator.paragraph_index.is_none());
    assert!(hit.text_locator.sentence_index.is_none());
    assert!(hit.text_locator.normalized_range.is_none());
    assert!(hit.text_locator.segmentation_version.is_none());
    assert!(
        hit.location
            .native_location
            .as_deref()
            .is_some_and(|value| value.contains("search-unit")),
        "legacy retrieval location may remain narrower than the canonical handoff"
    );

    let read = ReadDocumentUseCase::new(repository)
        .read_exact(ReadExactTargetCommand {
            document_id: opened.document_id,
            target_locator: hit.text_locator.clone(),
            max_chars: None,
        })
        .await
        .expect("search locator should hand off to exact read");
    assert!(read.content.contains("First paragraph gives context."));
    assert!(read.content.contains("Needle phrase lives"));
    assert!(!read.content.contains("Child-only text."));
}

#[tokio::test]
async fn title_only_hit_remains_section_level_and_never_fabricates_text_unit_identity() {
    let directory = tempdir().expect("temp directory");
    let path = directory.path().join("titles.md");
    tokio::fs::write(&path, "# Book\n\nIntro.\n\n## Empty Target\n")
        .await
        .expect("fixture");

    let repository = Arc::new(InMemoryDocumentRepository::default());
    let index = Arc::new(InMemorySearchIndex::default());
    let opened = OpenDocumentUseCase::new(
        Arc::new(LocalFileSourcePolicy::allow_roots([directory.path()])),
        Arc::new(FileRetriever),
        Arc::new(ParserRouter::phase1()),
        repository.clone(),
        index.clone(),
    )
    .execute(OpenDocumentCommand {
        source: DocumentSource(path.to_string_lossy().into_owned()),
        options: RetrievalOptions::default(),
    })
    .await
    .expect("open");

    let searched = SearchDocumentUseCase::new(index, repository)
        .execute(SearchDocumentCommand {
            document_id: opened.document_id,
            query: "Empty Target".into(),
            limit: 10,
        })
        .await
        .expect("search");
    let hit = searched.hits.first().expect("title hit");

    assert_eq!(hit.candidate_kind, SearchCandidateKind::Section);
    assert_eq!(
        hit.text_locator.owner_section_id.0,
        "section://book/empty-target"
    );
    assert!(hit.text_locator.paragraph_index.is_none());
    assert!(hit.text_locator.sentence_index.is_none());
    assert!(hit.text_locator.normalized_range.is_none());
    assert!(hit.text_locator.segmentation_version.is_none());
}
