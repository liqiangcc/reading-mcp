use std::sync::Arc;

#[path = "support/keyed_index.rs"]
mod keyed_index;
use keyed_index::KeyedTextUnitIndex;

use reading_mcp::application::get_text_units::RequestedTextUnitKind;
use reading_mcp::application::open_document::{OpenDocumentCommand, OpenDocumentUseCase};
use reading_mcp::application::outline_reading::{OutlineReadingCommand, OutlineReadingUseCase};
use reading_mcp::application::ports::{
    ApplicationError, DocumentRepository, RetrievalOptions, SourcePolicy, TextUnitIndex,
};
use reading_mcp::application::read_reading::{
    ReadReadingCommand, ReadReadingResult, ReadReadingUseCase,
};
use reading_mcp::application::short_handle::{DocumentHandle, SectionHandle, handle_checksum};
use reading_mcp::domain::{DocumentSource, SectionId};
use reading_mcp::infrastructure::{InMemoryDocumentRepository, NoopSearchIndex};
use reading_mcp::parsing::ParserRouter;
use reading_mcp::retrieval::{FileRetriever, LocalFileSourcePolicy};
use tempfile::tempdir;

#[tokio::test]
async fn markdown_open_structure_and_read_form_a_complete_loop() {
    let directory = tempdir().expect("temp directory should be created");
    let path = directory.path().join("operating-systems.md");
    tokio::fs::write(
        &path,
        "# Operating Systems\n\nCore concepts.\n\n## Virtual Memory\n\nVirtual memory intro.\n\n### Page Tables\n\nPage table details.\n\n## Processes\n\nProcess details.\n",
    )
    .await
    .expect("fixture should be written");

    let repository = Arc::new(InMemoryDocumentRepository::default());
    let open = OpenDocumentUseCase::new(
        Arc::new(LocalFileSourcePolicy::allow_roots([directory.path()])),
        Arc::new(FileRetriever),
        Arc::new(ParserRouter::phase1()),
        repository.clone(),
        Arc::new(NoopSearchIndex),
    );

    let opened = open
        .execute(OpenDocumentCommand {
            source: DocumentSource(path.to_string_lossy().into_owned()),
            options: RetrievalOptions::default(),
        })
        .await
        .expect("markdown should open");

    assert_eq!(opened.title, "Operating Systems");
    assert_eq!(opened.media_type.0, "text/markdown");
    assert_eq!(opened.section_count, 4);

    let keyed = Arc::new(KeyedTextUnitIndex::default());
    let document = repository
        .get(&opened.document_id)
        .await
        .expect("stored document")
        .expect("the opened document is stored");
    keyed.store(&document).await;
    let units: Arc<dyn TextUnitIndex> = keyed;

    let handle = document_handle(&units, &document).await;
    let outline = OutlineReadingUseCase::new(repository.clone(), units.clone())
        .execute(OutlineReadingCommand {
            at: handle.to_string(),
            depth: 4,
            limit: 100,
            cursor: None,
        })
        .await
        .expect("structure should be available");
    let shape: Vec<(&str, u8)> = outline
        .items
        .iter()
        .map(|item| (item.title.as_str(), item.level))
        .collect();
    assert_eq!(
        shape,
        vec![
            ("Operating Systems", 1),
            ("Virtual Memory", 2),
            ("Page Tables", 3),
            ("Processes", 2),
        ]
    );

    let section = section_handle(
        &units,
        &document,
        "section://operating-systems/virtual-memory",
    )
    .await;
    // A v2 `read` streams forward from the anchor to the end of the document, so the batch
    // below covers the `Virtual Memory` subtree and the sibling `Processes` section too.
    let result = read_items(repository, units, &section.to_string(), Some(1_000)).await;
    let text = items_text(&result);
    assert!(text.contains("Virtual memory intro."));
    assert!(text.contains("Page table details."));
    assert_eq!(result.sections[0].title, "Virtual Memory");
    assert!(
        result
            .sections
            .iter()
            .any(|section| section.title == "Page Tables")
    );
    assert!(result.next.is_none());
}

#[tokio::test]
async fn plain_text_is_exposed_as_one_logical_section() {
    let directory = tempdir().expect("temp directory should be created");
    let path = directory.path().join("notes.txt");
    tokio::fs::write(&path, "line one.\nline two.\n")
        .await
        .expect("fixture should be written");

    let repository = Arc::new(InMemoryDocumentRepository::default());
    let open = OpenDocumentUseCase::new(
        Arc::new(LocalFileSourcePolicy::allow_roots([directory.path()])),
        Arc::new(FileRetriever),
        Arc::new(ParserRouter::phase1()),
        repository.clone(),
        Arc::new(NoopSearchIndex),
    );

    let opened = open
        .execute(OpenDocumentCommand {
            source: DocumentSource(path.to_string_lossy().into_owned()),
            options: RetrievalOptions::default(),
        })
        .await
        .expect("text should open");

    assert_eq!(opened.title, "notes");
    assert_eq!(opened.media_type.0, "text/plain");
    assert_eq!(opened.section_count, 1);

    let keyed = Arc::new(KeyedTextUnitIndex::default());
    let document = repository
        .get(&opened.document_id)
        .await
        .expect("stored document")
        .expect("the opened document is stored");
    keyed.store(&document).await;
    let units: Arc<dyn TextUnitIndex> = keyed;
    let handle = document_handle(&units, &document).await;
    let result = read_items(repository, units, &handle.to_string(), Some(12)).await;
    assert!(result.next.is_some(), "a 12-char budget must leave a next");
    assert_eq!(result.items[0].text, "line one.");
}

#[tokio::test]
async fn local_file_policy_rejects_network_sources() {
    let directory = tempdir().expect("temp directory should be created");
    let error = LocalFileSourcePolicy::allow_roots([directory.path()])
        .validate(&DocumentSource("https://example.com/book.md".into()))
        .await
        .expect_err("network source must be blocked in local file mode");

    assert!(matches!(error, ApplicationError::BlockedSource(_)));
}

#[tokio::test]
async fn local_file_policy_is_disabled_by_default_and_blocks_outside_roots() {
    let allowed = tempdir().expect("allowed temp directory should be created");
    let outside = tempdir().expect("outside temp directory should be created");
    let outside_path = outside.path().join("secret.md");
    tokio::fs::write(&outside_path, "# secret\n")
        .await
        .expect("outside fixture should be written");

    let disabled = LocalFileSourcePolicy::disabled()
        .validate(&DocumentSource(outside_path.to_string_lossy().into_owned()))
        .await
        .expect_err("default policy must block local file access");
    assert!(matches!(disabled, ApplicationError::BlockedSource(_)));

    let outside_error = LocalFileSourcePolicy::allow_roots([allowed.path()])
        .validate(&DocumentSource(outside_path.to_string_lossy().into_owned()))
        .await
        .expect_err("paths outside configured roots must be blocked");
    assert!(matches!(outside_error, ApplicationError::BlockedSource(_)));
}

// The v2 reading helpers: every reader below runs through the public v2 use cases.
async fn document_handle(
    units: &Arc<dyn TextUnitIndex>,
    document: &reading_mcp::domain::Document,
) -> DocumentHandle {
    DocumentHandle {
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
    }
}

async fn section_handle(
    units: &Arc<dyn TextUnitIndex>,
    document: &reading_mcp::domain::Document,
    section_id: &str,
) -> SectionHandle {
    SectionHandle {
        document: document_handle(units, document).await,
        section_key: units
            .section_key(&document.id, &SectionId(section_id.into()))
            .await
            .expect("section key")
            .expect("stored section"),
    }
}

async fn read_items(
    repository: Arc<dyn DocumentRepository>,
    units: Arc<dyn TextUnitIndex>,
    from: &str,
    max_chars: Option<usize>,
) -> ReadReadingResult {
    ReadReadingUseCase::new(repository, units)
        .execute(ReadReadingCommand {
            from: from.to_string(),
            unit: RequestedTextUnitKind::Sentence,
            max_chars,
            direction: None,
            back: 0,
        })
        .await
        .expect("read")
}

fn items_text(result: &ReadReadingResult) -> String {
    result
        .items
        .iter()
        .map(|item| item.text.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}
