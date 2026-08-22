use std::collections::BTreeMap;
use std::sync::Arc;

use reading_mcp::application::get_context::{
    ContextRelation, ContextTarget, ContextUnit, GetContextUseCase, GetStructuredContextCommand,
};
use reading_mcp::application::ports::{ApplicationError, DocumentRepository};
use reading_mcp::application::read_document::{ReadDocumentUseCase, ReadExactTargetCommand};
use reading_mcp::domain::{
    ContentHash, Document, DocumentId, DocumentSource, Location, MediaType, Section, SectionId,
    TextLocator,
};
use reading_mcp::infrastructure::InMemoryDocumentRepository;

#[tokio::test]
async fn sentence_locator_is_resolved_consistently_by_read_and_context() {
    let document = document();
    let repository = Arc::new(InMemoryDocumentRepository::default());
    repository.save(document.clone()).await.expect("save");
    let section = document
        .find_section(&SectionId("section://root".into()))
        .expect("root");
    let sentence = document
        .sentence_text_units()
        .units
        .into_iter()
        .find(|unit| unit.sentence_index == 2)
        .expect("second sentence");
    let locator = TextLocator::for_sentence(&document, section, &sentence);

    let read = ReadDocumentUseCase::new(repository.clone())
        .read_exact(ReadExactTargetCommand {
            document_id: document.id.clone(),
            target_locator: locator.clone(),
            max_chars: None,
        })
        .await
        .expect("read accepts locator");
    assert_eq!(read.content, sentence.text);
    assert_eq!(read.resolved_target_locator, locator);

    let context = GetContextUseCase::new(repository)
        .execute_structured(GetStructuredContextCommand {
            document_id: document.id,
            target: ContextTarget::Locator(locator.clone()),
            relation: ContextRelation::Neighbor {
                unit: ContextUnit::Sentence,
                before: 0,
                after: 0,
            },
            max_chars: None,
        })
        .await
        .expect("context accepts same locator");
    assert_eq!(context.anchor_locator, locator);
    assert_eq!(context.items.len(), 1);
    assert_eq!(context.items[0].content.as_deref(), Some(sentence.text.as_str()));
}

#[tokio::test]
async fn normalized_change_stales_same_sentence_locator_for_both_consumers() {
    let document = document();
    let repository = Arc::new(InMemoryDocumentRepository::default());
    repository.save(document.clone()).await.expect("save");
    let section = document
        .find_section(&SectionId("section://root".into()))
        .expect("root");
    let sentence = document
        .sentence_text_units()
        .units
        .into_iter()
        .next()
        .expect("sentence");
    let locator = TextLocator::for_sentence(&document, section, &sentence);

    let mut changed = document.clone();
    changed.root_sections[0].content = "Changed first sentence. Second sentence.".into();
    repository.save(changed).await.expect("replace");

    let read_error = ReadDocumentUseCase::new(repository.clone())
        .read_exact(ReadExactTargetCommand {
            document_id: document.id.clone(),
            target_locator: locator.clone(),
            max_chars: None,
        })
        .await
        .expect_err("read must stale locator");
    assert!(matches!(read_error, ApplicationError::StaleLocator(_)));

    let context_error = GetContextUseCase::new(repository)
        .execute_structured(GetStructuredContextCommand {
            document_id: document.id,
            target: ContextTarget::Locator(locator),
            relation: ContextRelation::Neighbor {
                unit: ContextUnit::Sentence,
                before: 0,
                after: 0,
            },
            max_chars: None,
        })
        .await
        .expect_err("context must stale same locator");
    assert!(matches!(context_error, ApplicationError::StaleLocator(_)));
}

fn document() -> Document {
    Document {
        id: DocumentId("doc:locator-parity".into()),
        source: DocumentSource("memory:locator-parity".into()),
        title: "Locator parity".into(),
        media_type: MediaType("text/plain".into()),
        content_hash: ContentHash("sha256:raw".into()),
        metadata: BTreeMap::new(),
        root_sections: vec![Section {
            id: SectionId("section://root".into()),
            parent_id: None,
            title: "Root".into(),
            level: 1,
            content: "First sentence. Second sentence.".into(),
            location: Location {
                section_path: vec!["Root".into()],
                ..Location::default()
            },
            children: vec![],
        }],
    }
}
