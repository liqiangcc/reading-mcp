use std::collections::BTreeMap;
use std::sync::Arc;

use reading_mcp::application::get_text_units::{
    EffectiveTextUnitKind, GetTextUnitsCommand, GetTextUnitsUseCase, RequestedTextUnitKind,
    TextUnitCoveragePolicy, TextUnitDirection,
};
use reading_mcp::application::ports::{
    ApplicationError, DocumentRepository, Parser, RetrievedResource, SectionTextUnits,
    StoredStreamItem, TextUnitIndex, TextUnitStreamKey, TextUnitStreamSummary,
};
use reading_mcp::application::text_unit_store::store_text_units;
use reading_mcp::domain::{
    ContentHash, Document, DocumentId, DocumentSource, Location, MediaType, ParagraphTextUnitSet,
    Section, SectionId, SentenceTextUnitSet,
};
use reading_mcp::infrastructure::{
    InMemoryTextUnitIndex, SqliteDocumentRepository, SqliteTextUnitIndex,
};
use reading_mcp::parsing::{HtmlParser, MarkdownParser, TextParser};
use tempfile::tempdir;

const PLAIN: &str = "Alpha one. Alpha two. Alpha three.\n\nBeta one. Beta two.\n\nGamma one. Gamma two. Gamma three.";

const MARKDOWN: &str = "# Alpha\n\nAlpha first. Alpha second.\n\n- List item one. List item two.\n\n> Quote alpha. Quote beta.\n\n# Beta\n\nBeta first. Beta second.\n\n```text\ncode line one. code line two.\n```\n\n# Gamma\n\nGamma first. Gamma second. Gamma third.\n";

const HTML: &str = r#"<html><body>
<h1>Chapter</h1>
<p>First. Second.</p>
<blockquote><p>Quote one.</p><p>Quote two.</p></blockquote>
<ul><li>Item one. Item two.</li></ul>
<pre>code. next.</pre>
<table><tr><td>A.</td><td>B.</td></tr></table>
</body></html>"#;

#[tokio::test]
async fn sqlite_streams_match_the_default_planner_for_every_section_kind_and_policy() {
    let directory = tempdir().expect("temporary directory should be created");
    let database = directory.path().join("state.sqlite");
    let sqlite: Arc<dyn TextUnitIndex> =
        Arc::new(SqliteTextUnitIndex::open(&database).expect("sqlite index should open"));
    let memory: Arc<dyn TextUnitIndex> = Arc::new(InMemoryTextUnitIndex::default());

    for document in [
        parse_plain().await,
        parse_markdown().await,
        parse_html().await,
    ] {
        let paragraphs = document
            .try_paragraph_text_units()
            .expect("valid paragraph set");
        let sentences = document
            .try_sentence_text_units()
            .expect("valid sentence set");
        store_text_units(&*sqlite, &document)
            .await
            .expect("sqlite units should persist");
        store_text_units(&*memory, &document)
            .await
            .expect("memory units should persist");

        compare_indexes(&*sqlite, &*memory, &document, &paragraphs, &sentences).await;
    }
}

async fn compare_indexes(
    sqlite: &dyn TextUnitIndex,
    memory: &dyn TextUnitIndex,
    document: &Document,
    paragraphs: &ParagraphTextUnitSet,
    sentences: &SentenceTextUnitSet,
) {
    for section in all_sections(document) {
        let units = SectionTextUnits::from_sets(paragraphs, sentences, &section.id);
        for kind in [
            RequestedTextUnitKind::Paragraph,
            RequestedTextUnitKind::Sentence,
        ] {
            for policy in [
                TextUnitCoveragePolicy::PreserveSource,
                TextUnitCoveragePolicy::EligibleOnly,
            ] {
                let key = TextUnitStreamKey {
                    document_id: &document.id,
                    section_id: &section.id,
                    kind,
                    policy,
                };
                let sqlite_summary = summarize(sqlite.section_stream_summary(key).await);
                let memory_summary = summarize(memory.section_stream_summary(key).await);
                assert_eq!(
                    sqlite_summary, memory_summary,
                    "summary disagreement for section {}/{:?}/{policy:?}",
                    section.id.0, kind
                );
                let Ok(Some(summary)) = sqlite_summary else {
                    continue;
                };

                for limit in [1usize, 3, 256] {
                    for direction in [TextUnitDirection::Forward, TextUnitDirection::Backward] {
                        let sqlite_items =
                            collect_stream(sqlite, key, direction, limit, summary.total_items)
                                .await;
                        let memory_items =
                            collect_stream(memory, key, direction, limit, summary.total_items)
                                .await;
                        assert_eq!(
                            sqlite_items, memory_items,
                            "page disagreement for section {}/{:?}/{policy:?}/{direction:?} limit {limit}",
                            section.id.0, kind
                        );
                        assert_eq!(sqlite_items.len(), summary.total_items);
                        for (position, item) in sqlite_items.iter().enumerate() {
                            assert_eq!(
                                item.position, position,
                                "dense positions for section {}/{:?}/{policy:?}",
                                section.id.0, kind
                            );
                        }
                    }
                }

                compare_positions(sqlite, memory, &units, key).await;
            }
        }
    }
}

async fn compare_positions(
    sqlite: &dyn TextUnitIndex,
    memory: &dyn TextUnitIndex,
    units: &SectionTextUnits,
    key: TextUnitStreamKey<'_>,
) {
    let kind = key.kind;
    let policy = key.policy;
    let mut probes = Vec::new();
    for paragraph in &units.paragraphs {
        probes.push((
            EffectiveTextUnitKind::Paragraph,
            paragraph.paragraph_index,
            None,
        ));
    }
    for sentence in &units.sentences {
        probes.push((
            EffectiveTextUnitKind::Sentence,
            sentence.paragraph_index,
            Some(sentence.sentence_index),
        ));
    }
    probes.push((EffectiveTextUnitKind::Paragraph, 9999, None));
    probes.push((EffectiveTextUnitKind::Sentence, 9999, Some(0)));

    for (unit_kind, paragraph_index, sentence_index) in probes {
        let sqlite_position = sqlite
            .section_stream_position(key, unit_kind, paragraph_index, sentence_index)
            .await
            .expect("sqlite position lookup should succeed");
        let memory_position = memory
            .section_stream_position(key, unit_kind, paragraph_index, sentence_index)
            .await
            .expect("memory position lookup should succeed");
        assert_eq!(
            sqlite_position, memory_position,
            "position disagreement for section {}/{:?}/{policy:?} probe ({unit_kind:?}, {paragraph_index}, {sentence_index:?})",
            key.section_id.0, kind
        );
    }
}

async fn collect_stream(
    index: &dyn TextUnitIndex,
    key: TextUnitStreamKey<'_>,
    direction: TextUnitDirection,
    limit: usize,
    total_items: usize,
) -> Vec<StoredStreamItem> {
    let mut collected = Vec::new();
    let mut position = match direction {
        TextUnitDirection::Forward => 0,
        TextUnitDirection::Backward => total_items,
    };
    loop {
        let batch = index
            .section_stream_items(key, direction, position, limit)
            .await
            .expect("stream page should load");
        if batch.is_empty() {
            break;
        }
        match direction {
            TextUnitDirection::Forward => position += batch.len(),
            TextUnitDirection::Backward => position -= batch.len(),
        }
        collected.extend(batch);
        assert!(collected.len() <= total_items, "page loop must terminate");
        if position == 0 {
            break;
        }
    }
    match direction {
        TextUnitDirection::Forward => collected,
        // Backward batches arrive most-recent-first; sorting by position gives the whole stream order.
        TextUnitDirection::Backward => {
            collected.sort_by_key(|item| item.position);
            collected
        }
    }
}

#[tokio::test]
async fn large_section_pages_preserve_positions_and_eligible_only_is_dense() {
    let directory = tempdir().expect("temporary directory should be created");
    let database = directory.path().join("state.sqlite");
    let index = SqliteTextUnitIndex::open(&database).expect("index should open");
    let document = large_document(5_000);
    store_text_units(&index, &document)
        .await
        .expect("units should persist");
    let section_id = document.root_sections[0].id.clone();

    let preserve_key = TextUnitStreamKey {
        document_id: &document.id,
        section_id: &section_id,
        kind: RequestedTextUnitKind::Sentence,
        policy: TextUnitCoveragePolicy::PreserveSource,
    };

    let summary = index
        .section_stream_summary(preserve_key)
        .await
        .expect("summary should load")
        .expect("stream should be stored");
    assert_eq!(summary.total_items, 5_000);

    for start in [0usize, 2_500, 4_990] {
        let page = index
            .section_stream_items(preserve_key, TextUnitDirection::Forward, start, 10)
            .await
            .expect("forward page should load");
        let expected: Vec<usize> = (start..(start + 10).min(5_000)).collect();
        assert_eq!(
            page.iter().map(|item| item.position).collect::<Vec<_>>(),
            expected
        );
    }

    for end in [10usize, 2_500, 5_000] {
        let page = index
            .section_stream_items(preserve_key, TextUnitDirection::Backward, end, 10)
            .await
            .expect("backward page should load");
        let expected: Vec<usize> = ((end - 10)..end).collect();
        assert_eq!(
            page.iter().map(|item| item.position).collect::<Vec<_>>(),
            expected
        );
    }

    let eligible_key = TextUnitStreamKey {
        policy: TextUnitCoveragePolicy::EligibleOnly,
        ..preserve_key
    };
    let eligible = index
        .section_stream_summary(eligible_key)
        .await
        .expect("eligible summary should load")
        .expect("eligible stream should be stored");
    assert_eq!(eligible.total_items, 5_000);
    let first_page = index
        .section_stream_items(eligible_key, TextUnitDirection::Forward, 0, 256)
        .await
        .expect("eligible page should load");
    assert_eq!(
        first_page
            .iter()
            .map(|item| item.position)
            .collect::<Vec<_>>(),
        (0..256).collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn replacing_the_unit_sets_replaces_the_stream_and_summary_rows() {
    let directory = tempdir().expect("temporary directory should be created");
    let database = directory.path().join("state.sqlite");
    let index = SqliteTextUnitIndex::open(&database).expect("index should open");

    let first = document_with_content("doc:replace", "First only. Second only.");
    let second = document_with_content("doc:replace", "Replacement text. Another sentence.");
    store_text_units(&index, &first)
        .await
        .expect("first sets should persist");
    store_text_units(&index, &second)
        .await
        .expect("second sets should replace the first");

    let section_id = SectionId("section://root".into());
    let summary = index
        .section_stream_summary(TextUnitStreamKey {
            document_id: &second.id,
            section_id: &section_id,
            kind: RequestedTextUnitKind::Sentence,
            policy: TextUnitCoveragePolicy::PreserveSource,
        })
        .await
        .expect("summary should load")
        .expect("summary should exist");
    let total_items = summary.total_items as i64;

    let connection =
        rusqlite::Connection::open(&database).expect("database should open for row counts");
    let stream_rows: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM text_unit_streams_v1 WHERE document_id = ?1",
            rusqlite::params![&second.id.0],
            |row| row.get(0),
        )
        .expect("stream rows should count");
    let summary_rows: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM text_unit_stream_summaries_v1 WHERE document_id = ?1",
            rusqlite::params![&second.id.0],
            |row| row.get(0),
        )
        .expect("summary rows should count");

    // One paragraph, one sentence stream entry per kind, and four summary rows (2 kinds x 2 policies).
    assert_eq!(stream_rows, 3);
    assert_eq!(summary_rows, 4);
    assert_eq!(total_items, 2);
}

#[tokio::test]
async fn missing_stream_summary_is_reported_as_stale() {
    let directory = tempdir().expect("temporary directory should be created");
    let database = directory.path().join("state.sqlite");
    let repository: Arc<dyn DocumentRepository> =
        Arc::new(SqliteDocumentRepository::open(&database).expect("repository should open"));
    let index: Arc<dyn TextUnitIndex> =
        Arc::new(SqliteTextUnitIndex::open(&database).expect("index should open"));
    let document = parse_plain().await;
    repository
        .save(document.clone())
        .await
        .expect("document should persist");
    store_text_units(&*index, &document)
        .await
        .expect("units should persist");

    rusqlite::Connection::open(&database)
        .expect("database should open")
        .execute(
            "DELETE FROM text_unit_stream_summaries_v1 WHERE document_id = ?1",
            rusqlite::params![&document.id.0],
        )
        .expect("summary rows should be deleted");

    let error = GetTextUnitsUseCase::new(repository, index)
        .execute(GetTextUnitsCommand {
            document_id: document.id.clone(),
            section_id: document.root_sections[0].id.clone(),
            requested_kind: RequestedTextUnitKind::Paragraph,
            direction: TextUnitDirection::Forward,
            coverage_policy: TextUnitCoveragePolicy::PreserveSource,
            max_items: 8,
            max_chars: None,
            cursor: None,
        })
        .await
        .expect_err("a missing summary must fail closed");
    assert!(matches!(error, ApplicationError::StaleDocument(_)));
}

fn summarize(
    result: Result<Option<TextUnitStreamSummary>, ApplicationError>,
) -> Result<Option<TextUnitStreamSummary>, String> {
    result.map_err(|error| error.to_string())
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

async fn parse_html() -> Document {
    HtmlParser
        .parse(resource("memory:blocks.html", "text/html", HTML))
        .await
        .expect("html should parse")
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

fn large_document(sentence_count: usize) -> Document {
    // Alpha-only sentences keep the fallback paragraph classifier from mistaking a run of consecutive
    // four-digit numbers for bibliographic metadata, which would make the paragraph coarse.
    let words = ["Alpha", "Beta", "Gamma", "Delta", "Epsilon"];
    let content = (0..sentence_count)
        .map(|index| format!("{} prose.", words[index % words.len()]))
        .collect::<Vec<_>>()
        .join(" ");
    document_with_content("doc:large-stream", &content)
}

fn document_with_content(id: &str, content: &str) -> Document {
    Document {
        id: DocumentId(id.into()),
        source: DocumentSource("memory:stream".into()),
        title: "Stream".into(),
        media_type: MediaType("text/plain".into()),
        content_hash: ContentHash(format!("sha256:{id}")),
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
