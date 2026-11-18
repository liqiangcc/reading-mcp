// PDF-6e, issue #232: the #139 fallback guard stopped counting one more junction shape, for PDF
// prose only. A camel-case name the paragraph is about comes back in every sentence (`ChatGPT`,
// `LLaMA`, `ArabicaQA`), and the junction cannot be told from glue by its own characters, so a PDF
// paragraph that is otherwise clean must be split. Every other format keeps the guard exactly as
// #139 wrote it, and a PDF paragraph with real glue - including a glued word that repeats, and a
// name that occurs once - stays coarse.

use std::collections::BTreeMap;

use reading_mcp::domain::{
    ContentHash, Document, DocumentId, DocumentSource, Location, MediaType, ParagraphContentClass,
    Section, SectionId, SentenceEligibility,
};

fn document_with_media_type(content: &str, media_type: &str) -> Document {
    Document {
        id: DocumentId("doc:pdf6e-guard".into()),
        source: DocumentSource("memory:pdf6e-guard".into()),
        title: "PDF-6e guard".into(),
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

/// Three `ChatGPT` junctions and clean spacing: the guard used to see three glue events and hand
/// the whole paragraph back. In a PDF the name repeats and is ordinary prose.
const ONE_NAME: &str = "Our benchmark compares ChatGPT with a tuned baseline. We ask ChatGPT to \
                        summarise each section of the report. ChatGPT answers every prompt in the \
                        same style.";

#[test]
fn a_pdf_paragraph_about_one_name_is_split_into_sentences() {
    let document = document_with_media_type(ONE_NAME, "application/pdf");
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
        "Our benchmark compares ChatGPT with a tuned baseline."
    );
    assert_eq!(
        set.units[1].text,
        "We ask ChatGPT to summarise each section of the report."
    );
    assert_eq!(
        set.units[2].text,
        "ChatGPT answers every prompt in the same style."
    );
}

#[test]
fn the_same_one_name_text_stays_coarse_in_plain_text() {
    let document = document_with_media_type(ONE_NAME, "text/plain");
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
fn the_same_one_name_text_stays_coarse_in_markdown() {
    let document = document_with_media_type(ONE_NAME, "text/markdown");
    let set = document.sentence_text_units();

    assert!(set.units.is_empty());
    assert_eq!(
        set.coverage[0].eligibility,
        SentenceEligibility::CoarseParagraphOnly
    );
}

/// The name exemption needs the two-capital run: `thecaseofRedPajama` repeats three times and still
/// counts, because a merged pair of ordinary words never carries one.
#[test]
fn a_pdf_paragraph_that_repeats_a_glued_word_stays_coarse() {
    let document = document_with_media_type(
        "thecaseofRedPajama is one dataset. thecaseofRedPajama is another one. \
         thecaseofRedPajama is the third.",
        "application/pdf",
    );
    let set = document.sentence_text_units();

    assert!(set.units.is_empty());
    assert_eq!(
        set.coverage[0].eligibility,
        SentenceEligibility::CoarseParagraphOnly
    );
}

/// The name exemption needs the repetition: three names that each occur once still count, including
/// `GraphCL`, which does carry a two-capital run.
#[test]
fn a_pdf_paragraph_of_names_that_occur_once_stays_coarse() {
    let document = document_with_media_type(
        "PyTorch, DeepSeek and GraphCL are the three baselines we compare in this section.",
        "application/pdf",
    );
    let set = document.sentence_text_units();

    assert!(set.units.is_empty());
    assert_eq!(
        set.coverage[0].eligibility,
        SentenceEligibility::CoarseParagraphOnly
    );
}

/// The punctuation rule is unchanged: an initial glued to the surname that follows it
/// (`J.Platt`, `Z.Phys.`, `A.Einstein`) is exactly the lost space the guard exists for, so the
/// paragraph stays coarse in every format.
#[test]
fn a_pdf_paragraph_of_initials_glued_to_surnames_stays_coarse() {
    let document = document_with_media_type(
        "See J.Platt (2019) and Z.Phys. vol. 24 for the two sources, and A.Einstein for the third.",
        "application/pdf",
    );
    let set = document.sentence_text_units();

    assert!(set.units.is_empty());
    assert_eq!(
        set.coverage[0].eligibility,
        SentenceEligibility::CoarseParagraphOnly
    );
}

/// The exemption removes its own junction and nothing else: the repeated name stops counting, the
/// glued words in the same paragraph keep counting, so the paragraph stays coarse.
#[test]
fn a_pdf_paragraph_with_an_exempt_name_and_real_glue_stays_coarse() {
    let document = document_with_media_type(
        "ChatGPT and thecaseofRedPajama and StrongInference and others.John and ChatGPT appear here.",
        "application/pdf",
    );
    let set = document.sentence_text_units();

    assert!(set.units.is_empty());
    assert_eq!(
        set.coverage[0].eligibility,
        SentenceEligibility::CoarseParagraphOnly
    );
}

/// #139's own shapes, unchanged in a PDF: `JohnR` (two fragments merged), `3642SCIENCE` (a digit
/// run and a capital run), `others.John` (a dot with no space after it), `thecaseofRedPajama` (a
/// merged word that repeats but carries no two-capital run).
#[test]
fn the_139_glue_shapes_still_count_in_a_pdf() {
    let document = document_with_media_type(
        "JohnR.Platt and 3642SCIENCE and others.John and thecaseofRedPajama appear here.",
        "application/pdf",
    );
    let set = document.sentence_text_units();

    assert!(set.units.is_empty());
    assert_eq!(
        set.coverage[0].eligibility,
        SentenceEligibility::CoarseParagraphOnly
    );
}

/// A 2403.02247-like line: every word boundary is lost, so the paragraph stays coarse.
#[test]
fn a_pdf_paragraph_of_lost_word_boundaries_stays_coarse() {
    let document = document_with_media_type(
        "DatasetCurationvsGeneration: ThesuccessofrecentLLM-generateddatasets \
         likeStanfordAlpaca is promising.",
        "application/pdf",
    );
    let set = document.sentence_text_units();

    assert!(set.units.is_empty());
    assert_eq!(
        set.coverage[0].eligibility,
        SentenceEligibility::CoarseParagraphOnly
    );
}
