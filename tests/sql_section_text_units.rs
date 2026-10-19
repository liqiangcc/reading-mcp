use std::collections::BTreeMap;
use std::sync::Arc;

use reading_mcp::application::get_text_units::{
    GetTextUnitsCommand, GetTextUnitsUseCase, RequestedTextUnitKind, TextUnitCoveragePolicy,
    TextUnitDirection,
};
use reading_mcp::application::ports::{
    DocumentRepository, Parser, RetrievedResource, SectionTextUnits, TextUnitIndex,
};
use reading_mcp::domain::{
    Document, DocumentId, DocumentSource, MediaType, NormalizedDocumentHash, Section, SectionId,
};
use reading_mcp::infrastructure::{SqliteDocumentRepository, SqliteTextUnitIndex};
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
        let paragraphs = document
            .try_paragraph_text_units()
            .expect("valid paragraph set");
        let sentences = document
            .try_sentence_text_units()
            .expect("valid sentence set");
        index
            .replace_document_sets(
                &document.id,
                Arc::new(paragraphs.clone()),
                Arc::new(sentences.clone()),
            )
            .await
            .expect("both unit sets should persist");

        let normalized_hash = document.normalized_document_hash();
        let sections = all_sections(&document);
        assert!(!sections.is_empty(), "document must have sections");
        for section in &sections {
            let expected = SectionTextUnits::from_sets(&paragraphs, &sentences, &section.id);
            let actual = index
                .section_units(&document.id, &normalized_hash, &section.id)
                .await
                .expect("persisted section units should load");
            assert_eq!(actual, Some(expected));
        }

        let other_hash = NormalizedDocumentHash("sha256:not-this-document".into());
        for section in &sections {
            assert_eq!(
                index
                    .section_units(&document.id, &other_hash, &section.id)
                    .await
                    .expect("mismatched hash must not error"),
                None
            );
        }
    }
}

#[tokio::test]
async fn get_text_units_is_identical_with_and_without_persisted_section_units() {
    for document in [parse_plain().await, parse_markdown().await] {
        let directory = tempfile::tempdir().expect("temporary directory should be created");
        let database = directory.path().join("state.sqlite");
        let repository: Arc<dyn DocumentRepository> =
            Arc::new(SqliteDocumentRepository::open(&database).expect("repository should open"));
        let index: Arc<dyn TextUnitIndex> =
            Arc::new(SqliteTextUnitIndex::open(&database).expect("text unit index should open"));

        let paragraphs = document
            .try_paragraph_text_units()
            .expect("valid paragraph set");
        let sentences = document
            .try_sentence_text_units()
            .expect("valid sentence set");
        index
            .replace_document_sets(&document.id, Arc::new(paragraphs), Arc::new(sentences))
            .await
            .expect("both unit sets should persist");
        repository
            .save(document.clone())
            .await
            .expect("document should persist");

        let without_index = GetTextUnitsUseCase::new(repository.clone());
        let with_index =
            GetTextUnitsUseCase::new(repository.clone()).with_text_unit_index(index.clone());

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
                            &without_index,
                            &with_index,
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
    without_index: &GetTextUnitsUseCase,
    with_index: &GetTextUnitsUseCase,
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

    let mut without_cursor: Option<String> = None;
    let mut with_cursor: Option<String> = None;
    let mut pages = 0usize;
    loop {
        let mut without_command = base.clone();
        without_command.cursor = without_cursor.clone();
        let mut with_command = base.clone();
        with_command.cursor = with_cursor.clone();

        let without_result = without_index
            .execute(without_command)
            .await
            .expect("enumeration without the index should succeed");
        let with_result = with_index
            .execute(with_command)
            .await
            .expect("enumeration with the index should succeed");
        assert_eq!(without_result, with_result);

        pages += 1;
        assert!(pages <= 4096, "page loop must terminate");

        match (without_result.next_cursor, with_result.next_cursor) {
            (None, None) => break,
            (Some(without_next), Some(with_next)) => {
                without_cursor = Some(without_next);
                with_cursor = Some(with_next);
            }
            cursors => panic!("cursor disagreement between read paths: {cursors:?}"),
        }
    }
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
