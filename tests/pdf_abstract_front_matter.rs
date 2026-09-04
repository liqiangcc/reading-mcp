use lopdf::content::{Content, Operation};
use lopdf::{Document as PdfDocument, Object, Stream, dictionary};
use reading_mcp::application::ports::{Parser, RetrievedResource};
use reading_mcp::domain::{DocumentSource, MediaType, OriginalSourceTarget};
use reading_mcp::parsing::PdfParser;

#[tokio::test]
async fn reliable_abstract_heading_becomes_its_own_canonical_section() {
    let resource = pdf_resource(
        "memory:abstract-structure.pdf",
        build_pdf(true),
    );

    let parsed = PdfParser
        .parse(resource)
        .await
        .expect("front-matter PDF should parse");

    assert_eq!(
        parsed
            .metadata
            .get("pdf_front_matter_inference_version")
            .map(String::as_str),
        Some("pdf-front-matter-inference/v1")
    );
    assert_eq!(
        parsed
            .metadata
            .get("pdf_front_matter_abstract_count")
            .map(String::as_str),
        Some("1")
    );

    let titles = parsed
        .root_sections
        .iter()
        .map(|section| section.title.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        titles,
        vec!["Preamble", "Abstract", "1 Introduction", "2 Replication"]
    );

    let preamble = parsed
        .root_sections
        .iter()
        .find(|section| section.title == "Preamble")
        .expect("residual preamble should remain");
    assert!(preamble.content.contains("Conference paper title"));
    assert!(preamble.content.contains("Author Name"));
    assert!(!preamble.content.contains("Abstract body sentinel"));

    let abstract_section = parsed
        .root_sections
        .iter()
        .find(|section| section.title == "Abstract")
        .expect("Abstract should become canonical structure");
    assert_eq!(abstract_section.id.0, "section://abstract");
    assert!(abstract_section.content.contains("Abstract body sentinel"));
    assert!(!abstract_section.content.contains("Conference paper title"));
    assert!(!abstract_section.content.contains("1 Introduction"));

    let binding_map = parsed
        .original_source_binding_map()
        .expect("binding map should remain valid")
        .expect("PDF should retain source bindings");
    let abstract_binding = binding_map
        .bindings
        .iter()
        .find(|binding| binding.owner_section_id.0 == "section://abstract")
        .expect("Abstract should retain original-page evidence");
    assert_eq!(
        abstract_binding.target,
        OriginalSourceTarget::Page { page_number: 1 }
    );
}

#[tokio::test]
async fn lexical_abstract_without_distinct_layout_evidence_fails_closed() {
    let resource = pdf_resource(
        "memory:abstract-degraded.pdf",
        build_pdf(false),
    );

    let parsed = PdfParser
        .parse(resource)
        .await
        .expect("degraded front-matter PDF should parse");

    assert_eq!(
        parsed
            .metadata
            .get("pdf_front_matter_abstract_count")
            .map(String::as_str),
        Some("0")
    );
    assert!(
        parsed
            .root_sections
            .iter()
            .all(|section| section.id.0 != "section://abstract")
    );
    let preamble = parsed
        .root_sections
        .iter()
        .find(|section| section.title == "Preamble")
        .expect("coarse Preamble must remain on degradation");
    assert!(preamble.content.contains("Abstract"));
    assert!(preamble.content.contains("Abstract body sentinel"));
}

fn pdf_resource(source: &str, bytes: Vec<u8>) -> RetrievedResource {
    RetrievedResource {
        source: DocumentSource(source.into()),
        final_source: DocumentSource(source.into()),
        media_type: MediaType("application/pdf".into()),
        bytes,
        etag: None,
        last_modified: None,
        metadata: Default::default(),
    }
}

fn build_pdf(distinct_abstract_style: bool) -> Vec<u8> {
    let mut document = PdfDocument::with_version("1.5");
    let pages_id = document.new_object_id();
    let body_font = document.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => "Helvetica",
    });
    let heading_font = document.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => "Helvetica-Bold",
    });
    let resources_id = document.add_object(dictionary! {
        "Font" => dictionary! {
            "FBody" => body_font,
            "FHeading" => heading_font,
        },
    });

    let page_one = vec![
        text_line("FHeading", 16, 72, 760, "Conference paper title"),
        text_line("FBody", 10, 72, 736, "Author Name"),
        text_line(
            if distinct_abstract_style { "FHeading" } else { "FBody" },
            if distinct_abstract_style { 12 } else { 10 },
            72,
            700,
            "Abstract",
        ),
        text_line("FBody", 10, 72, 682, "Abstract body sentinel with enough text."),
        text_line("FHeading", 12, 72, 640, "1 Introduction"),
        text_line("FBody", 10, 72, 622, "Introduction body sentinel."),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>();
    let page_two = vec![
        text_line("FHeading", 12, 72, 740, "2 Replication"),
        text_line("FBody", 10, 72, 722, "Replication body sentinel."),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>();

    let mut page_ids = Vec::new();
    for operations in [page_one, page_two] {
        let content_id = document.add_object(Stream::new(
            dictionary! {},
            Content { operations }
                .encode()
                .expect("fixture content should encode"),
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

fn text_line(
    font: &str,
    size: i64,
    x: i64,
    y: i64,
    text: &str,
) -> Vec<Operation> {
    vec![
        Operation::new("BT", vec![]),
        Operation::new("Tf", vec![font.into(), size.into()]),
        Operation::new("Tm", vec![1.into(), 0.into(), 0.into(), 1.into(), x.into(), y.into()]),
        Operation::new("Tj", vec![Object::string_literal(text)]),
        Operation::new("ET", vec![]),
    ]
}
