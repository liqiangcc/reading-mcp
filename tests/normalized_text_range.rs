use std::collections::BTreeMap;

use reading_mcp::application::ports::DocumentRepository;
use reading_mcp::domain::{
    ContentHash, Document, DocumentId, DocumentSource, Location, MediaType, NORMALIZATION_VERSION,
    NORMALIZED_DOCUMENT_HASH_VERSION, NORMALIZED_TEXT_COORDINATE_SPACE, NormalizedTextRange,
    NormalizedTextRangeError, Section, SectionId,
};
use reading_mcp::infrastructure::SqliteDocumentRepository;
use reading_mcp::mcp::contracts::OpenDocumentResponse;
use tempfile::tempdir;

#[test]
fn normalized_range_is_section_relative_half_open_and_unicode_scalar_based() {
    let section = Section {
        id: SectionId("section://unicode".into()),
        parent_id: None,
        title: "Unicode".into(),
        level: 1,
        content: "A中🙂Z".into(),
        location: Location {
            char_start: Some(400),
            char_end: Some(900),
            native_location: Some("source:legacy-range".into()),
            ..Location::default()
        },
        children: vec![],
    };
    let range = NormalizedTextRange::new(1, 3).expect("range should be ordered");

    assert_eq!(range.start(), 1);
    assert_eq!(range.end(), 3);
    assert_eq!(range.len(), 2);
    assert!(!range.is_empty());
    assert_eq!(section.normalized_text_len(), 4);
    assert_eq!(
        section
            .normalized_text_slice(range)
            .expect("range should resolve"),
        "中🙂"
    );
}

#[test]
fn normalized_range_preserves_exact_owner_text_without_trimming() {
    let section = Section {
        id: SectionId("section://whitespace".into()),
        parent_id: None,
        title: "Whitespace".into(),
        level: 1,
        content: "  first\nsecond  ".into(),
        location: Location::default(),
        children: vec![],
    };
    let full = NormalizedTextRange::new(0, section.normalized_text_len())
        .expect("full range should be ordered");
    let empty_at_end =
        NormalizedTextRange::new(section.normalized_text_len(), section.normalized_text_len())
            .expect("empty terminal range should be valid");

    assert_eq!(
        section
            .normalized_text_slice(full)
            .expect("full range should resolve"),
        "  first\nsecond  "
    );
    assert_eq!(
        section
            .normalized_text_slice(empty_at_end)
            .expect("empty range should resolve"),
        ""
    );
    assert!(empty_at_end.is_empty());
}

#[test]
fn normalized_range_validator_rejects_reversed_and_out_of_bounds_ranges() {
    assert_eq!(
        NormalizedTextRange::new(4, 3),
        Err(NormalizedTextRangeError::StartAfterEnd { start: 4, end: 3 })
    );

    let range = NormalizedTextRange::new(1, 5).expect("range order should be valid");
    assert_eq!(
        range.validate_for_text("A中🙂Z"),
        Err(NormalizedTextRangeError::OutOfBounds {
            start: 1,
            end: 5,
            owner_len: 4,
        })
    );
}

#[test]
fn normalized_hash_ignores_raw_and_legacy_location_provenance() {
    let first = canonical_document();
    let mut second = first.clone();
    second.source = DocumentSource("file:///another/source.md".into());
    second.content_hash = ContentHash("sha256:different-raw-source".into());
    second
        .metadata
        .insert("parser_note".into(), "changed".into());
    second.root_sections[0].location = Location {
        page: Some(99),
        chapter: Some("Different native chapter".into()),
        section_path: ["Different", "Display", "Path"]
            .into_iter()
            .map(String::from)
            .collect(),
        anchor: Some("different-anchor".into()),
        paragraph: Some(44),
        char_start: Some(1_000),
        char_end: Some(2_000),
        native_location: Some("epub:different.xhtml#anchor".into()),
    };

    assert_eq!(
        first.normalized_document_hash(),
        second.normalized_document_hash()
    );
}

#[test]
fn normalized_hash_changes_for_addressing_relevant_canonical_facts() {
    let original = canonical_document();
    let original_hash = original.normalized_document_hash();

    let mut changed_content = original.clone();
    changed_content.root_sections[0].content.push('!');
    assert_ne!(original_hash, changed_content.normalized_document_hash());

    let mut changed_title = original.clone();
    changed_title.root_sections[0].title.push_str(" changed");
    assert_ne!(original_hash, changed_title.normalized_document_hash());

    let mut changed_level = original.clone();
    changed_level.root_sections[0].level = 2;
    assert_ne!(original_hash, changed_level.normalized_document_hash());

    let mut changed_id = original.clone();
    changed_id.root_sections[0].id = SectionId("section://renamed".into());
    assert_ne!(original_hash, changed_id.normalized_document_hash());

    let mut changed_parent = original.clone();
    changed_parent.root_sections[0].children[0].parent_id = None;
    assert_ne!(original_hash, changed_parent.normalized_document_hash());

    let mut changed_order = original.clone();
    changed_order.root_sections[0].children.swap(0, 1);
    assert_ne!(original_hash, changed_order.normalized_document_hash());
}

#[tokio::test]
async fn normalized_hash_rebuilds_from_persisted_canonical_document() {
    let directory = tempdir().expect("temporary directory should be created");
    let repository = SqliteDocumentRepository::open(directory.path().join("state.sqlite"))
        .expect("SQLite repository should open");
    let document = canonical_document();
    let expected = document.normalized_document_hash();

    repository
        .save(document.clone())
        .await
        .expect("document should persist");
    let restored = repository
        .get(&document.id)
        .await
        .expect("repository read should succeed")
        .expect("document should exist");

    assert_eq!(expected, restored.normalized_document_hash());
}

#[test]
fn open_contract_advertises_normalized_identity_and_coordinate_versions() {
    assert_eq!(NORMALIZATION_VERSION, "reading-mcp-normalization/v8");
    assert_eq!(
        NORMALIZED_DOCUMENT_HASH_VERSION,
        "normalized-document-hash/v2"
    );
    assert_eq!(
        NORMALIZED_TEXT_COORDINATE_SPACE,
        "section-content-unicode-scalar/v1"
    );

    let schema = schemars::schema_for!(OpenDocumentResponse);
    let schema = serde_json::to_value(schema).expect("schema should serialize");
    let properties = schema
        .get("properties")
        .and_then(|value| value.as_object())
        .expect("open response schema should expose properties");

    assert!(properties.contains_key("content_hash"));
    assert!(properties.contains_key("normalized_document_hash"));
    assert!(properties.contains_key("normalized_document_hash_version"));
    assert!(properties.contains_key("normalization_version"));
    assert!(properties.contains_key("normalized_text_coordinate_space"));
}

fn canonical_document() -> Document {
    Document {
        id: DocumentId("doc:canonical".into()),
        source: DocumentSource("memory:canonical.md".into()),
        title: "Canonical Document".into(),
        media_type: MediaType("text/markdown".into()),
        content_hash: ContentHash("sha256:raw-source".into()),
        metadata: BTreeMap::new(),
        root_sections: vec![Section {
            id: SectionId("section://root".into()),
            parent_id: None,
            title: "Root".into(),
            level: 1,
            content: "Exact persisted root content.\n".into(),
            location: Location {
                section_path: vec!["Root".into()],
                char_start: Some(17),
                char_end: Some(48),
                native_location: Some("markdown:line:1".into()),
                ..Location::default()
            },
            children: vec![
                Section {
                    id: SectionId("section://root/first".into()),
                    parent_id: Some(SectionId("section://root".into())),
                    title: "First".into(),
                    level: 2,
                    content: "First child.".into(),
                    location: Location::default(),
                    children: vec![],
                },
                Section {
                    id: SectionId("section://root/second".into()),
                    parent_id: Some(SectionId("section://root".into())),
                    title: "Second".into(),
                    level: 2,
                    content: "Second child.".into(),
                    location: Location::default(),
                    children: vec![],
                },
            ],
        }],
    }
}
