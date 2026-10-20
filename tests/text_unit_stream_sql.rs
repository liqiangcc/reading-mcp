use std::collections::BTreeMap;
use std::sync::Arc;

use reading_mcp::application::derived_text_units::DerivedTextUnits;
use reading_mcp::application::get_context::{
    ContextContainerKind, ContextRelation, ContextTarget, ContextUnit, GetContextUseCase,
    GetStructuredContextCommand,
};
use reading_mcp::application::get_text_units::{
    EffectiveTextUnitKind, GetTextUnitsCommand, GetTextUnitsUseCase, RequestedTextUnitKind,
    TextUnitCoveragePolicy, TextUnitDirection,
};
use reading_mcp::application::ports::{
    ApplicationError, DocumentRepository, Parser, RetrievedResource, SectionTextUnits,
    StoredStreamItem, TextUnitIndex, TextUnitStreamKey, TextUnitStreamSummary,
};
use reading_mcp::application::text_unit_store::store_text_units;
use reading_mcp::application::text_unit_stream::{
    ParagraphCoverageDesc, ParagraphDesc, SectionCoverageDesc, SentenceDesc, StreamEntryDesc,
    StreamUnit, UnitRef, plan_section_stream, plan_section_stream_desc,
};
use reading_mcp::domain::{
    ContentHash, Document, DocumentId, DocumentSource, Location, MediaType, ParagraphTextUnitSet,
    Section, SectionId, SentenceTextUnitSet, TextLocator,
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
        let sqlite_units = sqlite
            .section_units(&document.id, &section.id)
            .await
            .expect("sqlite section units should load");
        let memory_units = memory
            .section_units(&document.id, &section.id)
            .await
            .expect("memory section units should load");
        assert_eq!(
            sqlite_units, memory_units,
            "section units disagreement for section {}",
            section.id.0
        );
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
async fn replacing_the_unit_sets_replaces_the_section_text_and_layout_rows() {
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
    let text_rows: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM section_text_v1
             WHERE doc_key = (SELECT doc_key FROM text_unit_documents_v2 WHERE document_id = ?1)",
            rusqlite::params![&second.id.0],
            |row| row.get(0),
        )
        .expect("section text rows should count");
    let layout_rows: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM section_layout_v1
             WHERE doc_key = (SELECT doc_key FROM text_unit_documents_v2 WHERE document_id = ?1)",
            rusqlite::params![&second.id.0],
            |row| row.get(0),
        )
        .expect("section layout rows should count");

    // The replacement keeps one text row and one layout row for the single section, and the
    // sentence stream still has one paragraph and one sentence.
    assert_eq!(text_rows, 1);
    assert_eq!(layout_rows, 1);
    assert_eq!(total_items, 2);
}

#[tokio::test]
async fn replacing_the_unit_sets_leaves_the_legacy_table_empty_and_list_document_reads_v2() {
    let directory = tempdir().expect("temporary directory should be created");
    let database = directory.path().join("state.sqlite");
    let index = SqliteTextUnitIndex::open(&database).expect("index should open");
    let document = parse_markdown().await;
    store_text_units(&index, &document)
        .await
        .expect("units should persist");

    let connection = rusqlite::Connection::open(&database).expect("database should open");
    let legacy_rows: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM text_units WHERE document_id = ?1",
            rusqlite::params![&document.id.0],
            |row| row.get(0),
        )
        .expect("legacy rows should count");
    assert_eq!(legacy_rows, 0);

    let expected = document
        .try_paragraph_text_units()
        .expect("valid paragraph set")
        .units;
    assert_eq!(
        index
            .list_document(&document.id)
            .await
            .expect("list_document should read v2"),
        expected
    );
}

#[tokio::test]
async fn opening_the_index_drops_v1_tables_and_their_documents_are_missing() {
    let directory = tempdir().expect("temporary directory should be created");
    let database = directory.path().join("state.sqlite");
    {
        let connection = rusqlite::Connection::open(&database).expect("database should open");
        connection
            .execute_batch(
                "CREATE TABLE text_unit_sets_v1 (
                    document_id TEXT PRIMARY KEY,
                    segmentation_version TEXT NOT NULL
                 );
                 CREATE TABLE text_unit_sections_v1 (
                    document_id TEXT NOT NULL,
                    owner_section_id TEXT NOT NULL,
                    paragraph_coverage_json TEXT NOT NULL,
                    PRIMARY KEY (document_id, owner_section_id)
                 );
                 CREATE TABLE text_unit_items_v1 (
                    document_id TEXT NOT NULL,
                    owner_section_id TEXT NOT NULL,
                    kind TEXT NOT NULL,
                    source_order INTEGER NOT NULL,
                    unit_json TEXT NOT NULL,
                    PRIMARY KEY (document_id, kind, source_order)
                 );
                 CREATE INDEX idx_text_unit_items_v1_section
                    ON text_unit_items_v1(document_id, owner_section_id, kind, source_order);
                 CREATE TABLE text_unit_sentence_coverage_v1 (
                    document_id TEXT NOT NULL,
                    owner_section_id TEXT NOT NULL,
                    position INTEGER NOT NULL,
                    coverage_json TEXT NOT NULL,
                    PRIMARY KEY (document_id, owner_section_id, position)
                 );
                 CREATE TABLE text_unit_streams_v1 (
                    document_id TEXT NOT NULL,
                    owner_section_id TEXT NOT NULL,
                    requested_kind TEXT NOT NULL,
                    position INTEGER NOT NULL,
                    eligible_position INTEGER,
                    unit_kind TEXT NOT NULL,
                    unit_source_order INTEGER NOT NULL,
                    paragraph_index INTEGER NOT NULL,
                    sentence_index INTEGER,
                    content_class TEXT NOT NULL,
                    degradation TEXT,
                    char_count INTEGER NOT NULL,
                    PRIMARY KEY (document_id, owner_section_id, requested_kind, position)
                 );
                 CREATE TABLE text_unit_stream_summaries_v1 (
                    document_id TEXT NOT NULL,
                    owner_section_id TEXT NOT NULL,
                    requested_kind TEXT NOT NULL,
                    coverage_policy TEXT NOT NULL,
                    total_items INTEGER NOT NULL,
                    coverage_json TEXT NOT NULL,
                    PRIMARY KEY (document_id, owner_section_id, requested_kind, coverage_policy)
                 );
                 INSERT INTO text_unit_sets_v1(document_id, segmentation_version)
                    VALUES ('doc:v1', 'text-segmentation/v3');
                 INSERT INTO text_unit_sections_v1(
                    document_id, owner_section_id, paragraph_coverage_json
                 ) VALUES ('doc:v1', 'section://root', '{}');
                 INSERT INTO text_unit_stream_summaries_v1(
                    document_id, owner_section_id, requested_kind, coverage_policy,
                    total_items, coverage_json
                 ) VALUES ('doc:v1', 'section://root', 'paragraph', 'preserve_source', 0, '{}');",
            )
            .expect("v1 tables should be created");
    }

    let index = SqliteTextUnitIndex::open(&database).expect("index should open");
    let connection = rusqlite::Connection::open(&database).expect("database should open");
    for table in [
        "text_unit_sets_v1",
        "text_unit_sections_v1",
        "text_unit_items_v1",
        "text_unit_sentence_coverage_v1",
        "text_unit_streams_v1",
        "text_unit_stream_summaries_v1",
    ] {
        let count: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
                rusqlite::params![table],
                |row| row.get(0),
            )
            .expect("sqlite_master should be readable");
        assert_eq!(count, 0, "v1 table {table} should be dropped");
    }

    let document_id = DocumentId("doc:v1".into());
    let section_id = SectionId("section://root".into());
    let summary = index
        .section_stream_summary(TextUnitStreamKey {
            document_id: &document_id,
            section_id: &section_id,
            kind: RequestedTextUnitKind::Paragraph,
            policy: TextUnitCoveragePolicy::PreserveSource,
        })
        .await
        .expect("summary lookup should succeed");
    assert!(summary.is_none(), "v1-only documents must read as missing");
}

#[tokio::test]
async fn replacing_the_unit_sets_twice_keeps_one_set_of_rows_and_the_same_doc_key() {
    let directory = tempdir().expect("temporary directory should be created");
    let database = directory.path().join("state.sqlite");
    let index = SqliteTextUnitIndex::open(&database).expect("index should open");
    let document = parse_markdown().await;
    let tables = ["section_text_v1", "section_layout_v1"];

    store_text_units(&index, &document)
        .await
        .expect("first sets should persist");
    let connection = rusqlite::Connection::open(&database).expect("database should open");
    let doc_key: i64 = connection
        .query_row(
            "SELECT doc_key FROM text_unit_documents_v2 WHERE document_id = ?1",
            rusqlite::params![&document.id.0],
            |row| row.get(0),
        )
        .expect("doc_key should exist");
    let mut first_counts = Vec::new();
    for table in tables {
        let count: i64 = connection
            .query_row(
                &format!("SELECT COUNT(*) FROM {table} WHERE doc_key = ?1"),
                rusqlite::params![doc_key],
                |row| row.get(0),
            )
            .expect("rows should count");
        assert!(count > 0, "{table} should have rows after the first write");
        first_counts.push(count);
    }
    drop(connection);

    store_text_units(&index, &document)
        .await
        .expect("second sets should replace the first");
    let connection = rusqlite::Connection::open(&database).expect("database should open");
    let doc_key_after: i64 = connection
        .query_row(
            "SELECT doc_key FROM text_unit_documents_v2 WHERE document_id = ?1",
            rusqlite::params![&document.id.0],
            |row| row.get(0),
        )
        .expect("doc_key should still exist");
    assert_eq!(doc_key_after, doc_key, "doc_key must be stable");
    for (table, expected) in tables.iter().zip(first_counts) {
        let count: i64 = connection
            .query_row(
                &format!("SELECT COUNT(*) FROM {table} WHERE doc_key = ?1"),
                rusqlite::params![doc_key],
                |row| row.get(0),
            )
            .expect("rows should count");
        assert_eq!(
            count, expected,
            "{table} should keep exactly one set of rows"
        );
    }
}

#[tokio::test]
async fn missing_section_layout_is_reported_as_stale() {
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
            "DELETE FROM section_layout_v1
             WHERE doc_key = (SELECT doc_key FROM text_unit_documents_v2 WHERE document_id = ?1)",
            rusqlite::params![&document.id.0],
        )
        .expect("section layout rows should be deleted");

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

#[tokio::test]
async fn sqlite_section_unit_matches_memory_and_derived_for_every_unit() {
    let directory = tempdir().expect("temporary directory should be created");
    let database = directory.path().join("state.sqlite");
    let sqlite = SqliteTextUnitIndex::open(&database).expect("sqlite index should open");
    let memory = InMemoryTextUnitIndex::default();

    for document in [
        parse_plain().await,
        parse_markdown().await,
        parse_html().await,
    ] {
        store_text_units(&sqlite, &document)
            .await
            .expect("sqlite units should persist");
        store_text_units(&memory, &document)
            .await
            .expect("memory units should persist");
        let derived = DerivedTextUnits::new(Arc::new(document.clone()));
        let paragraphs = document
            .try_paragraph_text_units()
            .expect("valid paragraph set");
        let sentences = document
            .try_sentence_text_units()
            .expect("valid sentence set");

        for section in all_sections(&document) {
            let units = SectionTextUnits::from_sets(&paragraphs, &sentences, &section.id);
            for paragraph in &units.paragraphs {
                let probe = (
                    EffectiveTextUnitKind::Paragraph,
                    paragraph.paragraph_index,
                    None,
                );
                let expected = memory
                    .section_unit(&document.id, &section.id, probe.0, probe.1, probe.2)
                    .await
                    .expect("memory lookup should succeed");
                assert_eq!(
                    sqlite
                        .section_unit(&document.id, &section.id, probe.0, probe.1, probe.2)
                        .await
                        .expect("sqlite lookup should succeed"),
                    expected,
                    "paragraph lookup disagreement for {}/{}",
                    section.id.0,
                    paragraph.paragraph_index
                );
                assert_eq!(
                    derived
                        .section_unit(&document.id, &section.id, probe.0, probe.1, probe.2)
                        .await
                        .expect("derived lookup should succeed"),
                    expected,
                    "derived paragraph lookup disagreement for {}/{}",
                    section.id.0,
                    paragraph.paragraph_index
                );
            }
            for sentence in &units.sentences {
                let probe = (
                    EffectiveTextUnitKind::Sentence,
                    sentence.paragraph_index,
                    Some(sentence.sentence_index),
                );
                let expected = memory
                    .section_unit(&document.id, &section.id, probe.0, probe.1, probe.2)
                    .await
                    .expect("memory lookup should succeed");
                assert_eq!(
                    sqlite
                        .section_unit(&document.id, &section.id, probe.0, probe.1, probe.2)
                        .await
                        .expect("sqlite lookup should succeed"),
                    expected,
                    "sentence lookup disagreement for {}/{}.{}",
                    section.id.0,
                    sentence.paragraph_index,
                    sentence.sentence_index
                );
                assert_eq!(
                    derived
                        .section_unit(&document.id, &section.id, probe.0, probe.1, probe.2)
                        .await
                        .expect("derived lookup should succeed"),
                    expected,
                    "derived sentence lookup disagreement for {}/{}.{}",
                    section.id.0,
                    sentence.paragraph_index,
                    sentence.sentence_index
                );
            }
            let missing = (
                EffectiveTextUnitKind::Paragraph,
                units.paragraphs.len() + 1,
                None,
            );
            let indexes: [&dyn TextUnitIndex; 3] = [&sqlite, &memory, &derived];
            for index in indexes {
                assert_eq!(
                    index
                        .section_unit(&document.id, &section.id, missing.0, missing.1, missing.2)
                        .await
                        .expect("missing paragraph lookup should succeed"),
                    None,
                    "missing paragraph must be absent in section {}",
                    section.id.0
                );
            }
            for index in indexes {
                assert_eq!(
                    index
                        .section_unit(
                            &document.id,
                            &section.id,
                            EffectiveTextUnitKind::Sentence,
                            1,
                            Some(99)
                        )
                        .await
                        .expect("missing sentence lookup should succeed"),
                    None,
                    "missing sentence must be absent in section {}",
                    section.id.0
                );
            }
        }
    }
}

#[tokio::test]
async fn get_context_matches_derived_mode_for_every_anchor_and_window() {
    let directory = tempdir().expect("temporary directory should be created");
    let database = directory.path().join("state.sqlite");
    let repository: Arc<dyn DocumentRepository> =
        Arc::new(SqliteDocumentRepository::open(&database).expect("repository should open"));
    let index: Arc<dyn TextUnitIndex> =
        Arc::new(SqliteTextUnitIndex::open(&database).expect("index should open"));

    for document in [
        parse_plain().await,
        parse_markdown().await,
        parse_html().await,
    ] {
        repository
            .save(document.clone())
            .await
            .expect("document should persist");
        store_text_units(&*index, &document)
            .await
            .expect("units should persist");
        let derived: Arc<dyn TextUnitIndex> =
            Arc::new(DerivedTextUnits::new(Arc::new(document.clone())));
        let stored_context =
            GetContextUseCase::new(repository.clone()).with_text_unit_index(Arc::clone(&index));
        let derived_context =
            GetContextUseCase::new(repository.clone()).with_text_unit_index(derived);
        let paragraphs = document
            .try_paragraph_text_units()
            .expect("valid paragraph set");
        let sentences = document
            .try_sentence_text_units()
            .expect("valid sentence set");

        for section in all_sections(&document) {
            let units = SectionTextUnits::from_sets(&paragraphs, &sentences, &section.id);
            let mut anchors = Vec::new();
            for paragraph in &units.paragraphs {
                anchors.push((
                    ContextUnit::Paragraph,
                    TextLocator::for_paragraph(&document, &section, paragraph),
                ));
            }
            for sentence in &units.sentences {
                anchors.push((
                    ContextUnit::Sentence,
                    TextLocator::for_sentence(&document, &section, sentence),
                ));
            }

            for (unit, locator) in &anchors {
                for before in [0usize, 1, 2] {
                    for after in [0usize, 1, 2] {
                        let command = GetStructuredContextCommand {
                            document_id: document.id.clone(),
                            target: ContextTarget::Locator(locator.clone()),
                            relation: ContextRelation::Neighbor {
                                unit: *unit,
                                before,
                                after,
                            },
                            max_chars: None,
                        };
                        let stored = stored_context
                            .execute_structured(command.clone())
                            .await
                            .expect("stored context should succeed");
                        let derived = derived_context
                            .execute_structured(command)
                            .await
                            .expect("derived context should succeed");
                        assert_eq!(
                            stored, derived,
                            "neighbor disagreement for {}/{:?} before {before} after {after}",
                            section.id.0, unit
                        );
                    }
                }
            }

            for (_, locator) in &anchors {
                let command = GetStructuredContextCommand {
                    document_id: document.id.clone(),
                    target: ContextTarget::Locator(locator.clone()),
                    relation: ContextRelation::Container {
                        kind: ContextContainerKind::Paragraph,
                    },
                    max_chars: None,
                };
                let stored = stored_context
                    .execute_structured(command.clone())
                    .await
                    .expect("stored paragraph container should succeed");
                let derived = derived_context
                    .execute_structured(command)
                    .await
                    .expect("derived paragraph container should succeed");
                assert_eq!(
                    stored,
                    derived,
                    "paragraph container disagreement for {}/{}",
                    section.id.0,
                    locator.paragraph_index.unwrap_or(0)
                );
            }
        }
    }
}

#[tokio::test]
async fn stored_context_reports_stale_instead_of_deriving_after_stream_rows_are_deleted() {
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

    let connection = rusqlite::Connection::open(&database).expect("database should open");
    for table in ["section_layout_v1"] {
        connection
            .execute(
                &format!(
                    "DELETE FROM {table}
                     WHERE doc_key = (SELECT doc_key FROM text_unit_documents_v2 WHERE document_id = ?1)"
                ),
                rusqlite::params![&document.id.0],
            )
            .expect("section layout rows should be deleted");
    }

    let section = document.root_sections[0].clone();
    let paragraph = document
        .try_paragraph_text_units()
        .expect("valid paragraph set")
        .units
        .into_iter()
        .find(|unit| unit.owner_section_id == section.id)
        .expect("paragraph should exist");
    let error = GetContextUseCase::new(repository)
        .with_text_unit_index(index)
        .execute_structured(GetStructuredContextCommand {
            document_id: document.id.clone(),
            target: ContextTarget::Locator(TextLocator::for_paragraph(
                &document, &section, &paragraph,
            )),
            relation: ContextRelation::Neighbor {
                unit: ContextUnit::Paragraph,
                before: 2,
                after: 2,
            },
            max_chars: None,
        })
        .await
        .expect_err("missing stored units must fail closed");
    assert!(matches!(error, ApplicationError::StaleDocument(_)));
}

#[tokio::test]
async fn desc_planner_with_range_lengths_equals_text_planner() {
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

        for section in all_sections(&document) {
            let section_paragraphs = paragraphs
                .units
                .iter()
                .filter(|unit| unit.owner_section_id == section.id)
                .collect::<Vec<_>>();
            let section_sentences = sentences
                .units
                .iter()
                .filter(|unit| unit.owner_section_id == section.id)
                .collect::<Vec<_>>();
            let paragraph_coverage = paragraphs
                .coverage
                .iter()
                .find(|coverage| coverage.owner_section_id == section.id);
            let sentence_coverage = sentences
                .coverage
                .iter()
                .filter(|coverage| coverage.owner_section_id == section.id)
                .collect::<Vec<_>>();

            for kind in [
                RequestedTextUnitKind::Paragraph,
                RequestedTextUnitKind::Sentence,
            ] {
                let text_plan = plan_section_stream(
                    &section.id,
                    &section_paragraphs,
                    paragraph_coverage,
                    &section_sentences,
                    &sentence_coverage,
                    kind,
                )
                .expect("text planner should plan");

                let paragraph_descs = section_paragraphs
                    .iter()
                    .map(|unit| ParagraphDesc {
                        paragraph_index: unit.paragraph_index,
                        char_count: unit.normalized_range.len(),
                    })
                    .collect::<Vec<_>>();
                let sentence_descs = section_sentences
                    .iter()
                    .map(|unit| SentenceDesc {
                        paragraph_index: unit.paragraph_index,
                        sentence_index: unit.sentence_index,
                        char_count: unit.normalized_range.len(),
                    })
                    .collect::<Vec<_>>();
                let coverage_descs = sentence_coverage
                    .iter()
                    .map(|coverage| ParagraphCoverageDesc {
                        paragraph_index: coverage.paragraph_index,
                        content_class: coverage.content_class,
                        eligibility: coverage.eligibility,
                        separator_chars: coverage.separator_chars,
                    })
                    .collect::<Vec<_>>();
                let section_coverage_desc =
                    paragraph_coverage.map(|coverage| SectionCoverageDesc {
                        owner_chars: coverage.owner_chars,
                        separator_chars: coverage.separator_chars,
                        paragraph_count: coverage.paragraph_count,
                    });

                let desc_plan = plan_section_stream_desc(
                    &section.id,
                    &paragraph_descs,
                    section_coverage_desc.as_ref(),
                    &sentence_descs,
                    &coverage_descs,
                    kind,
                )
                .expect("descriptor planner should plan");

                let text_entries = text_plan
                    .entries
                    .iter()
                    .map(|entry| StreamEntryDesc {
                        unit: match entry.unit {
                            StreamUnit::Paragraph(unit) => UnitRef::Paragraph(
                                section_paragraphs
                                    .iter()
                                    .position(|candidate| std::ptr::eq(*candidate, unit))
                                    .expect("paragraph should come from the section slice"),
                            ),
                            StreamUnit::Sentence(unit) => UnitRef::Sentence(
                                section_sentences
                                    .iter()
                                    .position(|candidate| std::ptr::eq(*candidate, unit))
                                    .expect("sentence should come from the section slice"),
                            ),
                        },
                        content_class: entry.content_class,
                        degradation: entry.degradation,
                        char_count: entry.char_count,
                        eligible: entry.eligible,
                    })
                    .collect::<Vec<_>>();

                assert_eq!(
                    text_entries, desc_plan.entries,
                    "entries disagree for section {}/{:?}",
                    section.id.0, kind
                );
                assert_eq!(
                    text_plan.preserve_source, desc_plan.preserve_source,
                    "preserve coverage disagrees for section {}/{:?}",
                    section.id.0, kind
                );
                assert_eq!(
                    text_plan.eligible_only, desc_plan.eligible_only,
                    "eligible coverage disagrees for section {}/{:?}",
                    section.id.0, kind
                );
            }
        }
    }
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

#[tokio::test]
async fn opening_the_index_drops_v2_tables_and_their_documents_are_missing() {
    let directory = tempdir().expect("temporary directory should be created");
    let database = directory.path().join("state.sqlite");
    {
        let connection = rusqlite::Connection::open(&database).expect("database should open");
        connection
            .execute_batch(
                "CREATE TABLE text_unit_documents_v2 (
                    doc_key INTEGER PRIMARY KEY,
                    document_id TEXT NOT NULL UNIQUE,
                    content_hash TEXT NOT NULL,
                    normalized_document_hash TEXT NOT NULL,
                    segmentation_version TEXT NOT NULL
                 );
                 CREATE TABLE text_unit_sections_v2 (
                    doc_key INTEGER NOT NULL,
                    section_key INTEGER NOT NULL,
                    section_id TEXT NOT NULL,
                    paragraph_coverage_json TEXT,
                    PRIMARY KEY (doc_key, section_key)
                 ) WITHOUT ROWID;
                 CREATE TABLE text_unit_items_v2 (
                    doc_key INTEGER NOT NULL,
                    kind INTEGER NOT NULL,
                    source_order INTEGER NOT NULL,
                    section_key INTEGER NOT NULL,
                    paragraph_index INTEGER NOT NULL,
                    sentence_index INTEGER NOT NULL,
                    range_start INTEGER NOT NULL,
                    range_end INTEGER NOT NULL,
                    parent_start INTEGER,
                    parent_end INTEGER,
                    text TEXT NOT NULL,
                    PRIMARY KEY (doc_key, kind, source_order)
                 ) WITHOUT ROWID;
                 CREATE TABLE text_unit_sentence_coverage_v2 (
                    doc_key INTEGER NOT NULL,
                    section_key INTEGER NOT NULL,
                    position INTEGER NOT NULL,
                    paragraph_index INTEGER NOT NULL,
                    paragraph_start INTEGER NOT NULL,
                    paragraph_end INTEGER NOT NULL,
                    content_class INTEGER NOT NULL,
                    eligibility INTEGER NOT NULL,
                    paragraph_chars INTEGER NOT NULL,
                    sentence_chars INTEGER NOT NULL,
                    separator_chars INTEGER NOT NULL,
                    coarse_only_chars INTEGER NOT NULL,
                    sentence_count INTEGER NOT NULL,
                    PRIMARY KEY (doc_key, section_key, position)
                 ) WITHOUT ROWID;
                 CREATE TABLE text_unit_streams_v2 (
                    doc_key INTEGER NOT NULL,
                    section_key INTEGER NOT NULL,
                    requested_kind INTEGER NOT NULL,
                    position INTEGER NOT NULL,
                    eligible_position INTEGER,
                    unit_kind INTEGER NOT NULL,
                    unit_source_order INTEGER NOT NULL,
                    content_class INTEGER NOT NULL,
                    degradation INTEGER,
                    char_count INTEGER NOT NULL,
                    PRIMARY KEY (doc_key, section_key, requested_kind, position)
                 ) WITHOUT ROWID;
                 CREATE TABLE text_unit_stream_summaries_v2 (
                    doc_key INTEGER NOT NULL,
                    section_key INTEGER NOT NULL,
                    requested_kind INTEGER NOT NULL,
                    coverage_policy INTEGER NOT NULL,
                    total_items INTEGER NOT NULL,
                    coverage_json TEXT NOT NULL,
                    PRIMARY KEY (doc_key, section_key, requested_kind, coverage_policy)
                 ) WITHOUT ROWID;
                 INSERT INTO text_unit_documents_v2(
                    doc_key, document_id, content_hash, normalized_document_hash,
                    segmentation_version
                 ) VALUES (1, 'doc:v2', 'sha256:content', 'sha256:normalized', 'text-segmentation/v3');
                 INSERT INTO text_unit_sections_v2(
                    doc_key, section_key, section_id, paragraph_coverage_json
                 ) VALUES (1, 0, 'section://root', '{}');",
            )
            .expect("v2 tables should be created");
    }

    let index = SqliteTextUnitIndex::open(&database).expect("index should open");
    let connection = rusqlite::Connection::open(&database).expect("database should open");
    for table in [
        "text_unit_sections_v2",
        "text_unit_items_v2",
        "text_unit_sentence_coverage_v2",
        "text_unit_streams_v2",
        "text_unit_stream_summaries_v2",
    ] {
        let count: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
                rusqlite::params![table],
                |row| row.get(0),
            )
            .expect("sqlite_master should be readable");
        assert_eq!(count, 0, "v2 table {table} should be dropped");
    }

    let document_id = DocumentId("doc:v2".into());
    let section_id = SectionId("section://root".into());
    let summary = index
        .section_stream_summary(TextUnitStreamKey {
            document_id: &document_id,
            section_id: &section_id,
            kind: RequestedTextUnitKind::Paragraph,
            policy: TextUnitCoveragePolicy::PreserveSource,
        })
        .await
        .expect("summary lookup should succeed");
    assert!(summary.is_none(), "v2-only documents must read as missing");
}
