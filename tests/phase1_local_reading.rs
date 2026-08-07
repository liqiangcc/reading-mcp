use std::sync::Arc;

use reading_mcp::application::get_document_structure::GetDocumentStructureUseCase;
use reading_mcp::application::open_document::{OpenDocumentCommand, OpenDocumentUseCase};
use reading_mcp::application::ports::{ApplicationError, RetrievalOptions, SourcePolicy};
use reading_mcp::application::read_document::{ReadDocumentUseCase, ReadSectionCommand};
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
        Arc::new(LocalFileSourcePolicy),
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

    let structure = GetDocumentStructureUseCase::new(repository.clone())
        .execute(opened.document_id.clone(), None)
        .await
        .expect("structure should be available");

    assert_eq!(structure.sections.len(), 1);
    let root = &structure.sections[0];
    assert_eq!(root.section_id.0, "section://operating-systems");
    assert_eq!(root.children.len(), 2);
    assert_eq!(
        root.children[0].section_id.0,
        "section://operating-systems/virtual-memory"
    );
    assert_eq!(
        root.children[0].children[0].section_id.0,
        "section://operating-systems/virtual-memory/page-tables"
    );

    let read = ReadDocumentUseCase::new(repository)
        .execute(ReadSectionCommand {
            document_id: opened.document_id,
            section_id: SectionId("section://operating-systems/virtual-memory".into()),
            max_chars: None,
        })
        .await
        .expect("section should be readable");

    assert!(read.content.contains("Virtual memory intro."));
    assert!(read.content.contains("### Page Tables"));
    assert!(read.content.contains("Page table details."));
    assert_eq!(
        read.location.section_path,
        vec!["Operating Systems", "Virtual Memory"]
    );
    assert!(!read.truncated);
}

#[tokio::test]
async fn plain_text_is_exposed_as_one_logical_section() {
    let directory = tempdir().expect("temp directory should be created");
    let path = directory.path().join("notes.txt");
    tokio::fs::write(&path, "line one\nline two\n")
        .await
        .expect("fixture should be written");

    let repository = Arc::new(InMemoryDocumentRepository::default());
    let open = OpenDocumentUseCase::new(
        Arc::new(LocalFileSourcePolicy),
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

    let read = ReadDocumentUseCase::new(repository)
        .execute(ReadSectionCommand {
            document_id: opened.document_id,
            section_id: SectionId("section://document".into()),
            max_chars: Some(12),
        })
        .await
        .expect("text section should be readable");

    assert!(read.truncated);
    assert_eq!(read.content.chars().count(), 12);
}

#[tokio::test]
async fn local_file_policy_rejects_network_sources() {
    let error = LocalFileSourcePolicy
        .validate(&DocumentSource("https://example.com/book.md".into()))
        .await
        .expect_err("network source must be blocked in local file mode");

    assert!(matches!(error, ApplicationError::BlockedSource(_)));
}
