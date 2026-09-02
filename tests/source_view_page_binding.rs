use std::sync::Arc;

use lopdf::content::{Content, Operation};
use lopdf::{Bookmark, Document as PdfDocument, Object, Stream, dictionary};
use reading_mcp::application::get_text_units::{
    GetTextUnitsCommand, GetTextUnitsUseCase, RequestedTextUnitKind, TextUnitCoveragePolicy,
    TextUnitDirection,
};
use reading_mcp::application::open_document::{OpenDocumentCommand, OpenDocumentUseCase};
use reading_mcp::application::ports::RetrievalOptions;
use reading_mcp::application::source_view::{
    GetSourceViewCommand, SourceViewLimits, SourceViewRepresentation, SourceViewUseCase,
};
use reading_mcp::domain::{DocumentSource, SectionId, ORIGINAL_SOURCE_BINDING_MODEL_VERSION};
use reading_mcp::infrastructure::{InMemoryDocumentRepository, InMemorySearchIndex};
use reading_mcp::parsing::{ParserRouter, PdfSourceViewRenderer};
use reading_mcp::retrieval::{FileRetriever, LocalFileSourcePolicy};

#[tokio::test]
async fn sentence_on_second_page_of_multi_page_toc_section_renders_page_two() {
    let directory = tempfile::tempdir().expect("temporary directory should be created");
    let path = directory.path().join("multi-page-section.pdf");
    tokio::fs::write(&path, build_multi_page_toc_pdf())
        .await
        .expect("PDF fixture should be written");

    let repository = Arc::new(InMemoryDocumentRepository::default());
    let opened = OpenDocumentUseCase::new(
        Arc::new(LocalFileSourcePolicy::allow_roots([directory.path()])),
        Arc::new(FileRetriever),
        Arc::new(ParserRouter::phase4()),
        repository.clone(),
        Arc::new(InMemorySearchIndex::default()),
    )
    .execute(OpenDocumentCommand {
        source: DocumentSource(path.to_string_lossy().into_owned()),
        options: RetrievalOptions::default(),
    })
    .await
    .expect("PDF should open");

    let units = GetTextUnitsUseCase::new(repository.clone())
        .execute(GetTextUnitsCommand {
            document_id: opened.document_id.clone(),
            section_id: SectionId("section://chapter-a".into()),
            requested_kind: RequestedTextUnitKind::Sentence,
            direction: TextUnitDirection::Forward,
            coverage_policy: TextUnitCoveragePolicy::PreserveSource,
            max_items: 10,
            max_chars: None,
            cursor: None,
        })
        .await
        .expect("multi-page section sentences should be available");
    let second_page = units
        .items
        .iter()
        .find(|item| item.text.contains("SECOND PAGE SENTENCE"))
        .expect("second-page sentence should be represented")
        .locator
        .clone();

    let result = SourceViewUseCase::new(
        repository,
        Arc::new(FileRetriever),
        Arc::new(PdfSourceViewRenderer),
        SourceViewLimits::default(),
    )
    .execute(GetSourceViewCommand {
        document_id: opened.document_id,
        target_locator: second_page,
        representation: SourceViewRepresentation::Original,
        dpi: Some(72),
    })
    .await
    .expect("second-page locator should resolve to its actual original page");

    assert_eq!(result.page_number, 2);
    assert_eq!(result.page_count, 3);
    assert_eq!(result.source_binding_version, ORIGINAL_SOURCE_BINDING_MODEL_VERSION);
}

fn build_multi_page_toc_pdf() -> Vec<u8> {
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

    for text in [
        "FIRST PAGE SENTENCE.",
        "SECOND PAGE SENTENCE.",
        "THIRD PAGE SENTENCE.",
    ] {
        let content = Content {
            operations: vec![
                Operation::new("BT", vec![]),
                Operation::new("Tf", vec!["F1".into(), 14.into()]),
                Operation::new("Td", vec![72.into(), 720.into()]),
                Operation::new("Tj", vec![Object::string_literal(text)]),
                Operation::new("ET", vec![]),
            ],
        };
        let content_id = document.add_object(Stream::new(
            dictionary! {},
            content.encode().expect("fixture content should encode"),
        ));
        page_ids.push(document.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "Contents" => content_id,
        }));
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

    document.add_bookmark(
        Bookmark::new("Chapter A".into(), [0.0, 0.0, 0.0], 0, page_ids[0]),
        None,
    );
    document.add_bookmark(
        Bookmark::new("Chapter B".into(), [0.0, 0.0, 0.0], 0, page_ids[2]),
        None,
    );
    if let Some(outline_id) = document.build_outline() {
        document
            .get_dictionary_mut(catalog_id)
            .expect("catalog should exist")
            .set("Outlines", outline_id);
    }

    let mut bytes = Vec::new();
    document
        .save_to(&mut bytes)
        .expect("fixture PDF should serialize");
    bytes
}
