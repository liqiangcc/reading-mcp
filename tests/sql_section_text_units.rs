use std::collections::BTreeMap;
use std::sync::Arc;

use reading_mcp::application::get_text_units::{
    GetTextUnitsCommand, GetTextUnitsUseCase, RequestedTextUnitKind, TextUnitCoveragePolicy,
    TextUnitDirection,
};
use reading_mcp::application::ports::{
    ApplicationError, DocumentRepository, Parser, RetrievedResource, SectionTextUnits,
    TextUnitIndex,
};
use reading_mcp::application::text_unit_store::store_text_units;
use reading_mcp::domain::{Document, DocumentId, DocumentSource, MediaType, Section, SectionId};
use reading_mcp::infrastructure::{
    InMemoryTextUnitIndex, SqliteDocumentRepository, SqliteTextUnitIndex,
};
use reading_mcp::parsing::{MarkdownParser, TextParser};

const PLAIN: &str = "Alpha one. Alpha two. Alpha three.\n\nBeta one. Beta two.\n\nGamma one. Gamma two. Gamma three.";

const MARKDOWN: &str = "# Alpha\n\nAlpha first. Alpha second.\n\n- List item one. List item two.\n\n> Quote alpha. Quote beta.\n\n# Beta\n\nBeta first. Beta second.\n\n```text\ncode line one. code line two.\n```\n\n# Gamma\n\nGamma first. Gamma second. Gamma third.\n";

#[tokio::test]
async fn sqlite_section_units_match_from_sets_for_every_section() {
    let directory = tempfile::tempdir().expect("temporary directory should be created");
    let database = directory.path().join("state.sqlite");
    let index: Arc<dyn TextUnitIndex> =
        Arc::new(SqliteTextUnitIndex::open(&database).expect("text unit index should open"));

    for document in [parse_plain().await, parse_markdown().await] {
        let document = Arc::new(document);
        let paragraphs = document
            .try_paragraph_text_units()
            .expect("valid paragraph set");
        let sentences = document
            .try_sentence_text_units()
            .expect("valid sentence set");
        index
            .replace_document_sets(
                &document,
                Arc::new(paragraphs.clone()),
                Arc::new(sentences.clone()),
            )
            .await
            .expect("both unit sets should persist");

        let sections = all_sections(&document);
        assert!(!sections.is_empty(), "document must have sections");
        for section in &sections {
            let expected = SectionTextUnits::from_sets(&paragraphs, &sentences, &section.id);
            let actual = index
                .section_units(&document.id, &section.id)
                .await
                .expect("persisted section units should load");
            assert_eq!(actual, Some(expected));
        }
    }
}

#[tokio::test]
async fn get_text_units_is_identical_for_in_memory_and_sqlite_indexes() {
    for document in [parse_plain().await, parse_markdown().await] {
        let directory = tempfile::tempdir().expect("temporary directory should be created");
        let database = directory.path().join("state.sqlite");
        let repository: Arc<dyn DocumentRepository> =
            Arc::new(SqliteDocumentRepository::open(&database).expect("repository should open"));
        let memory_index: Arc<dyn TextUnitIndex> = Arc::new(InMemoryTextUnitIndex::default());
        let sqlite_index: Arc<dyn TextUnitIndex> =
            Arc::new(SqliteTextUnitIndex::open(&database).expect("text unit index should open"));

        store_text_units(&*memory_index, &document)
            .await
            .expect("in-memory units should persist");
        store_text_units(&*sqlite_index, &document)
            .await
            .expect("sqlite units should persist");
        repository
            .save(document.clone())
            .await
            .expect("document should persist");

        let memory_use_case = GetTextUnitsUseCase::new(repository.clone(), memory_index);
        let sqlite_use_case = GetTextUnitsUseCase::new(repository.clone(), sqlite_index);

        let sections = all_sections(&document);
        for section in &sections {
            for requested_kind in [
                RequestedTextUnitKind::Paragraph,
                RequestedTextUnitKind::Sentence,
            ] {
                for coverage_policy in [
                    TextUnitCoveragePolicy::PreserveSource,
                    TextUnitCoveragePolicy::EligibleOnly,
                ] {
                    for direction in [TextUnitDirection::Forward, TextUnitDirection::Backward] {
                        compare_pages(
                            &memory_use_case,
                            &sqlite_use_case,
                            document.id.clone(),
                            section.id.clone(),
                            requested_kind,
                            coverage_policy,
                            direction,
                        )
                        .await;
                    }
                }
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn compare_pages(
    memory_use_case: &GetTextUnitsUseCase,
    sqlite_use_case: &GetTextUnitsUseCase,
    document_id: DocumentId,
    section_id: SectionId,
    requested_kind: RequestedTextUnitKind,
    coverage_policy: TextUnitCoveragePolicy,
    direction: TextUnitDirection,
) {
    let base = GetTextUnitsCommand {
        document_id,
        section_id,
        requested_kind,
        direction,
        coverage_policy,
        max_items: 2,
        max_chars: None,
        cursor: None,
    };

    let mut memory_cursor: Option<String> = None;
    let mut sqlite_cursor: Option<String> = None;
    let mut pages = 0usize;
    loop {
        let mut memory_command = base.clone();
        memory_command.cursor = memory_cursor.clone();
        let mut sqlite_command = base.clone();
        sqlite_command.cursor = sqlite_cursor.clone();

        let memory_result = memory_use_case
            .execute(memory_command)
            .await
            .expect("enumeration through the in-memory index should succeed");
        let sqlite_result = sqlite_use_case
            .execute(sqlite_command)
            .await
            .expect("enumeration through the sqlite index should succeed");
        assert_eq!(memory_result, sqlite_result);

        pages += 1;
        assert!(pages <= 4096, "page loop must terminate");

        match (memory_result.next_cursor, sqlite_result.next_cursor) {
            (None, None) => break,
            (Some(memory_next), Some(sqlite_next)) => {
                memory_cursor = Some(memory_next);
                sqlite_cursor = Some(sqlite_next);
            }
            cursors => panic!("cursor disagreement between read paths: {cursors:?}"),
        }
    }
}

#[tokio::test]
async fn never_stored_document_has_no_section_units_and_fails_as_stale() {
    let directory = tempfile::tempdir().expect("temporary directory should be created");
    let database = directory.path().join("state.sqlite");
    let document = parse_plain().await;
    let repository: Arc<dyn DocumentRepository> =
        Arc::new(SqliteDocumentRepository::open(&database).expect("repository should open"));
    let index: Arc<dyn TextUnitIndex> =
        Arc::new(SqliteTextUnitIndex::open(&database).expect("text unit index should open"));
    repository
        .save(document.clone())
        .await
        .expect("document should persist");

    let section_id = document.root_sections[0].id.clone();
    assert_eq!(
        index
            .section_units(&document.id, &section_id)
            .await
            .expect("a missing set must not error"),
        None
    );

    let use_case = GetTextUnitsUseCase::new(repository, index);
    let error = use_case
        .execute(GetTextUnitsCommand {
            document_id: document.id,
            section_id,
            requested_kind: RequestedTextUnitKind::Paragraph,
            direction: TextUnitDirection::Forward,
            coverage_policy: TextUnitCoveragePolicy::PreserveSource,
            max_items: 2,
            max_chars: None,
            cursor: None,
        })
        .await
        .expect_err("a document that was never stored must not be read");
    assert!(matches!(error, ApplicationError::StaleDocument(_)));
}

fn all_sections(document: &Document) -> Vec<Section> {
    fn collect(sections: &[Section], out: &mut Vec<Section>) {
        for section in sections {
            out.push(section.clone());
            collect(&section.children, out);
        }
    }
    let mut sections = Vec::new();
    collect(&document.root_sections, &mut sections);
    sections
}

async fn parse_plain() -> Document {
    TextParser
        .parse(resource("memory:plain.txt", "text/plain", PLAIN))
        .await
        .expect("plain text should parse")
}

async fn parse_markdown() -> Document {
    MarkdownParser
        .parse(resource("memory:sections.md", "text/markdown", MARKDOWN))
        .await
        .expect("markdown should parse")
}

fn resource(source: &str, media_type: &str, body: &str) -> RetrievedResource {
    RetrievedResource {
        source: DocumentSource(source.into()),
        final_source: DocumentSource(source.into()),
        media_type: MediaType(media_type.into()),
        bytes: body.as_bytes().to_vec(),
        etag: None,
        last_modified: None,
        metadata: BTreeMap::new(),
    }
}

#[tokio::test]
async fn sqlite_section_units_read_long_sections_across_chunks() {
    let directory = tempfile::tempdir().expect("temporary directory should be created");
    let database = directory.path().join("state.sqlite");
    let index: Arc<dyn TextUnitIndex> =
        Arc::new(SqliteTextUnitIndex::open(&database).expect("text unit index should open"));

    let body = "Alpha one. ".repeat(4_000);
    let document = parse_text(&body).await;
    let paragraphs = document
        .try_paragraph_text_units()
        .expect("valid paragraph set");
    let sentences = document
        .try_sentence_text_units()
        .expect("valid sentence set");
    store_text_units(&*index, &document)
        .await
        .expect("long units should persist");
    let connection = rusqlite::Connection::open(&database).expect("database should open");
    let chunk_count: i64 = connection
        .query_row(
            "SELECT chunk_count FROM section_layout_v2
              WHERE doc_key = (SELECT doc_key FROM text_unit_documents_v2 WHERE document_id = ?1)",
            rusqlite::params![&document.id.0],
            |row| row.get(0),
        )
        .expect("chunk count should load");
    drop(connection);
    assert!(
        chunk_count >= 3,
        "the long section must span at least three layout chunks, got {chunk_count}"
    );

    for section in all_sections(&document) {
        let expected = SectionTextUnits::from_sets(&paragraphs, &sentences, &section.id);
        let actual = index
            .section_units(&document.id, &section.id)
            .await
            .expect("persisted section units should load")
            .expect("the section should be stored");
        assert_eq!(actual, expected, "chunked section {}", section.id.0);
    }

    assert!(
        sentences.units.iter().any(|unit| {
            unit.normalized_range.start() / 16_384 != (unit.normalized_range.end() - 1) / 16_384
        }),
        "the fixture must contain a sentence that straddles a text chunk"
    );
}

#[tokio::test]
async fn replacing_sets_twice_updates_the_stored_section_layout() {
    let directory = tempfile::tempdir().expect("temporary directory should be created");
    let database = directory.path().join("state.sqlite");
    let index: Arc<dyn TextUnitIndex> =
        Arc::new(SqliteTextUnitIndex::open(&database).expect("text unit index should open"));

    let document_id = DocumentId("doc:cache".into());
    let section_id = SectionId("section://root".into());

    let mut first = parse_text("First version. Only one sentence.").await;
    first.id = document_id.clone();
    first.root_sections[0].id = section_id.clone();
    let first_paragraphs = first
        .try_paragraph_text_units()
        .expect("valid first paragraph set");
    let first_sentences = first
        .try_sentence_text_units()
        .expect("valid first sentence set");
    store_text_units(&*index, &first)
        .await
        .expect("first units should persist");
    let before = index
        .section_units(&document_id, &section_id)
        .await
        .expect("first read should succeed")
        .expect("first section should be stored");
    assert_eq!(
        before,
        SectionTextUnits::from_sets(&first_paragraphs, &first_sentences, &section_id)
    );

    let mut second = parse_text("Second version. It has two sentences. And a third.").await;
    second.id = document_id.clone();
    second.root_sections[0].id = section_id.clone();
    let second_paragraphs = second
        .try_paragraph_text_units()
        .expect("valid second paragraph set");
    let second_sentences = second
        .try_sentence_text_units()
        .expect("valid second sentence set");
    store_text_units(&*index, &second)
        .await
        .expect("second units should replace the first");
    let after = index
        .section_units(&document_id, &section_id)
        .await
        .expect("second read should succeed")
        .expect("second section should be stored");
    assert_eq!(
        after,
        SectionTextUnits::from_sets(&second_paragraphs, &second_sentences, &section_id)
    );
    assert_ne!(after.paragraphs, before.paragraphs);
}

async fn parse_text(body: &str) -> Document {
    TextParser
        .parse(resource("memory:long.txt", "text/plain", body))
        .await
        .expect("plain text should parse")
}
