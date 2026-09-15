use std::collections::BTreeMap;
use std::sync::Arc;

use reading_mcp::application::get_text_units::{
    GetTextUnitsCommand, GetTextUnitsUseCase, RequestedTextUnitKind, TextUnitCoveragePolicy,
    TextUnitDirection,
};
use reading_mcp::application::ports::{DocumentReliabilityInspector, DocumentRepository};
use reading_mcp::domain::{
    ContentHash, Document, DocumentId, DocumentSource, Location, MediaType, ParagraphContentClass,
    Section, SectionId, SentenceEligibility,
};
use reading_mcp::infrastructure::InMemoryDocumentRepository;
use reading_mcp::parsing::PersistedDocumentReliabilityInspector;

// Issue #139: PDF fallback extraction merges masthead/title/byline/front matter and
// corrupted fragments into single fallback paragraphs. A clean `effective_kind=sentence`
// claim on such blobs breaks no-lookahead. These paragraphs must stay coarse/degraded
// so strict callers can fail closed, while genuinely clean prose keeps real sentences.

#[test]
fn merged_masthead_title_and_opening_blob_is_not_a_clean_sentence() {
    let document = document_with_content(
        "16 October 1964, Volume 146, Number 3642 Strong Inference Certain systematic methods \
         of scientific thinking may produce much more rapid progress than others. John R. Platt \
         Scientists these days tend to keep up a polite fiction that all science is equal.",
    );
    let set = document.sentence_text_units();

    assert!(set.units.is_empty());
    assert_eq!(set.coverage.len(), 1);
    assert_eq!(
        set.coverage[0].eligibility,
        SentenceEligibility::CoarseParagraphOnly
    );
    assert_eq!(
        set.coverage[0].content_class,
        ParagraphContentClass::ProseOrUnknown
    );
    assert_eq!(
        set.coverage[0].coarse_only_chars,
        set.coverage[0].paragraph_chars
    );
}

#[test]
fn doubled_character_corruption_is_not_a_clean_sentence() {
    let document = document_with_content(
        "NNoottee:: WWhhiillee ggrreeaatt ccaarree wwaass ggiivveenn ttoo rreepprroodduuccee \
         tthhiiss aarrttiiccllee eexxaaccttllyy.. 16 OCTOBER 1964, Volume 146, Number 3642 \
         SCIENCEStrong InferenceCertain systematic methods.",
    );
    let set = document.sentence_text_units();

    assert!(set.units.is_empty());
    assert_eq!(
        set.coverage[0].eligibility,
        SentenceEligibility::CoarseParagraphOnly
    );
}

#[test]
fn missing_space_concatenation_is_not_a_clean_sentence() {
    let document = document_with_content(
        "16 October 1964, Volume 146,Number 3642SCIENCEStrong InferenceCertain systematic \
         methods of scientific thinking may produce much more rapid progressthan others.John \
         R. Platt1Scientists these days tend to keep up a polite fiction.",
    );
    let set = document.sentence_text_units();

    assert!(set.units.is_empty());
    assert_eq!(
        set.coverage[0].eligibility,
        SentenceEligibility::CoarseParagraphOnly
    );
}

#[test]
fn bibliographic_metadata_blob_is_not_a_clean_sentence() {
    let document = document_with_content(
        "StrongInference JohnR.Platt Science ,NewSeries,Vol.146,No.3642.(Oct.16,1964),pp.347-353.",
    );
    let set = document.sentence_text_units();

    assert!(set.units.is_empty());
    assert_eq!(
        set.coverage[0].eligibility,
        SentenceEligibility::CoarseParagraphOnly
    );
}

#[test]
fn genuinely_clean_fallback_sentences_stay_sentence_eligible() {
    let document = document_with_content(
        "Strong Inference\n\nCertain systematic methods of scientific thinking may produce much \
         more rapid progress than others. Scientists these days tend to keep up a polite \
         fiction that all science is equal.",
    );
    let paragraphs = document.paragraph_text_units();
    let set = document.sentence_text_units();

    assert_eq!(paragraphs.units.len(), 2);
    assert_eq!(set.units.len(), 3);
    assert_eq!(set.units[0].text, "Strong Inference");
    assert_eq!(
        set.units[1].text,
        "Certain systematic methods of scientific thinking may produce much more rapid \
         progress than others."
    );
    for coverage in &set.coverage {
        assert_eq!(coverage.eligibility, SentenceEligibility::Eligible);
        assert_eq!(coverage.coarse_only_chars, 0);
    }
}

#[test]
fn citation_terms_inside_legitimate_prose_do_not_degrade() {
    let document = document_with_content(
        "In 1964 the journal's Volume 146 carried this argument forward. Later reviews in 1998 \
         confirmed the point.",
    );
    let set = document.sentence_text_units();

    assert_eq!(set.units.len(), 2);
    assert_eq!(set.coverage[0].eligibility, SentenceEligibility::Eligible);
}

#[tokio::test]
async fn degraded_blob_enumerates_as_paragraph_and_still_reads_exactly() {
    let repository = Arc::new(InMemoryDocumentRepository::default());
    let document = document_with_content(
        "16 October 1964, Volume 146, Number 3642 Strong Inference Certain systematic methods \
         of scientific thinking may produce much more rapid progress than others.\n\nBody prose \
         sentence one. Body prose sentence two.",
    );
    repository
        .save(document.clone())
        .await
        .expect("document should save");
    let use_case = GetTextUnitsUseCase::new(repository);

    let result = use_case
        .execute(command(&document, RequestedTextUnitKind::Sentence, 10))
        .await
        .expect("sentence enumeration should succeed");

    assert_eq!(result.items.len(), 3);
    let degraded = &result.items[0];
    assert_eq!(degraded.effective_kind.as_str(), "paragraph");
    assert_eq!(degraded.content_class.as_str(), "unknown");
    assert_eq!(degraded.content_class_detail, "prose_or_unknown");
    assert_eq!(
        degraded.degradation.as_deref(),
        Some("requested_sentence_but_paragraph_is_coarse_only")
    );
    assert_eq!(result.coverage.coarse_non_prose_items, 1);
    assert_eq!(result.items[1].text, "Body prose sentence one.");
    assert_eq!(result.items[2].effective_kind.as_str(), "sentence");

    // A degraded unit must still be exactly readable by its locator.
    let section = &document.root_sections[0];
    let range = degraded
        .locator
        .normalized_range
        .expect("degraded paragraph item must carry an exact normalized range");
    assert_eq!(
        section
            .normalized_text_slice(range)
            .expect("degraded locator must resolve to the exact owner slice"),
        degraded.text
    );
}

#[test]
fn fallback_pdf_with_unreliable_paragraphs_reports_reliability_degradation() {
    let mut document = document_with_content(
        "16 October 1964, Volume 146, Number 3642 Strong Inference Certain systematic methods \
         of scientific thinking may produce much more rapid progress than others.",
    );
    document.media_type = MediaType("application/pdf".into());
    document
        .metadata
        .insert("pdf_structure_provenance".into(), "page_fallback".into());

    let summary = PersistedDocumentReliabilityInspector
        .inspect(&document)
        .expect("reliability inspection should succeed");

    let evidence = summary
        .evidence
        .iter()
        .find(|evidence| evidence.degradation_count > 0)
        .expect("unreliable fallback paragraphs must surface degradation evidence");
    assert!(
        evidence
            .degradation_codes
            .iter()
            .any(|code| code == "pdf_fallback_paragraph_unreliable")
    );
}

#[test]
fn clean_fallback_pdf_keeps_not_applicable_reliability() {
    let mut document = document_with_content(
        "Certain systematic methods of scientific thinking may produce much more rapid progress \
         than others. Scientists these days tend to keep up a polite fiction.",
    );
    document.media_type = MediaType("application/pdf".into());
    document
        .metadata
        .insert("pdf_structure_provenance".into(), "page_fallback".into());

    let summary = PersistedDocumentReliabilityInspector
        .inspect(&document)
        .expect("reliability inspection should succeed");

    assert!(
        summary
            .evidence
            .iter()
            .all(|evidence| evidence.degradation_count == 0)
    );
}

fn command(
    document: &Document,
    requested_kind: RequestedTextUnitKind,
    max_items: usize,
) -> GetTextUnitsCommand {
    GetTextUnitsCommand {
        document_id: document.id.clone(),
        section_id: SectionId("section://root".into()),
        requested_kind,
        direction: TextUnitDirection::Forward,
        coverage_policy: TextUnitCoveragePolicy::PreserveSource,
        max_items,
        max_chars: None,
        cursor: None,
    }
}

fn document_with_content(content: &str) -> Document {
    Document {
        id: DocumentId("doc:fallback-degradation".into()),
        source: DocumentSource("memory:fallback-degradation".into()),
        title: "Fallback degradation".into(),
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
