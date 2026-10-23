use std::sync::Arc;

use lopdf::content::{Content, Operation};
use lopdf::{Dictionary, Document as PdfDocument, Object, Stream, dictionary};
use reading_mcp::application::open_document::{OpenDocumentCommand, OpenDocumentUseCase};
use reading_mcp::application::ports::ApplicationError;
use reading_mcp::domain::DocumentSource;
use reading_mcp::infrastructure::{InMemorySearchIndex, SqliteOpenResultStore};
use reading_mcp::parsing::{ArchiveLimits, ParserRouter};
use reading_mcp::retrieval::{FileRetriever, LocalFileSourcePolicy};
use tempfile::tempdir;

fn image_only_pdf() -> Vec<u8> {
    let mut document = PdfDocument::with_version("1.5");
    let pages_id = document.new_object_id();
    let image = document.add_object(Stream::new(
        dictionary! {
            "Type" => "XObject",
            "Subtype" => "Image",
            "Width" => 1_i64,
            "Height" => 1_i64,
            "ColorSpace" => "DeviceGray",
            "BitsPerComponent" => 8_i64,
        },
        vec![0_u8],
    ));
    let operations = vec![
        Operation::new("q", vec![]),
        Operation::new(
            "cm",
            vec![
                595.into(),
                0.into(),
                0.into(),
                842.into(),
                0.into(),
                0.into(),
            ],
        ),
        Operation::new("Do", vec![Object::Name(b"Im1".to_vec())]),
        Operation::new("Q", vec![]),
    ];
    let content = document.add_object(Stream::new(
        Dictionary::new(),
        Content { operations }.encode().expect("page content"),
    ));
    let page = document.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => pages_id,
        "Contents" => content,
        "Resources" => dictionary! { "XObject" => dictionary! { "Im1" => image } },
        "MediaBox" => vec![0.into(), 0.into(), 595.into(), 842.into()],
    });
    document.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => vec![Object::Reference(page)],
            "Count" => 1_i64,
        }),
    );
    let catalog = document.add_object(dictionary! {
        "Type" => "Catalog",
        "Pages" => pages_id,
    });
    document.trailer.set("Root", catalog);
    let mut bytes = Vec::new();
    document.save_to(&mut bytes).expect("pdf saves");
    bytes
}

#[tokio::test]
async fn an_image_only_pdf_reports_ocr_required_again_on_reopen() {
    let directory = tempdir().expect("temp directory");
    let path = directory.path().join("scan.pdf");
    tokio::fs::write(&path, image_only_pdf())
        .await
        .expect("fixture written");
    let store = Arc::new(
        SqliteOpenResultStore::open(directory.path().join("state.sqlite")).expect("open store"),
    );
    let use_case = OpenDocumentUseCase::new(
        Arc::new(LocalFileSourcePolicy::allow_roots([directory.path()])),
        Arc::new(FileRetriever),
        Arc::new(ParserRouter::release(1_000, ArchiveLimits::default())),
        Arc::new(reading_mcp::infrastructure::InMemoryDocumentRepository::default()),
        Arc::new(InMemorySearchIndex::default()),
    )
    .with_open_result_store(store, "test-fingerprint".into());
    let command = || OpenDocumentCommand {
        source: DocumentSource(path.to_string_lossy().into_owned()),
        options: Default::default(),
    };

    let first = use_case
        .execute(command())
        .await
        .expect_err("an image-only PDF must not open");
    assert!(matches!(first, ApplicationError::OcrRequired(_)));
    let second = use_case
        .execute(command())
        .await
        .expect_err("a reopen must report the same failure, not flap to ok");
    assert!(matches!(second, ApplicationError::OcrRequired(_)));
    assert_eq!(first.to_string(), second.to_string());
}
