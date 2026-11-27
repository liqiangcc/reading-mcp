// PDF-7c, issue #232: three missing-space glue narrowings, all PDF-only like PDF-6d/6e/6f,
// priced on the 133-input census of `investigate/pdfc`.
//
// 1. The repeated-name exemption also covers a token that starts with a capital and carries
//    one interior capital (`TensorFlow`, `PyTorch`, `WebTables`, `EchoMimicV2`). PDF-6e's
//    two-adjacent-capitals shape is only the acronym-shaped half of the names a paper repeats;
//    starting with a capital is what keeps a merged run of ordinary words
//    (`thecaseofRedPajama`) counting.
// 2. `caps_tail` counts only for an uppercase run of 4 or more continued by a lowercase tail
//    of 4 or more (`SCIENCEStrong`). The short shapes (`SASRec`, `RESTful`, `MLPs`) are names,
//    and 0 of the 177 junctions the census judged was real glue.
// 3. `digit_upper` is exempt for a tight unit: an uppercase run of at most two letters that
//    ends its token (`36GB`, `2PC`, `3PC`, `474GB`). The reference extractor writes those
//    tokens the same way, so the guard was inventing a lost space.
//
// Every other format keeps the all-formats guard byte for byte, and each narrowing keeps a
// real glue junction refused.

use std::collections::BTreeMap;

use reading_mcp::domain::{
    ContentHash, Document, DocumentId, DocumentSource, Location, MediaType, ParagraphContentClass,
    Section, SectionId, SentenceEligibility,
};

fn document_with_media_type(content: &str, media_type: &str) -> Document {
    Document {
        id: DocumentId("doc:pdf7c-glue-rules".into()),
        source: DocumentSource("memory:pdf7c-glue-rules".into()),
        title: "PDF-7c glue rules".into(),
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

fn coarse_only(document: &Document) -> bool {
    let set = document.sentence_text_units();
    assert_eq!(set.coverage.len(), 1);
    set.coverage[0].eligibility == SentenceEligibility::CoarseParagraphOnly
}

fn split_cleanly(document: &Document) -> bool {
    let set = document.sentence_text_units();
    assert_eq!(set.coverage.len(), 1);
    set.coverage[0].eligibility == SentenceEligibility::Eligible
        && set.coverage[0].coarse_only_chars == 0
        && !set.units.is_empty()
}

// ---------------------------------------------------------------------------------------------
// W1b: a repeated capitalized name with one interior capital is a name.
// ---------------------------------------------------------------------------------------------

/// `TensorFlow` repeats in every sentence and carries exactly one interior capital: PDF-6e's
/// two-adjacent-capitals shape does not cover it, so the paragraph used to hand back whole.
const TENSORFLOW: &str = "TensorFlow is the framework every baseline in this paper uses. We \
                          train each TensorFlow model on the same split. TensorFlow serves \
                          the final deployment without changes.";

#[test]
fn a_pdf_paragraph_of_a_repeated_camel_case_name_is_split() {
    let document = document_with_media_type(TENSORFLOW, "application/pdf");
    assert!(split_cleanly(&document));
    let set = document.sentence_text_units();
    assert_eq!(
        set.coverage[0].content_class,
        ParagraphContentClass::ProseOrUnknown
    );
    assert_eq!(set.units.len(), 3);
    assert_eq!(
        set.units[0].text,
        "TensorFlow is the framework every baseline in this paper uses."
    );
}

#[test]
fn the_same_camel_case_text_stays_coarse_in_plain_text() {
    let document = document_with_media_type(TENSORFLOW, "text/plain");
    assert!(coarse_only(&document));
}

#[test]
fn the_same_camel_case_text_stays_coarse_in_markdown() {
    let document = document_with_media_type(TENSORFLOW, "text/markdown");
    assert!(coarse_only(&document));
}

/// The shape must start with a capital: `thecaseofRedPajama` repeats and carries interior
/// capitals, but it is a merged run of ordinary words and keeps counting. This is the real
/// glue junction W1b must stay refused on.
#[test]
fn a_repeated_lowercase_led_merged_word_still_refuses() {
    let document = document_with_media_type(
        "thecaseofRedPajama thecaseofRedPajama thecaseofRedPajama is one dataset. \
         thecaseofRedPajama is another one.",
        "application/pdf",
    );
    assert!(coarse_only(&document));
}

/// Both halves of the shape are required. `theofRedPajama` repeats and carries interior
/// capitals but starts lowercase, and `Webframework` repeats and starts with a capital but
/// carries none: both are merged runs of ordinary words and keep counting. (A capitalized
/// token without an interior capital produces no lower->upper junction at all, so the merged
/// lowercase-led word carries the events here.)
#[test]
fn a_repeated_merged_run_without_the_name_shape_still_refuses() {
    let document = document_with_media_type(
        "theofRedPajama theofRedPajama theofRedPajama appear here. A clean sentence follows.",
        "application/pdf",
    );
    assert!(coarse_only(&document));
}

/// The same sentence with `WebTables` is split: one interior capital, starting with a
/// capital, repeated - a name.
#[test]
fn a_pdf_paragraph_of_a_repeated_name_with_one_interior_capital_is_split() {
    let document = document_with_media_type(
        "The WebTables corpus is large. WebTables rows feed every baseline. WebTables columns \
         feed them too.",
        "application/pdf",
    );
    assert!(split_cleanly(&document));
}

// ---------------------------------------------------------------------------------------------
// W2: caps_tail counts only for a run of 4+ continued by a tail of 4+.
// ---------------------------------------------------------------------------------------------

/// `SNNs`, `LLMs` and `PDFs` are acronyms with a plural `s`: a run of three with a tail of
/// one. The guard used to count all three and hand the whole paragraph back.
const ACRONYM_TRIPLE: &str = "The SNNs, the LLMs and the PDFs are all here. Every one of them \
                              is ordinary prose. The reader should see these sentences.";

#[test]
fn a_pdf_paragraph_of_short_caps_tails_is_split() {
    let document = document_with_media_type(ACRONYM_TRIPLE, "application/pdf");
    assert!(split_cleanly(&document));
    let set = document.sentence_text_units();
    assert_eq!(set.units.len(), 3);
    assert_eq!(
        set.units[0].text,
        "The SNNs, the LLMs and the PDFs are all here."
    );
}

#[test]
fn the_same_acronym_text_stays_coarse_in_plain_text() {
    let document = document_with_media_type(ACRONYM_TRIPLE, "text/plain");
    assert!(coarse_only(&document));
}

/// The long merged fragment the rule was written for keeps counting: `SCIENCEStrong` is a
/// run of seven continued by a tail of six. This is the real glue junction W2 must stay
/// refused on.
#[test]
fn a_pdf_paragraph_with_a_long_caps_tail_still_refuses() {
    let document = document_with_media_type(
        "Number 3642SCIENCEStrong InferenceCertain systematic methods of scientific thinking \
         may produce much more rapid progressthan others in one merged block.",
        "application/pdf",
    );
    assert!(coarse_only(&document));
}

/// A run of four needs a tail of four too: `DINOCell` has the run but not the tail, so the
/// three repetitions here are names, not glue.
#[test]
fn a_pdf_paragraph_of_caps_runs_with_short_tails_is_split() {
    let document = document_with_media_type(
        "DINOCell is the first model. DINOCell is the second model too. DINOCell is the \
         third model as well.",
        "application/pdf",
    );
    assert!(split_cleanly(&document));
}

// ---------------------------------------------------------------------------------------------
// W3: digit_upper is exempt for a tight unit.
// ---------------------------------------------------------------------------------------------

/// `36GB`, `2.2GHz` and `3PC` are units and a protocol, written without a space by the PDF
/// itself; the reference extractor produces the same tokens. This is the census fixture of
/// PR-4's acceptance.
const TIGHT_UNITS: &str = "Each node has 36GB of memory and 2.2GHz CPUs. The 3PC setting is \
                           small. A clean final sentence follows here.";

#[test]
fn a_pdf_paragraph_of_tight_units_is_split() {
    let document = document_with_media_type(TIGHT_UNITS, "application/pdf");
    assert!(split_cleanly(&document));
    let set = document.sentence_text_units();
    assert_eq!(set.units.len(), 3);
    assert_eq!(
        set.units[0].text,
        "Each node has 36GB of memory and 2.2GHz CPUs."
    );
    assert_eq!(set.units[1].text, "The 3PC setting is small.");
}

#[test]
fn the_same_tight_unit_text_stays_coarse_in_plain_text() {
    let document = document_with_media_type(TIGHT_UNITS, "text/plain");
    assert!(coarse_only(&document));
}

/// A digit before a capital run of three or more that ends its token is real damage: the
/// Caesar-shifted raster pages (`7KHREVHUYDWRU`) and a page number glued to a running header
/// (`w24458NATIONAL`). Three of them in one sentence keep the paragraph coarse. This is the
/// real glue junction W3 must stay refused on.
#[test]
fn a_pdf_paragraph_of_long_capital_runs_after_digits_still_refuses() {
    let document = document_with_media_type(
        "The 7KHREVHUYDWRU page and the 474GBHIDDEN block and the w24458NATIONAL running \
         header are merged. A clean sentence follows.",
        "application/pdf",
    );
    assert!(coarse_only(&document));
}

/// A capital run continued by lowercase keeps counting whatever its length: `7Binstruct` and
/// `13Bvariant` and `3Dprinter` are a size glued to the word that follows it.
#[test]
fn a_pdf_paragraph_of_sizes_glued_to_words_still_refuses() {
    let document = document_with_media_type(
        "The 7Binstruct-model 13Bvariant 3Dprinter paper is different. A second sentence is \
         clean.",
        "application/pdf",
    );
    assert!(coarse_only(&document));
}
