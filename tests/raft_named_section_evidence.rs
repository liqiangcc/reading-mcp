#[path = "support/keyed_index.rs"]
mod keyed_index;
use keyed_index::KeyedTextUnitIndex;

use std::sync::Arc;

use reading_mcp::application::get_text_units::{
    RequestedTextUnitKind, TextUnitCoveragePolicy, TextUnitDirection,
};
use reading_mcp::application::outline_reading::{
    MAX_OUTLINE_DEPTH, OutlineReadingCommand, OutlineReadingUseCase,
};
use reading_mcp::application::ports::{
    DocumentRepository, OwnedStreamUnit, Parser, RetrievedResource, TextUnitIndex,
    TextUnitStreamKey,
};
use reading_mcp::application::short_handle::{DocumentHandle, handle_checksum};
use reading_mcp::application::text_unit_store::store_text_units;
use reading_mcp::domain::{Document, DocumentSource, MediaType, NORMALIZATION_VERSION, Section};
use reading_mcp::infrastructure::{InMemoryDocumentRepository, InMemoryTextUnitIndex};
use reading_mcp::parsing::PdfParser;

const RAFT_URL: &str =
    "https://www.usenix.org/system/files/conference/atc14/atc14-paper-ongaro.pdf";
const BASELINE_DOCUMENT_ID: &str =
    "doc:sha256:6b910bccce5cabc0f7e14f4c131c361edc055fb5b0703b0a1aac2049a379bbdf";
const BASELINE_CONTENT_HASH: &str =
    "sha256:e6345fcba31cbc747ab41755aa62654859c4403dbb687da0021079f78181a7b5";
const V6_NORMALIZED_HASH: &str =
    "sha256:bced1dc57972b784215245749745ab33d34267463a451384c9372aa8e145432f";
const KAFKA_URL: &str = "https://www.microsoft.com/en-us/research/wp-content/uploads/2017/09/Kafka.pdf?msockid=34f2cedc4c716aeb0399dbe34d3b6bcf";
const KAFKA_CONTENT_HASH: &str =
    "sha256:4abdeba2503eb20a5d7ed84aa8e7680bcbe3088541712626315deae0b07c2821";

#[tokio::test]
#[ignore = "requires canonical Raft 2014 PDF evidence"]
async fn real_raft_named_section_scope_gate_is_structure_only_and_fail_closed() {
    let path = std::env::var("READING_MCP_RAFT_EVIDENCE_PDF")
        .expect("dedicated Raft evidence workflow must provide the downloaded PDF path");
    let bytes = tokio::fs::read(path)
        .await
        .expect("Raft evidence PDF should be readable");
    let document = PdfParser
        .parse(RetrievedResource {
            source: DocumentSource(RAFT_URL.into()),
            final_source: DocumentSource(RAFT_URL.into()),
            media_type: MediaType("application/pdf".into()),
            bytes,
            etag: None,
            last_modified: None,
            metadata: Default::default(),
        })
        .await
        .expect("real Raft PDF should parse");

    assert_eq!(document.id.0, BASELINE_DOCUMENT_ID);
    assert_eq!(document.content_hash.0, BASELINE_CONTENT_HASH);
    assert_eq!(NORMALIZATION_VERSION, "reading-mcp-normalization/v11");
    assert_eq!(
        document
            .metadata
            .get("pdf_structure_provenance")
            .map(String::as_str),
        Some("inferred_numbered_headings")
    );
    assert_eq!(
        document
            .metadata
            .get("pdf_front_matter_inference_version")
            .map(String::as_str),
        Some("pdf-front-matter-inference/v1")
    );
    assert_eq!(
        document
            .metadata
            .get("pdf_front_matter_abstract_count")
            .map(String::as_str),
        Some("1"),
        "real Raft must promote exactly one reliable front-matter Abstract; inference_status={:?}",
        document.metadata.get("pdf_front_matter_inference_status")
    );
    let abstract_section = document
        .root_sections
        .iter()
        .find(|section| section.id.0 == "section://abstract")
        .expect("real Raft must expose Abstract as canonical structure");
    assert_eq!(abstract_section.title, "Abstract");
    assert!(!abstract_section.content.trim().is_empty());
    assert!(!abstract_section.content.contains("1 Introduction"));

    let normalized_hash = document.normalized_document_hash();
    assert_ne!(normalized_hash.0, V6_NORMALIZED_HASH);
    println!(
        "EVIDENCE_A document_id={} content_hash={} normalized_document_hash={} normalization_version={}",
        document.id.0, document.content_hash.0, normalized_hash.0, NORMALIZATION_VERSION
    );

    // The numbered sections are consecutive in body order: the section after `1 Introduction`
    // (outside its subtree) starts with `2 `. The v1 boundary resolver is gone; the parsed tree
    // itself carries this evidence.
    let flat = preorder(&document.root_sections);
    let (position, (introduction, introduction_level)) = flat
        .iter()
        .enumerate()
        .find(|(_, (section, _))| section.title == "1 Introduction")
        .expect("real Raft must parse a `1 Introduction` section");
    let (next_section, _) = flat
        .iter()
        .skip(position + 1)
        .find(|(_, level)| level <= introduction_level)
        .expect("`1 Introduction` must have a next section after its subtree");
    assert!(
        next_section.title.starts_with("2 "),
        "the section after `1 Introduction` must be `2 ...`, got {:?}",
        next_section.title
    );
    println!(
        "EVIDENCE_BC introduction_section_id={} next_title={}",
        introduction.id.0, next_section.title
    );

    // Body reads: each section's stored sentence stream yields a non-empty sentence owned by
    // that section. This is the shared text-unit stream that v2 `read` serves.
    let index = Arc::new(InMemoryTextUnitIndex::default());
    store_text_units(&*index, &document).await.unwrap();

    let abstract_items = first_sentences(index.as_ref(), &document, abstract_section).await;
    assert_eq!(abstract_items.len(), 1);
    assert_eq!(abstract_items[0].0, abstract_section.id);
    assert!(!abstract_items[0].1.is_empty());
    println!("EVIDENCE_ABSTRACT_BODY explicit_abstract_body_reveal=PASS");

    let revealed = first_sentences(index.as_ref(), &document, introduction).await;
    assert_eq!(revealed.len(), 1);
    assert_eq!(revealed[0].0, introduction.id);
    assert!(!revealed[0].1.is_empty());
    println!("EVIDENCE_D explicit_allowed_body_reveal=PASS");
}

#[tokio::test]
#[ignore = "requires canonical Kafka 2011 PDF evidence"]
async fn real_kafka_pdf_regression_preserves_raw_identity_and_structure_navigation() {
    let path = std::env::var("READING_MCP_KAFKA_EVIDENCE_PDF")
        .expect("dedicated Kafka regression workflow must provide the downloaded PDF path");
    let bytes = tokio::fs::read(path)
        .await
        .expect("Kafka evidence PDF should be readable");
    let document = PdfParser
        .parse(RetrievedResource {
            source: DocumentSource(KAFKA_URL.into()),
            final_source: DocumentSource(KAFKA_URL.into()),
            media_type: MediaType("application/pdf".into()),
            bytes,
            etag: None,
            last_modified: None,
            metadata: Default::default(),
        })
        .await
        .expect("real Kafka PDF should still parse under normalization v8");

    assert_eq!(document.content_hash.0, KAFKA_CONTENT_HASH);
    assert_eq!(
        document.metadata.get("pdf_page_count").map(String::as_str),
        Some("7")
    );
    assert!(document.section_count() >= 7);
    assert!(matches!(
        document
            .metadata
            .get("pdf_structure_provenance")
            .map(String::as_str),
        Some("native_toc" | "inferred_numbered_headings" | "page_fallback")
    ));

    let repository = Arc::new(InMemoryDocumentRepository::default());
    repository
        .save(document.clone())
        .await
        .expect("Kafka canonical document should save");
    let index = Arc::new(KeyedTextUnitIndex::default());
    index.store(&document).await;
    let index: Arc<dyn TextUnitIndex> = index;

    // The v2 `outline` tool enumerates the whole tree: paging with a small limit must still
    // cover every section node, with the cursor ending cleanly (no truncation).
    let handle = DocumentHandle {
        doc_key: index
            .document_key(&document.id)
            .await
            .expect("document key")
            .expect("stored document"),
        checksum: handle_checksum(
            &document.id,
            &document.normalized_document_hash(),
            &document.media_type,
        ),
    };
    let outline = OutlineReadingUseCase::new(repository, index);
    let mut cursor = None;
    let mut listed = 0usize;
    loop {
        let page = outline
            .execute(OutlineReadingCommand {
                at: handle.to_string(),
                depth: MAX_OUTLINE_DEPTH,
                limit: 3,
                cursor: cursor.clone(),
            })
            .await
            .expect("Kafka canonical structure should remain enumerable through v2 outline");
        listed += page.items.len();
        match page.next {
            Some(next) => cursor = Some(next),
            None => break,
        }
    }
    assert_eq!(
        listed,
        document.section_count(),
        "v2 outline paging must cover every section without truncation"
    );

    println!(
        "EVIDENCE_F_KAFKA content_hash={} page_count=7 structure_nodes={} provenance={}",
        document.content_hash.0,
        listed,
        document
            .metadata
            .get("pdf_structure_provenance")
            .expect("PDF structure provenance should be recorded")
    );
}

/// The sections in document body order, tagged with their depth (roots are level 0).
fn preorder(sections: &[Section]) -> Vec<(&Section, usize)> {
    fn collect<'a>(sections: &'a [Section], level: usize, out: &mut Vec<(&'a Section, usize)>) {
        for section in sections {
            out.push((section, level));
            collect(&section.children, level + 1, out);
        }
    }
    let mut out = Vec::new();
    collect(sections, 0, &mut out);
    out
}

/// The first stored item(s) of one section's `PreserveSource` sentence stream, as
/// `(owner_section_id, text)`. A sentence stream can yield a whole paragraph when the paragraph
/// is one coarse unit; ownership and text carry the evidence either way.
async fn first_sentences(
    index: &dyn TextUnitIndex,
    document: &Document,
    section: &Section,
) -> Vec<(reading_mcp::domain::SectionId, String)> {
    let items = index
        .section_stream_items(
            TextUnitStreamKey {
                document_id: &document.id,
                section_id: &section.id,
                kind: RequestedTextUnitKind::Sentence,
                policy: TextUnitCoveragePolicy::PreserveSource,
            },
            TextUnitDirection::Forward,
            0,
            1,
        )
        .await
        .expect("stored section units should stream");
    items
        .into_iter()
        .map(|item| match item.unit {
            OwnedStreamUnit::Sentence(sentence) => (sentence.owner_section_id, sentence.text),
            OwnedStreamUnit::Paragraph(paragraph) => (paragraph.owner_section_id, paragraph.text),
        })
        .collect()
}
