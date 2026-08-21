use std::collections::BTreeMap;
use std::sync::Arc;

use reading_mcp::application::ports::{ApplicationError, DocumentRepository};
use reading_mcp::application::read_document::{
    ContinueReadCommand, ReadDocumentUseCase, ReadSectionCommand,
};
use reading_mcp::domain::{
    ContentHash, Document, DocumentId, DocumentSource, Location, MediaType, Section, SectionId,
};
use reading_mcp::infrastructure::InMemoryDocumentRepository;

#[tokio::test]
async fn continuation_reconstructs_the_complete_section_tree_without_gap_or_overlap() {
    let document = document("sha256:raw", root_content());
    let repository = Arc::new(InMemoryDocumentRepository::default());
    repository
        .save(document.clone())
        .await
        .expect("document should save");
    let use_case = ReadDocumentUseCase::new(repository);

    let full = use_case
        .execute(ReadSectionCommand {
            document_id: document.id.clone(),
            section_id: SectionId("section://root".into()),
            max_chars: None,
        })
        .await
        .expect("full read should succeed");
    assert!(full.complete);
    assert!(!full.truncated);
    assert!(full.next_cursor.is_none());

    let mut segment = use_case
        .execute(ReadSectionCommand {
            document_id: document.id.clone(),
            section_id: SectionId("section://root".into()),
            max_chars: Some(17),
        })
        .await
        .expect("initial bounded read should succeed");
    let expected_total = segment.stream.total_chars;
    let mut previous_end = 0;
    let mut reconstructed = String::new();
    let mut calls = 0;

    loop {
        calls += 1;
        assert!(calls < 100, "continuation must make finite progress");
        assert_eq!(segment.stream.read_mode, "section_tree");
        assert_eq!(segment.stream.rendering_version, "section-tree-markdown/v1");
        assert_eq!(segment.stream.start_char, previous_end);
        assert!(segment.stream.end_char > segment.stream.start_char);
        assert_eq!(segment.stream.total_chars, expected_total);
        assert_eq!(segment.truncated, !segment.complete);
        reconstructed.push_str(&segment.content);
        previous_end = segment.stream.end_char;

        if segment.complete {
            assert!(segment.next_cursor.is_none());
            break;
        }

        let cursor = segment
            .next_cursor
            .clone()
            .expect("incomplete read must return a cursor");
        segment = use_case
            .continue_read(ContinueReadCommand {
                document_id: document.id.clone(),
                section_id: SectionId("section://root".into()),
                cursor,
                max_chars: Some(13),
            })
            .await
            .expect("continuation should succeed");
    }

    assert_eq!(previous_end, expected_total);
    assert_eq!(reconstructed, full.content);
}

#[tokio::test]
async fn cursor_survives_use_case_and_repository_reconstruction() {
    let document = document("sha256:raw", root_content());
    let first_repository = Arc::new(InMemoryDocumentRepository::default());
    first_repository
        .save(document.clone())
        .await
        .expect("document should save");
    let first_use_case = ReadDocumentUseCase::new(first_repository);
    let first = first_use_case
        .execute(ReadSectionCommand {
            document_id: document.id.clone(),
            section_id: SectionId("section://root".into()),
            max_chars: Some(20),
        })
        .await
        .expect("initial read should succeed");
    let cursor = first.next_cursor.expect("cursor should be returned");

    let second_repository = Arc::new(InMemoryDocumentRepository::default());
    second_repository
        .save(document.clone())
        .await
        .expect("same canonical document should save");
    let second_use_case = ReadDocumentUseCase::new(second_repository);
    let continued = second_use_case
        .continue_read(ContinueReadCommand {
            document_id: document.id,
            section_id: SectionId("section://root".into()),
            cursor,
            max_chars: Some(20),
        })
        .await
        .expect("self-contained cursor should survive reconstruction");

    assert_eq!(continued.stream.start_char, first.stream.end_char);
}

#[tokio::test]
async fn cursor_fails_closed_when_normalized_document_changes() {
    let original = document("sha256:raw-unchanged", root_content());
    let repository = Arc::new(InMemoryDocumentRepository::default());
    repository
        .save(original.clone())
        .await
        .expect("document should save");
    let use_case = ReadDocumentUseCase::new(repository.clone());
    let first = use_case
        .execute(ReadSectionCommand {
            document_id: original.id.clone(),
            section_id: SectionId("section://root".into()),
            max_chars: Some(15),
        })
        .await
        .expect("initial read should succeed");

    repository
        .save(document(
            "sha256:raw-unchanged",
            "Canonical normalized content changed while raw provenance stayed the same.",
        ))
        .await
        .expect("changed normalized document should replace old state");

    let error = use_case
        .continue_read(ContinueReadCommand {
            document_id: original.id,
            section_id: SectionId("section://root".into()),
            cursor: first.next_cursor.expect("cursor should exist"),
            max_chars: Some(15),
        })
        .await
        .expect_err("changed normalized facts must stale the cursor");

    assert!(matches!(error, ApplicationError::StaleCursor(_)));
}

#[tokio::test]
async fn cursor_fails_closed_when_raw_content_hash_changes() {
    let original = document("sha256:raw-one", root_content());
    let repository = Arc::new(InMemoryDocumentRepository::default());
    repository
        .save(original.clone())
        .await
        .expect("document should save");
    let use_case = ReadDocumentUseCase::new(repository.clone());
    let first = use_case
        .execute(ReadSectionCommand {
            document_id: original.id.clone(),
            section_id: SectionId("section://root".into()),
            max_chars: Some(15),
        })
        .await
        .expect("initial read should succeed");

    repository
        .save(document("sha256:raw-two", root_content()))
        .await
        .expect("changed raw version should replace old state");

    let error = use_case
        .continue_read(ContinueReadCommand {
            document_id: original.id,
            section_id: SectionId("section://root".into()),
            cursor: first.next_cursor.expect("cursor should exist"),
            max_chars: Some(15),
        })
        .await
        .expect_err("changed raw hash must stale the cursor");

    assert!(matches!(error, ApplicationError::StaleCursor(_)));
}

#[tokio::test]
async fn cursor_cannot_be_reused_for_another_section() {
    let document = document("sha256:raw", root_content());
    let repository = Arc::new(InMemoryDocumentRepository::default());
    repository
        .save(document.clone())
        .await
        .expect("document should save");
    let use_case = ReadDocumentUseCase::new(repository);
    let first = use_case
        .execute(ReadSectionCommand {
            document_id: document.id.clone(),
            section_id: SectionId("section://root".into()),
            max_chars: Some(15),
        })
        .await
        .expect("initial read should succeed");

    let error = use_case
        .continue_read(ContinueReadCommand {
            document_id: document.id,
            section_id: SectionId("section://root/child".into()),
            cursor: first.next_cursor.expect("cursor should exist"),
            max_chars: Some(15),
        })
        .await
        .expect_err("cursor target mismatch must fail");

    assert!(matches!(error, ApplicationError::CursorTargetMismatch(_)));
}

#[tokio::test]
async fn tampered_cursor_and_zero_budget_are_rejected() {
    let document = document("sha256:raw", root_content());
    let repository = Arc::new(InMemoryDocumentRepository::default());
    repository
        .save(document.clone())
        .await
        .expect("document should save");
    let use_case = ReadDocumentUseCase::new(repository);

    let zero_error = use_case
        .execute(ReadSectionCommand {
            document_id: document.id.clone(),
            section_id: SectionId("section://root".into()),
            max_chars: Some(0),
        })
        .await
        .expect_err("zero budget cannot make continuation progress");
    assert!(matches!(zero_error, ApplicationError::InvalidRequest(_)));

    let first = use_case
        .execute(ReadSectionCommand {
            document_id: document.id.clone(),
            section_id: SectionId("section://root".into()),
            max_chars: Some(15),
        })
        .await
        .expect("initial read should succeed");
    let mut cursor = first.next_cursor.expect("cursor should exist");
    let last = cursor.pop().expect("cursor should be non-empty");
    cursor.push(if last == '0' { '1' } else { '0' });

    let cursor_error = use_case
        .continue_read(ContinueReadCommand {
            document_id: document.id,
            section_id: SectionId("section://root".into()),
            cursor,
            max_chars: Some(15),
        })
        .await
        .expect_err("tampered cursor must fail");
    assert!(matches!(cursor_error, ApplicationError::InvalidCursor(_)));
}

fn document(content_hash: &str, root_content: &str) -> Document {
    Document {
        id: DocumentId("doc:continuation".into()),
        source: DocumentSource("memory:continuation.md".into()),
        title: "Continuation".into(),
        media_type: MediaType("text/markdown".into()),
        content_hash: ContentHash(content_hash.into()),
        metadata: BTreeMap::new(),
        root_sections: vec![Section {
            id: SectionId("section://root".into()),
            parent_id: None,
            title: "Root".into(),
            level: 1,
            content: root_content.into(),
            location: Location {
                section_path: vec!["Root".into()],
                ..Location::default()
            },
            children: vec![Section {
                id: SectionId("section://root/child".into()),
                parent_id: Some(SectionId("section://root".into())),
                title: "Child".into(),
                level: 2,
                content: "Child content with Unicode: 进程、内存、🙂.".into(),
                location: Location {
                    section_path: vec!["Root".into(), "Child".into()],
                    ..Location::default()
                },
                children: vec![],
            }],
        }],
    }
}

fn root_content() -> &'static str {
    "First paragraph contains enough text to require several bounded reads.\n\nSecond paragraph preserves source order across Unicode: 系统调用 fork() mmap() 🙂."
}

#[test]
fn read_contract_exposes_actionable_continuation_metadata() {
    let request_schema = schemars::schema_for!(reading_mcp::mcp::contracts::ReadDocumentRequest);
    let request = serde_json::to_value(request_schema).expect("request schema should serialize");
    let request_properties = request
        .get("properties")
        .and_then(|value| value.as_object())
        .expect("request schema should contain properties");
    assert!(request_properties.contains_key("cursor"));

    let response_schema = schemars::schema_for!(reading_mcp::mcp::contracts::ReadDocumentResponse);
    let response = serde_json::to_value(response_schema).expect("response schema should serialize");
    let response_properties = response
        .get("properties")
        .and_then(|value| value.as_object())
        .expect("response schema should contain properties");
    assert!(response_properties.contains_key("complete"));
    assert!(response_properties.contains_key("next_cursor"));
    assert!(response_properties.contains_key("stream"));
}
