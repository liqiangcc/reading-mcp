use std::sync::Arc;

use lopdf::content::{Content, Operation};
use lopdf::{Bookmark, Document as PdfDocument, Object, Stream, dictionary, text_string};
use reading_mcp::application::get_document_structure::GetDocumentStructureUseCase;
use reading_mcp::application::open_document::{OpenDocumentCommand, OpenDocumentUseCase};
use reading_mcp::application::ports::{Parser, RetrievalOptions, RetrievedResource};
use reading_mcp::application::read_document::{ReadDocumentUseCase, ReadSectionCommand};
use reading_mcp::application::search_document::{SearchDocumentCommand, SearchDocumentUseCase};
use reading_mcp::domain::{DocumentSource, MediaType, SectionId};
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

    let structure = GetDocumentStructureUseCase::new(repository.clone())
        .execute(opened.document_id.clone(), None)
        .await
        .expect("PDF structure should be available");

    assert_eq!(structure.sections.len(), 2);
    assert_eq!(
        structure.sections[0].section_id.0,
        "section://virtual-memory"
    );
    assert_eq!(structure.sections[0].children.len(), 1);
    assert_eq!(
        structure.sections[0].children[0].section_id.0,
        "section://virtual-memory/page-tables"
    );
    assert_eq!(structure.sections[1].section_id.0, "section://processes");

    let searched = SearchDocumentUseCase::new(index)
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

    let read = ReadDocumentUseCase::new(repository)
        .execute(ReadSectionCommand {
            document_id: opened.document_id,
            section_id: SectionId("section://virtual-memory".into()),
            max_chars: None,
        })
        .await
        .expect("existing read use case should work for PDF");

    assert!(read.content.contains("Address spaces give each process"));
    assert!(read.content.contains("## Page Tables"));
    assert!(read.content.contains("physical frames"));
    assert_eq!(read.location.page, Some(1));
    assert_eq!(read.location.native_location.as_deref(), Some("pdf:page:1"));
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
