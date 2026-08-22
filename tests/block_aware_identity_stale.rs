use std::collections::BTreeMap;
use std::sync::Arc;

use reading_mcp::application::get_text_units::{
    GetTextUnitsCommand, GetTextUnitsUseCase, RequestedTextUnitKind, TextUnitCoveragePolicy,
    TextUnitDirection,
};
use reading_mcp::application::ports::{ApplicationError, DocumentRepository};
use reading_mcp::application::read_document::{ReadDocumentUseCase, ReadExactTargetCommand};
use reading_mcp::domain::{
    ContentHash, Document, DocumentId, DocumentSource, Location, MediaType, Section, SectionId,
    TextLocator,
};
use reading_mcp::infrastructure::InMemoryDocumentRepository;
use serde::Serialize;
use sha2::{Digest, Sha256};

#[tokio::test]
async fn v1_paragraph_locator_is_stale_even_when_current_range_still_matches() {
    let document = fixture();
    let paragraph = document.paragraph_text_units().units[0].clone();
    let section = &document.root_sections[0];
    let mut locator = TextLocator::for_paragraph(&document, section, &paragraph);
    locator.segmentation_version = Some("text-segmentation/v1".into());

    let repository = Arc::new(InMemoryDocumentRepository::default());
    repository.save(document.clone()).await.expect("save");
    let error = ReadDocumentUseCase::new(repository)
        .read_exact(ReadExactTargetCommand {
            document_id: document.id,
            target_locator: locator,
            max_chars: None,
        })
        .await
        .expect_err("v1 locator must never be reinterpreted as v2");

    assert!(matches!(error, ApplicationError::StaleLocator(_)));
}

#[tokio::test]
async fn v1_text_unit_cursor_is_stale_before_it_can_resume_a_v2_stream() {
    let document = fixture();
    let section_id = document.root_sections[0].id.clone();
    let cursor = encode_old_v1_cursor(CursorClaims {
        schema_version: "text-unit-cursor/v1".into(),
        document_id: document.id.0.clone(),
        content_hash: document.content_hash.0.clone(),
        normalized_document_hash: document.normalized_document_hash().0,
        section_id: section_id.0.clone(),
        segmentation_version: "text-segmentation/v1".into(),
        requested_kind: "paragraph".into(),
        direction: "forward".into(),
        coverage_policy: "preserve_source".into(),
        next_index: 1,
        total_items: 2,
    });

    let repository = Arc::new(InMemoryDocumentRepository::default());
    repository.save(document.clone()).await.expect("save");
    let error = GetTextUnitsUseCase::new(repository)
        .execute(GetTextUnitsCommand {
            document_id: document.id,
            section_id,
            requested_kind: RequestedTextUnitKind::Paragraph,
            direction: TextUnitDirection::Forward,
            coverage_policy: TextUnitCoveragePolicy::PreserveSource,
            max_items: 1,
            max_chars: None,
            cursor: Some(cursor),
        })
        .await
        .expect_err("v1 cursor must never resume v2 enumeration");

    assert!(matches!(error, ApplicationError::StaleCursor(_)));
}

#[derive(Serialize)]
struct CursorClaims {
    schema_version: String,
    document_id: String,
    content_hash: String,
    normalized_document_hash: String,
    section_id: String,
    segmentation_version: String,
    requested_kind: String,
    direction: String,
    coverage_policy: String,
    next_index: usize,
    total_items: usize,
}

#[derive(Serialize)]
struct CursorEnvelope {
    claims: CursorClaims,
    checksum: String,
}

fn encode_old_v1_cursor(claims: CursorClaims) -> String {
    let claims_bytes = serde_json::to_vec(&claims).expect("claims JSON");
    let mut hasher = Sha256::new();
    hasher.update(b"reading-mcp/text-unit-cursor-checksum/v1\0");
    hasher.update(&claims_bytes);
    let envelope = CursorEnvelope {
        claims,
        checksum: format!("sha256:{:x}", hasher.finalize()),
    };
    let bytes = serde_json::to_vec(&envelope).expect("envelope JSON");
    format!("tuc1.{}", encode_hex(&bytes))
}

fn encode_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(char::from(HEX[usize::from(byte >> 4)]));
        output.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    output
}

fn fixture() -> Document {
    Document {
        id: DocumentId("doc:stale-v1".into()),
        source: DocumentSource("memory:stale-v1".into()),
        title: "Stale identity".into(),
        media_type: MediaType("text/plain".into()),
        content_hash: ContentHash("sha256:stale-v1".into()),
        metadata: BTreeMap::new(),
        root_sections: vec![Section {
            id: SectionId("section://root".into()),
            parent_id: None,
            title: "Root".into(),
            level: 1,
            content: "First paragraph.\n\nSecond paragraph.".into(),
            location: Location::default(),
            children: vec![],
        }],
    }
}
