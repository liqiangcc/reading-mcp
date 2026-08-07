use std::sync::Arc;

use reading_mcp::application::get_context::{GetContextCommand, GetContextUseCase};
use reading_mcp::application::open_document::{OpenDocumentCommand, OpenDocumentUseCase};
use reading_mcp::application::ports::{ApplicationError, RetrievalOptions, SearchIndex};
use reading_mcp::application::read_document::{ReadDocumentUseCase, ReadSectionCommand};
use reading_mcp::application::search_document::{SearchDocumentCommand, SearchDocumentUseCase};
use reading_mcp::domain::{DocumentSource, SectionId};
use reading_mcp::infrastructure::{InMemoryDocumentRepository, InMemorySearchIndex};
use reading_mcp::parsing::ParserRouter;
use reading_mcp::retrieval::{FileRetriever, LocalFileSourcePolicy};
use tempfile::tempdir;

#[tokio::test]
async fn search_units_are_smaller_than_logical_read_sections() {
    let directory = tempdir().expect("temp directory should be created");
    let path = directory.path().join("operating-systems.md");
    tokio::fs::write(
        &path,
        "# Operating Systems\n\nCore concepts.\n\n## Virtual Memory\n\nAddress spaces give each process an isolated view of memory.\n\nPage replacement algorithms decide which resident page should be evicted.\n\n### Page Tables\n\nPage table entries map virtual pages to physical frames.\n\n## Processes\n\nProcesses own execution state and resources.\n",
    )
    .await
    .expect("fixture should be written");

    let repository = Arc::new(InMemoryDocumentRepository::default());
    let index = Arc::new(InMemorySearchIndex::default());
    let open = OpenDocumentUseCase::new(
        Arc::new(LocalFileSourcePolicy),
        Arc::new(FileRetriever),
        Arc::new(ParserRouter::phase1()),
        repository.clone(),
        index.clone(),
    );

    let opened = open
        .execute(OpenDocumentCommand {
            source: DocumentSource(path.to_string_lossy().into_owned()),
            options: RetrievalOptions::default(),
        })
        .await
        .expect("markdown should open and index");

    let searched = SearchDocumentUseCase::new(index)
        .execute(SearchDocumentCommand {
            document_id: opened.document_id.clone(),
            query: "replacement algorithms".into(),
            limit: 10,
        })
        .await
        .expect("search should succeed");

    assert!(!searched.hits.is_empty());
    let hit = &searched.hits[0];
    assert_eq!(
        hit.section_id.0,
        "section://operating-systems/virtual-memory"
    );
    assert!(hit.snippet.contains("Page replacement algorithms"));
    assert!(!hit.snippet.contains("Address spaces give each process"));
    assert!(
        hit.location
            .native_location
            .as_deref()
            .is_some_and(|location| location.contains("search-unit"))
    );

    let read = ReadDocumentUseCase::new(repository)
        .execute(ReadSectionCommand {
            document_id: opened.document_id,
            section_id: hit.section_id.clone(),
            max_chars: None,
        })
        .await
        .expect("owning section should be readable");

    assert!(read.content.contains("Address spaces give each process"));
    assert!(read.content.contains("Page replacement algorithms"));
    assert!(read.content.contains("### Page Tables"));
    assert!(
        read.content
            .contains("Page table entries map virtual pages")
    );
}

#[tokio::test]
async fn context_expansion_reads_canonical_document_not_search_snippets() {
    let directory = tempdir().expect("temp directory should be created");
    let path = directory.path().join("book.md");
    tokio::fs::write(
        &path,
        "# Operating Systems\n\nOverview.\n\n## Virtual Memory\n\nVirtual memory intro.\n\n### Page Tables\n\nPage table details.\n\n## Processes\n\nProcess details.\n",
    )
    .await
    .expect("fixture should be written");

    let repository = Arc::new(InMemoryDocumentRepository::default());
    let index = Arc::new(InMemorySearchIndex::default());
    let opened = OpenDocumentUseCase::new(
        Arc::new(LocalFileSourcePolicy),
        Arc::new(FileRetriever),
        Arc::new(ParserRouter::phase1()),
        repository.clone(),
        index,
    )
    .execute(OpenDocumentCommand {
        source: DocumentSource(path.to_string_lossy().into_owned()),
        options: RetrievalOptions::default(),
    })
    .await
    .expect("markdown should open");

    let context = GetContextUseCase::new(repository)
        .execute(GetContextCommand {
            document_id: opened.document_id,
            section_id: SectionId("section://operating-systems/virtual-memory/page-tables".into()),
            before: 1,
            after: 1,
            max_chars: None,
        })
        .await
        .expect("context should expand around owner section");

    assert_eq!(
        context.owner_section_id.0,
        "section://operating-systems/virtual-memory/page-tables"
    );
    assert!(context.content.contains("## Virtual Memory"));
    assert!(context.content.contains("Virtual memory intro."));
    assert!(context.content.contains("### Page Tables"));
    assert!(context.content.contains("Page table details."));
    assert!(context.content.contains("## Processes"));
    assert!(context.content.contains("Process details."));
    assert_eq!(context.content.matches("Page table details.").count(), 1);
    assert!(!context.truncated);
}

#[tokio::test]
async fn search_index_is_rebuildable_derived_state() {
    let fresh_index = InMemorySearchIndex::default();
    let error = fresh_index
        .search(
            &reading_mcp::domain::DocumentId("doc:not-indexed".into()),
            "memory",
            10,
        )
        .await
        .expect_err("an empty derived index should not invent documents");

    assert_eq!(error, ApplicationError::DocumentNotFound);
}
