use std::collections::BTreeMap;
use std::sync::Arc;

use lopdf::content::{Content, Operation};
use lopdf::{Document as PdfDocument, Object, Stream, dictionary};
use reading_mcp::application::get_text_units::{
    GetTextUnitsCommand, GetTextUnitsUseCase, RequestedTextUnitKind, TextUnitCoveragePolicy,
    TextUnitDirection,
};
use reading_mcp::application::open_document::{OpenDocumentCommand, OpenDocumentUseCase};
use reading_mcp::application::ports::{
    DocumentRepository, RetrievalOptions, RetrievedResource, SourceViewRenderOptions,
    SourceViewRenderer,
};
use reading_mcp::application::source_view::{
    GetSourceViewCommand, SourceViewLimits, SourceViewRepresentation, SourceViewUseCase,
};
use reading_mcp::domain::{DocumentSource, MediaType, SectionId};
use reading_mcp::infrastructure::InMemoryDocumentRepository;
use reading_mcp::parsing::{ParserRouter, PdfSourceViewRenderer};
use reading_mcp::retrieval::{FileRetriever, LocalFileSourcePolicy};
use tempfile::tempdir;

#[tokio::test]
async fn locator_resolves_to_original_pdf_page_and_renders_visual_fixture() {
    let directory = tempdir().expect("temporary directory should be created");
    let path = directory.path().join("fidelity.pdf");
    tokio::fs::write(&path, build_fidelity_pdf())
        .await
        .expect("PDF fixture should be written");

    let repository = Arc::new(InMemoryDocumentRepository::default());
    let opened = OpenDocumentUseCase::new(
        Arc::new(LocalFileSourcePolicy::allow_roots([directory.path()])),
        Arc::new(FileRetriever),
        Arc::new(ParserRouter::phase4()),
        repository.clone(),
        Arc::new(reading_mcp::infrastructure::InMemorySearchIndex::default()),
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
            section_id: SectionId("section://page-2".into()),
            requested_kind: RequestedTextUnitKind::Paragraph,
            direction: TextUnitDirection::Forward,
            coverage_policy: TextUnitCoveragePolicy::PreserveSource,
            max_items: 8,
            max_chars: None,
            cursor: None,
        })
        .await
        .expect("page text units should be available");
    let locator = units.items[0].locator.clone();

    let result = SourceViewUseCase::new(
        repository.clone(),
        Arc::new(FileRetriever),
        Arc::new(PdfSourceViewRenderer),
        SourceViewLimits::default(),
    )
    .execute(GetSourceViewCommand {
        document_id: opened.document_id.clone(),
        target_locator: locator.clone(),
        representation: SourceViewRepresentation::Original,
        dpi: Some(144),
    })
    .await
    .expect("source view should render");

    assert_eq!(result.page_number, 2);
    assert_eq!(result.page_count, 2);
    assert_eq!(result.target_locator, locator);
    assert_eq!(result.view.media_type.0, "image/png");
    assert_eq!(&result.view.bytes[..8], b"\x89PNG\r\n\x1a\n");
    let decoded = image::load_from_memory(&result.view.bytes).expect("result should be PNG");
    assert_eq!(decoded.width(), result.view.width);
    assert_eq!(decoded.height(), result.view.height);
    assert!(
        decoded
            .to_rgba8()
            .pixels()
            .any(|pixel| pixel.0 != [255, 255, 255, 255])
    );
}

#[tokio::test]
async fn source_view_fails_closed_when_source_bytes_change() {
    let directory = tempdir().expect("temporary directory should be created");
    let path = directory.path().join("fidelity.pdf");
    let original = build_fidelity_pdf();
    tokio::fs::write(&path, &original)
        .await
        .expect("PDF fixture should be written");

    let repository = Arc::new(InMemoryDocumentRepository::default());
    let opened = OpenDocumentUseCase::new(
        Arc::new(LocalFileSourcePolicy::allow_roots([directory.path()])),
        Arc::new(FileRetriever),
        Arc::new(ParserRouter::phase4()),
        repository.clone(),
        Arc::new(reading_mcp::infrastructure::InMemorySearchIndex::default()),
    )
    .execute(OpenDocumentCommand {
        source: DocumentSource(path.to_string_lossy().into_owned()),
        options: RetrievalOptions::default(),
    })
    .await
    .expect("PDF should open");
    let document = repository
        .get(&opened.document_id)
        .await
        .expect("repository read")
        .expect("document should be persisted");
    let locator =
        reading_mcp::domain::TextLocator::for_section(&document, &document.root_sections[1]);

    let mut changed = original;
    changed.extend_from_slice(b"changed source bytes");
    tokio::fs::write(&path, changed)
        .await
        .expect("changed source should be written");

    let error = SourceViewUseCase::new(
        repository,
        Arc::new(FileRetriever),
        Arc::new(PdfSourceViewRenderer),
        SourceViewLimits::default(),
    )
    .execute(GetSourceViewCommand {
        document_id: opened.document_id,
        target_locator: locator,
        representation: SourceViewRepresentation::Original,
        dpi: None,
    })
    .await
    .expect_err("changed source must not render through an old locator");
    assert!(matches!(
        error,
        reading_mcp::application::ports::ApplicationError::StaleLocator(_)
    ));
}

#[tokio::test]
async fn source_view_enforces_render_dimensions_before_allocating_image() {
    let resource = RetrievedResource {
        source: DocumentSource("memory:fidelity.pdf".into()),
        final_source: DocumentSource("memory:fidelity.pdf".into()),
        media_type: MediaType("application/pdf".into()),
        bytes: build_fidelity_pdf(),
        etag: None,
        last_modified: None,
        metadata: BTreeMap::new(),
    };
    let renderer = PdfSourceViewRenderer;
    let error = SourceViewRenderer::render(
        &renderer,
        resource.bytes,
        resource.media_type,
        1,
        SourceViewRenderOptions {
            dpi: 144,
            max_pages: 2_000,
            max_width: 100,
            max_height: 100,
            max_pixels: 10_000,
            max_image_bytes: 8 * 1024 * 1024,
            max_decoded_stream_bytes: 16 * 1024 * 1024,
        },
    )
    .expect_err("oversized source-view dimensions must be rejected");
    assert!(matches!(
        error,
        reading_mcp::application::ports::ApplicationError::ResourceLimitExceeded(_)
    ));
}

fn build_fidelity_pdf() -> Vec<u8> {
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

    for operations in [
        vec![
            Operation::new("BT", vec![]),
            Operation::new("Tf", vec!["F1".into(), 18.into()]),
            Operation::new("Td", vec![72.into(), 720.into()]),
            Operation::new("Tj", vec![Object::string_literal("LEFT COLUMN")]),
            Operation::new("ET", vec![]),
            Operation::new("BT", vec![]),
            Operation::new("Tf", vec!["F1".into(), 18.into()]),
            Operation::new("Td", vec![320.into(), 720.into()]),
            Operation::new("Tj", vec![Object::string_literal("RIGHT COLUMN")]),
            Operation::new("ET", vec![]),
        ],
        vec![
            Operation::new("BT", vec![]),
            Operation::new("Tf", vec!["F1".into(), 18.into()]),
            Operation::new("Td", vec![72.into(), 720.into()]),
            Operation::new("Tj", vec![Object::string_literal("Equation: E = mc^2")]),
            Operation::new("ET", vec![]),
            Operation::new("BT", vec![]),
            Operation::new("Tf", vec!["F1".into(), 14.into()]),
            Operation::new("Td", vec![100.into(), 440.into()]),
            Operation::new("Tj", vec![Object::string_literal("Figure 1")]),
            Operation::new("ET", vec![]),
            Operation::new("RG", vec![0.into(), 0.into(), 0.into()]),
            Operation::new("re", vec![72.into(), 500.into(), 180.into(), 120.into()]),
            Operation::new("S", vec![]),
            Operation::new("re", vec![300.into(), 500.into(), 220.into(), 120.into()]),
            Operation::new("S", vec![]),
            Operation::new("m", vec![410.into(), 500.into()]),
            Operation::new("l", vec![410.into(), 620.into()]),
            Operation::new("m", vec![300.into(), 560.into()]),
            Operation::new("l", vec![520.into(), 560.into()]),
            Operation::new("S", vec![]),
            Operation::new("BT", vec![]),
            Operation::new("Tf", vec!["F1".into(), 14.into()]),
            Operation::new("Td", vec![330.into(), 590.into()]),
            Operation::new("Tj", vec![Object::string_literal("TABLE 1")]),
            Operation::new("ET", vec![]),
        ],
    ] {
        let content = Content { operations };
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

    let mut bytes = Vec::new();
    document
        .save_to(&mut bytes)
        .expect("fixture PDF should serialize");
    bytes
}
