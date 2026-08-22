use std::collections::BTreeMap;
use std::sync::Arc;

use reading_mcp::application::get_text_units::{
    GetTextUnitsCommand, GetTextUnitsUseCase, RequestedTextUnitKind, TextUnitCoveragePolicy,
    TextUnitDirection,
};
use reading_mcp::application::ports::{ApplicationError, DocumentRepository};
use reading_mcp::domain::{
    ContentHash, Document, DocumentId, DocumentSource, Location, MediaType, Section, SectionId,
};
use reading_mcp::infrastructure::InMemoryDocumentRepository;

#[tokio::test]
async fn forward_sentence_pages_are_gap_free_overlap_free_and_complete() {
    let repository = Arc::new(InMemoryDocumentRepository::default());
    let document = document_with_content("One. Two. Three. Four.");
    repository
        .save(document.clone())
        .await
        .expect("document should save");
    let use_case = GetTextUnitsUseCase::new(repository);

    let first = use_case
        .execute(command(&document, RequestedTextUnitKind::Sentence, 2, None))
        .await
        .expect("first page should enumerate");
    assert_eq!(texts(&first), vec!["One.", "Two."]);
    assert!(!first.complete);
    assert!(!first.section_complete);
    assert_eq!(first.stream.start_index, 0);
    assert_eq!(first.stream.end_index, 2);
    assert_eq!(first.stream.total_items, 4);

    let mut second_command = command(&document, RequestedTextUnitKind::Sentence, 2, None);
    second_command.cursor = first.next_cursor.clone();
    let second = use_case
        .execute(second_command)
        .await
        .expect("second page should enumerate");
    assert_eq!(texts(&second), vec!["Three.", "Four."]);
    assert!(second.complete);
    assert!(second.section_complete);
    assert!(second.next_cursor.is_none());
    assert_eq!(first.stream.end_index, second.stream.start_index);
    assert_eq!(second.stream.end_index, second.stream.total_items);
}

#[tokio::test]
async fn backward_pages_preserve_source_order_inside_each_page_without_overlap() {
    let repository = Arc::new(InMemoryDocumentRepository::default());
    let document = document_with_content("One. Two. Three. Four.");
    repository
        .save(document.clone())
        .await
        .expect("document should save");
    let use_case = GetTextUnitsUseCase::new(repository);

    let mut first_command = command(&document, RequestedTextUnitKind::Sentence, 2, None);
    first_command.direction = TextUnitDirection::Backward;
    let first = use_case
        .execute(first_command)
        .await
        .expect("backward page should enumerate");
    assert_eq!(texts(&first), vec!["Three.", "Four."]);
    assert_eq!((first.stream.start_index, first.stream.end_index), (2, 4));
    assert!(!first.complete);

    let mut second_command = command(&document, RequestedTextUnitKind::Sentence, 2, None);
    second_command.direction = TextUnitDirection::Backward;
    second_command.cursor = first.next_cursor.clone();
    let second = use_case
        .execute(second_command)
        .await
        .expect("backward continuation should enumerate");
    assert_eq!(texts(&second), vec!["One.", "Two."]);
    assert_eq!((second.stream.start_index, second.stream.end_index), (0, 2));
    assert_eq!(second.stream.end_index, first.stream.start_index);
    assert!(second.complete);
    assert!(second.section_complete);
}

#[tokio::test]
async fn sentence_preserve_source_emits_coarse_non_prose_while_eligible_only_accounts_for_skip() {
    let repository = Arc::new(InMemoryDocumentRepository::default());
    let document = document_with_content(
        "```rust\nfn main() { println!(\"Hi.\"); }\n```\n\nReal prose. Next sentence.",
    );
    repository
        .save(document.clone())
        .await
        .expect("document should save");
    let use_case = GetTextUnitsUseCase::new(repository);

    let preserve = use_case
        .execute(command(
            &document,
            RequestedTextUnitKind::Sentence,
            10,
            None,
        ))
        .await
        .expect("source-preserving enumeration should succeed");
    assert_eq!(preserve.items.len(), 3);
    assert_eq!(preserve.items[0].effective_kind.as_str(), "paragraph");
    assert!(preserve.items[0].degradation.is_some());
    assert_eq!(preserve.items[1].text, "Real prose.");
    assert_eq!(preserve.items[2].text, "Next sentence.");
    assert_eq!(preserve.coverage.non_prose_paragraphs, 1);
    assert_eq!(preserve.coverage.coarse_non_prose_items, 1);
    assert_eq!(preserve.coverage.intentionally_skipped, 0);
    assert!(preserve.coverage.source_complete);
    assert!(preserve.section_complete);

    let mut eligible_command = command(&document, RequestedTextUnitKind::Sentence, 10, None);
    eligible_command.coverage_policy = TextUnitCoveragePolicy::EligibleOnly;
    let eligible = use_case
        .execute(eligible_command)
        .await
        .expect("eligible-only enumeration should succeed");
    assert_eq!(texts(&eligible), vec!["Real prose.", "Next sentence."]);
    assert_eq!(eligible.coverage.intentionally_skipped, 1);
    assert!(!eligible.coverage.source_complete);
    assert!(eligible.complete);
    assert!(!eligible.section_complete);
}

#[tokio::test]
async fn paragraph_enumeration_returns_exact_locators_and_does_not_cross_into_child_sections() {
    let repository = Arc::new(InMemoryDocumentRepository::default());
    let mut document = document_with_content("Root paragraph one.\n\nRoot paragraph two.");
    document.root_sections[0].children.push(Section {
        id: SectionId("section://root/child".into()),
        parent_id: Some(SectionId("section://root".into())),
        title: "Child".into(),
        level: 2,
        content: "Child paragraph.".into(),
        location: Location {
            section_path: vec!["Root".into(), "Child".into()],
            ..Location::default()
        },
        children: vec![],
    });
    repository
        .save(document.clone())
        .await
        .expect("document should save");
    let use_case = GetTextUnitsUseCase::new(repository);

    let result = use_case
        .execute(command(
            &document,
            RequestedTextUnitKind::Paragraph,
            10,
            None,
        ))
        .await
        .expect("paragraph enumeration should succeed");
    assert_eq!(texts(&result), vec!["Root paragraph one.", "Root paragraph two."]);
    assert!(result.items.iter().all(|item| {
        item.locator.owner_section_id.0 == "section://root"
            && item.locator.sentence_index.is_none()
            && item.locator.paragraph_index.is_some()
            && item.locator.normalized_range.is_some()
    }));
    assert_eq!(result.target_section_locator.owner_section_id.0, "section://root");
}

#[tokio::test]
async fn continuation_cursor_fails_closed_after_normalized_document_changes() {
    let repository = Arc::new(InMemoryDocumentRepository::default());
    let document = document_with_content("One. Two.");
    repository
        .save(document.clone())
        .await
        .expect("document should save");
    let use_case = GetTextUnitsUseCase::new(repository.clone());

    let first = use_case
        .execute(command(&document, RequestedTextUnitKind::Sentence, 1, None))
        .await
        .expect("first page should enumerate");
    let cursor = first.next_cursor.expect("first page should have cursor");

    let mut changed = document.clone();
    changed.root_sections[0].content = "Changed. Two.".into();
    repository
        .save(changed)
        .await
        .expect("changed document should replace repository value");

    let mut continuation = command(&document, RequestedTextUnitKind::Sentence, 1, None);
    continuation.cursor = Some(cursor);
    let error = use_case
        .execute(continuation)
        .await
        .expect_err("changed normalized identity must stale the cursor");
    assert!(matches!(error, ApplicationError::StaleCursor(_)));
}

#[tokio::test]
async fn cursor_cannot_redefine_requested_kind_or_stream_policy() {
    let repository = Arc::new(InMemoryDocumentRepository::default());
    let document = document_with_content("One. Two.");
    repository
        .save(document.clone())
        .await
        .expect("document should save");
    let use_case = GetTextUnitsUseCase::new(repository);

    let first = use_case
        .execute(command(&document, RequestedTextUnitKind::Sentence, 1, None))
        .await
        .expect("first page should enumerate");
    let mut changed_kind = command(&document, RequestedTextUnitKind::Paragraph, 1, None);
    changed_kind.cursor = first.next_cursor;
    let error = use_case
        .execute(changed_kind)
        .await
        .expect_err("cursor must bind requested kind");
    assert!(matches!(error, ApplicationError::CursorTargetMismatch(_)));
}

#[tokio::test]
async fn max_chars_never_splits_a_text_unit_and_reports_actionable_resource_limit() {
    let repository = Arc::new(InMemoryDocumentRepository::default());
    let document = document_with_content("A sentence that is longer than five characters.");
    repository
        .save(document.clone())
        .await
        .expect("document should save");
    let use_case = GetTextUnitsUseCase::new(repository);

    let error = use_case
        .execute(command(
            &document,
            RequestedTextUnitKind::Sentence,
            10,
            Some(5),
        ))
        .await
        .expect_err("a single unit must never be split to satisfy max_chars");
    assert!(matches!(error, ApplicationError::ResourceLimitExceeded(_)));
}

fn texts(result: &reading_mcp::application::get_text_units::GetTextUnitsResult) -> Vec<&str> {
    result.items.iter().map(|item| item.text.as_str()).collect()
}

fn command(
    document: &Document,
    requested_kind: RequestedTextUnitKind,
    max_items: usize,
    max_chars: Option<usize>,
) -> GetTextUnitsCommand {
    GetTextUnitsCommand {
        document_id: document.id.clone(),
        section_id: SectionId("section://root".into()),
        requested_kind,
        direction: TextUnitDirection::Forward,
        coverage_policy: TextUnitCoveragePolicy::PreserveSource,
        max_items,
        max_chars,
        cursor: None,
    }
}

fn document_with_content(content: &str) -> Document {
    Document {
        id: DocumentId("doc:enumeration".into()),
        source: DocumentSource("memory:enumeration".into()),
        title: "Enumeration".into(),
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
