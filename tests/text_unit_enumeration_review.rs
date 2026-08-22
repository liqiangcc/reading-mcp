use std::collections::BTreeMap;
use std::sync::Arc;

use reading_mcp::application::get_text_units::{
    GetTextUnitsCommand, GetTextUnitsUseCase, RequestedTextUnitKind, TextUnitCoveragePolicy,
    TextUnitDirection,
};
use reading_mcp::application::ports::DocumentRepository;
use reading_mcp::domain::{
    ContentHash, Document, DocumentId, DocumentSource, Location, MediaType, Section, SectionId,
};
use reading_mcp::infrastructure::{InMemoryDocumentRepository, SqliteDocumentRepository};
use tempfile::tempdir;

#[tokio::test]
async fn eligible_only_never_claims_all_source_complete_even_for_all_prose() {
    let repository = Arc::new(InMemoryDocumentRepository::default());
    let document = document("One sentence. Two sentences.");
    repository
        .save(document.clone())
        .await
        .expect("document should save");
    let use_case = GetTextUnitsUseCase::new(repository);

    let mut command = sentence_command(&document, 10);
    command.coverage_policy = TextUnitCoveragePolicy::EligibleOnly;
    let result = use_case
        .execute(command)
        .await
        .expect("eligible-only prose should enumerate");

    assert!(result.complete);
    assert_eq!(result.coverage.intentionally_skipped, 0);
    assert!(!result.coverage.source_complete);
    assert!(!result.section_complete);
}

#[tokio::test]
async fn text_unit_cursor_survives_repository_restart_without_sentence_persistence() {
    let directory = tempdir().expect("temporary directory should be created");
    let database = directory.path().join("state.sqlite");
    let document = document("One. Two. Three.");

    let repository = Arc::new(
        SqliteDocumentRepository::open(&database).expect("repository should open before restart"),
    );
    repository
        .save(document.clone())
        .await
        .expect("document should persist");
    let first_use_case = GetTextUnitsUseCase::new(repository);
    let first = first_use_case
        .execute(sentence_command(&document, 1))
        .await
        .expect("first page should enumerate");
    assert_eq!(first.items[0].text, "One.");
    let cursor = first.next_cursor.expect("first page should be resumable");
    drop(first_use_case);

    let reopened = Arc::new(
        SqliteDocumentRepository::open(&database).expect("repository should reopen after restart"),
    );
    let second_use_case = GetTextUnitsUseCase::new(reopened);
    let mut continuation = sentence_command(&document, 2);
    continuation.cursor = Some(cursor);
    let second = second_use_case
        .execute(continuation)
        .await
        .expect("cursor should continue after repository restart");

    assert_eq!(
        second
            .items
            .iter()
            .map(|item| item.text.as_str())
            .collect::<Vec<_>>(),
        vec!["Two.", "Three."]
    );
    assert!(second.complete);
    assert!(second.section_complete);
    assert!(second.next_cursor.is_none());
}

fn sentence_command(document: &Document, max_items: usize) -> GetTextUnitsCommand {
    GetTextUnitsCommand {
        document_id: document.id.clone(),
        section_id: SectionId("section://root".into()),
        requested_kind: RequestedTextUnitKind::Sentence,
        direction: TextUnitDirection::Forward,
        coverage_policy: TextUnitCoveragePolicy::PreserveSource,
        max_items,
        max_chars: None,
        cursor: None,
    }
}

fn document(content: &str) -> Document {
    Document {
        id: DocumentId("doc:enumeration-review".into()),
        source: DocumentSource("memory:enumeration-review".into()),
        title: "Enumeration review".into(),
        media_type: MediaType("text/plain".into()),
        content_hash: ContentHash("sha256:raw".into()),
        metadata: BTreeMap::new(),
        root_sections: vec![Section {
            id: SectionId("section://root".into()),
            parent_id: None,
            title: "Root".into(),
            level: 1,
            content: content.into(),
            location: Location {
                section_path: vec!["Root".into()],
                ..Location::default()
            },
            children: vec![],
        }],
    }
}
