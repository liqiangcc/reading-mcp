//! `document_json` keeps no section text when `section_text_v3` already stores it for the same
//! document version, and reads it back on load.

use std::collections::BTreeMap;
use std::io::{Cursor, Write};
use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use reading_mcp::application::get_text_units::{RequestedTextUnitKind, TextUnitCoveragePolicy};
use reading_mcp::application::open_document::{OpenDocumentCommand, OpenDocumentUseCase};
use reading_mcp::application::ports::{
    ApplicationError, DocumentRepository, Parser, RetrievalOptions, RetrievedResource, Retriever,
    SourcePolicy, TextUnitIndex, TextUnitStreamKey,
};
use reading_mcp::domain::{
    ContentHash, Document, DocumentId, DocumentSource, Location, MediaType, Section, SectionId,
};
use reading_mcp::infrastructure::{
    SqliteDocumentRepository, SqliteSearchIndex, SqliteTextUnitIndex,
};
use reading_mcp::mcp::contracts::{GetDocumentStructureResponse, OpenDocumentResponse};
use reading_mcp::parsing::MarkdownParser;
use rmcp::ServiceExt;
use rmcp::model::CallToolRequestParams;
use rmcp::transport::TokioChildProcess;
use rusqlite::{Connection, params};
use serde_json::{Map, Value, json};
use tempfile::tempdir;
use tokio::process::Command;
use zip::write::SimpleFileOptions;

const MARKDOWN: &str = "# Operating Systems\n\nCore concepts of the book.\n\n## Virtual Memory\n\nVirtual memory maps addresses. Page tables hold entries.\n\n## Processes\n\nA process owns an address space.\n";

/// The JSON a `main`-era repository writes for [`standalone_document`]: every section carries its
/// text inline and no `content_store` key exists.
const STANDALONE_INLINE_JSON: &str = concat!(
    r#"{"normalization_version":"reading-mcp-normalization/v11","#,
    r#""id":"doc:standalone","source":"memory:standalone.md","title":"Standalone","#,
    r#""media_type":"text/markdown","content_hash":"sha256:standalone","#,
    r#""metadata":{"k":"v"},"root_sections":[{"id":"section://standalone","#,
    r#""parent_id":null,"title":"Standalone","level":1,"content":"Body text.","#,
    r#""location":{"page":null,"chapter":null,"section_path":[],"anchor":null,"#,
    r#""paragraph":null,"char_start":null,"char_end":null,"native_location":null},"#,
    r#""children":[]}]}"#,
);

struct AllowAllPolicy;

#[async_trait]
impl SourcePolicy for AllowAllPolicy {
    async fn validate(&self, _source: &DocumentSource) -> Result<(), ApplicationError> {
        Ok(())
    }
}

struct StaticRetriever;

#[async_trait]
impl Retriever for StaticRetriever {
    async fn retrieve(
        &self,
        source: &DocumentSource,
        _options: &RetrievalOptions,
    ) -> Result<RetrievedResource, ApplicationError> {
        Ok(RetrievedResource {
            source: source.clone(),
            final_source: source.clone(),
            media_type: MediaType("text/markdown".into()),
            bytes: MARKDOWN.as_bytes().to_vec(),
            etag: None,
            last_modified: None,
            metadata: BTreeMap::new(),
        })
    }
}

/// Returns one fixed parsed document, so the saved document is exactly the one the test compares.
struct StaticParser(Arc<Document>);

#[async_trait]
impl Parser for StaticParser {
    async fn parse(&self, _resource: RetrievedResource) -> Result<Document, ApplicationError> {
        Ok((*self.0).clone())
    }
}

async fn parse_markdown() -> Document {
    MarkdownParser
        .parse(RetrievedResource {
            source: DocumentSource("memory:operating-systems.md".into()),
            final_source: DocumentSource("memory:operating-systems.md".into()),
            media_type: MediaType("text/markdown".into()),
            bytes: MARKDOWN.as_bytes().to_vec(),
            etag: None,
            last_modified: None,
            metadata: BTreeMap::new(),
        })
        .await
        .expect("markdown should parse")
}

/// Ingests [`parse_markdown`] through the open use case over a SQLite repository, text-unit store and
/// search index: the ingest path of the runtime, minus the parser.
async fn open_with_section_text(directory: &Path) -> (std::path::PathBuf, Document) {
    let database = directory.join("state.sqlite");
    let document = parse_markdown().await;
    let repository = Arc::new(SqliteDocumentRepository::open(&database).expect("repository"));
    let text_unit_index: Arc<dyn TextUnitIndex> =
        Arc::new(SqliteTextUnitIndex::open(&database).expect("text unit index"));
    let search_index = Arc::new(SqliteSearchIndex::open(&database).expect("search index"));
    let open = OpenDocumentUseCase::with_text_unit_index(
        Arc::new(AllowAllPolicy),
        Arc::new(StaticRetriever),
        Arc::new(StaticParser(Arc::new(document.clone()))),
        repository,
        text_unit_index,
        search_index,
    );
    open.execute(OpenDocumentCommand {
        source: DocumentSource("memory:operating-systems.md".into()),
        options: RetrievalOptions::default(),
    })
    .await
    .expect("document should open");
    (database, document)
}

fn stored_json(database: &Path, document_id: &DocumentId) -> Value {
    let connection = Connection::open(database).expect("database should open");
    let json: String = connection
        .query_row(
            "SELECT document_json FROM documents WHERE id = ?1",
            params![&document_id.0],
            |row| row.get(0),
        )
        .expect("stored document should exist");
    serde_json::from_str(&json).expect("stored document should be JSON")
}

fn stored_json_text(database: &Path, document_id: &DocumentId) -> String {
    let connection = Connection::open(database).expect("database should open");
    connection
        .query_row(
            "SELECT document_json FROM documents WHERE id = ?1",
            params![&document_id.0],
            |row| row.get(0),
        )
        .expect("stored document should exist")
}

fn doc_key(database: &Path, document_id: &DocumentId) -> i64 {
    let connection = Connection::open(database).expect("database should open");
    connection
        .query_row(
            "SELECT doc_key FROM text_unit_documents_v2 WHERE document_id = ?1",
            params![&document_id.0],
            |row| row.get(0),
        )
        .expect("stored document should have a doc key")
}

/// Asserts every stored section's `content` is empty and returns how many sections there are.
fn assert_empty_contents(sections: &Value) -> usize {
    let sections = sections.as_array().expect("sections should be an array");
    let mut count = 0;
    for section in sections {
        assert_eq!(
            section["content"],
            Value::String(String::new()),
            "section {} must store no text",
            section["id"]
        );
        count += 1 + assert_empty_contents(&section["children"]);
    }
    count
}

fn standalone_document() -> Document {
    Document {
        id: DocumentId("doc:standalone".into()),
        source: DocumentSource("memory:standalone.md".into()),
        title: "Standalone".into(),
        media_type: MediaType("text/markdown".into()),
        content_hash: ContentHash("sha256:standalone".into()),
        metadata: BTreeMap::from([("k".into(), "v".into())]),
        root_sections: vec![Section {
            id: SectionId("section://standalone".into()),
            parent_id: None,
            title: "Standalone".into(),
            level: 1,
            content: "Body text.".into(),
            location: Location::default(),
            children: vec![],
        }],
    }
}

#[tokio::test]
async fn runtime_open_stores_no_section_text_and_a_new_repository_reads_it_back() {
    let directory = tempdir().expect("temporary directory should be created");
    let (database, document) = open_with_section_text(directory.path()).await;

    let stored = stored_json(&database, &document.id);
    assert_eq!(
        stored["content_store"],
        Value::String("section_text_v3".into())
    );
    assert_eq!(
        assert_empty_contents(&stored["root_sections"]),
        document.section_count(),
        "every section must be stored"
    );

    let reopened = SqliteDocumentRepository::open(&database).expect("repository should reopen");
    let loaded = reopened
        .get(&document.id)
        .await
        .expect("document should load")
        .expect("document should exist");
    assert_eq!(*loaded, document);
}

#[tokio::test]
async fn a_repository_on_its_own_keeps_the_section_text_inline() {
    let directory = tempdir().expect("temporary directory should be created");
    let database = directory.path().join("state.sqlite");
    let document = standalone_document();
    let repository = SqliteDocumentRepository::open(&database).expect("repository should open");
    repository
        .save_shared(Arc::new(document.clone()))
        .await
        .expect("document should save");

    let stored = stored_json_text(&database, &document.id);
    assert!(
        !stored.contains("content_store"),
        "a repository without stored section text must not name a content store: {stored}"
    );
    assert_eq!(stored, STANDALONE_INLINE_JSON);

    let reopened = SqliteDocumentRepository::open(&database).expect("repository should reopen");
    let loaded = reopened
        .get(&document.id)
        .await
        .expect("document should load")
        .expect("document should exist");
    assert_eq!(*loaded, document);
}

#[tokio::test]
async fn an_old_inline_row_still_loads() {
    let directory = tempdir().expect("temporary directory should be created");
    let database = directory.path().join("state.sqlite");
    let document = standalone_document();
    let repository = SqliteDocumentRepository::open(&database).expect("repository should open");
    drop(repository);
    let connection = Connection::open(&database).expect("database should open");
    connection
        .execute(
            "INSERT INTO documents(id, source, content_hash, document_json)
             VALUES (?1, ?2, ?3, ?4)",
            params![
                &document.id.0,
                &document.source.0,
                &document.content_hash.0,
                STANDALONE_INLINE_JSON
            ],
        )
        .expect("old row should insert");
    drop(connection);

    let reopened = SqliteDocumentRepository::open(&database).expect("repository should reopen");
    let loaded = reopened
        .get(&document.id)
        .await
        .expect("old row should load")
        .expect("old row should exist");
    assert_eq!(*loaded, document);
}

#[tokio::test]
async fn a_missing_section_text_chunk_makes_the_row_stale() {
    let directory = tempdir().expect("temporary directory should be created");
    let (database, document) = open_with_section_text(directory.path()).await;
    let doc_key = doc_key(&database, &document.id);
    let connection = Connection::open(&database).expect("database should open");
    let removed = connection
        .execute(
            "DELETE FROM section_text_v3 WHERE doc_key = ?1 AND section_key = 1",
            params![doc_key],
        )
        .expect("chunk should delete");
    assert_eq!(removed, 1, "the second section should have one chunk");
    drop(connection);

    let reopened = SqliteDocumentRepository::open(&database).expect("repository should reopen");
    let error = reopened
        .get(&document.id)
        .await
        .expect_err("a damaged row must not load");
    assert!(
        matches!(error, ApplicationError::StaleDocument(_)),
        "expected StaleDocument, got {error:?}"
    );
    assert!(
        error
            .to_string()
            .ends_with("explicit source reopen required"),
        "stale message should ask for a reopen: {error}"
    );
}

#[tokio::test]
async fn a_changed_section_char_len_makes_the_row_stale() {
    let directory = tempdir().expect("temporary directory should be created");
    let (database, document) = open_with_section_text(directory.path()).await;
    let doc_key = doc_key(&database, &document.id);
    let connection = Connection::open(&database).expect("database should open");
    connection
        .execute(
            "UPDATE section_layout_v2 SET char_len = char_len + 1
              WHERE doc_key = ?1 AND section_key = 1",
            params![doc_key],
        )
        .expect("char_len should update");
    drop(connection);

    let reopened = SqliteDocumentRepository::open(&database).expect("repository should reopen");
    let error = reopened
        .get(&document.id)
        .await
        .expect_err("a damaged row must not load");
    assert!(
        matches!(error, ApplicationError::StaleDocument(_)),
        "expected StaleDocument, got {error:?}"
    );
}

#[tokio::test]
async fn a_v1_content_store_name_is_reported_as_stale() {
    let directory = tempdir().expect("temporary directory should be created");
    let (database, document) = open_with_section_text(directory.path()).await;
    let mut stored: Value = serde_json::from_str(&stored_json_text(&database, &document.id))
        .expect("stored document should be JSON");
    stored["content_store"] = json!("section_text_v1");
    let connection = Connection::open(&database).expect("database should open");
    connection
        .execute(
            "UPDATE documents SET document_json = ?1 WHERE id = ?2",
            params![stored.to_string(), &document.id.0],
        )
        .expect("content store name should update");
    drop(connection);

    let reopened = SqliteDocumentRepository::open(&database).expect("repository should reopen");
    let error = reopened
        .get(&document.id)
        .await
        .expect_err("a removed content store must not load");
    assert!(
        matches!(error, ApplicationError::StaleDocument(_)),
        "expected StaleDocument, got {error:?}"
    );
    assert!(
        error.to_string().contains("unknown store section_text_v1"),
        "stale message should name the removed store: {error}"
    );
    assert!(
        error
            .to_string()
            .ends_with("explicit source reopen required"),
        "stale message should ask for a reopen: {error}"
    );
}

#[tokio::test]
async fn an_old_database_with_v1_tables_opens_and_reingests_on_the_next_open() {
    let directory = tempdir().expect("temporary directory should be created");
    let (database, document) = open_with_section_text(directory.path()).await;
    let mut stored: Value = serde_json::from_str(&stored_json_text(&database, &document.id))
        .expect("stored document should be JSON");
    stored["content_store"] = json!("section_text_v1");
    let connection = Connection::open(&database).expect("database should open");
    connection
        .execute(
            "UPDATE documents SET document_json = ?1 WHERE id = ?2",
            params![stored.to_string(), &document.id.0],
        )
        .expect("content store name should update");
    // Rewrite the column order of a database written before this change: one text and one layout
    // table, no header and no layout chunks.
    connection
        .execute_batch(
            "CREATE TABLE section_text_v1 (
                doc_key INTEGER NOT NULL,
                section_key INTEGER NOT NULL,
                chunk_no INTEGER NOT NULL,
                text TEXT NOT NULL,
                PRIMARY KEY (doc_key, section_key, chunk_no)
             ) WITHOUT ROWID;
             INSERT INTO section_text_v1
                SELECT doc_key, section_key, chunk_no, data FROM section_text_v3;
             CREATE TABLE section_layout_v1 (
                doc_key INTEGER NOT NULL,
                section_key INTEGER NOT NULL,
                section_id TEXT NOT NULL,
                char_len INTEGER NOT NULL,
                paragraph_coverage_json TEXT,
                layout BLOB NOT NULL,
                PRIMARY KEY (doc_key, section_key)
             ) WITHOUT ROWID;
             INSERT INTO section_layout_v1
                SELECT doc_key, section_key, section_id, char_len, paragraph_coverage_json, x'00'
                  FROM section_layout_v2;
             DROP TABLE section_text_v3;
             DROP TABLE section_layout_chunk_v2;
             DROP TABLE section_layout_v2;",
        )
        .expect("old tables should replace the v4 tables");
    drop(connection);

    let section_id = document.root_sections[0].id.clone();
    let index = SqliteTextUnitIndex::open(&database).expect("old database should open");
    let summary = index
        .section_stream_summary(TextUnitStreamKey {
            document_id: &document.id,
            section_id: &section_id,
            kind: RequestedTextUnitKind::Sentence,
            policy: TextUnitCoveragePolicy::PreserveSource,
        })
        .await
        .expect("an old database must still answer");
    assert!(
        summary.is_none(),
        "a document without layout rows must read as not stored"
    );
    drop(index);

    let reopened = SqliteDocumentRepository::open(&database).expect("repository should reopen");
    assert!(
        matches!(
            reopened.get(&document.id).await,
            Err(ApplicationError::StaleDocument(_))
        ),
        "a v1 row must report stale until the source is reopened"
    );
    drop(reopened);

    let (database, reparsed) = open_with_section_text(directory.path()).await;
    assert_eq!(reparsed, document);
    let stored = stored_json(&database, &document.id);
    assert_eq!(
        stored["content_store"],
        Value::String("section_text_v3".into())
    );
    assert_eq!(
        assert_empty_contents(&stored["root_sections"]),
        document.section_count(),
        "the re-ingested row must keep its text out of `document_json`"
    );
    let index = SqliteTextUnitIndex::open(&database).expect("index should reopen");
    let summary = index
        .section_stream_summary(TextUnitStreamKey {
            document_id: &document.id,
            section_id: &section_id,
            kind: RequestedTextUnitKind::Sentence,
            policy: TextUnitCoveragePolicy::PreserveSource,
        })
        .await
        .expect("summary should load")
        .expect("the re-ingest must store a summary");
    assert!(summary.total_items > 0);
    let reopened = SqliteDocumentRepository::open(&database).expect("repository should reopen");
    let loaded = reopened
        .get(&document.id)
        .await
        .expect("document should load")
        .expect("document should exist");
    assert_eq!(*loaded, document);
}

#[tokio::test]
async fn stdio_responses_are_identical_after_a_restart() {
    let directory = tempdir().expect("temporary directory should be created");
    let epub = directory.path().join("book.epub");
    std::fs::write(&epub, build_epub()).expect("EPUB fixture should be written");
    let state = directory.path().join("state");

    let first = run_stdio_session(directory.path(), &state, &epub).await;
    let second = run_stdio_session(directory.path(), &state, &epub).await;
    assert_eq!(
        first, second,
        "the same calls must answer identically from a fresh process"
    );
    assert_eq!(first.len(), 6, "every call must be compared");
}

async fn run_stdio_session(root: &Path, state: &Path, epub: &Path) -> Vec<(String, Vec<u8>)> {
    let local_roots = std::env::join_paths([root]).expect("local roots should be valid");
    let mut command = Command::new(env!("CARGO_BIN_EXE_reading-mcp"));
    command
        .env("READING_MCP_LOCAL_ROOTS", local_roots)
        .env("READING_MCP_STATE_DIR", state)
        .env("READING_MCP_TELEMETRY", "false")
        .env("READING_MCP_TOOLS", "v1");
    let transport = TokioChildProcess::new(command).expect("server process");
    let client = ().serve(transport).await.expect("MCP initialize");

    let mut responses = Vec::new();
    let opened = client
        .call_tool(
            CallToolRequestParams::new("open_document").with_arguments(arguments(json!({
                "source": epub.to_string_lossy()
            }))),
        )
        .await
        .expect("open should succeed");
    let document_id = opened
        .clone()
        .into_typed::<OpenDocumentResponse>()
        .expect("typed open")
        .document_id;
    responses.push((String::from("open_document"), json_bytes(&opened)));

    let structure = client
        .call_tool(
            CallToolRequestParams::new("get_document_structure").with_arguments(arguments(json!({
                "document_id": document_id
            }))),
        )
        .await
        .expect("structure should succeed");
    let section_id = {
        let typed = structure
            .clone()
            .into_typed::<GetDocumentStructureResponse>()
            .expect("typed structure");
        let root = typed
            .sections
            .first()
            .expect("the EPUB should have a root section");
        root.children.first().unwrap_or(root).section_id.clone()
    };
    responses.push((
        String::from("get_document_structure"),
        json_bytes(&structure),
    ));

    let units = client
        .call_tool(
            CallToolRequestParams::new("get_text_units").with_arguments(arguments(json!({
                "document_id": document_id,
                "section_id": section_id,
                "requested_kind": "sentence",
                "coverage_policy": "preserve_source",
                "max_items": 10
            }))),
        )
        .await
        .expect("text units should succeed");
    responses.push((String::from("get_text_units"), json_bytes(&units)));

    let read = client
        .call_tool(
            CallToolRequestParams::new("read_document").with_arguments(arguments(json!({
                "document_id": document_id,
                "section_id": section_id
            }))),
        )
        .await
        .expect("read should succeed");
    responses.push((String::from("read_document"), json_bytes(&read)));

    let search = client
        .call_tool(
            CallToolRequestParams::new("search_document").with_arguments(arguments(json!({
                "document_id": document_id,
                "query": "memory",
                "limit": 5
            }))),
        )
        .await
        .expect("search should succeed");
    responses.push((String::from("search_document"), json_bytes(&search)));

    let context = client
        .call_tool(
            CallToolRequestParams::new("get_context").with_arguments(arguments(json!({
                "document_id": document_id,
                "section_id": section_id,
                "before": 1,
                "after": 1
            }))),
        )
        .await
        .expect("context should succeed");
    responses.push((String::from("get_context"), json_bytes(&context)));

    client.cancel().await.expect("server should stop");
    responses
}

fn json_bytes<T: serde::Serialize>(value: &T) -> Vec<u8> {
    serde_json::to_vec(value).expect("response should serialize")
}

fn arguments(value: Value) -> Map<String, Value> {
    value
        .as_object()
        .expect("tool arguments should be a JSON object")
        .clone()
}

fn build_epub() -> Vec<u8> {
    build_zip(&[
        ("mimetype", "application/epub+zip"),
        (
            "META-INF/container.xml",
            r#"<?xml version="1.0"?>
<container xmlns="urn:oasis:names:tc:opendocument:xmlns:container">
  <rootfiles><rootfile full-path="OPS/package.opf" media-type="application/oebps-package+xml"/></rootfiles>
</container>"#,
        ),
        (
            "OPS/package.opf",
            r#"<?xml version="1.0"?>
<package xmlns="http://www.idpf.org/2007/opf" version="3.0">
  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:title>Operating Systems EPUB</dc:title></metadata>
  <manifest>
    <item id="one" href="chapter-one.xhtml" media-type="application/xhtml+xml"/>
    <item id="two" href="chapter-two.xhtml" media-type="application/xhtml+xml"/>
  </manifest>
  <spine><itemref idref="one"/><itemref idref="two"/></spine>
</package>"#,
        ),
        (
            "OPS/chapter-one.xhtml",
            r#"<!doctype html><html><head><title>Virtual Memory</title></head><body><main>
<h1 id="vm">Virtual Memory</h1><p>Orbital memory in EPUB. Page tables map addresses.</p>
<h2 id="tables">Page Tables</h2><p>Page tables hold one entry per page.</p>
</main></body></html>"#,
        ),
        (
            "OPS/chapter-two.xhtml",
            r#"<!doctype html><html><head><title>Processes</title></head><body><main>
<h1 id="processes">Processes</h1><p>A process owns an address space and its memory.</p>
</main></body></html>"#,
        ),
    ])
}

fn build_zip(entries: &[(&str, &str)]) -> Vec<u8> {
    let cursor = Cursor::new(Vec::new());
    let mut writer = zip::ZipWriter::new(cursor);
    let options = SimpleFileOptions::default();
    for (name, content) in entries {
        writer
            .start_file(*name, options)
            .expect("ZIP fixture entry should start");
        writer
            .write_all(content.as_bytes())
            .expect("ZIP fixture entry should write");
    }
    writer
        .finish()
        .expect("ZIP fixture should finish")
        .into_inner()
}
