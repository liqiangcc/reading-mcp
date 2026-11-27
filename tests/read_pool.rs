//! The read path's read-only connections (`READING_MCP_READ_POOL`).
//!
//! A store keeps one writer connection behind one mutex, and `run_sqlite` holds that mutex for the
//! whole closure, so an ingest write or a whole-book scan used to stall every read. The read path
//! now reads on a small pool of `query_only` connections instead. These tests hold the two ends of
//! that change:
//!
//! - every read-path answer is identical with the pool and with `READING_MCP_READ_POOL=0`, which
//!   runs every operation on the writer connection as before;
//! - a read on another document completes while a long write holds the writer, and with the pool
//!   off it does not;
//! - a pooled read never writes, even for a row that has no stored normalized hash.
//!
//! A read takes a pooled connection only when the writer is busy (an idle writer serves it, with no
//! connection to open), so the tests that mean to exercise the pool hold the writer with a write on
//! another document.

use std::sync::Arc;

use reading_mcp::application::cite_reading::CiteReadingUseCase;
use reading_mcp::application::get_text_units::{RequestedTextUnitKind, TextUnitDirection};
use reading_mcp::application::outline_reading::{OutlineReadingCommand, OutlineReadingUseCase};
use reading_mcp::application::ports::{
    DocumentRepository, HandleRemapUnit, OpenResultStore, Parser, RetrievedResource, SearchIndex,
    TextUnitIndex,
};
use reading_mcp::application::read_reading::{ReadReadingCommand, ReadReadingUseCase};
use reading_mcp::application::search_document::{SearchDocumentCommand, SearchDocumentUseCase};
use reading_mcp::application::short_handle::{DocumentHandle, SectionHandle, handle_checksum};
use reading_mcp::domain::{
    Document, DocumentSegmentation, DocumentSource, MediaType, NormalizedTextRange, Section,
};
use reading_mcp::infrastructure::{
    SqliteDocumentRepository, SqliteOpenResultStore, SqliteSearchIndex, SqliteTextUnitIndex,
};
use reading_mcp::parsing::MarkdownParser;

/// One state database with the given documents stored, opened with `read_pool` read connections.
struct Store {
    _directory: tempfile::TempDir,
    repository: Arc<SqliteDocumentRepository>,
    units: Arc<SqliteTextUnitIndex>,
    open_results: Arc<SqliteOpenResultStore>,
    index: Arc<SqliteSearchIndex>,
}

impl Store {
    fn document_repository(&self) -> Arc<dyn DocumentRepository> {
        Arc::clone(&self.repository) as Arc<dyn DocumentRepository>
    }

    fn text_units(&self) -> Arc<dyn TextUnitIndex> {
        Arc::clone(&self.units) as Arc<dyn TextUnitIndex>
    }
}

/// Stores `documents` the way `open_document` does: the section text first, then the document row.
async fn store(read_pool: usize, documents: &[Document]) -> Store {
    let directory = tempfile::tempdir().expect("temp directory");
    let database = directory.path().join("read-pool.sqlite");
    let repository = Arc::new(
        SqliteDocumentRepository::open_with_read_pool(&database, read_pool)
            .expect("repository should open"),
    );
    let units = Arc::new(
        SqliteTextUnitIndex::open_with_read_pool(&database, read_pool)
            .expect("text unit index should open"),
    );
    let open_results = Arc::new(
        SqliteOpenResultStore::open_with_read_pool(&database, read_pool)
            .expect("open result store should open"),
    );
    let index = Arc::new(SqliteSearchIndex::new(
        Arc::clone(&repository) as Arc<dyn DocumentRepository>,
        Arc::clone(&units),
    ));
    for document in documents {
        index
            .index(document)
            .await
            .expect("section text should store");
        repository
            .save(document.clone())
            .await
            .expect("document should save");
    }
    Store {
        _directory: directory,
        repository,
        units,
        open_results,
        index,
    }
}

// ------------------------------------------------------------------------------------------------
// The same answers with the pool and without it
// ------------------------------------------------------------------------------------------------

#[tokio::test]
async fn the_read_path_answers_identically_with_the_pool_and_on_the_writer_alone() {
    let document = fixture_document().await;
    let pooled = store(4, std::slice::from_ref(&document)).await;
    // 0 is `READING_MCP_READ_POOL=0`: every operation on the writer connection, as before the pool.
    let writer_only = store(0, std::slice::from_ref(&document)).await;

    // The reads that never need the writer answer the same while another document's write holds
    // it, so every one of them runs on a pooled connection.
    let expected = read_answers(&writer_only, &document).await;
    let write = hold_the_writer(&pooled, 6_000).await;
    let actual = read_answers(&pooled, &document).await;
    assert!(
        !write.is_finished(),
        "the reads must run while the write holds the writer, so they run on the pool"
    );
    write
        .await
        .expect("the write task should not panic")
        .expect("the write should not fail");
    assert_same_answers(&actual, &expected);

    // The calls that do walk the writer — the full decode and the whole-book scan — answer the same
    // with the writer idle.
    assert_same_answers(
        &writer_answers(&pooled, &document).await,
        &writer_answers(&writer_only, &document).await,
    );
}

fn assert_same_answers(actual: &[String], expected: &[String]) {
    assert_eq!(
        actual.len(),
        expected.len(),
        "the two stores answered a different number of reads"
    );
    for (label, (actual, expected)) in actual.iter().zip(expected).enumerate() {
        assert_eq!(actual, expected, "answer {label} differs");
    }
    assert!(
        expected.iter().all(|answer| !answer.contains("Err")),
        "no read may fail: {expected:?}"
    );
}

/// Every read-path call of the read, outline and cite use cases, plus the point readers they share,
/// as one list of answers in a fixed order. None of these needs the writer connection, so all of
/// them run on the read pool while a write holds the writer.
async fn read_answers(store: &Store, document: &Document) -> Vec<String> {
    let repository = store.document_repository();
    let units = store.text_units();
    let id = &document.id;
    let handle = document_handle(store, document).await;
    let section = section_handle(store, document, "第一章").await;
    let chapter = &section_by_title(document, "第一章").id;
    let mut out = Vec::new();

    // The read path: a sentence step, a batch, a backward walk, the document handle, paragraphs.
    let read = ReadReadingUseCase::new(Arc::clone(&repository), Arc::clone(&units));
    for command in [
        ReadReadingCommand {
            from: section.to_string(),
            unit: RequestedTextUnitKind::Sentence,
            max_chars: None,
            direction: None,
            back: 0,
        },
        ReadReadingCommand {
            from: section.to_string(),
            unit: RequestedTextUnitKind::Sentence,
            max_chars: Some(64),
            direction: None,
            back: 0,
        },
        ReadReadingCommand {
            from: section.to_string(),
            unit: RequestedTextUnitKind::Sentence,
            max_chars: None,
            direction: Some(TextUnitDirection::Backward),
            back: 2,
        },
        ReadReadingCommand {
            from: handle.to_string(),
            unit: RequestedTextUnitKind::Sentence,
            max_chars: None,
            direction: None,
            back: 0,
        },
        ReadReadingCommand {
            from: section.to_string(),
            unit: RequestedTextUnitKind::Paragraph,
            max_chars: None,
            direction: None,
            back: 0,
        },
    ] {
        out.push(format!("read {:?}", read.execute(command).await));
    }

    let outline = OutlineReadingUseCase::new(Arc::clone(&repository), Arc::clone(&units));
    for (at, depth) in [(handle.to_string(), 1), (handle.to_string(), 3)] {
        out.push(format!(
            "outline {:?}",
            outline
                .execute(OutlineReadingCommand {
                    at,
                    depth,
                    limit: 100,
                    cursor: None,
                })
                .await
        ));
    }

    let cite = CiteReadingUseCase::new(Arc::clone(&repository), Arc::clone(&units));
    out.push(format!(
        "cite {:?}",
        cite.execute(&[section.to_string(), handle.to_string()])
            .await
    ));

    // The point readers the use cases share, called directly so a difference names itself.
    out.push(format!(
        "document_header {:?}",
        repository.document_header(id).await
    ));
    out.push(format!(
        "get_light_with_hash {:?}",
        repository.get_light_with_hash(id).await
    ));
    out.push(format!(
        "load_metadata {:?}",
        repository.load_metadata(id, "missing-key").await
    ));
    out.push(format!(
        "load_section_text {:?}",
        repository.load_section_text(id, chapter).await
    ));
    out.push(format!(
        "load_section_char_len {:?}",
        repository.load_section_char_len(id, chapter).await
    ));
    out.push(format!(
        "original_source_page_at {:?}",
        repository.original_source_page_at(id, chapter, 0).await
    ));
    out.push(format!(
        "original_source_page_for_range {:?}",
        repository
            .original_source_page_for_range(
                id,
                chapter,
                NormalizedTextRange::new(0, 4).expect("range"),
            )
            .await
    ));
    out.push(format!(
        "original_source_bindings_stale {:?}",
        repository.original_source_bindings_stale(id).await
    ));
    out.push(format!("body_order {:?}", repository.body_order(id).await));
    out.push(format!(
        "body_order_keys {:?}",
        repository.body_order_keys(id).await
    ));
    let len = repository
        .body_order_len(id)
        .await
        .expect("body order length")
        .expect("stored body order");
    out.push(format!("body_order_len {len}"));
    for ord in [0, len / 2, len - 1] {
        out.push(format!(
            "body_order_at {ord} {:?}",
            repository.body_order_at(id, ord).await
        ));
    }
    let middle = repository
        .body_order_at(id, len / 2)
        .await
        .expect("body order at")
        .expect("stored position");
    out.push(format!(
        "body_order_window {:?}",
        repository.body_order_window(id, middle, 1, 1).await
    ));
    out.push(format!(
        "body_order_position {:?}",
        repository.body_order_position(id, middle).await
    ));
    out.push(format!(
        "section_by_id {:?}",
        repository.section_by_id(id, chapter).await
    ));
    out.push(format!(
        "section_by_key {:?}",
        repository.section_by_key(id, 0).await
    ));
    out.push(format!(
        "sections_in_key_range {:?}",
        repository.sections_in_key_range(id, 0, 100, 50).await
    ));
    out.push(format!(
        "section_children none {:?}",
        repository.section_children(id, None, 100).await
    ));
    out.push(format!(
        "section_children some {:?}",
        repository.section_children(id, Some(0), 100).await
    ));
    out.push(format!(
        "section_ancestors {:?}",
        repository
            .section_ancestors(id, i64::try_from(len - 1).expect("key"))
            .await
    ));

    // The text-unit readers: the key lookups and the stored units and streams.
    out.push(format!("document_key {:?}", units.document_key(id).await));
    let doc_key = units
        .document_key(id)
        .await
        .expect("document key")
        .expect("stored document");
    out.push(format!(
        "document_id_for_key {:?}",
        units.document_id_for_key(doc_key).await
    ));
    out.push(format!(
        "document_key_row {:?}",
        units.document_key_row(doc_key).await
    ));
    let key = units
        .section_key(id, chapter)
        .await
        .expect("section key")
        .expect("stored section");
    out.push(format!(
        "section_id_for_key {:?}",
        units.section_id_for_key(id, key).await
    ));
    out.push(format!(
        "section_units {:?}",
        units.section_units(id, chapter).await
    ));
    out.push(format!(
        "section_unit {:?}",
        units
            .section_unit(
                id,
                chapter,
                reading_mcp::application::get_text_units::EffectiveTextUnitKind::Paragraph,
                0,
                None,
            )
            .await
    ));
    let checksum = handle.checksum.clone();
    out.push(format!(
        "handle_remap {:?}",
        units
            .handle_remap(doc_key, &checksum, &HandleRemapUnit::Document)
            .await
    ));
    out.push(format!(
        "handle_remap_at_stream_position {:?}",
        units
            .handle_remap_at_stream_position(
                doc_key,
                &checksum,
                key,
                RequestedTextUnitKind::Sentence,
                1,
            )
            .await
    ));

    // The open-result cache read.
    out.push(format!(
        "open_result {:?}",
        store.open_results.get(id, "no-such-fingerprint").await
    ));

    out
}

/// The three reads that run on the writer connection whatever the pool says: the full decode (which
/// stores the normalized hash) and the whole-book scan behind a search.
async fn writer_answers(store: &Store, document: &Document) -> Vec<String> {
    let repository = store.document_repository();
    let units = store.text_units();
    let id = &document.id;
    let search_index = Arc::clone(&store.index) as Arc<dyn SearchIndex>;
    let search = SearchDocumentUseCase::new(search_index, Arc::clone(&repository))
        .with_text_unit_index(Arc::clone(&units));
    vec![
        format!(
            "search {:?}",
            search
                .execute(SearchDocumentCommand {
                    document_id: id.clone(),
                    query: "皇帝".into(),
                    limit: 10,
                })
                .await
        ),
        format!("get_with_hash {:?}", repository.get_with_hash(id).await),
        format!("get {:?}", repository.get(id).await),
    ]
}

async fn document_handle(store: &Store, document: &Document) -> DocumentHandle {
    DocumentHandle {
        doc_key: store
            .units
            .document_key(&document.id)
            .await
            .expect("document key")
            .expect("stored document"),
        checksum: handle_checksum(
            &document.id,
            &document.normalized_document_hash(),
            &document.media_type,
        ),
    }
}

async fn section_handle(store: &Store, document: &Document, title: &str) -> SectionHandle {
    SectionHandle {
        document: document_handle(store, document).await,
        section_key: store
            .units
            .section_key(&document.id, &section_by_title(document, title).id)
            .await
            .expect("section key")
            .expect("stored section"),
    }
}

fn section_by_title<'a>(document: &'a Document, title: &str) -> &'a Section {
    fn walk<'a>(sections: &'a [Section], title: &str) -> Option<&'a Section> {
        for section in sections {
            if section.title == title {
                return Some(section);
            }
            if let Some(found) = walk(&section.children, title) {
                return Some(found);
            }
        }
        None
    }
    walk(&document.root_sections, title)
        .unwrap_or_else(|| panic!("the fixture has no section titled {title}"))
}

// ------------------------------------------------------------------------------------------------
// A read does not wait for a write
// ------------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_read_on_another_document_completes_while_a_long_write_holds_the_writer() {
    let small = fixture_document().await;
    let other = small.id.clone();
    // The whole-book ingest write of a large book, built before the clock starts.
    let big = Arc::new(large_document(3_000).await);
    let segmentation = Arc::new(big.try_segmentation().expect("the fixture should segment"));

    // With the pool, the read finishes before the write does: it never asks for the writer's mutex.
    let pooled = store(4, std::slice::from_ref(&small)).await;
    let mut write = tokio::spawn({
        let units = Arc::clone(&pooled.units);
        let big = Arc::clone(&big);
        let segmentation = Arc::clone(&segmentation);
        async move {
            units
                .replace_document_segmentation(&big, segmentation)
                .await
        }
    });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert!(
        !write.is_finished(),
        "the synthetic write finished within 50 ms; make it larger so the test can order the two"
    );
    let read = tokio::select! {
        read = pooled.units.document_key(&other) => read,
        _ = &mut write => panic!("the read waited for the whole write with a read pool"),
    };
    assert!(
        !write.is_finished(),
        "the read finished only after the write: it waited for the writer"
    );
    assert_eq!(
        read.expect("the read should not fail"),
        Some(1),
        "the read answered from another document"
    );
    write
        .await
        .expect("the write task should not panic")
        .expect("the write should not fail");

    // With `READING_MCP_READ_POOL=0` the same read waits for the writer: it can only finish after
    // the write released the mutex, so its duration covers the rest of the write.
    let writer_only = store(0, std::slice::from_ref(&small)).await;
    let write_started = std::time::Instant::now();
    let write = tokio::spawn({
        let units = Arc::clone(&writer_only.units);
        let big = Arc::clone(&big);
        let segmentation = Arc::clone(&segmentation);
        async move {
            units
                .replace_document_segmentation(&big, segmentation)
                .await
        }
    });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert!(
        !write.is_finished(),
        "the synthetic write finished within 50 ms; make it larger so the test can order the two"
    );
    let read_started = std::time::Instant::now();
    let read = writer_only
        .units
        .document_key(&other)
        .await
        .expect("the read should not fail");
    let read_duration = read_started.elapsed();
    let write_duration = write_started.elapsed();
    write
        .await
        .expect("the write task should not panic")
        .expect("the write should not fail");
    assert_eq!(read, Some(1), "the read answers after the write");
    assert!(
        read_duration + std::time::Duration::from_millis(100) >= write_duration,
        "a read with the pool off waits for the write: the read took {read_duration:?} of the \
         write's {write_duration:?}"
    );
}

// ------------------------------------------------------------------------------------------------
// A pooled read never writes
// ------------------------------------------------------------------------------------------------

#[tokio::test]
async fn a_legacy_row_without_a_stored_hash_reads_without_writing_it() {
    let document = fixture_document().await;
    let directory = tempfile::tempdir().expect("temp directory");

    // The row shape a binary before the normalized-hash column wrote: no column at all.
    let seed_database = directory.path().join("seed.sqlite");
    let seed = SqliteDocumentRepository::open_with_read_pool(&seed_database, 4).expect("seed");
    seed.save(document.clone()).await.expect("seed should save");
    let stored_json: String = rusqlite::Connection::open(&seed_database)
        .expect("seed should open")
        .query_row(
            "SELECT document_json FROM documents WHERE id = ?1",
            rusqlite::params![&document.id.0],
            |row| row.get(0),
        )
        .expect("seed document_json should read");
    drop(seed);

    let database = directory.path().join("legacy.sqlite");
    let legacy = rusqlite::Connection::open(&database).expect("legacy should open");
    legacy
        .execute_batch(
            "CREATE TABLE documents (
                id TEXT PRIMARY KEY,
                source TEXT NOT NULL,
                content_hash TEXT NOT NULL,
                document_json TEXT NOT NULL
             );",
        )
        .expect("legacy schema should be created");
    legacy
        .execute(
            "INSERT INTO documents(id, source, content_hash, document_json) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![
                &document.id.0,
                &document.source.0,
                &document.content_hash.0,
                &stored_json
            ],
        )
        .expect("legacy row should be written");
    drop(legacy);

    let repository =
        SqliteDocumentRepository::open_with_read_pool(&database, 4).expect("legacy should open");
    let units = Arc::new(
        SqliteTextUnitIndex::open_with_read_pool(&database, 4).expect("units should open"),
    );
    // Hold the writer so the reads below take a `query_only` connection: a read that stored the
    // hash would be refused by SQLite.
    let other = Arc::new(large_document(2_000).await);
    let segmentation = Arc::new(
        other
            .try_segmentation()
            .expect("the fixture should segment"),
    );
    let write = hold_the_writer_with(&units, other, segmentation).await;
    // Every point reader of a row without a stored hash falls back to the full decode. On a pooled
    // connection that decode computes the hash and does not store it, so the read still answers and
    // never hits `query_only`.
    let header = repository
        .document_header(&document.id)
        .await
        .expect("the header should read")
        .expect("the document should exist");
    assert_eq!(header.id, document.id);
    assert_eq!(
        repository
            .body_order_len(&document.id)
            .await
            .expect("the body order should read"),
        Some(document.section_count())
    );
    assert!(
        repository
            .section_by_key(&document.id, 0)
            .await
            .expect("the section should read")
            .is_some()
    );
    assert!(
        !write.is_finished(),
        "the reads must run while the write holds the writer, so they run on the pool"
    );
    write
        .await
        .expect("the write task should not panic")
        .expect("the write should not fail");

    let connection = rusqlite::Connection::open(&database).expect("legacy should reopen");
    let backfilled: Option<String> = connection
        .query_row(
            "SELECT normalized_document_hash FROM documents WHERE id = ?1",
            rusqlite::params![&document.id.0],
            |row| row.get(0),
        )
        .expect("the hash column should read");
    assert_eq!(
        backfilled, None,
        "a pooled read must not store the hash; the full decode on the writer does"
    );

    // The full decode runs on the writer, so it keeps the backfill it always did.
    let hash = repository
        .get_with_hash(&document.id)
        .await
        .expect("the full decode should read")
        .expect("the document should exist")
        .1;
    assert_eq!(hash, document.normalized_document_hash());
    let connection = rusqlite::Connection::open(&database).expect("legacy should reopen");
    let backfilled: Option<String> = connection
        .query_row(
            "SELECT normalized_document_hash FROM documents WHERE id = ?1",
            rusqlite::params![&document.id.0],
            |row| row.get(0),
        )
        .expect("the hash column should read");
    assert_eq!(
        backfilled,
        Some(document.normalized_document_hash().0),
        "the writer's full decode stores the hash"
    );
}

// ------------------------------------------------------------------------------------------------
// Fixtures
// ------------------------------------------------------------------------------------------------

async fn fixture_document() -> Document {
    markdown_document(
        "memory:read-pool.md",
        "\
# 第一卷\n\n\
皇帝在朝堂上議事。群臣列坐，節度使入朝。\n\n\
## 第一章\n\n\
節度使入朝議事，皇帝遠在京城。群臣列坐。\n\n\
### 第一節\n\n\
寶玉在大觀園裡讀書，黛玉在旁邊看花。\n\n\
## 第二章\n\n\
記憶體與頁表映射，進程只能看到自己的地址空間。\n\n\
# 第二卷\n\n\
The winter light fell on the old house by the river. It was cold.\n",
    )
    .await
}

/// `sections` headings, each with one paragraph: big enough that storing it holds the writer's
/// mutex for long enough to order a read against it.
/// Holds the writer of `store` for the length of another document's ingest write, so a read issued
/// while it runs cannot be served by the idle writer and takes a pooled connection instead.
async fn hold_the_writer(
    store: &Store,
    sections: usize,
) -> tokio::task::JoinHandle<Result<(), reading_mcp::application::ports::ApplicationError>> {
    let document = Arc::new(large_document(sections).await);
    let segmentation = Arc::new(
        document
            .try_segmentation()
            .expect("the fixture should segment"),
    );
    hold_the_writer_with(&store.units, document, segmentation).await
}

/// The same, on a text-unit index the caller opened itself.
async fn hold_the_writer_with(
    units: &Arc<SqliteTextUnitIndex>,
    document: Arc<Document>,
    segmentation: Arc<DocumentSegmentation>,
) -> tokio::task::JoinHandle<Result<(), reading_mcp::application::ports::ApplicationError>> {
    let handle = tokio::spawn({
        let units = Arc::clone(units);
        async move {
            units
                .replace_document_segmentation(&document, segmentation)
                .await
        }
    });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert!(
        !handle.is_finished(),
        "the synthetic write finished within 50 ms; make it larger so the test can order the two"
    );
    handle
}

async fn large_document(sections: usize) -> Document {
    let mut text = String::new();
    for section in 0..sections {
        text.push_str(&format!("# 第{section}節\n\n"));
        text.push_str(
            "皇帝在朝堂上議事，群臣列坐，節度使入朝，寶玉在大觀園裡讀書，\
             記憶體與頁表映射，進程只能看到自己的地址空間。\n\n",
        );
    }
    markdown_document("memory:read-pool-large.md", &text).await
}

async fn markdown_document(source: &str, text: &str) -> Document {
    MarkdownParser
        .parse(RetrievedResource {
            source: DocumentSource(source.into()),
            final_source: DocumentSource(source.into()),
            media_type: MediaType("text/markdown".into()),
            bytes: text.as_bytes().to_vec(),
            etag: None,
            last_modified: None,
            metadata: Default::default(),
        })
        .await
        .expect("parse markdown")
}
