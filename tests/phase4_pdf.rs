#[path = "support/keyed_index.rs"]
mod keyed_index;
use keyed_index::KeyedTextUnitIndex;

use std::sync::Arc;

use lopdf::content::{Content, Operation};
use lopdf::{Bookmark, Document as PdfDocument, Object, Stream, dictionary, text_string};
use reading_mcp::application::get_text_units::RequestedTextUnitKind;
use reading_mcp::application::open_document::{OpenDocumentCommand, OpenDocumentUseCase};
use reading_mcp::application::outline_reading::{OutlineReadingCommand, OutlineReadingUseCase};
use reading_mcp::application::ports::{DocumentRepository, TextUnitIndex};
use reading_mcp::application::ports::{Parser, RetrievalOptions, RetrievedResource};
use reading_mcp::application::read_reading::{
    ReadReadingCommand, ReadReadingResult, ReadReadingUseCase,
};
use reading_mcp::application::search_document::{SearchDocumentCommand, SearchDocumentUseCase};
use reading_mcp::application::short_handle::{DocumentHandle, SectionHandle, handle_checksum};
use reading_mcp::domain::{DocumentId, DocumentSource, MediaType, SectionId};
use reading_mcp::infrastructure::{InMemoryDocumentRepository, InMemorySearchIndex};
use reading_mcp::parsing::{ParserRouter, PdfParser};
use reading_mcp::retrieval::{FileRetriever, LocalFileSourcePolicy};
use tempfile::tempdir;

#[tokio::test]
async fn pdf_toc_reuses_existing_open_structure_search_and_read_flow() {
    let bytes = build_pdf_bytes(
        &[
            "Address spaces give each process an isolated memory view.",
            "Page table entries map virtual pages to physical frames.",
            "Processes own resources and execution state.",
        ],
        true,
    );
    let directory = tempdir().expect("temp directory should be created");
    let path = directory.path().join("operating-systems.pdf");
    tokio::fs::write(&path, bytes)
        .await
        .expect("PDF fixture should be written");

    let repository = Arc::new(InMemoryDocumentRepository::default());
    let index = Arc::new(InMemorySearchIndex::default());
    let opened = OpenDocumentUseCase::new(
        Arc::new(LocalFileSourcePolicy::allow_roots([directory.path()])),
        Arc::new(FileRetriever),
        Arc::new(ParserRouter::phase4()),
        repository.clone(),
        index.clone(),
    )
    .execute(OpenDocumentCommand {
        source: DocumentSource(path.to_string_lossy().into_owned()),
        options: RetrievalOptions::default(),
    })
    .await
    .expect("PDF should open through the existing application use case");

    assert_eq!(opened.title, "Operating Systems");
    assert_eq!(opened.media_type.0, "application/pdf");
    assert_eq!(opened.section_count, 3);

    let (units, document) = open_and_store(&repository, &opened.document_id).await;
    let handle = document_handle(&units, &document).await;
    let outline = OutlineReadingUseCase::new(repository.clone(), units.clone())
        .execute(OutlineReadingCommand {
            at: handle.to_string(),
            depth: 4,
            limit: 100,
            cursor: None,
        })
        .await
        .expect("PDF structure should be available");
    let shape: Vec<(&str, u8)> = outline
        .items
        .iter()
        .map(|item| (item.title.as_str(), item.level))
        .collect();
    assert_eq!(
        shape,
        vec![("Virtual Memory", 1), ("Page Tables", 2), ("Processes", 1),]
    );

    let searched = SearchDocumentUseCase::new(index, repository.clone())
        .execute(SearchDocumentCommand {
            document_id: opened.document_id.clone(),
            query: "physical frames".into(),
            limit: 10,
        })
        .await
        .expect("existing search use case should work for PDF");

    assert!(!searched.hits.is_empty());
    assert_eq!(
        searched.hits[0].section_id.0,
        "section://virtual-memory/page-tables"
    );
    assert_eq!(searched.hits[0].location.page, Some(2));

    let section = section_handle(&units, &document, "section://virtual-memory").await;
    // A v2 `read` streams forward from the anchor to the end of the document.
    let result = read(
        repository.clone(),
        units.clone(),
        &section.to_string(),
        Some(16_000),
    )
    .await;
    let text = items_text(&result);

    assert!(text.contains("Address spaces give each process"));
    assert!(text.contains("physical frames"));
    assert!(
        result
            .sections
            .iter()
            .any(|section| section.title == "Page Tables")
    );

    // The first item still binds page 1 of the source.
    let cite =
        reading_mcp::application::cite_reading::CiteReadingUseCase::new(repository.clone(), units)
            .execute(&[result.items[0].handle.clone()])
            .await
            .expect("cite");
    assert_eq!(
        cite.items[0]
            .locator
            .as_ref()
            .expect("locator")
            .native_location
            .as_deref(),
        Some("pdf:page:1")
    );
}

#[tokio::test]
async fn pdf_without_toc_falls_back_to_page_sections_with_stable_locations() {
    let resource = RetrievedResource {
        source: DocumentSource("memory:book.pdf".into()),
        final_source: DocumentSource("memory:book.pdf".into()),
        media_type: MediaType("application/pdf".into()),
        bytes: build_pdf_bytes(
            &[
                "First page contains the introduction.",
                "Second page contains virtual memory details.",
            ],
            false,
        ),
        etag: None,
        last_modified: None,
        metadata: Default::default(),
    };

    let parsed = PdfParser
        .parse(resource)
        .await
        .expect("text PDF without outlines should still parse");

    assert_eq!(parsed.root_sections.len(), 2);
    assert_eq!(parsed.root_sections[0].id.0, "section://page-1");
    assert_eq!(parsed.root_sections[0].location.page, Some(1));
    assert_eq!(
        parsed.root_sections[0].location.native_location.as_deref(),
        Some("pdf:page:1")
    );
    assert_eq!(parsed.root_sections[1].id.0, "section://page-2");
    assert!(
        parsed.root_sections[1]
            .content
            .contains("virtual memory details")
    );
}

#[tokio::test]
async fn pdf_without_extractable_text_reports_ocr_boundary_explicitly() {
    let resource = RetrievedResource {
        source: DocumentSource("memory:scan.pdf".into()),
        final_source: DocumentSource("memory:scan.pdf".into()),
        media_type: MediaType("application/pdf".into()),
        bytes: build_pdf_bytes(&[""], false),
        etag: None,
        last_modified: None,
        metadata: Default::default(),
    };

    let error = PdfParser
        .parse(resource)
        .await
        .expect_err("PDF without extractable text should not silently pretend to be readable");

    assert!(error.to_string().contains("OCR"));
}

fn build_pdf_bytes(page_texts: &[&str], with_toc: bool) -> Vec<u8> {
    let mut document = PdfDocument::with_version("1.5");
    let pages_id = document.new_object_id();
    let font_id = document.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => "Helvetica",
    });
    let resources_id = document.add_object(dictionary! {
        "Font" => dictionary! {
            "F1" => font_id,
        },
    });

    let mut page_ids = Vec::new();
    for text in page_texts {
        let content = Content {
            operations: vec![
                Operation::new("BT", vec![]),
                Operation::new("Tf", vec!["F1".into(), 12.into()]),
                Operation::new("Td", vec![72.into(), 720.into()]),
                Operation::new("Tj", vec![Object::string_literal(*text)]),
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

    let kids = page_ids
        .iter()
        .copied()
        .map(Object::Reference)
        .collect::<Vec<_>>();
    document.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => kids,
            "Count" => page_ids.len() as i64,
            "Resources" => resources_id,
            "MediaBox" => vec![0.into(), 0.into(), 595.into(), 842.into()],
        }),
    );

    let catalog_id = document.add_object(dictionary! {
        "Type" => "Catalog",
        "Pages" => pages_id,
    });
    let info_id = document.add_object(dictionary! {
        "Title" => text_string("Operating Systems"),
    });
    document.trailer.set("Root", catalog_id);
    document.trailer.set("Info", info_id);

    if with_toc && page_ids.len() >= 3 {
        let virtual_memory = document.add_bookmark(
            Bookmark::new("Virtual Memory".into(), [0.0, 0.0, 0.0], 0, page_ids[0]),
            None,
        );
        document.add_bookmark(
            Bookmark::new("Page Tables".into(), [0.0, 0.0, 0.0], 0, page_ids[1]),
            Some(virtual_memory),
        );
        document.add_bookmark(
            Bookmark::new("Processes".into(), [0.0, 0.0, 0.0], 0, page_ids[2]),
            None,
        );

        if let Some(outline_id) = document.build_outline() {
            document
                .get_dictionary_mut(catalog_id)
                .expect("catalog should exist")
                .set("Outlines", outline_id);
        }
    }

    let mut bytes = Vec::new();
    document
        .save_to(&mut bytes)
        .expect("fixture PDF should serialize");
    bytes
}

// The v2 reading helpers: every reader below runs through the public v2 use cases.
async fn open_and_store(
    repository: &Arc<InMemoryDocumentRepository>,
    document_id: &DocumentId,
) -> (Arc<dyn TextUnitIndex>, Arc<reading_mcp::domain::Document>) {
    let keyed = Arc::new(KeyedTextUnitIndex::default());
    let document = repository
        .get(document_id)
        .await
        .expect("stored document")
        .expect("the opened document is stored");
    keyed.store(&document).await;
    let units: Arc<dyn TextUnitIndex> = keyed;
    (units, document)
}

async fn document_handle(
    units: &Arc<dyn TextUnitIndex>,
    document: &reading_mcp::domain::Document,
) -> DocumentHandle {
    DocumentHandle {
        doc_key: units
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

async fn section_handle(
    units: &Arc<dyn TextUnitIndex>,
    document: &reading_mcp::domain::Document,
    section_id: &str,
) -> SectionHandle {
    SectionHandle {
        document: document_handle(units, document).await,
        section_key: units
            .section_key(&document.id, &SectionId(section_id.into()))
            .await
            .expect("section key")
            .expect("stored section"),
    }
}

async fn read(
    repository: Arc<dyn DocumentRepository>,
    units: Arc<dyn TextUnitIndex>,
    from: &str,
    max_chars: Option<usize>,
) -> ReadReadingResult {
    ReadReadingUseCase::new(repository, units)
        .execute(ReadReadingCommand {
            from: from.to_string(),
            unit: RequestedTextUnitKind::Sentence,
            max_chars,
            direction: None,
            back: 0,
        })
        .await
        .expect("read")
}

fn items_text(result: &ReadReadingResult) -> String {
    result
        .items
        .iter()
        .map(|item| item.text.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}
