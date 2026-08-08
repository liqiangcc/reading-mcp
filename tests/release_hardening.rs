use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use lopdf::content::{Content, Operation};
use lopdf::{Document as PdfDocument, Object, Stream, dictionary};
use reading_mcp::application::ports::{
    ApplicationError, DocumentRepository, Parser, RetrievalOptions, RetrievedResource, Retriever,
    SearchIndex,
};
use reading_mcp::domain::{
    ContentHash, Document, DocumentId, DocumentSource, Location, MediaType, Section, SectionId,
};
use reading_mcp::infrastructure::{
    BudgetedParser, ResourceBudget, SqliteDocumentRepository, SqliteSearchIndex,
};
use reading_mcp::parsing::LimitedPdfParser;
use reading_mcp::retrieval::LimitedFileRetriever;
use tempfile::tempdir;

#[tokio::test]
async fn local_file_budget_is_checked_before_reading_body() {
    let directory = tempdir().expect("temp directory should be created");
    let path = directory.path().join("large.txt");
    tokio::fs::write(&path, b"0123456789abcdef")
        .await
        .expect("fixture should be written");

    let error = LimitedFileRetriever::new(8)
        .retrieve(
            &DocumentSource(path.to_string_lossy().into_owned()),
            &RetrievalOptions::default(),
        )
        .await
        .expect_err("oversized local file must be rejected before reading");

    assert!(matches!(error, ApplicationError::ResourceLimitExceeded(_)));
    assert!(error.to_string().contains("16 bytes"));
}

#[tokio::test]
async fn normalized_document_budget_rejects_excessive_content() {
    let budget = ResourceBudget {
        max_normalized_chars: 5,
        ..ResourceBudget::default()
    };
    let parser = BudgetedParser::new(Arc::new(StaticParser), budget);

    let error = parser
        .parse(markdown_resource())
        .await
        .expect_err("oversized normalized document must be rejected");

    assert!(matches!(error, ApplicationError::ResourceLimitExceeded(_)));
    assert!(error.to_string().contains("characters"));
}

#[tokio::test]
async fn parser_timeout_is_exposed_as_resource_limit() {
    let budget = ResourceBudget {
        parse_timeout: Duration::from_millis(10),
        ..ResourceBudget::default()
    };
    let parser = BudgetedParser::new(Arc::new(SlowParser), budget);

    let error = parser
        .parse(markdown_resource())
        .await
        .expect_err("slow parser must be bounded");

    assert!(matches!(error, ApplicationError::ResourceLimitExceeded(_)));
    assert!(error.to_string().contains("timeout"));
}

#[tokio::test]
async fn pdf_total_page_budget_is_enforced_before_text_extraction() {
    let resource = RetrievedResource {
        source: DocumentSource("memory:large.pdf".into()),
        final_source: DocumentSource("memory:large.pdf".into()),
        media_type: MediaType("application/pdf".into()),
        bytes: build_pdf(2),
        etag: None,
        last_modified: None,
        metadata: BTreeMap::new(),
    };

    let error = LimitedPdfParser::new(1)
        .parse(resource)
        .await
        .expect_err("PDF over page budget must be rejected");

    assert!(matches!(error, ApplicationError::ResourceLimitExceeded(_)));
    assert!(error.to_string().contains("2 pages"));
}

#[tokio::test]
async fn sqlite_repository_and_fts_survive_reopen() {
    let directory = tempdir().expect("temp directory should be created");
    let database = directory.path().join("reading.sqlite");
    let document = sample_document();

    {
        let repository = SqliteDocumentRepository::open(&database)
            .expect("SQLite document repository should open");
        let index = SqliteSearchIndex::open(&database).expect("SQLite FTS index should open");
        repository
            .save(document.clone())
            .await
            .expect("document should persist");
        index.index(&document).await.expect("document should index");
    }

    let repository =
        SqliteDocumentRepository::open(&database).expect("repository should reopen cleanly");
    let restored = repository
        .get(&document.id)
        .await
        .expect("repository read should succeed")
        .expect("document should survive process-style reopen");
    assert_eq!(restored, document);

    let index = SqliteSearchIndex::open(&database).expect("FTS index should reopen cleanly");
    let hits = index
        .search(&document.id, "replacement algorithms", 10)
        .await
        .expect("persistent FTS search should succeed");
    assert!(!hits.is_empty());
    assert_eq!(hits[0].section_id.0, "section://virtual-memory");
    assert_eq!(hits[0].title, "Virtual Memory");
    assert_eq!(hits[0].source, document.source);
}

struct StaticParser;

#[async_trait]
impl Parser for StaticParser {
    async fn parse(&self, _resource: RetrievedResource) -> Result<Document, ApplicationError> {
        Ok(sample_document())
    }
}

struct SlowParser;

#[async_trait]
impl Parser for SlowParser {
    async fn parse(&self, _resource: RetrievedResource) -> Result<Document, ApplicationError> {
        tokio::time::sleep(Duration::from_millis(100)).await;
        Ok(sample_document())
    }
}

fn markdown_resource() -> RetrievedResource {
    RetrievedResource {
        source: DocumentSource("memory:test.md".into()),
        final_source: DocumentSource("memory:test.md".into()),
        media_type: MediaType("text/markdown".into()),
        bytes: b"# Test".to_vec(),
        etag: None,
        last_modified: None,
        metadata: BTreeMap::new(),
    }
}

fn sample_document() -> Document {
    Document {
        id: DocumentId("doc:persistent".into()),
        source: DocumentSource("file:///books/os.md".into()),
        title: "Operating Systems".into(),
        media_type: MediaType("text/markdown".into()),
        content_hash: ContentHash("sha256:persistent".into()),
        metadata: BTreeMap::new(),
        root_sections: vec![Section {
            id: SectionId("section://virtual-memory".into()),
            parent_id: None,
            title: "Virtual Memory".into(),
            level: 1,
            content:
                "Address spaces isolate memory.\n\nPage replacement algorithms choose victims."
                    .into(),
            location: Location {
                section_path: vec!["Virtual Memory".into()],
                native_location: Some("markdown:#virtual-memory".into()),
                ..Location::default()
            },
            children: vec![],
        }],
    }
}

fn build_pdf(page_count: usize) -> Vec<u8> {
    let mut document = PdfDocument::with_version("1.5");
    let pages_id = document.new_object_id();
    let font_id = document.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => "Helvetica",
    });
    let resources_id = document.add_object(dictionary! {
        "Font" => dictionary! { "F1" => font_id },
    });

    let mut page_ids = Vec::new();
    for index in 0..page_count {
        let content = Content {
            operations: vec![
                Operation::new("BT", vec![]),
                Operation::new("Tf", vec!["F1".into(), 12.into()]),
                Operation::new("Td", vec![72.into(), 720.into()]),
                Operation::new(
                    "Tj",
                    vec![Object::string_literal(format!("Page {}", index + 1))],
                ),
                Operation::new("ET", vec![]),
            ],
        };
        let content_id = document.add_object(Stream::new(
            dictionary! {},
            content.encode().expect("fixture content should encode"),
        ));
        let page_id = document.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "Contents" => content_id,
        });
        page_ids.push(page_id);
    }

    document.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => page_ids.iter().copied().map(Object::Reference).collect::<Vec<_>>(),
            "Count" => page_ids.len() as i64,
            "Resources" => resources_id,
            "MediaBox" => vec![0.into(), 0.into(), 595.into(), 842.into()],
        }),
    );
    let catalog_id = document.add_object(dictionary! {
        "Type" => "Catalog",
        "Pages" => pages_id,
    });
    document.trailer.set("Root", catalog_id);

    let mut bytes = Vec::new();
    document
        .save_to(&mut bytes)
        .expect("fixture PDF should serialize");
    bytes
}
