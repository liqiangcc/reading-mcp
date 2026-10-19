use std::sync::Arc;

use reading_mcp::application::open_document::{OpenDocumentCommand, OpenDocumentUseCase};
use reading_mcp::application::ports::{DocumentRepository, RetrievalOptions};
use reading_mcp::domain::{DocumentSource, NormalizedTextRange, Section, TextLocator};
use reading_mcp::infrastructure::{InMemoryDocumentRepository, NoopSearchIndex};
use reading_mcp::parsing::ParserRouter;
use reading_mcp::retrieval::{FileRetriever, LocalFileSourcePolicy};
use tempfile::tempdir;

fn collect_sections<'a>(section: &'a Section, output: &mut Vec<&'a Section>) {
    output.push(section);
    for child in &section.children {
        collect_sections(child, output);
    }
}

#[tokio::test]
async fn with_hash_constructors_match_the_existing_constructors() {
    let directory = tempdir().expect("temp directory should be created");
    let path = directory.path().join("book.md");
    tokio::fs::write(
        &path,
        "# Book\n\nIntro paragraph.\n\n## Chapter One\n\nFirst chapter paragraph.\n\n## Chapter Two\n\nSecond chapter paragraph.\n\n### Section Two A\n\nNested paragraph.\n",
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
    let document = repository
        .get(&opened.document_id)
        .await
        .expect("repository lookup should succeed")
        .expect("opened document should be stored");

    let normalized_document_hash = document.normalized_document_hash();

    let mut sections = Vec::new();
    for section in &document.root_sections {
        collect_sections(section, &mut sections);
    }
    assert!(
        sections.len() >= 3,
        "fixture should parse into several sections, got {}",
        sections.len()
    );

    for section in sections {
        assert_eq!(
            TextLocator::for_section_with_hash(&document, section, &normalized_document_hash),
            TextLocator::for_section(&document, section),
            "for_section_with_hash differs from for_section for {}",
            section.id.0
        );

        let range = NormalizedTextRange::new(0, section.normalized_text_len())
            .expect("zero-to-length range is valid");
        assert_eq!(
            TextLocator::for_character_range_with_hash(
                &document,
                section,
                range,
                &normalized_document_hash,
            ),
            TextLocator::for_character_range(&document, section, range),
            "for_character_range_with_hash differs from for_character_range for {}",
            section.id.0
        );
    }
}
