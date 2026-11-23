#[path = "support/keyed_index.rs"]
mod keyed_index;
use keyed_index::KeyedTextUnitIndex;

use std::sync::Arc;

use reading_mcp::application::get_text_units::RequestedTextUnitKind;
use reading_mcp::application::open_document::{OpenDocumentCommand, OpenDocumentUseCase};
use reading_mcp::application::ports::{ApplicationError, RetrievalOptions, SearchIndex};
use reading_mcp::application::ports::{DocumentRepository, TextUnitIndex};
use reading_mcp::application::read_reading::{ReadReadingCommand, ReadReadingUseCase};
use reading_mcp::application::search_document::{SearchDocumentCommand, SearchDocumentUseCase};
use reading_mcp::application::short_handle::{DocumentHandle, handle_checksum};
use reading_mcp::domain::DocumentSource;
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
        Arc::new(LocalFileSourcePolicy::allow_roots([directory.path()])),
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

    let searched = SearchDocumentUseCase::new(index, repository.clone())
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

    let document = repository
        .get(&opened.document_id)
        .await
        .expect("stored document")
        .expect("the opened document is stored");
    let text = read_document_text(&repository, &document).await;

    assert!(text.contains("Address spaces give each process"));
    assert!(text.contains("Page replacement algorithms"));
    assert!(text.contains("Page table entries map virtual pages"));
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
        Arc::new(LocalFileSourcePolicy::allow_roots([directory.path()])),
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

    let text = read_document_text(&repository, &{
        repository
            .get(&opened.document_id)
            .await
            .expect("stored document")
            .expect("the opened document is stored")
    })
    .await;

    assert!(text.contains("Virtual memory intro."));
    assert!(text.contains("Page table details."));
    assert!(text.contains("Process details."));
    assert_eq!(text.matches("Page table details.").count(), 1);
}

// The whole book as one text, through the v2 `read` use case: the section items in body order.
async fn read_document_text(
    repository: &Arc<InMemoryDocumentRepository>,
    document: &std::sync::Arc<reading_mcp::domain::Document>,
) -> String {
    let keyed = Arc::new(KeyedTextUnitIndex::default());
    keyed.store(document.as_ref()).await;
    let units: Arc<dyn TextUnitIndex> = keyed;
    let handle = DocumentHandle {
        doc_key: units
            .document_key(&document.id)
            .await
            .expect("document key")
            .expect("stored document"),
        checksum: handle_checksum(
            &document.id,
            &document.normalized_document_hash(),
            &document.media_type,
        ),
    };
    let mut text = String::new();
    let mut titles = Vec::new();
    let mut from = handle.to_string();
    loop {
        let result = ReadReadingUseCase::new(repository.clone(), units.clone())
            .execute(ReadReadingCommand {
                from,
                unit: RequestedTextUnitKind::Sentence,
                max_chars: Some(16_000),
                direction: None,
                back: 0,
            })
            .await
            .expect("read");
        for item in &result.items {
            if !text.is_empty() {
                text.push('\n');
            }
            text.push_str(&item.text);
        }
        titles.extend(result.sections.iter().map(|section| section.title.clone()));
        match result.next {
            Some(next) => from = next,
            None => break,
        }
    }
    for title in ["Virtual Memory", "Page Tables", "Processes"] {
        assert!(titles.contains(&title.to_string()), "titles: {titles:?}");
    }
    text
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
