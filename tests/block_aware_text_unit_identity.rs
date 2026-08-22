use std::collections::BTreeMap;
use std::sync::Arc;

use reading_mcp::application::get_text_units::{
    GetTextUnitsCommand, GetTextUnitsUseCase, RequestedTextUnitKind, TextUnitCoveragePolicy,
    TextUnitDirection,
};
use reading_mcp::application::ports::{ApplicationError, DocumentRepository, Parser, SearchIndex};
use reading_mcp::domain::{
    ContentHash, Document, DocumentId, DocumentSource, Location, MediaType, NormalizedBlock,
    NormalizedBlockKind, NormalizedBlockMap, NormalizedBlockProvenance, NormalizedTextRange,
    ParagraphContentClass, Section, SectionId, SentenceEligibility, TEXT_SEGMENTATION_VERSION,
};
use reading_mcp::infrastructure::{InMemoryDocumentRepository, InMemorySearchIndex};
use reading_mcp::parsing::HtmlParser;

#[tokio::test]
async fn native_block_v1_evidence_drives_v2_paragraph_and_sentence_policy() {
    let document = parse_html(
        r#"<html><body>
<h1>Chapter</h1>
<p>First. Second.</p>
<blockquote><p>Quote one.</p><p>Quote two.</p></blockquote>
<ul><li>Item one. Item two.</li></ul>
<pre>code. next.</pre>
<table><tr><td>A.</td><td>B.</td></tr></table>
</body></html>"#,
    )
    .await;

    let paragraphs = document
        .try_paragraph_text_units()
        .expect("valid native block evidence");
    assert_eq!(TEXT_SEGMENTATION_VERSION, "text-segmentation/v2");
    assert_eq!(paragraphs.units.len(), 5);
    assert_eq!(
        paragraphs
            .units
            .iter()
            .map(|unit| unit.text.as_str())
            .collect::<Vec<_>>(),
        vec![
            "First. Second.",
            "Quote one.Quote two.",
            "Item one. Item two.",
            "code. next.",
            "A. B.",
        ]
    );

    let sentence_set = document
        .try_sentence_text_units()
        .expect("valid sentence materialization");
    assert_eq!(
        sentence_set
            .units
            .iter()
            .map(|unit| unit.text.as_str())
            .collect::<Vec<_>>(),
        vec!["First.", "Second."]
    );
    assert_eq!(
        sentence_set
            .coverage
            .iter()
            .map(|coverage| (coverage.content_class, coverage.eligibility))
            .collect::<Vec<_>>(),
        vec![
            (
                ParagraphContentClass::NativeParagraph,
                SentenceEligibility::Eligible,
            ),
            (
                ParagraphContentClass::BlockQuote,
                SentenceEligibility::CoarseParagraphOnly,
            ),
            (
                ParagraphContentClass::ListItem,
                SentenceEligibility::CoarseParagraphOnly,
            ),
            (
                ParagraphContentClass::Preformatted,
                SentenceEligibility::CoarseParagraphOnly,
            ),
            (
                ParagraphContentClass::Table,
                SentenceEligibility::CoarseParagraphOnly,
            ),
        ]
    );
}

#[tokio::test]
async fn sentence_enumeration_preserves_flat_structural_and_non_prose_blocks_coarsely() {
    let document = parse_html(
        r#"<html><body>
<h1>Chapter</h1>
<p>First. Second.</p>
<blockquote><p>Quote one.</p><p>Quote two.</p></blockquote>
<ul><li>Item one. Item two.</li></ul>
<pre>code. next.</pre>
<table><tr><td>A.</td><td>B.</td></tr></table>
</body></html>"#,
    )
    .await;
    let section_id = document.root_sections[0].id.clone();
    let repository = Arc::new(InMemoryDocumentRepository::default());
    repository.save(document.clone()).await.expect("save");
    let use_case = GetTextUnitsUseCase::new(repository);

    let preserved = use_case
        .execute(GetTextUnitsCommand {
            document_id: document.id.clone(),
            section_id: section_id.clone(),
            requested_kind: RequestedTextUnitKind::Sentence,
            direction: TextUnitDirection::Forward,
            coverage_policy: TextUnitCoveragePolicy::PreserveSource,
            max_items: 32,
            max_chars: None,
            cursor: None,
        })
        .await
        .expect("preserve-source enumeration");

    assert_eq!(preserved.items.len(), 6);
    assert_eq!(preserved.coverage.sentence_eligible_paragraphs, 1);
    assert_eq!(preserved.coverage.coarse_structural_paragraphs, 2);
    assert_eq!(preserved.coverage.non_prose_paragraphs, 2);
    assert_eq!(preserved.coverage.coarse_structural_items, 2);
    assert_eq!(preserved.coverage.coarse_non_prose_items, 2);
    assert!(preserved.coverage.source_complete);
    assert!(preserved.section_complete);
    assert_eq!(
        preserved
            .items
            .iter()
            .filter_map(|item| item.degradation.as_deref())
            .collect::<Vec<_>>(),
        vec![
            "flat_native_container_no_nested_textunit_evidence",
            "flat_native_container_no_nested_textunit_evidence",
            "requested_sentence_but_non_prose_is_paragraph_only",
            "requested_sentence_but_non_prose_is_paragraph_only",
        ]
    );

    let eligible = use_case
        .execute(GetTextUnitsCommand {
            document_id: document.id,
            section_id,
            requested_kind: RequestedTextUnitKind::Sentence,
            direction: TextUnitDirection::Forward,
            coverage_policy: TextUnitCoveragePolicy::EligibleOnly,
            max_items: 32,
            max_chars: None,
            cursor: None,
        })
        .await
        .expect("eligible-only enumeration");
    assert_eq!(eligible.items.len(), 2);
    assert_eq!(eligible.coverage.intentionally_skipped, 4);
    assert!(!eligible.coverage.source_complete);
    assert!(!eligible.section_complete);
}

#[test]
fn native_and_fallback_ranges_merge_in_exact_section_source_order() {
    let mut document = plain_document("Leading fallback.\n\nNative body.\n\nTrailing fallback.");
    let prefix_len = "Leading fallback.\n\n".chars().count();
    let native_len = "Native body.".chars().count();
    let native_range =
        NormalizedTextRange::new(prefix_len, prefix_len + native_len).expect("native range");
    document
        .set_normalized_block_map(NormalizedBlockMap::new(vec![NormalizedBlock {
            owner_section_id: document.root_sections[0].id.clone(),
            block_index: 1,
            source_order: 0,
            kind: NormalizedBlockKind::Paragraph,
            normalized_range: native_range,
            native_anchor: None,
            native_location: None,
            provenance: NormalizedBlockProvenance::XhtmlNativeBlock,
        }]))
        .expect("valid partial native map");

    let paragraph_set = document
        .try_paragraph_text_units()
        .expect("mixed native/fallback materialization");
    assert_eq!(
        paragraph_set
            .units
            .iter()
            .map(|unit| (
                unit.paragraph_index,
                unit.text.as_str(),
                unit.normalized_range
            ))
            .collect::<Vec<_>>(),
        vec![
            (
                1,
                "Leading fallback.",
                NormalizedTextRange::new(0, "Leading fallback.".chars().count()).unwrap(),
            ),
            (2, "Native body.", native_range),
            (
                3,
                "Trailing fallback.",
                NormalizedTextRange::new(
                    prefix_len + native_len + 2,
                    document.root_sections[0].content.chars().count(),
                )
                .unwrap(),
            ),
        ]
    );
    let coverage = &paragraph_set.coverage[0];
    assert_eq!(coverage.native_paragraph_chars, native_len);
    assert_eq!(
        coverage.fallback_chars,
        "Leading fallback.Trailing fallback.".chars().count()
    );
    assert_eq!(coverage.separator_chars, 4);
}

#[tokio::test]
async fn invalid_declared_block_map_fails_closed_for_text_units_and_lexical_index() {
    let mut document = parse_html("<html><body><h1>Chapter</h1><p>Body.</p></body></html>").await;
    document
        .metadata
        .insert("normalized_block_map".into(), "{not valid json".into());

    assert!(document.try_paragraph_text_units().is_err());
    assert!(document.try_sentence_text_units().is_err());

    let index = InMemorySearchIndex::default();
    let error = index
        .index(&document)
        .await
        .expect_err("invalid native evidence must fail lexical rebuild");
    assert!(matches!(error, ApplicationError::IndexFailed(_)));

    let repository = Arc::new(InMemoryDocumentRepository::default());
    repository
        .save(document.clone())
        .await
        .expect("save corrupt fixture");
    let error = GetTextUnitsUseCase::new(repository)
        .execute(GetTextUnitsCommand {
            document_id: document.id,
            section_id: document.root_sections[0].id.clone(),
            requested_kind: RequestedTextUnitKind::Paragraph,
            direction: TextUnitDirection::Forward,
            coverage_policy: TextUnitCoveragePolicy::PreserveSource,
            max_items: 32,
            max_chars: None,
            cursor: None,
        })
        .await
        .expect_err("enumeration must fail closed");
    assert!(matches!(error, ApplicationError::TextUnitIndexFailed(_)));
}

#[test]
fn identity_bearing_block_kind_changes_normalized_hash_and_text_unit_ids() {
    let mut paragraph = plain_document("Same exact text.");
    let range = NormalizedTextRange::new(0, paragraph.root_sections[0].content.chars().count())
        .expect("range");
    let owner = paragraph.root_sections[0].id.clone();
    paragraph
        .set_normalized_block_map(NormalizedBlockMap::new(vec![NormalizedBlock {
            owner_section_id: owner.clone(),
            block_index: 1,
            source_order: 0,
            kind: NormalizedBlockKind::Paragraph,
            normalized_range: range,
            native_anchor: None,
            native_location: None,
            provenance: NormalizedBlockProvenance::XhtmlNativeBlock,
        }]))
        .expect("paragraph map");

    let mut quote = paragraph.clone();
    quote
        .set_normalized_block_map(NormalizedBlockMap::new(vec![NormalizedBlock {
            owner_section_id: owner,
            block_index: 1,
            source_order: 0,
            kind: NormalizedBlockKind::BlockQuote,
            normalized_range: range,
            native_anchor: None,
            native_location: None,
            provenance: NormalizedBlockProvenance::XhtmlNativeBlock,
        }]))
        .expect("quote map");

    assert_ne!(
        paragraph.normalized_document_hash(),
        quote.normalized_document_hash()
    );
    assert_ne!(
        paragraph.paragraph_text_units().units[0].id,
        quote.paragraph_text_units().units[0].id
    );
    assert_eq!(paragraph.sentence_text_units().units.len(), 1);
    assert!(quote.sentence_text_units().units.is_empty());
}

async fn parse_html(source: &str) -> Document {
    HtmlParser
        .parse(reading_mcp::application::ports::RetrievedResource {
            source: DocumentSource("memory:block-aware.html".into()),
            final_source: DocumentSource("memory:block-aware.html".into()),
            media_type: MediaType("text/html".into()),
            bytes: source.as_bytes().to_vec(),
            etag: None,
            last_modified: None,
            metadata: Default::default(),
        })
        .await
        .expect("HTML should parse")
}

fn plain_document(content: &str) -> Document {
    Document {
        id: DocumentId("doc:block-aware".into()),
        source: DocumentSource("memory:block-aware".into()),
        title: "Block aware".into(),
        media_type: MediaType("text/plain".into()),
        content_hash: ContentHash("sha256:block-aware".into()),
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
