use std::sync::Arc;

use reading_mcp::application::get_document_structure::GetDocumentStructureUseCase;
use reading_mcp::application::open_document::{OpenDocumentCommand, OpenDocumentUseCase};
use reading_mcp::application::ports::{Parser, RetrievalOptions, RetrievedResource};
use reading_mcp::application::read_document::{ReadDocumentUseCase, ReadSectionCommand};
use reading_mcp::application::search_document::{SearchDocumentCommand, SearchDocumentUseCase};
use reading_mcp::domain::{DocumentSource, MediaType, SectionId};
use reading_mcp::infrastructure::{InMemoryDocumentRepository, InMemorySearchIndex};
use reading_mcp::parsing::{HtmlParser, ParserRouter};
use reading_mcp::retrieval::{FileRetriever, LocalFileSourcePolicy};
use tempfile::tempdir;

#[tokio::test]
async fn html_reuses_open_structure_search_and_read_without_special_use_cases() {
    let directory = tempdir().expect("temp directory should be created");
    let path = directory.path().join("operating-systems.html");
    tokio::fs::write(
        &path,
        r#"<!doctype html>
<html>
  <head>
    <title>Operating Systems Guide</title>
    <style>.hidden { display: none; }</style>
    <script>window.secretNoise = 'ignore me';</script>
  </head>
  <body>
    <nav><h2>Navigation</h2><p>Navigation noise.</p></nav>
    <main>
      <h1 id="operating-systems">Operating Systems</h1>
      <p>Core concepts for the book.</p>
      <h2 id="virtual-memory">Virtual Memory</h2>
      <p>Address spaces give each process an isolated view of memory.</p>
      <p>Page replacement algorithms decide which resident page should be evicted.</p>
      <h3 id="page-tables">Page Tables</h3>
      <p>Page table entries map virtual pages to physical frames.</p>
      <footer><p>Footer noise.</p></footer>
    </main>
  </body>
</html>"#,
    )
    .await
    .expect("fixture should be written");

    let repository = Arc::new(InMemoryDocumentRepository::default());
    let index = Arc::new(InMemorySearchIndex::default());
    let opened = OpenDocumentUseCase::new(
        Arc::new(LocalFileSourcePolicy::allow_roots([directory.path()])),
        Arc::new(FileRetriever),
        Arc::new(ParserRouter::phase3()),
        repository.clone(),
        index.clone(),
    )
    .execute(OpenDocumentCommand {
        source: DocumentSource(path.to_string_lossy().into_owned()),
        options: RetrievalOptions::default(),
    })
    .await
    .expect("HTML should open through the same application use case");

    assert_eq!(opened.title, "Operating Systems");
    assert_eq!(opened.media_type.0, "text/html");
    assert_eq!(opened.section_count, 3);

    let structure = GetDocumentStructureUseCase::new(repository.clone())
        .execute(opened.document_id.clone(), None)
        .await
        .expect("HTML structure should be available");

    assert_eq!(structure.sections.len(), 1);
    let root = &structure.sections[0];
    assert_eq!(root.section_id.0, "section://operating-systems");
    assert_eq!(root.children.len(), 1);
    assert_eq!(
        root.children[0].section_id.0,
        "section://operating-systems/virtual-memory"
    );
    assert_eq!(
        root.children[0].children[0].section_id.0,
        "section://operating-systems/virtual-memory/page-tables"
    );

    let searched = SearchDocumentUseCase::new(index, repository.clone())
        .execute(SearchDocumentCommand {
            document_id: opened.document_id.clone(),
            query: "replacement algorithms".into(),
            limit: 10,
        })
        .await
        .expect("existing search use case should work for HTML");

    assert!(!searched.hits.is_empty());
    assert_eq!(
        searched.hits[0].section_id.0,
        "section://operating-systems/virtual-memory"
    );
    assert!(
        searched.hits[0]
            .snippet
            .contains("Page replacement algorithms")
    );

    let read = ReadDocumentUseCase::new(repository)
        .execute(ReadSectionCommand {
            document_id: opened.document_id,
            section_id: SectionId("section://operating-systems/virtual-memory".into()),
            max_chars: None,
        })
        .await
        .expect("existing read use case should work for HTML");

    assert!(read.content.contains("Address spaces give each process"));
    assert!(read.content.contains("Page replacement algorithms"));
    assert!(read.content.contains("### Page Tables"));
    assert!(
        read.content
            .contains("Page table entries map virtual pages")
    );
    assert!(!read.content.contains("Navigation noise"));
    assert!(!read.content.contains("Footer noise"));
    assert_eq!(read.location.anchor.as_deref(), Some("virtual-memory"));
    assert_eq!(
        read.location.native_location.as_deref(),
        Some("html:#virtual-memory")
    );
}

#[tokio::test]
async fn html_parser_extracts_document_metadata_without_network_logic() {
    let resource = RetrievedResource {
        source: DocumentSource("https://example.com/docs/page".into()),
        final_source: DocumentSource("https://cdn.example.com/docs/page.html".into()),
        media_type: MediaType("text/html; charset=utf-8".into()),
        bytes: br#"<!doctype html>
<html>
  <head>
    <title>Fallback HTML Title</title>
    <link rel="canonical alternate" href="https://example.com/docs/canonical">
  </head>
  <body>
    <article>
      <h2 id="intro">Introduction</h2>
      <p>Canonical content.</p>
    </article>
  </body>
</html>"#
            .to_vec(),
        etag: None,
        last_modified: None,
        metadata: Default::default(),
    };

    let parsed = HtmlParser
        .parse(resource)
        .await
        .expect("HTML parser should only need retrieved bytes and metadata");

    assert_eq!(parsed.source.0, "https://cdn.example.com/docs/page.html");
    assert_eq!(parsed.title, "Fallback HTML Title");
    assert_eq!(
        parsed.metadata.get("canonical_href").map(String::as_str),
        Some("https://example.com/docs/canonical")
    );
    assert_eq!(
        parsed.metadata.get("html_title").map(String::as_str),
        Some("Fallback HTML Title")
    );
    assert_eq!(
        parsed.root_sections[0].location.anchor.as_deref(),
        Some("intro")
    );
}

#[tokio::test]
async fn phase1_router_remains_closed_to_html() {
    let resource = RetrievedResource {
        source: DocumentSource("memory:test.html".into()),
        final_source: DocumentSource("memory:test.html".into()),
        media_type: MediaType("text/html".into()),
        bytes: b"<h1>Title</h1><p>Body</p>".to_vec(),
        etag: None,
        last_modified: None,
        metadata: Default::default(),
    };

    let error = ParserRouter::phase1()
        .parse(resource)
        .await
        .expect_err("phase1 composition should not silently gain HTML support");

    assert!(error.to_string().contains("unsupported media type"));
}
