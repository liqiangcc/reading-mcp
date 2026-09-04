use lopdf::content::{Content, Operation};
use lopdf::{Document as PdfDocument, Object, Stream, dictionary};
use reading_mcp::application::ports::{Parser, RetrievedResource};
use reading_mcp::domain::{DocumentSource, MediaType};
use reading_mcp::parsing::PdfParser;

#[tokio::test]
async fn pdf_without_native_toc_infers_coherent_numbered_heading_structure() {
    let resource = RetrievedResource {
        source: DocumentSource("memory:numbered.pdf".into()),
        final_source: DocumentSource("memory:numbered.pdf".into()),
        media_type: MediaType("application/pdf".into()),
        bytes: build_multiline_pdf(&[
            &[
                "Conference paper title",
                "Author Name",
                "1 Introduction",
                "Introduction body sentinel.",
                "1.1 Scope",
                "Scope body sentinel.",
            ],
            &["2 Replication", "Future body sentinel."],
        ]),
        etag: None,
        last_modified: None,
        metadata: Default::default(),
    };

    let parsed = PdfParser
        .parse(resource)
        .await
        .expect("coherent numbered headings should parse");

    assert_eq!(
        parsed
            .metadata
            .get("pdf_structure_provenance")
            .map(String::as_str),
        Some("inferred_numbered_headings")
    );
    assert_eq!(
        parsed
            .metadata
            .get("pdf_heading_inference_version")
            .map(String::as_str),
        Some("pdf-numbered-heading-inference/v1")
    );
    assert_eq!(parsed.root_sections[0].id.0, "section://preamble");
    assert!(
        parsed.root_sections[0]
            .content
            .contains("Conference paper title")
    );

    let introduction = parsed
        .root_sections
        .iter()
        .find(|section| section.title == "1 Introduction")
        .expect("top-level Introduction heading should become canonical structure");
    assert!(introduction.content.contains("Introduction body sentinel"));
    assert!(!introduction.content.contains("1 Introduction"));
    assert_eq!(introduction.location.page, Some(1));
    assert_eq!(introduction.children.len(), 1);
    assert_eq!(introduction.children[0].title, "1.1 Scope");
    assert!(
        introduction.children[0]
            .content
            .contains("Scope body sentinel")
    );

    let replication = parsed
        .root_sections
        .iter()
        .find(|section| section.title == "2 Replication")
        .expect("second top-level numbered heading should be canonical structure");
    assert_eq!(replication.location.page, Some(2));
    assert!(replication.content.contains("Future body sentinel"));
}

fn build_multiline_pdf(page_lines: &[&[&str]]) -> Vec<u8> {
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
    for lines in page_lines {
        let mut operations = vec![
            Operation::new("BT", vec![]),
            Operation::new("Tf", vec!["F1".into(), 12.into()]),
            Operation::new("TL", vec![18.into()]),
            Operation::new("Td", vec![72.into(), 720.into()]),
        ];
        for (index, line) in lines.iter().enumerate() {
            if index > 0 {
                operations.push(Operation::new("T*", vec![]));
            }
            operations.push(Operation::new("Tj", vec![Object::string_literal(*line)]));
        }
        operations.push(Operation::new("ET", vec![]));

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
