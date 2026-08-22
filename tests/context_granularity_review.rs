use std::collections::BTreeMap;
use std::sync::Arc;

use reading_mcp::application::get_context::{
    ContextItemKind, ContextRelation, ContextTarget, ContextUnit, GetContextUseCase,
    GetStructuredContextCommand,
};
use reading_mcp::application::get_text_units::{
    GetTextUnitsCommand, GetTextUnitsUseCase, RequestedTextUnitKind, TextUnitCoveragePolicy,
    TextUnitDirection,
};
use reading_mcp::application::ports::DocumentRepository;
use reading_mcp::domain::{
    ContentHash, Document, DocumentId, DocumentSource, Location, MediaType, Section, SectionId,
};
use reading_mcp::infrastructure::InMemoryDocumentRepository;

#[tokio::test]
async fn sentence_context_matches_preserve_source_enumeration_order_and_coarse_semantics() {
    let repository = Arc::new(InMemoryDocumentRepository::default());
    let document = document(
        "```rust\nfn main() { println!(\"Hi.\"); }\n```\n\nFirst prose sentence. Second prose sentence.",
    );
    repository
        .save(document.clone())
        .await
        .expect("document should save");

    let enumeration = GetTextUnitsUseCase::new(repository.clone())
        .execute(GetTextUnitsCommand {
            document_id: document.id.clone(),
            section_id: SectionId("section://root".into()),
            requested_kind: RequestedTextUnitKind::Sentence,
            direction: TextUnitDirection::Forward,
            coverage_policy: TextUnitCoveragePolicy::PreserveSource,
            max_items: 10,
            max_chars: None,
            cursor: None,
        })
        .await
        .expect("enumeration should succeed");
    assert_eq!(enumeration.items.len(), 3);
    assert_eq!(enumeration.items[0].effective_kind.as_str(), "paragraph");
    assert!(enumeration.items[0].degradation.is_some());

    let anchor = enumeration.items[1].locator.clone();
    let context = GetContextUseCase::new(repository)
        .execute_structured(GetStructuredContextCommand {
            document_id: document.id,
            target: ContextTarget::Locator(anchor),
            relation: ContextRelation::Neighbor {
                unit: ContextUnit::Sentence,
                before: 1,
                after: 1,
            },
            max_chars: None,
        })
        .await
        .expect("context should succeed");

    assert!(context.content.is_empty());
    assert_eq!(context.items.len(), enumeration.items.len());
    for (context_item, enumeration_item) in context.items.iter().zip(&enumeration.items) {
        assert_eq!(context_item.locator, enumeration_item.locator);
        assert_eq!(
            context_item.content.as_deref(),
            Some(enumeration_item.text.as_str())
        );
    }
    assert_eq!(context.items[0].effective_kind, ContextItemKind::Paragraph);
    assert!(context.items[0].degradation.is_some());
    assert_eq!(context.items[1].effective_kind, ContextItemKind::Sentence);
    assert_eq!(context.items[2].effective_kind, ContextItemKind::Sentence);
}

fn document(content: &str) -> Document {
    Document {
        id: DocumentId("doc:context-review".into()),
        source: DocumentSource("memory:context-review".into()),
        title: "Context review".into(),
        media_type: MediaType("text/plain".into()),
        content_hash: ContentHash("sha256:raw".into()),
        metadata: BTreeMap::new(),
        root_sections: vec![Section {
            id: SectionId("section://root".into()),
            parent_id: None,
            title: "Root".into(),
            level: 1,
            content: content.into(),
            location: Location {
                section_path: vec!["Root".into()],
                ..Location::default()
            },
            children: vec![],
        }],
    }
}
