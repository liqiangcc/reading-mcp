// PDF-6d, issue #232: the #139 fallback guard stopped counting one junction shape for PDF prose
// only. A digit directly before a single capital that ends its token is a model size ("7B"), a
// dimension ("3D"), a unit ("4K,") or a panel label ("Fig. 3A"), so a PDF paragraph that is
// otherwise clean must be split. Every other format keeps the guard exactly as #139 wrote it, and
// a PDF paragraph with real glue stays coarse.

use std::collections::BTreeMap;

use reading_mcp::domain::{
    ContentHash, Document, DocumentId, DocumentSource, Location, MediaType, ParagraphContentClass,
    Section, SectionId, SentenceEligibility,
};

fn document_with_media_type(content: &str, media_type: &str) -> Document {
    Document {
        id: DocumentId("doc:pdf6d-guard".into()),
        source: DocumentSource("memory:pdf6d-guard".into()),
        title: "PDF-6d guard".into(),
        media_type: MediaType(media_type.into()),
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

/// Three model-size junctions and clean spacing: the guard used to see three glue events and hand
/// the whole paragraph back. In a PDF it is ordinary prose and must be split.
const MODEL_SIZES: &str = "We compare the 7B model with the 13B model and the 3D baseline on \
                           every task. The 7B run is the cheapest of the three. Our 3D baseline \
                           is a different setting altogether.";

#[test]
fn a_pdf_paragraph_of_model_sizes_is_split_into_sentences() {
    let document = document_with_media_type(MODEL_SIZES, "application/pdf");
    let set = document.sentence_text_units();

    assert_eq!(set.coverage.len(), 1);
    assert_eq!(
        set.coverage[0].content_class,
        ParagraphContentClass::ProseOrUnknown
    );
    assert_eq!(set.coverage[0].eligibility, SentenceEligibility::Eligible);
    assert_eq!(set.coverage[0].coarse_only_chars, 0);
    assert_eq!(set.units.len(), 3);
    assert_eq!(
        set.units[0].text,
        "We compare the 7B model with the 13B model and the 3D baseline on every task."
    );
    assert_eq!(
        set.units[1].text,
        "The 7B run is the cheapest of the three."
    );
    assert_eq!(
        set.units[2].text,
        "Our 3D baseline is a different setting altogether."
    );
}

#[test]
fn the_same_model_size_text_stays_coarse_in_plain_text() {
    let document = document_with_media_type(MODEL_SIZES, "text/plain");
    let set = document.sentence_text_units();

    assert!(set.units.is_empty());
    assert_eq!(set.coverage.len(), 1);
    assert_eq!(
        set.coverage[0].eligibility,
        SentenceEligibility::CoarseParagraphOnly
    );
    assert_eq!(
        set.coverage[0].coarse_only_chars,
        set.coverage[0].paragraph_chars
    );
}

#[test]
fn the_same_model_size_text_stays_coarse_in_markdown() {
    let document = document_with_media_type(MODEL_SIZES, "text/markdown");
    let set = document.sentence_text_units();

    assert!(set.units.is_empty());
    assert_eq!(
        set.coverage[0].eligibility,
        SentenceEligibility::CoarseParagraphOnly
    );
}

/// The #139 masthead shape, unchanged: a PDF paragraph that merges front matter must stay coarse.
const MASTHEAD: &str = "16 October 1964, Volume 146,Number 3642SCIENCEStrong InferenceCertain \
                        systematic methods of scientific thinking may produce much more rapid \
                        progressthan others.John R. Platt1Scientists these days tend to keep up a \
                        polite fiction.";

#[test]
fn a_pdf_paragraph_with_real_glue_stays_coarse() {
    let document = document_with_media_type(MASTHEAD, "application/pdf");
    let set = document.sentence_text_units();

    assert!(set.units.is_empty());
    assert_eq!(set.coverage.len(), 1);
    assert_eq!(
        set.coverage[0].eligibility,
        SentenceEligibility::CoarseParagraphOnly
    );
    assert_eq!(
        set.coverage[0].coarse_only_chars,
        set.coverage[0].paragraph_chars
    );
}

/// A size glued to the word that follows it is still glue in a PDF: the exemption needs the single
/// capital to end its token. PDF-6f: the three events must sit in ONE sentence for the paragraph
/// to stay coarse - spread over three sentences, each sentence is clean and the paragraph is
/// now split.
#[test]
fn a_pdf_paragraph_of_sizes_glued_to_words_stays_coarse() {
    let document = document_with_media_type(
        "The 7Binstruct-model 13Bvariant 3Dprinter paper is different. A second sentence is \
         clean.",
        "application/pdf",
    );
    let set = document.sentence_text_units();

    assert!(set.units.is_empty());
    assert_eq!(
        set.coverage[0].eligibility,
        SentenceEligibility::CoarseParagraphOnly
    );
}
