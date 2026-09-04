use std::sync::Arc;

use reading_mcp::application::get_document_structure::{
    GetDocumentStructureUseCase, NAMED_SECTION_RESOLUTION_VERSION, NamedSectionResolutionStatus,
    ResolveNamedSectionCommand,
};
use reading_mcp::application::get_text_units::{
    GetTextUnitsCommand, GetTextUnitsUseCase, RequestedTextUnitKind, TextUnitCoveragePolicy,
    TextUnitDirection,
};
use reading_mcp::application::ports::{
    ApplicationError, DocumentRepository, Parser, RetrievedResource,
};
use reading_mcp::domain::{DocumentSource, MediaType};
use reading_mcp::infrastructure::InMemoryDocumentRepository;
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
    assert_eq!(
        document
            .metadata
            .get("pdf_structure_provenance")
            .map(String::as_str),
        Some("inferred_numbered_headings")
    );
    let normalized_hash = document.normalized_document_hash();
    assert_ne!(normalized_hash.0, V6_NORMALIZED_HASH);
    println!(
        "EVIDENCE_A document_id={} content_hash={} normalized_document_hash={}",
        document.id.0, document.content_hash.0, normalized_hash.0
    );

    let repository = Arc::new(InMemoryDocumentRepository::default());
    repository
        .save(document.clone())
        .await
        .expect("Raft canonical document should save");
    let structure = GetDocumentStructureUseCase::new(repository.clone());

    let mut resolved_section_id = None;
    let mut resolved_boundary = None;
    for query in ["1 Introduction", "Section 1 Introduction", "Introduction"] {
        let result = structure
            .resolve_named_section(ResolveNamedSectionCommand {
                document_id: document.id.clone(),
                query: query.into(),
                expected_content_hash: document.content_hash.0.clone(),
                expected_normalized_document_hash: normalized_hash.0.clone(),
                expected_structure_resolution_version: Some(
                    NAMED_SECTION_RESOLUTION_VERSION.into(),
                ),
            })
            .await
            .expect("Raft Section 1 should resolve structurally");
        assert_eq!(
            result.resolution.status,
            NamedSectionResolutionStatus::Resolved
        );
        let matched = result
            .resolution
            .matched
            .expect("resolved Raft structural metadata should be present");
        assert_eq!(matched.title, "1 Introduction");
        assert!(matched.start_locator.normalized_range.is_none());
        if let Some(expected) = &resolved_section_id {
            assert_eq!(&matched.section_id, expected);
        } else {
            resolved_section_id = Some(matched.section_id.clone());
        }
        let boundary = result
            .resolution
            .boundary
            .expect("Raft Section 1 should have an executable boundary");
        let next = boundary
            .end_exclusive
            .as_ref()
            .expect("Raft Section 1 should have a next body owner");
        assert!(next.title.starts_with("2 "));
        assert!(
            boundary
                .intervals
                .iter()
                .all(|interval| next.body_order < interval.start || next.body_order >= interval.end)
        );
        resolved_boundary = Some(boundary);
    }
    let section_id = resolved_section_id.expect("all queries should resolve the same Section 1");
    let boundary = resolved_boundary.expect("Raft scope boundary should be captured");
    println!(
        "EVIDENCE_BC section_id={} intervals={:?} end_exclusive_body_order={}",
        section_id.0,
        boundary
            .intervals
            .iter()
            .map(|interval| (interval.start, interval.end))
            .collect::<Vec<_>>(),
        boundary
            .end_exclusive
            .as_ref()
            .map(|next| next.body_order)
            .expect("next owner should exist")
    );

    let stale = structure
        .resolve_named_section(ResolveNamedSectionCommand {
            document_id: document.id.clone(),
            query: "Introduction".into(),
            expected_content_hash: document.content_hash.0.clone(),
            expected_normalized_document_hash: V6_NORMALIZED_HASH.into(),
            expected_structure_resolution_version: Some(NAMED_SECTION_RESOLUTION_VERSION.into()),
        })
        .await
        .expect_err("v6 normalized identity must fail closed after normalization v7");
    assert!(matches!(stale, ApplicationError::StaleStructure(_)));
    println!("EVIDENCE_E stale_v6_identity=PASS");

    let revealed = GetTextUnitsUseCase::new(repository)
        .execute(GetTextUnitsCommand {
            document_id: document.id,
            section_id: section_id.clone(),
            requested_kind: RequestedTextUnitKind::Sentence,
            direction: TextUnitDirection::Forward,
            coverage_policy: TextUnitCoveragePolicy::PreserveSource,
            max_items: 1,
            max_chars: None,
            cursor: None,
        })
        .await
        .expect("explicit allowed-scope body reveal should remain available");
    assert_eq!(revealed.items.len(), 1);
    assert_eq!(revealed.items[0].locator.owner_section_id, section_id);
    assert!(!revealed.items[0].text.is_empty());
    println!("EVIDENCE_D explicit_allowed_body_reveal=PASS");
}

#[tokio::test]
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
        .expect("real Kafka PDF should still parse under normalization v7");

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
    let result = GetDocumentStructureUseCase::new(repository)
        .execute(document.id.clone(), None)
        .await
        .expect("Kafka canonical structure should remain enumerable");
    assert!(result.complete);
    assert!(!result.truncated);
    assert_eq!(result.stream.total_nodes, document.section_count());
    assert!(!result.sections.is_empty());

    println!(
        "EVIDENCE_F_KAFKA content_hash={} page_count=7 structure_nodes={} provenance={}",
        document.content_hash.0,
        result.stream.total_nodes,
        document
            .metadata
            .get("pdf_structure_provenance")
            .expect("PDF structure provenance should be recorded")
    );
}
