use std::sync::Arc;

use reading_mcp::application::get_context::{GetContextCommand, GetContextUseCase};
use reading_mcp::application::get_document_structure::{
    GetDocumentStructureUseCase, SectionOutline,
};
use reading_mcp::application::ports::DocumentRepository;
use reading_mcp::application::read_document::{ReadDocumentUseCase, ReadSectionCommand};
use reading_mcp::domain::{
    ContentHash, Document, DocumentId, DocumentSource, Location, MediaType, Section, SectionId,
};
use reading_mcp::infrastructure::InMemoryDocumentRepository;
use reading_mcp::mcp::contracts::GetDocumentStructureResponse;

const DEFAULT_CONTENT_RESPONSE_CHARS: usize = 32_000;
const MAX_CONTENT_RESPONSE_CHARS: usize = 64_000;
const MAX_STRUCTURE_RESPONSE_NODES: usize = 1_000;

#[tokio::test]
async fn read_document_applies_default_budget_when_max_chars_is_omitted() {
    let repository = populated_repository(document_with_content(70_000)).await;
    let use_case = ReadDocumentUseCase::new(repository);

    let result = use_case
        .execute(ReadSectionCommand {
            document_id: DocumentId("doc:budget".into()),
            section_id: SectionId("section://root".into()),
            max_chars: None,
        })
        .await
        .expect("read should succeed");

    assert_eq!(result.content.chars().count(), DEFAULT_CONTENT_RESPONSE_CHARS);
    assert!(result.truncated);
}

#[tokio::test]
async fn read_document_caps_explicit_budget_at_server_hard_limit() {
    let repository = populated_repository(document_with_content(100_000)).await;
    let use_case = ReadDocumentUseCase::new(repository);

    let result = use_case
        .execute(ReadSectionCommand {
            document_id: DocumentId("doc:budget".into()),
            section_id: SectionId("section://root".into()),
            max_chars: Some(1_000_000),
        })
        .await
        .expect("read should succeed");

    assert_eq!(result.content.chars().count(), MAX_CONTENT_RESPONSE_CHARS);
    assert!(result.truncated);
}

#[tokio::test]
async fn get_context_caps_explicit_budget_at_server_hard_limit() {
    let repository = populated_repository(document_with_content(100_000)).await;
    let use_case = GetContextUseCase::new(repository);

    let result = use_case
        .execute(GetContextCommand {
            document_id: DocumentId("doc:budget".into()),
            section_id: SectionId("section://root".into()),
            before: 0,
            after: 0,
            max_chars: Some(1_000_000),
        })
        .await
        .expect("context should succeed");

    assert_eq!(result.content.chars().count(), MAX_CONTENT_RESPONSE_CHARS);
    assert!(result.truncated);
}

#[tokio::test]
async fn document_structure_is_bounded_by_server_node_limit() {
    let repository = populated_repository(document_with_sections(1_500)).await;
    let use_case = GetDocumentStructureUseCase::new(repository);

    let result = use_case
        .execute(DocumentId("doc:budget".into()), None)
        .await
        .expect("structure should succeed");

    assert_eq!(count_outline_nodes(&result.sections), MAX_STRUCTURE_RESPONSE_NODES);
}

#[test]
fn document_structure_contract_exposes_truncation_metadata() {
    let schema = schemars::schema_for!(GetDocumentStructureResponse);
    let schema = serde_json::to_value(schema).expect("schema should serialize");
    let properties = schema
        .get("properties")
        .and_then(|value| value.as_object())
        .expect("response schema should expose object properties");

    assert!(
        properties.contains_key("truncated"),
        "get_document_structure must tell clients when the server response budget truncated the tree"
    );
}

async fn populated_repository(document: Document) -> Arc<dyn DocumentRepository> {
    let repository = Arc::new(InMemoryDocumentRepository::default());
    repository
        .save(document)
        .await
        .expect("fixture document should be saved");
    repository
}

fn document_with_content(char_count: usize) -> Document {
    Document {
        id: DocumentId("doc:budget".into()),
        source: DocumentSource("memory:budget.md".into()),
        title: "Budget".into(),
        media_type: MediaType("text/markdown".into()),
        content_hash: ContentHash("sha256:budget".into()),
        metadata: Default::default(),
        root_sections: vec![Section {
            id: SectionId("section://root".into()),
            parent_id: None,
            title: "Root".into(),
            level: 1,
            content: "x".repeat(char_count),
            location: Location::default(),
            children: vec![],
        }],
    }
}

fn document_with_sections(section_count: usize) -> Document {
    let root_sections = (0..section_count)
        .map(|index| Section {
            id: SectionId(format!("section://{index}")),
            parent_id: None,
            title: format!("Section {index}"),
            level: 1,
            content: String::new(),
            location: Location::default(),
            children: vec![],
        })
        .collect();

    Document {
        id: DocumentId("doc:budget".into()),
        source: DocumentSource("memory:budget.md".into()),
        title: "Budget".into(),
        media_type: MediaType("text/markdown".into()),
        content_hash: ContentHash("sha256:budget".into()),
        metadata: Default::default(),
        root_sections,
    }
}

fn count_outline_nodes(sections: &[SectionOutline]) -> usize {
    sections
        .iter()
        .map(|section| 1 + count_outline_nodes(&section.children))
        .sum()
}
