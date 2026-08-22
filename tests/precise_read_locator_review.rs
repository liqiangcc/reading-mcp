use std::collections::BTreeMap;
use std::sync::Arc;

use reading_mcp::application::ports::{ApplicationError, DocumentRepository};
use reading_mcp::application::read_document::{
    ContinueExactReadCommand, ContinueReadCommand, ReadDocumentUseCase, ReadExactTargetCommand,
    ReadSectionCommand,
};
use reading_mcp::domain::{
    ContentHash, Document, DocumentId, DocumentSource, Location, MediaType, Section, SectionId,
    TextLocator,
};
use reading_mcp::infrastructure::InMemoryDocumentRepository;

#[tokio::test]
async fn legacy_and_exact_read_cursors_cannot_cross_read_modes() {
    let document = document();
    let repository = Arc::new(InMemoryDocumentRepository::default());
    repository.save(document.clone()).await.expect("save");
    let use_case = ReadDocumentUseCase::new(repository);
    let section_id = SectionId("section://root".into());
    let section = document.find_section(&section_id).expect("root");
    let paragraph = document
        .paragraph_text_units()
        .units
        .into_iter()
        .next()
        .expect("paragraph");
    let locator = TextLocator::for_paragraph(&document, section, &paragraph);

    let legacy = use_case
        .execute(ReadSectionCommand {
            document_id: document.id.clone(),
            section_id: section_id.clone(),
            max_chars: Some(5),
        })
        .await
        .expect("legacy read");
    let legacy_cursor = legacy.next_cursor.expect("legacy cursor");
    let exact_error = use_case
        .continue_exact(ContinueExactReadCommand {
            document_id: document.id.clone(),
            target_locator: locator.clone(),
            cursor: legacy_cursor,
            max_chars: Some(5),
        })
        .await
        .expect_err("legacy cursor cannot resume exact mode");
    assert!(matches!(exact_error, ApplicationError::StaleCursor(_)));

    let exact = use_case
        .read_exact(ReadExactTargetCommand {
            document_id: document.id.clone(),
            target_locator: locator,
            max_chars: Some(5),
        })
        .await
        .expect("exact read");
    let exact_cursor = exact.next_cursor.expect("exact cursor");
    let legacy_error = use_case
        .continue_read(ContinueReadCommand {
            document_id: document.id,
            section_id,
            cursor: exact_cursor,
            max_chars: Some(5),
        })
        .await
        .expect_err("exact cursor cannot resume legacy mode");
    assert!(matches!(legacy_error, ApplicationError::StaleCursor(_)));
}

#[tokio::test]
async fn exact_initial_zero_budget_preserves_progress_compatibility() {
    let document = document();
    let repository = Arc::new(InMemoryDocumentRepository::default());
    repository.save(document.clone()).await.expect("save");
    let use_case = ReadDocumentUseCase::new(repository);
    let section = document
        .find_section(&SectionId("section://root".into()))
        .expect("root");
    let paragraph = document
        .paragraph_text_units()
        .units
        .into_iter()
        .next()
        .expect("paragraph");
    let locator = TextLocator::for_paragraph(&document, section, &paragraph);

    let first = use_case
        .read_exact(ReadExactTargetCommand {
            document_id: document.id.clone(),
            target_locator: locator.clone(),
            max_chars: Some(0),
        })
        .await
        .expect("initial zero budget remains a valid bounded read");
    assert!(first.content.is_empty());
    assert!(!first.complete);
    assert_eq!(first.stream.start_char, 0);
    assert_eq!(first.stream.end_char, 0);
    assert_eq!(
        first
            .returned_locator
            .as_ref()
            .and_then(|locator| locator.normalized_range)
            .expect("empty exact segment still has a source range")
            .len(),
        0
    );

    let second = use_case
        .continue_exact(ContinueExactReadCommand {
            document_id: document.id,
            target_locator: locator,
            cursor: first.next_cursor.expect("zero position cursor"),
            max_chars: Some(7),
        })
        .await
        .expect("positive continuation must advance from zero");
    assert_eq!(second.stream.start_char, 0);
    assert_eq!(second.stream.end_char, 7);
    assert_eq!(second.content.chars().count(), 7);
}

fn document() -> Document {
    Document {
        id: DocumentId("doc:precise-review".into()),
        source: DocumentSource("memory:precise-review".into()),
        title: "Precise review".into(),
        media_type: MediaType("text/plain".into()),
        content_hash: ContentHash("sha256:raw".into()),
        metadata: BTreeMap::new(),
        root_sections: vec![Section {
            id: SectionId("section://root".into()),
            parent_id: None,
            title: "Root".into(),
            level: 1,
            content:
                "A deliberately long exact target verifies that cursor modes cannot be confused."
                    .into(),
            location: Location {
                section_path: vec!["Root".into()],
                ..Location::default()
            },
            children: vec![],
        }],
    }
}
