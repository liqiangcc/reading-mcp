use std::collections::BTreeMap;
use std::sync::Arc;

use reading_mcp::application::get_context::{
    ContextContainerKind, ContextItemKind, ContextItemRole, ContextRelation, ContextTarget,
    ContextUnit, GetContextCommand, GetContextUseCase, GetStructuredContextCommand,
    StructuralContextKind,
};
use reading_mcp::application::ports::{ApplicationError, DocumentRepository};
use reading_mcp::domain::{
    ContentHash, Document, DocumentId, DocumentSource, Location, MediaType, Section, SectionId,
    TextLocator,
};
use reading_mcp::infrastructure::InMemoryDocumentRepository;

#[tokio::test]
async fn sentence_neighbor_and_container_are_locator_driven() {
    let repository = Arc::new(InMemoryDocumentRepository::default());
    let document = document_fixture();
    repository.save(document.clone()).await.expect("save");
    let use_case = GetContextUseCase::new(repository);

    let section = document
        .find_section(&SectionId("section://root/topic-a".into()))
        .expect("topic A");
    let sentence = document
        .sentence_text_units()
        .units
        .into_iter()
        .find(|unit| {
            unit.owner_section_id == section.id
                && unit.paragraph_index == 1
                && unit.sentence_index == 2
        })
        .expect("second sentence");
    let locator = TextLocator::for_sentence(&document, section, &sentence);

    let neighbors = use_case
        .execute_structured(GetStructuredContextCommand {
            document_id: document.id.clone(),
            target: ContextTarget::Locator(locator.clone()),
            relation: ContextRelation::Neighbor {
                unit: ContextUnit::Sentence,
                before: 1,
                after: 1,
            },
            max_chars: None,
        })
        .await
        .expect("sentence neighbors");

    assert_eq!(neighbors.items.len(), 3);
    assert_eq!(
        neighbors
            .items
            .iter()
            .map(|item| item.content.as_deref())
            .collect::<Vec<_>>(),
        vec![
            Some("First sentence."),
            Some("Second sentence."),
            Some("Third paragraph sentence.")
        ]
    );
    assert_eq!(neighbors.items[0].role, ContextItemRole::Before);
    assert_eq!(neighbors.items[1].role, ContextItemRole::Anchor);
    assert_eq!(neighbors.items[2].role, ContextItemRole::After);
    assert_eq!(neighbors.anchor_locator, locator);
    assert!(neighbors.complete);
    assert!(!neighbors.truncated);

    let container = use_case
        .execute_structured(GetStructuredContextCommand {
            document_id: document.id.clone(),
            target: ContextTarget::Locator(locator),
            relation: ContextRelation::Container {
                kind: ContextContainerKind::Paragraph,
            },
            max_chars: None,
        })
        .await
        .expect("paragraph container");

    assert_eq!(container.items.len(), 1);
    assert_eq!(container.items[0].role, ContextItemRole::Container);
    assert_eq!(
        container.items[0].effective_kind,
        ContextItemKind::Paragraph
    );
    assert_eq!(
        container.items[0].content.as_deref(),
        Some("First sentence. Second sentence.")
    );
    assert_eq!(container.items[0].locator.paragraph_index, Some(1));
    assert_eq!(container.items[0].locator.sentence_index, None);
}

#[tokio::test]
async fn paragraph_neighbors_do_not_cross_section_boundaries() {
    let repository = Arc::new(InMemoryDocumentRepository::default());
    let document = document_fixture();
    repository.save(document.clone()).await.expect("save");
    let use_case = GetContextUseCase::new(repository);

    let section = document
        .find_section(&SectionId("section://root/topic-a".into()))
        .expect("topic A");
    let paragraph = document
        .paragraph_text_units()
        .units
        .into_iter()
        .find(|unit| unit.owner_section_id == section.id && unit.paragraph_index == 2)
        .expect("second paragraph");
    let locator = TextLocator::for_paragraph(&document, section, &paragraph);

    let result = use_case
        .execute_structured(GetStructuredContextCommand {
            document_id: document.id.clone(),
            target: ContextTarget::Locator(locator),
            relation: ContextRelation::Neighbor {
                unit: ContextUnit::Paragraph,
                before: 1,
                after: 5,
            },
            max_chars: None,
        })
        .await
        .expect("paragraph neighbors");

    assert_eq!(result.items.len(), 2);
    assert_eq!(
        result
            .items
            .iter()
            .map(|item| item.content.as_deref())
            .collect::<Vec<_>>(),
        vec![
            Some("First sentence. Second sentence."),
            Some("Third paragraph sentence.")
        ]
    );
    assert!(
        result
            .items
            .iter()
            .all(|item| item.locator.owner_section_id.0 == "section://root/topic-a")
    );
}

#[tokio::test]
async fn structural_relations_follow_owner_identity_not_title_search() {
    let repository = Arc::new(InMemoryDocumentRepository::default());
    let document = document_fixture();
    repository.save(document.clone()).await.expect("save");
    let use_case = GetContextUseCase::new(repository);

    let section = document
        .find_section(&SectionId("section://root/topic-a".into()))
        .expect("topic A");
    let sentence = document
        .sentence_text_units()
        .units
        .into_iter()
        .find(|unit| unit.owner_section_id == section.id)
        .expect("sentence");
    let locator = TextLocator::for_sentence(&document, section, &sentence);

    let ancestors = use_case
        .execute_structured(GetStructuredContextCommand {
            document_id: document.id.clone(),
            target: ContextTarget::Locator(locator.clone()),
            relation: ContextRelation::Structural {
                kind: StructuralContextKind::Ancestors,
            },
            max_chars: None,
        })
        .await
        .expect("ancestors");
    assert_eq!(ancestors.items.len(), 1);
    assert_eq!(ancestors.items[0].title.as_deref(), Some("Root"));
    assert_eq!(
        ancestors.items[0].locator.owner_section_id.0,
        "section://root"
    );

    let siblings = use_case
        .execute_structured(GetStructuredContextCommand {
            document_id: document.id.clone(),
            target: ContextTarget::Locator(locator.clone()),
            relation: ContextRelation::Structural {
                kind: StructuralContextKind::Siblings,
            },
            max_chars: None,
        })
        .await
        .expect("siblings");
    assert_eq!(siblings.items.len(), 1);
    assert_eq!(siblings.items[0].title.as_deref(), Some("Topic B"));
    assert_eq!(
        siblings.items[0].locator.owner_section_id.0,
        "section://root/topic-b"
    );

    let children = use_case
        .execute_structured(GetStructuredContextCommand {
            document_id: document.id.clone(),
            target: ContextTarget::Locator(locator.clone()),
            relation: ContextRelation::Structural {
                kind: StructuralContextKind::Children,
            },
            max_chars: None,
        })
        .await
        .expect("children");
    assert_eq!(children.items.len(), 1);
    assert_eq!(children.items[0].title.as_deref(), Some("Topic A Child"));

    let owner = use_case
        .execute_structured(GetStructuredContextCommand {
            document_id: document.id,
            target: ContextTarget::Locator(locator),
            relation: ContextRelation::Structural {
                kind: StructuralContextKind::OwnerSection,
            },
            max_chars: None,
        })
        .await
        .expect("owner");
    assert_eq!(owner.items.len(), 1);
    assert_eq!(owner.items[0].title.as_deref(), Some("Topic A"));
}

#[tokio::test]
async fn stale_and_malformed_locators_fail_closed() {
    let repository = Arc::new(InMemoryDocumentRepository::default());
    let document = document_fixture();
    repository.save(document.clone()).await.expect("save");
    let use_case = GetContextUseCase::new(repository.clone());

    let section = document
        .find_section(&SectionId("section://root/topic-a".into()))
        .expect("topic A");
    let sentence = document
        .sentence_text_units()
        .units
        .into_iter()
        .find(|unit| unit.owner_section_id == section.id)
        .expect("sentence");
    let locator = TextLocator::for_sentence(&document, section, &sentence);

    let mut changed = document.clone();
    changed.root_sections[0].children[0].content =
        "Changed sentence. Second sentence.\n\nThird paragraph sentence.".into();
    repository.save(changed).await.expect("replace");

    let error = use_case
        .execute_structured(GetStructuredContextCommand {
            document_id: document.id.clone(),
            target: ContextTarget::Locator(locator.clone()),
            relation: ContextRelation::Neighbor {
                unit: ContextUnit::Sentence,
                before: 1,
                after: 1,
            },
            max_chars: None,
        })
        .await
        .expect_err("normalized change must stale locator");
    assert!(matches!(error, ApplicationError::StaleLocator(_)));

    repository.save(document.clone()).await.expect("restore");
    let mut malformed = locator;
    malformed.paragraph_index = None;
    let error = use_case
        .execute_structured(GetStructuredContextCommand {
            document_id: document.id,
            target: ContextTarget::Locator(malformed),
            relation: ContextRelation::Container {
                kind: ContextContainerKind::Paragraph,
            },
            max_chars: None,
        })
        .await
        .expect_err("malformed locator must be rejected");
    assert!(matches!(error, ApplicationError::InvalidLocator(_)));
}

#[tokio::test]
async fn precise_context_budget_never_splits_text_units() {
    let repository = Arc::new(InMemoryDocumentRepository::default());
    let document = document_fixture();
    repository.save(document.clone()).await.expect("save");
    let use_case = GetContextUseCase::new(repository);

    let section = document
        .find_section(&SectionId("section://root/topic-a".into()))
        .expect("topic A");
    let sentence = document
        .sentence_text_units()
        .units
        .into_iter()
        .find(|unit| unit.owner_section_id == section.id)
        .expect("sentence");

    let error = use_case
        .execute_structured(GetStructuredContextCommand {
            document_id: document.id.clone(),
            target: ContextTarget::Locator(TextLocator::for_sentence(
                &document, section, &sentence,
            )),
            relation: ContextRelation::Neighbor {
                unit: ContextUnit::Sentence,
                before: 0,
                after: 0,
            },
            max_chars: Some(3),
        })
        .await
        .expect_err("precise item must remain atomic");
    assert!(matches!(error, ApplicationError::ResourceLimitExceeded(_)));
}

#[tokio::test]
async fn legacy_section_neighbor_contract_is_preserved() {
    let repository = Arc::new(InMemoryDocumentRepository::default());
    let document = document_fixture();
    repository.save(document.clone()).await.expect("save");
    let use_case = GetContextUseCase::new(repository);

    let result = use_case
        .execute(GetContextCommand {
            document_id: document.id,
            section_id: SectionId("section://root/topic-a".into()),
            before: 1,
            after: 1,
            max_chars: None,
        })
        .await
        .expect("legacy context");

    assert!(result.content.contains("# Root"));
    assert!(result.content.contains("## Topic A"));
    assert!(result.content.contains("### Topic A Child"));
    assert_eq!(result.items.len(), 3);
    assert_eq!(result.items[1].role, ContextItemRole::Anchor);
    assert_eq!(
        result.relation,
        ContextRelation::Neighbor {
            unit: ContextUnit::Section,
            before: 1,
            after: 1,
        }
    );
}

fn document_fixture() -> Document {
    Document {
        id: DocumentId("doc:context-granularity".into()),
        source: DocumentSource("memory:context-granularity".into()),
        title: "Context granularity".into(),
        media_type: MediaType("text/plain".into()),
        content_hash: ContentHash("sha256:raw".into()),
        metadata: BTreeMap::new(),
        root_sections: vec![Section {
            id: SectionId("section://root".into()),
            parent_id: None,
            title: "Root".into(),
            level: 1,
            content: "Root overview.".into(),
            location: Location {
                section_path: vec!["Root".into()],
                ..Location::default()
            },
            children: vec![
                Section {
                    id: SectionId("section://root/topic-a".into()),
                    parent_id: Some(SectionId("section://root".into())),
                    title: "Topic A".into(),
                    level: 2,
                    content: "First sentence. Second sentence.\n\nThird paragraph sentence.".into(),
                    location: Location {
                        section_path: vec!["Root".into(), "Topic A".into()],
                        ..Location::default()
                    },
                    children: vec![Section {
                        id: SectionId("section://root/topic-a/child".into()),
                        parent_id: Some(SectionId("section://root/topic-a".into())),
                        title: "Topic A Child".into(),
                        level: 3,
                        content: "Child text.".into(),
                        location: Location {
                            section_path: vec![
                                "Root".into(),
                                "Topic A".into(),
                                "Topic A Child".into(),
                            ],
                            ..Location::default()
                        },
                        children: vec![],
                    }],
                },
                Section {
                    id: SectionId("section://root/topic-b".into()),
                    parent_id: Some(SectionId("section://root".into())),
                    title: "Topic B".into(),
                    level: 2,
                    content: "Topic B text.".into(),
                    location: Location {
                        section_path: vec!["Root".into(), "Topic B".into()],
                        ..Location::default()
                    },
                    children: vec![],
                },
            ],
        }],
    }
}
