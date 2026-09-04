use std::sync::Arc;

use reading_mcp::application::get_document_structure::{
    GetDocumentStructureCommand, GetDocumentStructureUseCase,
};
use reading_mcp::application::ports::{DocumentRepository, Parser, RetrievedResource};
use reading_mcp::domain::{DocumentSource, MediaType};
use reading_mcp::infrastructure::InMemoryDocumentRepository;
use reading_mcp::parsing::PdfParser;

const KAFKA_SOURCE: &str = "https://raw.githubusercontent.com/liqiangcc/classic-papers-system-design/9469b1f0e83bfd3d5c59d15c6bfe42074139320e/sources/kafka/kafka-2011-distributed-messaging/paper.pdf";
const BASELINE_CONTENT_HASH: &str =
    "sha256:4abdeba2503eb20a5d7ed84aa8e7680bcbe3088541712626315deae0b07c2821";

#[tokio::test]
async fn real_kafka_pdf_remains_structurally_navigable_after_issue69_upgrade() {
    let path = std::env::var("READING_MCP_KAFKA_EVIDENCE_PDF")
        .expect("dedicated Kafka regression workflow must provide the downloaded PDF path");
    let bytes = tokio::fs::read(path)
        .await
        .expect("Kafka evidence PDF should be readable");
    let document = PdfParser
        .parse(RetrievedResource {
            source: DocumentSource(KAFKA_SOURCE.into()),
            final_source: DocumentSource(KAFKA_SOURCE.into()),
            media_type: MediaType("application/pdf".into()),
            bytes,
            etag: None,
            last_modified: None,
            metadata: Default::default(),
        })
        .await
        .expect("real Kafka PDF should remain parseable");

    assert_eq!(document.content_hash.0, BASELINE_CONTENT_HASH);
    assert_eq!(
        document.metadata.get("pdf_page_count").map(String::as_str),
        Some("7")
    );
    assert!(document.section_count() > 0);

    let repository = Arc::new(InMemoryDocumentRepository::default());
    repository
        .save(document.clone())
        .await
        .expect("Kafka canonical document should save");
    let structure = GetDocumentStructureUseCase::new(repository)
        .execute_command(GetDocumentStructureCommand {
            document_id: document.id,
            root_section_id: None,
            max_depth: None,
            max_nodes: Some(1000),
            cursor: None,
        })
        .await
        .expect("Kafka structure navigation should remain available");

    assert!(structure.stream.total_nodes > 0);
    assert_eq!(structure.stream.end_index, structure.stream.total_nodes);
    assert!(structure.complete);
    println!(
        "EVIDENCE_F_KAFKA content_hash={} structure_nodes={} provenance={}",
        BASELINE_CONTENT_HASH,
        structure.stream.total_nodes,
        document
            .metadata
            .get("pdf_structure_provenance")
            .map(String::as_str)
            .unwrap_or("unknown")
    );
}
