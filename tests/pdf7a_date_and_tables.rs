// PDF-7a, issue #232: two rules that ship together, both PDF-only like PDF-6d/6e/6f.
//
// 1. In `has_bibliographic_metadata_run`, a 4-digit year within +-2 words of a month word is part
//    of that date and does not count as a second marker. `January 2000 through December 2016`
//    used to count four markers in ten words and hand the whole paragraph back; volume, number,
//    pages, DOI/URL and rights markers are untouched, so a real masthead still refuses.
// 2. A block whose first line opens with a caption marker (`Table`, `Fig.`/`Figure`, `Panel`,
//    `Chart`, `Algorithm`, `Listing`) and whose body is mostly numeric or whitespace is the
//    existing non-prose class `table` before the #139 guard runs: one honest coarse unit instead
//    of a refused paragraph or a run of junk sentences.
//
// Every other format keeps the all-formats guard and the all-formats classes byte for byte.

use std::collections::BTreeMap;

use reading_mcp::domain::{
    ContentHash, Document, DocumentId, DocumentSource, Location, MediaType, ParagraphContentClass,
    Section, SectionId, SentenceEligibility,
};

fn document_with_media_type(content: &str, media_type: &str) -> Document {
    Document {
        id: DocumentId("doc:pdf7a-date-and-tables".into()),
        source: DocumentSource("memory:pdf7a-date-and-tables".into()),
        title: "PDF-7a dates and tables".into(),
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

/// Two dates in ten words: four markers under the old rule, two under the new one.
const SAMPLE_PERIOD: &str = "Our sample period is January 2000 through December 2016. We keep every firm that survives \
     the window. The panel is unbalanced by construction.";

#[test]
fn a_pdf_paragraph_of_two_dates_is_split_into_sentences() {
    let document = document_with_media_type(SAMPLE_PERIOD, "application/pdf");
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
        "Our sample period is January 2000 through December 2016."
    );
}

#[test]
fn the_same_date_text_still_refuses_in_plain_text() {
    // PDF-only: the all-formats guard counts the year beside the month exactly as #139 wrote it.
    let document = document_with_media_type(SAMPLE_PERIOD, "text/plain");
    let set = document.sentence_text_units();

    assert!(set.units.is_empty());
    assert_eq!(
        set.coverage[0].eligibility,
        SentenceEligibility::CoarseParagraphOnly
    );
}

/// The masthead the rule was written for: volume, number, pages and one date still reach four
/// markers, and the year inside `Oct. 16, 1964` is not a fifth.
const MASTHEAD: &str =
    "Science , New Series . Vol. 146 , No. 3642 . Oct. 16 , 1964 . Pp. 347-353 .";

#[test]
fn the_masthead_still_refuses() {
    let document = document_with_media_type(MASTHEAD, "application/pdf");
    let set = document.sentence_text_units();

    assert!(set.units.is_empty());
    assert_eq!(
        set.coverage[0].eligibility,
        SentenceEligibility::CoarseParagraphOnly
    );
    assert_eq!(
        set.coverage[0].coarse_only_chars,
        set.coverage[0].paragraph_chars
    );
}

/// A caption line over a numeric body: four rows that are each mostly numeric, so the body is a
/// table and the block is one non-prose unit.
const TABLE_BLOCK: &str = "Table 7 CH-3 alphas and standard errors\n\
                           Group    alpha   s.e.    t\n\
                           CH-3     0.11    0.03    3.67\n\
                           CH-5     0.19    0.05    3.80\n\
                           All      0.27    0.04    6.75";

#[test]
fn a_pdf_table_block_is_class_table_and_not_split() {
    let document = document_with_media_type(TABLE_BLOCK, "application/pdf");
    let set = document.sentence_text_units();

    assert!(set.units.is_empty());
    assert_eq!(set.coverage.len(), 1);
    assert_eq!(set.coverage[0].content_class, ParagraphContentClass::Table);
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
fn the_same_table_block_stays_prose_in_plain_text() {
    // PDF-only, like the guard: a plain-text block keeps today's class and verdict.
    let document = document_with_media_type(TABLE_BLOCK, "text/plain");
    let set = document.sentence_text_units();

    assert_eq!(
        set.coverage[0].content_class,
        ParagraphContentClass::ProseOrUnknown
    );
}

#[test]
fn a_pdf_prose_paragraph_that_mentions_a_table_is_still_prose() {
    // "Table 7" is a reference, not a caption: the paragraph is mostly letters, so the numeric
    // share keeps it prose and the guard decides its verdict as before.
    let document = document_with_media_type(
        "Our estimates, reported in Table 7, are stable across the three samples. The second \
         column of Table 7 shows the same pattern. We therefore prefer the simpler \
         specification.",
        "application/pdf",
    );
    let set = document.sentence_text_units();

    assert_eq!(
        set.coverage[0].content_class,
        ParagraphContentClass::ProseOrUnknown
    );
    assert_eq!(set.coverage[0].eligibility, SentenceEligibility::Eligible);
    assert_eq!(set.units.len(), 3);
}

#[test]
fn a_pdf_caption_over_a_prose_body_is_not_a_table() {
    // A caption marker alone is not enough: the body is prose, so the block keeps its class and
    // its sentences.
    let document = document_with_media_type(
        "Table 7 reports the two-sample estimates for every cohort. The first column holds the \
         treatment group and the second the control. Both are stable across the window.",
        "application/pdf",
    );
    let set = document.sentence_text_units();

    assert_eq!(
        set.coverage[0].content_class,
        ParagraphContentClass::ProseOrUnknown
    );
    assert_eq!(set.coverage[0].eligibility, SentenceEligibility::Eligible);
    assert_eq!(set.units.len(), 3);
}

#[test]
fn a_pdf_caption_block_with_a_real_glued_junction_is_still_a_table() {
    // The table class is decided before the guard, so a table body that also carries the
    // masthead-shaped glue never reaches the guard: one coarse unit, honestly labelled.
    let document = document_with_media_type(
        "Fig. 2 Firm-size distribution\n\
         Size      share   count\n\
         1-9       0.42    1,204\n\
         10-99     0.31      887\n\
         100+      0.27SCIENCEStrong   412\n\
         Total     1.00    2,503",
        "application/pdf",
    );
    let set = document.sentence_text_units();

    assert!(set.units.is_empty());
    assert_eq!(set.coverage[0].content_class, ParagraphContentClass::Table);
    assert_eq!(
        set.coverage[0].eligibility,
        SentenceEligibility::CoarseParagraphOnly
    );
}
