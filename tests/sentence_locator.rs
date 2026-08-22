use std::collections::BTreeMap;

use reading_mcp::application::ports::DocumentRepository;
use reading_mcp::domain::{
    ContentHash, Document, DocumentId, DocumentSource, Location, MediaType, ParagraphContentClass,
    Section, SectionId, SentenceEligibility, TEXT_SEGMENTATION_VERSION,
};
use reading_mcp::infrastructure::SqliteDocumentRepository;
use tempfile::tempdir;

#[test]
fn sentence_units_are_exact_section_relative_slices_with_paragraph_ownership() {
    let document = document_with_content(
        "Dr. Smith uses mmap(). Next uses version 3.14 and e.g. fork(). 第二句。最后一句！",
    );
    let paragraphs = document.paragraph_text_units();
    let set = document.sentence_text_units();

    assert_eq!(paragraphs.units.len(), 1);
    assert_eq!(set.units.len(), 4);
    assert_eq!(
        set.units.iter().map(|unit| unit.text.as_str()).collect::<Vec<_>>(),
        vec![
            "Dr. Smith uses mmap().",
            "Next uses version 3.14 and e.g. fork().",
            "第二句。",
            "最后一句！",
        ]
    );

    let paragraph = &paragraphs.units[0];
    let section = &document.root_sections[0];
    for (offset, sentence) in set.units.iter().enumerate() {
        assert_eq!(sentence.paragraph_index, 1);
        assert_eq!(sentence.sentence_index, offset + 1);
        assert_eq!(sentence.parent_paragraph_id, paragraph.id);
        assert_eq!(sentence.source_order, offset);
        assert_eq!(sentence.segmentation_version, TEXT_SEGMENTATION_VERSION);
        assert!(sentence.normalized_range.start() >= paragraph.normalized_range.start());
        assert!(sentence.normalized_range.end() <= paragraph.normalized_range.end());
        assert_eq!(
            section
                .normalized_text_slice(sentence.normalized_range)
                .expect("sentence range must resolve"),
            sentence.text
        );
    }

    let coverage = &set.coverage[0];
    assert_eq!(coverage.content_class, ParagraphContentClass::ProseOrUnknown);
    assert_eq!(coverage.eligibility, SentenceEligibility::Eligible);
    assert_eq!(coverage.sentence_count, 4);
    assert_eq!(coverage.coarse_only_chars, 0);
    assert_eq!(
        coverage.paragraph_chars,
        coverage.sentence_chars + coverage.separator_chars + coverage.coarse_only_chars
    );
}

#[test]
fn technical_periods_do_not_create_false_sentence_boundaries() {
    let document = document_with_content(
        "Run ./configure and read README.md before calling foo.bar(). Then use v3.14. Done.",
    );
    let set = document.sentence_text_units();

    assert_eq!(
        set.units.iter().map(|unit| unit.text.as_str()).collect::<Vec<_>>(),
        vec![
            "Run ./configure and read README.md before calling foo.bar().",
            "Then use v3.14.",
            "Done.",
        ]
    );
}

#[test]
fn cjk_terminal_punctuation_does_not_require_ascii_whitespace() {
    let document = document_with_content("第一句。第二句？第三句！Final sentence.");
    let set = document.sentence_text_units();

    assert_eq!(
        set.units.iter().map(|unit| unit.text.as_str()).collect::<Vec<_>>(),
        vec!["第一句。", "第二句？", "第三句！", "Final sentence."]
    );
}

#[test]
fn obvious_code_and_table_paragraphs_are_coarse_only_not_fake_sentences() {
    let content = "```rust\nfn main() { println!(\"Hi.\"); }\n```\n\n| Name | Value |\n| --- | --- |\n| x | 3.14 |\n\nReal prose. Next sentence.";
    let document = document_with_content(content);
    let paragraphs = document.paragraph_text_units();
    let set = document.sentence_text_units();

    assert_eq!(paragraphs.units.len(), 3);
    assert_eq!(set.units.len(), 2);
    assert_eq!(set.units[0].paragraph_index, 3);
    assert_eq!(set.units[0].sentence_index, 1);
    assert_eq!(set.units[1].sentence_index, 2);
    assert_eq!(set.units[0].text, "Real prose.");
    assert_eq!(set.units[1].text, "Next sentence.");

    assert_eq!(set.coverage[0].content_class, ParagraphContentClass::CodeBlock);
    assert_eq!(
        set.coverage[0].eligibility,
        SentenceEligibility::CoarseParagraphOnly
    );
    assert_eq!(set.coverage[0].sentence_count, 0);
    assert_eq!(set.coverage[0].coarse_only_chars, set.coverage[0].paragraph_chars);

    assert_eq!(set.coverage[1].content_class, ParagraphContentClass::Table);
    assert_eq!(
        set.coverage[1].eligibility,
        SentenceEligibility::CoarseParagraphOnly
    );
    assert_eq!(set.coverage[1].sentence_count, 0);
    assert_eq!(set.coverage[1].coarse_only_chars, set.coverage[1].paragraph_chars);

    assert_eq!(set.coverage[2].content_class, ParagraphContentClass::ProseOrUnknown);
    assert_eq!(set.coverage[2].eligibility, SentenceEligibility::Eligible);

    for coverage in &set.coverage {
        assert_eq!(
            coverage.paragraph_chars,
            coverage.sentence_chars + coverage.separator_chars + coverage.coarse_only_chars
        );
    }
}

#[test]
fn indented_code_is_kept_as_a_coarse_paragraph() {
    let document = document_with_content("    let x = 1.\n    println!(\"x = {x}.\");");
    let set = document.sentence_text_units();

    assert!(set.units.is_empty());
    assert_eq!(set.coverage.len(), 1);
    assert_eq!(set.coverage[0].content_class, ParagraphContentClass::CodeBlock);
    assert_eq!(
        set.coverage[0].eligibility,
        SentenceEligibility::CoarseParagraphOnly
    );
}

#[test]
fn sentence_identity_is_deterministic_and_scoped_to_normalized_facts() {
    let first = document_with_content("First sentence. Second sentence.");
    let rebuilt = first.clone();
    assert_eq!(first.sentence_text_units(), rebuilt.sentence_text_units());

    let first_set = first.sentence_text_units();
    let mut raw_changed = first.clone();
    raw_changed.content_hash = ContentHash("sha256:different-raw-provenance".into());
    let raw_changed_set = raw_changed.sentence_text_units();
    assert_eq!(
        first_set
            .units
            .iter()
            .map(|unit| unit.id.clone())
            .collect::<Vec<_>>(),
        raw_changed_set
            .units
            .iter()
            .map(|unit| unit.id.clone())
            .collect::<Vec<_>>()
    );
    assert_ne!(first_set.units[0].content_hash, raw_changed_set.units[0].content_hash);

    let changed = document_with_content("First sentence changed. Second sentence.");
    assert_ne!(first_set.units[0].id, changed.sentence_text_units().units[0].id);
}

#[test]
fn sentence_source_order_is_deterministic_across_sections_and_paragraphs() {
    let document = Document {
        id: DocumentId("doc:sentence-order".into()),
        source: DocumentSource("memory:sentence-order".into()),
        title: "Sentence order".into(),
        media_type: MediaType("text/plain".into()),
        content_hash: ContentHash("sha256:raw".into()),
        metadata: BTreeMap::new(),
        root_sections: vec![Section {
            id: SectionId("section://root".into()),
            parent_id: None,
            title: "Root".into(),
            level: 1,
            content: "R1. R2.\n\nR3.".into(),
            location: Location::default(),
            children: vec![Section {
                id: SectionId("section://root/child".into()),
                parent_id: Some(SectionId("section://root".into())),
                title: "Child".into(),
                level: 2,
                content: "C1。C2。".into(),
                location: Location::default(),
                children: vec![],
            }],
        }],
    };

    let set = document.sentence_text_units();
    assert_eq!(
        set.units
            .iter()
            .map(|unit| {
                (
                    unit.source_order,
                    unit.owner_section_id.0.as_str(),
                    unit.paragraph_index,
                    unit.sentence_index,
                    unit.text.as_str(),
                )
            })
            .collect::<Vec<_>>(),
        vec![
            (0, "section://root", 1, 1, "R1."),
            (1, "section://root", 1, 2, "R2."),
            (2, "section://root", 2, 1, "R3."),
            (3, "section://root/child", 1, 1, "C1。"),
            (4, "section://root/child", 1, 2, "C2。"),
        ]
    );
}

#[tokio::test]
async fn sentence_units_rebuild_identically_from_persisted_canonical_document() {
    let directory = tempdir().expect("temporary directory should be created");
    let repository = SqliteDocumentRepository::open(directory.path().join("state.sqlite"))
        .expect("repository should open");
    let document = document_with_content("A中🙂Z. 第二句。Last sentence!");
    let expected = document.sentence_text_units();

    repository
        .save(document.clone())
        .await
        .expect("document should persist");
    let restored = repository
        .get(&document.id)
        .await
        .expect("repository read should succeed")
        .expect("document should exist");

    assert_eq!(restored.sentence_text_units(), expected);
}

fn document_with_content(content: &str) -> Document {
    Document {
        id: DocumentId("doc:sentences".into()),
        source: DocumentSource("memory:sentences".into()),
        title: "Sentences".into(),
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
