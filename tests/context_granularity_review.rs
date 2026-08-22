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
use reading_mcp::application::ports::{DocumentRepository, Parser, RetrievedResource};
use reading_mcp::domain::{
    ContentHash, Document, DocumentId, DocumentSource, Location, MediaType, Section, SectionId,
};
use reading_mcp::infrastructure::InMemoryDocumentRepository;
use reading_mcp::parsing::HtmlParser;

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

#[tokio::test]
async fn native_block_context_preserves_enumeration_content_class_and_degradation_evidence() {
    let repository = Arc::new(InMemoryDocumentRepository::default());
    let document = HtmlParser
        .parse(RetrievedResource {
            source: DocumentSource("memory:context-native.html".into()),
            final_source: DocumentSource("memory:context-native.html".into()),
            media_type: MediaType("text/html".into()),
            bytes: br#"<html><body>
<h1>Chapter</h1>
<blockquote><p>Quote one.</p><p>Quote two.</p></blockquote>
<p>First. Second.</p>
<pre>code. next.</pre>
</body></html>"#
                .to_vec(),
            etag: None,
            last_modified: None,
            metadata: Default::default(),
        })
        .await
        .expect("HTML should parse");
    let section_id = document.root_sections[0].id.clone();
    repository
        .save(document.clone())
        .await
        .expect("document should save");

    let enumeration = GetTextUnitsUseCase::new(repository.clone())
        .execute(GetTextUnitsCommand {
            document_id: document.id.clone(),
            section_id,
            requested_kind: RequestedTextUnitKind::Sentence,
            direction: TextUnitDirection::Forward,
            coverage_policy: TextUnitCoveragePolicy::PreserveSource,
            max_items: 10,
            max_chars: None,
            cursor: None,
        })
        .await
        .expect("enumeration should succeed");
    assert_eq!(enumeration.items.len(), 4);
    assert_eq!(
        enumeration.items[0].degradation.as_deref(),
        Some("flat_native_container_no_nested_textunit_evidence")
    );
    assert_eq!(enumeration.items[1].content_class_detail, "native_paragraph");
    assert_eq!(enumeration.items[2].content_class_detail, "native_paragraph");
    assert_eq!(
        enumeration.items[3].degradation.as_deref(),
        Some("requested_sentence_but_non_prose_is_paragraph_only")
    );

    let context = GetContextUseCase::new(repository)
        .execute_structured(GetStructuredContextCommand {
            document_id: document.id,
            target: ContextTarget::Locator(enumeration.items[1].locator.clone()),
            relation: ContextRelation::Neighbor {
                unit: ContextUnit::Sentence,
                before: 1,
                after: 2,
            },
            max_chars: None,
        })
        .await
        .expect("context should succeed");

    assert_eq!(context.items.len(), enumeration.items.len());
    for (context_item, enumeration_item) in context.items.iter().zip(&enumeration.items) {
        assert_eq!(context_item.locator, enumeration_item.locator);
        assert_eq!(
            context_item.content.as_deref(),
            Some(enumeration_item.text.as_str())
        );
        assert_eq!(
            context_item.content_class.as_deref(),
            Some(enumeration_item.content_class_detail.as_str())
        );
        let expected_degradation = match enumeration_item.degradation.as_deref() {
            Some("requested_sentence_but_non_prose_is_paragraph_only") => {
                Some("requested_sentence_context_but_non_prose_is_paragraph_only")
            }
            other => other,
        };
        assert_eq!(context_item.degradation.as_deref(), expected_degradation);
    }
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
