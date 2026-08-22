use std::collections::BTreeMap;
use std::sync::Arc;

use reading_mcp::application::ports::{ApplicationError, DocumentRepository};
use reading_mcp::application::read_document::{
    ContinueExactReadCommand, ReadDocumentUseCase, ReadExactTargetCommand,
};
use reading_mcp::domain::{
    ContentHash, Document, DocumentId, DocumentSource, Location, MediaType, NormalizedTextRange,
    Section, SectionId, TextLocator,
};
use reading_mcp::infrastructure::InMemoryDocumentRepository;

#[tokio::test]
async fn exact_sentence_paragraph_and_character_range_reads_return_canonical_slices() {
    let document = document_fixture();
    let repository = Arc::new(InMemoryDocumentRepository::default());
    repository.save(document.clone()).await.expect("save");
    let use_case = ReadDocumentUseCase::new(repository);
    let section = document
        .find_section(&SectionId("section://root".into()))
        .expect("root section");

    let sentence = document
        .sentence_text_units()
        .units
        .into_iter()
        .find(|unit| unit.paragraph_index == 1 && unit.sentence_index == 2)
        .expect("second sentence");
    let sentence_locator = TextLocator::for_sentence(&document, section, &sentence);
    let sentence_read = use_case
        .read_exact(ReadExactTargetCommand {
            document_id: document.id.clone(),
            target_locator: sentence_locator.clone(),
            max_chars: None,
        })
        .await
        .expect("sentence exact read");
    assert_eq!(sentence_read.content, sentence.text);
    assert_eq!(sentence_read.resolved_target_locator, sentence_locator);
    assert_eq!(sentence_read.stream.read_mode, "exact_target");
    assert_eq!(
        sentence_read.stream.coordinate_space,
        "exact-target-unicode-scalar/v1"
    );
    assert!(sentence_read.complete);
    assert_source_segment_matches(&document, &sentence_read);

    let paragraph = document
        .paragraph_text_units()
        .units
        .into_iter()
        .find(|unit| unit.paragraph_index == 2)
        .expect("second paragraph");
    let paragraph_locator = TextLocator::for_paragraph(&document, section, &paragraph);
    let paragraph_read = use_case
        .read_exact(ReadExactTargetCommand {
            document_id: document.id.clone(),
            target_locator: paragraph_locator.clone(),
            max_chars: None,
        })
        .await
        .expect("paragraph exact read");
    assert_eq!(paragraph_read.content, paragraph.text);
    assert_eq!(paragraph_read.resolved_target_locator, paragraph_locator);
    assert_source_segment_matches(&document, &paragraph_read);

    let range = NormalizedTextRange::new(6, 9).expect("range");
    let range_locator = TextLocator::for_character_range(&document, section, range);
    let range_read = use_case
        .read_exact(ReadExactTargetCommand {
            document_id: document.id.clone(),
            target_locator: range_locator.clone(),
            max_chars: None,
        })
        .await
        .expect("character range exact read");
    assert_eq!(
        range_read.content,
        section.normalized_text_slice(range).unwrap()
    );
    assert_eq!(range_read.resolved_target_locator, range_locator);
    assert_source_segment_matches(&document, &range_read);
}

#[tokio::test]
async fn exact_section_locator_reads_only_section_content_not_child_subtree() {
    let document = document_fixture();
    let repository = Arc::new(InMemoryDocumentRepository::default());
    repository.save(document.clone()).await.expect("save");
    let use_case = ReadDocumentUseCase::new(repository);
    let section = document
        .find_section(&SectionId("section://root".into()))
        .expect("root section");

    let result = use_case
        .read_exact(ReadExactTargetCommand {
            document_id: document.id.clone(),
            target_locator: TextLocator::for_section(&document, section),
            max_chars: None,
        })
        .await
        .expect("exact Section.content read");

    assert_eq!(result.content, section.content);
    assert!(!result.content.contains("Child-only text"));
    assert_eq!(result.stream.read_mode, "exact_target");
    assert_source_segment_matches(&document, &result);
}

#[tokio::test]
async fn oversized_exact_target_continues_without_gap_or_overlap_and_returns_source_ranges() {
    let mut document = document_fixture();
    document.root_sections[0].content = format!(
        "{}",
        (0..30)
            .map(|index| format!("Sentence {index} keeps exact read continuation deterministic."))
            .collect::<Vec<_>>()
            .join(" ")
    );
    let repository = Arc::new(InMemoryDocumentRepository::default());
    repository.save(document.clone()).await.expect("save");
    let use_case = ReadDocumentUseCase::new(repository);
    let section = document
        .find_section(&SectionId("section://root".into()))
        .expect("root section");
    let paragraph = document
        .paragraph_text_units()
        .units
        .into_iter()
        .next()
        .expect("paragraph");
    let locator = TextLocator::for_paragraph(&document, section, &paragraph);

    let mut page = use_case
        .read_exact(ReadExactTargetCommand {
            document_id: document.id.clone(),
            target_locator: locator.clone(),
            max_chars: Some(37),
        })
        .await
        .expect("first page");
    let mut rebuilt = String::new();
    let mut previous_stream_end = 0usize;
    let mut previous_source_end = paragraph.normalized_range.start();
    let mut calls = 0usize;

    loop {
        calls += 1;
        assert!(calls < 100, "exact continuation must make finite progress");
        assert_eq!(page.resolved_target_locator, locator);
        assert_eq!(page.stream.read_mode, "exact_target");
        assert_eq!(page.stream.start_char, previous_stream_end);
        let returned = page
            .returned_locator
            .as_ref()
            .and_then(|locator| locator.normalized_range)
            .expect("exact page must expose source range");
        assert_eq!(returned.start(), previous_source_end);
        assert_source_segment_matches(&document, &page);
        previous_source_end = returned.end();
        previous_stream_end = page.stream.end_char;
        rebuilt.push_str(&page.content);

        if page.complete {
            assert!(page.next_cursor.is_none());
            break;
        }

        let cursor = page.next_cursor.clone().expect("cursor");
        page = use_case
            .continue_exact(ContinueExactReadCommand {
                document_id: document.id.clone(),
                target_locator: locator.clone(),
                cursor,
                max_chars: Some(31),
            })
            .await
            .expect("continuation");
    }

    assert_eq!(rebuilt, paragraph.text);
    assert_eq!(previous_source_end, paragraph.normalized_range.end());
    assert_eq!(previous_stream_end, paragraph.text.chars().count());
}

#[tokio::test]
async fn exact_cursor_cannot_be_reused_for_a_different_locator() {
    let document = document_fixture();
    let repository = Arc::new(InMemoryDocumentRepository::default());
    repository.save(document.clone()).await.expect("save");
    let use_case = ReadDocumentUseCase::new(repository);
    let section = document
        .find_section(&SectionId("section://root".into()))
        .expect("root section");
    let paragraphs = document.paragraph_text_units().units;
    let first_locator = TextLocator::for_paragraph(&document, section, &paragraphs[0]);
    let second_locator = TextLocator::for_paragraph(&document, section, &paragraphs[1]);

    let first = use_case
        .read_exact(ReadExactTargetCommand {
            document_id: document.id.clone(),
            target_locator: first_locator,
            max_chars: Some(5),
        })
        .await
        .expect("bounded read");
    let error = use_case
        .continue_exact(ContinueExactReadCommand {
            document_id: document.id,
            target_locator: second_locator,
            cursor: first.next_cursor.expect("cursor"),
            max_chars: Some(5),
        })
        .await
        .expect_err("cursor target must remain fixed");
    assert!(matches!(error, ApplicationError::CursorTargetMismatch(_)));
}

#[tokio::test]
async fn exact_locator_fails_closed_when_normalized_document_changes() {
    let document = document_fixture();
    let repository = Arc::new(InMemoryDocumentRepository::default());
    repository.save(document.clone()).await.expect("save");
    let use_case = ReadDocumentUseCase::new(repository.clone());
    let section = document
        .find_section(&SectionId("section://root".into()))
        .expect("root section");
    let sentence = document
        .sentence_text_units()
        .units
        .into_iter()
        .next()
        .expect("sentence");
    let locator = TextLocator::for_sentence(&document, section, &sentence);

    let mut changed = document.clone();
    changed.root_sections[0].content = "Changed normalized content.".into();
    repository.save(changed).await.expect("replace");

    let error = use_case
        .read_exact(ReadExactTargetCommand {
            document_id: document.id,
            target_locator: locator,
            max_chars: None,
        })
        .await
        .expect_err("old locator must stale");
    assert!(matches!(error, ApplicationError::StaleLocator(_)));
}

#[tokio::test]
async fn invalid_character_range_and_locator_shape_are_rejected() {
    let document = document_fixture();
    let repository = Arc::new(InMemoryDocumentRepository::default());
    repository.save(document.clone()).await.expect("save");
    let use_case = ReadDocumentUseCase::new(repository);
    let section = document
        .find_section(&SectionId("section://root".into()))
        .expect("root section");

    let out_of_bounds = NormalizedTextRange::new(0, section.normalized_text_len() + 1).unwrap();
    let error = use_case
        .read_exact(ReadExactTargetCommand {
            document_id: document.id.clone(),
            target_locator: TextLocator::for_character_range(&document, section, out_of_bounds),
            max_chars: None,
        })
        .await
        .expect_err("out-of-bounds range must fail");
    assert!(matches!(error, ApplicationError::InvalidLocator(_)));

    let mut malformed = TextLocator::for_section(&document, section);
    malformed.sentence_index = Some(1);
    let error = use_case
        .read_exact(ReadExactTargetCommand {
            document_id: document.id,
            target_locator: malformed,
            max_chars: None,
        })
        .await
        .expect_err("malformed locator must fail");
    assert!(matches!(error, ApplicationError::InvalidLocator(_)));
}

fn assert_source_segment_matches(
    document: &Document,
    result: &reading_mcp::application::read_document::ReadSectionResult,
) {
    let returned = result
        .returned_locator
        .as_ref()
        .expect("exact read must return a source locator");
    assert!(returned.paragraph_index.is_none());
    assert!(returned.sentence_index.is_none());
    assert!(returned.segmentation_version.is_none());
    let section = document
        .find_section(&returned.owner_section_id)
        .expect("returned owner section");
    let range = returned.normalized_range.expect("returned range");
    assert_eq!(
        section.normalized_text_slice(range).unwrap(),
        result.content
    );
}

fn document_fixture() -> Document {
    Document {
        id: DocumentId("doc:precise-read".into()),
        source: DocumentSource("memory:precise-read".into()),
        title: "Precise read".into(),
        media_type: MediaType("text/plain".into()),
        content_hash: ContentHash("sha256:raw".into()),
        metadata: BTreeMap::new(),
        root_sections: vec![Section {
            id: SectionId("section://root".into()),
            parent_id: None,
            title: "Root".into(),
            level: 1,
            content: "Alpha 中🙂. Second sentence.\n\nParagraph two remains exact.".into(),
            location: Location {
                section_path: vec!["Root".into()],
                ..Location::default()
            },
            children: vec![Section {
                id: SectionId("section://root/child".into()),
                parent_id: Some(SectionId("section://root".into())),
                title: "Child".into(),
                level: 2,
                content: "Child-only text must not leak into exact Section.content reads.".into(),
                location: Location {
                    section_path: vec!["Root".into(), "Child".into()],
                    ..Location::default()
                },
                children: vec![],
            }],
        }],
    }
}
