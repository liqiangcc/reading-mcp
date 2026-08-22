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
async fn forward_anchor_is_exclusive_and_cursor_preserves_anchored_origin() {
    let repository = Arc::new(InMemoryDocumentRepository::default());
    let document = fixture("One. Two. Three. Four.");
    repository.save(document.clone()).await.expect("save");
    let use_case = GetTextUnitsUseCase::new(repository);

    let full = use_case
        .execute(command(&document, 10))
        .await
        .expect("full enumeration");
    let anchor = full.items[1].locator.clone();

    let anchored = use_case
        .execute_from_anchor(command(&document, 1), anchor.clone())
        .await
        .expect("anchored enumeration");
    assert_eq!(texts(&anchored), vec!["Three."]);
    assert_eq!(anchored.start_anchor_locator.as_ref(), Some(&anchor));
    assert!(!anchored.complete);
    assert!(!anchored.section_complete);

    let mut continuation = command(&document, 10);
    continuation.cursor = anchored.next_cursor;
    let final_page = use_case
        .execute(continuation)
        .await
        .expect("anchored continuation");
    assert_eq!(texts(&final_page), vec!["Four."]);
    assert_eq!(final_page.start_anchor_locator.as_ref(), Some(&anchor));
    assert!(final_page.complete);
    assert!(!final_page.section_complete);
}

#[tokio::test]
async fn backward_anchor_is_exclusive_and_returns_source_order() {
    let repository = Arc::new(InMemoryDocumentRepository::default());
    let document = fixture("One. Two. Three. Four.");
    repository.save(document.clone()).await.expect("save");
    let use_case = GetTextUnitsUseCase::new(repository);

    let full = use_case
        .execute(command(&document, 10))
        .await
        .expect("full enumeration");
    let anchor = full.items[2].locator.clone();
    let mut anchored_command = command(&document, 10);
    anchored_command.direction = TextUnitDirection::Backward;

    let anchored = use_case
        .execute_from_anchor(anchored_command, anchor.clone())
        .await
        .expect("backward anchored enumeration");
    assert_eq!(texts(&anchored), vec!["One.", "Two."]);
    assert_eq!(anchored.start_anchor_locator.as_ref(), Some(&anchor));
    assert!(anchored.complete);
    assert!(!anchored.section_complete);
    assert_eq!(
        (anchored.stream.start_index, anchored.stream.end_index),
        (0, 2)
    );
}

#[tokio::test]
async fn anchor_must_be_a_member_of_the_declared_stream() {
    let repository = Arc::new(InMemoryDocumentRepository::default());
    let document = fixture("One. Two.");
    repository.save(document.clone()).await.expect("save");
    let use_case = GetTextUnitsUseCase::new(repository);

    let mut paragraph_command = command(&document, 10);
    paragraph_command.requested_kind = RequestedTextUnitKind::Paragraph;
    let paragraph = use_case
        .execute(paragraph_command)
        .await
        .expect("paragraph enumeration")
        .items[0]
        .locator
        .clone();

    let error = use_case
        .execute_from_anchor(command(&document, 10), paragraph)
        .await
        .expect_err("paragraph is not a sentence-stream item");
    assert!(matches!(error, ApplicationError::InvalidRequest(_)));
}

#[tokio::test]
async fn stale_anchor_fails_through_shared_locator_validation() {
    let repository = Arc::new(InMemoryDocumentRepository::default());
    let document = fixture("One. Two.");
    repository.save(document.clone()).await.expect("save");
    let use_case = GetTextUnitsUseCase::new(repository);

    let mut anchor = use_case
        .execute(command(&document, 10))
        .await
        .expect("enumeration")
        .items[0]
        .locator
        .clone();
    anchor.segmentation_version = Some("text-segmentation/v1".into());

    let error = use_case
        .execute_from_anchor(command(&document, 10), anchor)
        .await
        .expect_err("stale anchor must fail closed");
    assert!(matches!(error, ApplicationError::StaleLocator(_)));
}

#[tokio::test]
async fn initial_anchor_and_cursor_are_mutually_exclusive() {
    let repository = Arc::new(InMemoryDocumentRepository::default());
    let document = fixture("One. Two. Three.");
    repository.save(document.clone()).await.expect("save");
    let use_case = GetTextUnitsUseCase::new(repository);

    let first = use_case
        .execute(command(&document, 1))
        .await
        .expect("first page");
    let anchor = first.items[0].locator.clone();
    let mut invalid = command(&document, 1);
    invalid.cursor = first.next_cursor;

    let error = use_case
        .execute_from_anchor(invalid, anchor)
        .await
        .expect_err("anchor plus cursor must be rejected");
    assert!(matches!(error, ApplicationError::InvalidRequest(_)));
}

fn command(document: &Document, max_items: usize) -> GetTextUnitsCommand {
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

fn texts(result: &reading_mcp::application::get_text_units::GetTextUnitsResult) -> Vec<&str> {
    result.items.iter().map(|item| item.text.as_str()).collect()
}

fn fixture(content: &str) -> Document {
    Document {
        id: DocumentId("doc:anchor-enumeration".into()),
        source: DocumentSource("memory:anchor-enumeration".into()),
        title: "Anchor enumeration".into(),
        media_type: MediaType("text/plain".into()),
        content_hash: ContentHash("sha256:anchor-enumeration".into()),
        metadata: BTreeMap::new(),
        root_sections: vec![Section {
            id: SectionId("section://root".into()),
            parent_id: None,
            title: "Root".into(),
            level: 1,
            content: content.into(),
            location: Location::default(),
            children: vec![],
        }],
    }
}
