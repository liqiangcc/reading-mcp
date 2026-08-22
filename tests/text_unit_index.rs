use std::collections::BTreeMap;
use std::sync::Arc;

use reading_mcp::application::open_document::{OpenDocumentCommand, OpenDocumentUseCase};
use reading_mcp::application::ports::{DocumentRepository, RetrievalOptions, TextUnitIndex};
use reading_mcp::domain::{
    ContentHash, Document, DocumentId, DocumentSource, Location, MediaType, Section, SectionId,
    TEXT_SEGMENTATION_VERSION, TextUnitKind,
};
use reading_mcp::infrastructure::{
    InMemoryDocumentRepository, InMemoryTextUnitIndex, NoopSearchIndex, SqliteDocumentRepository,
    SqliteTextUnitIndex,
};
use reading_mcp::parsing::ParserRouter;
use reading_mcp::retrieval::{FileRetriever, LocalFileSourcePolicy};
use tempfile::tempdir;

#[test]
fn paragraph_text_units_are_exact_section_relative_unicode_scalar_slices() {
    let document = document_with_content("A中🙂Z\n\nSecond\n");
    let set = document.paragraph_text_units();

    assert_eq!(set.units.len(), 2);
    let first = &set.units[0];
    assert_eq!(first.kind, TextUnitKind::Paragraph);
    assert_eq!(first.paragraph_index, 1);
    assert_eq!(first.source_order, 0);
    assert_eq!(first.normalized_range.start(), 0);
    assert_eq!(first.normalized_range.end(), 4);
    assert_eq!(first.text, "A中🙂Z");
    assert_eq!(first.segmentation_version, TEXT_SEGMENTATION_VERSION);

    let second = &set.units[1];
    assert_eq!(second.paragraph_index, 2);
    assert_eq!(second.source_order, 1);
    assert_eq!(second.normalized_range.start(), 6);
    assert_eq!(second.normalized_range.end(), 12);
    assert_eq!(second.text, "Second");

    let section = &document.root_sections[0];
    for unit in &set.units {
        assert_eq!(
            section
                .normalized_text_slice(unit.normalized_range)
                .expect("unit range must resolve"),
            unit.text
        );
    }

    let coverage = &set.coverage[0];
    assert_eq!(coverage.owner_chars, 13);
    assert_eq!(coverage.paragraph_chars, 10);
    assert_eq!(coverage.separator_chars, 3);
    assert_eq!(coverage.paragraph_count, 2);
    assert_eq!(
        coverage.owner_chars,
        coverage.paragraph_chars + coverage.separator_chars
    );
}

#[test]
fn paragraph_segmentation_preserves_internal_and_edge_text_without_rewriting() {
    let document = document_with_content("  first  \ncontinues  \n\n   \n\tsecond\t");
    let set = document.paragraph_text_units();

    assert_eq!(set.units.len(), 2);
    assert_eq!(set.units[0].text, "  first  \ncontinues  ");
    assert_eq!(set.units[1].text, "\tsecond\t");
    assert!(!set.units[0].text.starts_with("first"));
    assert!(set.units[0].text.ends_with("  "));
}

#[test]
fn whitespace_only_section_has_truthful_separator_coverage_without_fake_paragraphs() {
    let document = document_with_content(" \n\t\n\r\n");
    let set = document.paragraph_text_units();

    assert!(set.units.is_empty());
    assert_eq!(set.coverage.len(), 1);
    assert_eq!(set.coverage[0].paragraph_count, 0);
    assert_eq!(set.coverage[0].paragraph_chars, 0);
    assert_eq!(
        set.coverage[0].separator_chars,
        document.root_sections[0].normalized_text_len()
    );
}

#[test]
fn text_unit_identity_is_deterministic_and_scoped_to_normalized_facts_not_raw_provenance() {
    let first = document_with_content("First\n\nSecond");
    let rebuilt = first.clone();
    assert_eq!(first.paragraph_text_units(), rebuilt.paragraph_text_units());

    let mut raw_changed = first.clone();
    raw_changed.content_hash = ContentHash("sha256:different-raw-provenance".into());
    let first_units = first.paragraph_text_units();
    let raw_changed_units = raw_changed.paragraph_text_units();
    assert_eq!(
        first_units
            .units
            .iter()
            .map(|unit| unit.id.clone())
            .collect::<Vec<_>>(),
        raw_changed_units
            .units
            .iter()
            .map(|unit| unit.id.clone())
            .collect::<Vec<_>>()
    );
    assert_ne!(
        first_units.units[0].content_hash,
        raw_changed_units.units[0].content_hash
    );

    let changed = document_with_content("First changed\n\nSecond");
    assert_ne!(
        first.paragraph_text_units().units[0].id,
        changed.paragraph_text_units().units[0].id
    );
}

#[test]
fn source_order_is_depth_first_section_order_then_paragraph_order() {
    let document = Document {
        id: DocumentId("doc:order".into()),
        source: DocumentSource("memory:order".into()),
        title: "Order".into(),
        media_type: MediaType("text/plain".into()),
        content_hash: ContentHash("sha256:raw".into()),
        metadata: BTreeMap::new(),
        root_sections: vec![Section {
            id: SectionId("section://root".into()),
            parent_id: None,
            title: "Root".into(),
            level: 1,
            content: "R1\n\nR2".into(),
            location: Location::default(),
            children: vec![Section {
                id: SectionId("section://root/child".into()),
                parent_id: Some(SectionId("section://root".into())),
                title: "Child".into(),
                level: 2,
                content: "C1\n\nC2".into(),
                location: Location::default(),
                children: vec![],
            }],
        }],
    };

    let set = document.paragraph_text_units();
    assert_eq!(
        set.units
            .iter()
            .map(|unit| {
                (
                    unit.source_order,
                    unit.owner_section_id.0.as_str(),
                    unit.paragraph_index,
                    unit.text.as_str(),
                )
            })
            .collect::<Vec<_>>(),
        vec![
            (0, "section://root", 1, "R1"),
            (1, "section://root", 2, "R2"),
            (2, "section://root/child", 1, "C1"),
            (3, "section://root/child", 2, "C2"),
        ]
    );
}

#[tokio::test]
async fn sqlite_text_unit_index_round_trips_and_replaces_rebuildable_units() {
    let directory = tempdir().expect("temporary directory should be created");
    let database = directory.path().join("state.sqlite");
    let index = SqliteTextUnitIndex::open(&database).expect("text unit index should open");
    let document = document_with_content("First\n\nSecond");
    let first_set = document.paragraph_text_units();

    index
        .replace_document(&document.id, &first_set.units)
        .await
        .expect("units should persist");
    drop(index);

    let reopened = SqliteTextUnitIndex::open(&database).expect("text unit index should reopen");
    assert_eq!(
        reopened
            .list_document(&document.id)
            .await
            .expect("persisted units should load"),
        first_set.units
    );

    let changed = document_with_content("Replacement only");
    let changed_set = changed.paragraph_text_units();
    reopened
        .replace_document(&changed.id, &changed_set.units)
        .await
        .expect("rebuild should replace prior units");
    assert_eq!(
        reopened
            .list_document(&changed.id)
            .await
            .expect("replacement units should load"),
        changed_set.units
    );
}

#[tokio::test]
async fn paragraph_units_rebuild_identically_from_persisted_canonical_document() {
    let directory = tempdir().expect("temporary directory should be created");
    let repository = SqliteDocumentRepository::open(directory.path().join("state.sqlite"))
        .expect("repository should open");
    let document = document_with_content("A中🙂Z\n\nSecond");
    let expected = document.paragraph_text_units();

    repository
        .save(document.clone())
        .await
        .expect("document should persist");
    let restored = repository
        .get(&document.id)
        .await
        .expect("repository read should succeed")
        .expect("document should exist");

    assert_eq!(restored.paragraph_text_units(), expected);
}

#[tokio::test]
async fn open_document_can_rebuild_the_text_unit_index_without_changing_search_contracts() {
    let directory = tempdir().expect("temporary directory should be created");
    let path = directory.path().join("paragraphs.md");
    tokio::fs::write(&path, "# Paragraphs\n\nFirst.\n\nSecond 中🙂.\n")
        .await
        .expect("fixture should be written");

    let repository = Arc::new(InMemoryDocumentRepository::default());
    let index = Arc::new(InMemoryTextUnitIndex::default());
    let open = OpenDocumentUseCase::with_text_unit_index(
        Arc::new(LocalFileSourcePolicy::allow_roots([directory.path()])),
        Arc::new(FileRetriever),
        Arc::new(ParserRouter::phase1()),
        repository,
        index.clone(),
        Arc::new(NoopSearchIndex),
    );

    let opened = open
        .execute(OpenDocumentCommand {
            source: DocumentSource(path.to_string_lossy().into_owned()),
            options: RetrievalOptions::default(),
        })
        .await
        .expect("document should open and index");
    let units = index
        .list_document(&opened.document_id)
        .await
        .expect("text units should be readable from derived index");

    assert_eq!(units.len(), 2);
    assert_eq!(units[0].text, "First.");
    assert_eq!(units[1].text, "Second 中🙂.");
    assert!(units.iter().all(|unit| {
        unit.normalized_document_hash == opened.normalized_document_hash
            && unit.segmentation_version == TEXT_SEGMENTATION_VERSION
    }));
}

fn document_with_content(content: &str) -> Document {
    Document {
        id: DocumentId("doc:paragraphs".into()),
        source: DocumentSource("memory:paragraphs".into()),
        title: "Paragraphs".into(),
        media_type: MediaType("text/plain".into()),
        content_hash: ContentHash("sha256:raw".into()),
        metadata: BTreeMap::new(),
        root_sections: vec![Section {
            id: SectionId("section://root".into()),
            parent_id: None,
            title: "Root".into(),
            level: 1,
            content: content.into(),
            location: Location::default(),
            children: vec![],
        }],
    }
}
