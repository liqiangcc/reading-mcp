//! Plans the declared TextUnit stream of one section without materializing any text.
//!
//! The planner is the single source of the stream order, of the per-unit classification and of the
//! coverage statistics `get_text_units` reports. It borrows the persisted units, so it never clones
//! unit text.

use std::collections::HashMap;

use crate::application::get_text_units::{
    RequestedTextUnitKind, TextUnitContentClass, TextUnitEnumerationCoverage,
};
use crate::application::ports::ApplicationError;
use crate::domain::{
    ParagraphContentClass, ParagraphSectionCoverage, SectionId, SentenceEligibility,
    SentenceParagraphCoverage, SentenceTextUnit, TextUnit,
};

/// Version of the stored per-section TextUnit stream. Bump it when the stream shape or its meaning changes.
pub const TEXT_UNIT_STREAM_VERSION: &str = "text-unit-stream/v1";

/// Version of the whole stored TextUnit index (documents, sections, items, coverage, streams, summaries).
/// Bump it when the store layout or its meaning changes so documents are re-ingested on reopen.
pub const TEXT_UNIT_STORE_VERSION: &str = "text-unit-store/v2";

/// A borrowed unit of a section stream.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StreamUnit<'a> {
    Paragraph(&'a TextUnit),
    Sentence(&'a SentenceTextUnit),
}

impl<'a> StreamUnit<'a> {
    /// The kind this unit is persisted as in the TextUnit store.
    pub fn kind_str(self) -> &'static str {
        match self {
            Self::Paragraph(_) => "paragraph",
            Self::Sentence(_) => "sentence",
        }
    }

    pub fn paragraph_index(self) -> usize {
        match self {
            Self::Paragraph(unit) => unit.paragraph_index,
            Self::Sentence(unit) => unit.paragraph_index,
        }
    }

    pub fn sentence_index(self) -> Option<usize> {
        match self {
            Self::Paragraph(_) => None,
            Self::Sentence(unit) => Some(unit.sentence_index),
        }
    }

    pub fn source_order(self) -> usize {
        match self {
            Self::Paragraph(unit) => unit.source_order,
            Self::Sentence(unit) => unit.source_order,
        }
    }
}

/// One item of the PreserveSource stream, with everything the read paths need except the text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StreamEntry<'a> {
    pub unit: StreamUnit<'a>,
    /// Class of the owning paragraph's coverage.
    pub content_class: ParagraphContentClass,
    pub degradation: Option<&'static str>,
    /// `unit.text.chars().count()`, as the char budget walks today.
    pub char_count: usize,
    /// `false` exactly for the coarse items that EligibleOnly skips.
    pub eligible: bool,
}

/// The PreserveSource stream of one section and the coverage of both policies.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SectionStreamPlan<'a> {
    pub entries: Vec<StreamEntry<'a>>,
    pub preserve_source: TextUnitEnumerationCoverage,
    pub eligible_only: TextUnitEnumerationCoverage,
}

/// Plans the section stream for `requested_kind`.
///
/// Both coverages equal what the enumeration loop used to compute for each policy: the EligibleOnly
/// stream is exactly the PreserveSource entries filtered by `eligible`.
pub fn plan_section_stream<'a>(
    section_id: &SectionId,
    paragraphs: &[&'a TextUnit],
    paragraph_coverage: Option<&ParagraphSectionCoverage>,
    sentences: &[&'a SentenceTextUnit],
    sentence_coverage: &[&SentenceParagraphCoverage],
    requested_kind: RequestedTextUnitKind,
) -> Result<SectionStreamPlan<'a>, ApplicationError> {
    let section_coverage = paragraph_coverage.ok_or_else(|| {
        ApplicationError::InvalidRequest(format!(
            "paragraph coverage is unavailable for section {}",
            section_id.0
        ))
    })?;

    let coverage_by_paragraph = sentence_coverage
        .iter()
        .map(|coverage| (coverage.paragraph_index, *coverage))
        .collect::<HashMap<_, _>>();

    let mut sentences_by_paragraph: HashMap<usize, Vec<&'a SentenceTextUnit>> = HashMap::new();
    for sentence in sentences {
        sentences_by_paragraph
            .entry(sentence.paragraph_index)
            .or_default()
            .push(sentence);
    }

    let sentence_eligible_paragraphs = coverage_by_paragraph
        .values()
        .filter(|coverage| coverage.eligibility == SentenceEligibility::Eligible)
        .count();
    let non_prose_paragraphs = coverage_by_paragraph
        .values()
        .filter(|coverage| is_non_prose_class(coverage.content_class))
        .count();
    let coarse_structural_paragraphs = coverage_by_paragraph
        .values()
        .filter(|coverage| is_coarse_structural_class(coverage.content_class))
        .count();
    let sentence_separator_chars = coverage_by_paragraph
        .values()
        .map(|coverage| coverage.separator_chars)
        .sum::<usize>();

    let mut entries = Vec::new();
    for paragraph in paragraphs {
        let paragraph_coverage = coverage_by_paragraph
            .get(&paragraph.paragraph_index)
            .copied()
            .ok_or_else(|| {
                ApplicationError::InvalidRequest(format!(
                    "sentence coverage is unavailable for paragraph {} in section {}",
                    paragraph.paragraph_index, section_id.0
                ))
            })?;
        let is_coarse = paragraph_coverage.eligibility == SentenceEligibility::CoarseParagraphOnly;

        match requested_kind {
            RequestedTextUnitKind::Paragraph => {
                entries.push(StreamEntry {
                    unit: StreamUnit::Paragraph(paragraph),
                    content_class: paragraph_coverage.content_class,
                    degradation: None,
                    char_count: paragraph.text.chars().count(),
                    eligible: !is_coarse,
                });
            }
            RequestedTextUnitKind::Sentence if is_coarse => {
                entries.push(StreamEntry {
                    unit: StreamUnit::Paragraph(paragraph),
                    content_class: paragraph_coverage.content_class,
                    degradation: Some(coarse_degradation(paragraph_coverage.content_class)),
                    char_count: paragraph.text.chars().count(),
                    eligible: false,
                });
            }
            RequestedTextUnitKind::Sentence => {
                for sentence in sentences_by_paragraph
                    .get(&paragraph.paragraph_index)
                    .into_iter()
                    .flatten()
                {
                    entries.push(StreamEntry {
                        unit: StreamUnit::Sentence(sentence),
                        content_class: paragraph_coverage.content_class,
                        degradation: None,
                        char_count: sentence.text.chars().count(),
                        eligible: true,
                    });
                }
            }
        }
    }

    let intentionally_skipped = entries.iter().filter(|entry| !entry.eligible).count();
    let preserve_represented_paragraphs = entries
        .iter()
        .filter(|entry| matches!(entry.unit, StreamUnit::Paragraph(_)))
        .count();
    let preserve_represented_sentences = entries
        .iter()
        .filter(|entry| matches!(entry.unit, StreamUnit::Sentence(_)))
        .count();
    let eligible_represented_paragraphs = entries
        .iter()
        .filter(|entry| entry.eligible && matches!(entry.unit, StreamUnit::Paragraph(_)))
        .count();
    let eligible_represented_sentences = entries
        .iter()
        .filter(|entry| entry.eligible && matches!(entry.unit, StreamUnit::Sentence(_)))
        .count();
    let (coarse_non_prose_items, coarse_structural_items) = if requested_kind
        == RequestedTextUnitKind::Sentence
    {
        (
            entries
                .iter()
                .filter(|entry| !entry.eligible && !is_coarse_structural_class(entry.content_class))
                .count(),
            entries
                .iter()
                .filter(|entry| !entry.eligible && is_coarse_structural_class(entry.content_class))
                .count(),
        )
    } else {
        (0, 0)
    };

    let preserve_source = TextUnitEnumerationCoverage {
        owner_chars: section_coverage.owner_chars,
        section_separator_chars: section_coverage.separator_chars,
        sentence_separator_chars,
        paragraph_count: section_coverage.paragraph_count,
        sentence_eligible_paragraphs,
        non_prose_paragraphs,
        coarse_structural_paragraphs,
        represented_paragraphs: preserve_represented_paragraphs,
        represented_sentences: preserve_represented_sentences,
        coarse_non_prose_items,
        coarse_structural_items,
        intentionally_skipped: 0,
        unsupported_gaps: 0,
        source_complete: true,
    };
    let eligible_only = TextUnitEnumerationCoverage {
        represented_paragraphs: eligible_represented_paragraphs,
        represented_sentences: eligible_represented_sentences,
        coarse_non_prose_items: 0,
        coarse_structural_items: 0,
        intentionally_skipped,
        source_complete: false,
        ..preserve_source.clone()
    };

    Ok(SectionStreamPlan {
        entries,
        preserve_source,
        eligible_only,
    })
}

fn coarse_degradation(class: ParagraphContentClass) -> &'static str {
    match class {
        ParagraphContentClass::BlockQuote | ParagraphContentClass::ListItem => {
            "flat_native_container_no_nested_textunit_evidence"
        }
        ParagraphContentClass::CodeBlock
        | ParagraphContentClass::Preformatted
        | ParagraphContentClass::Table => "requested_sentence_but_non_prose_is_paragraph_only",
        ParagraphContentClass::ProseOrUnknown | ParagraphContentClass::NativeParagraph => {
            "requested_sentence_but_paragraph_is_coarse_only"
        }
    }
}

pub(crate) fn content_class(class: ParagraphContentClass) -> (TextUnitContentClass, &'static str) {
    match class {
        ParagraphContentClass::ProseOrUnknown
        | ParagraphContentClass::NativeParagraph
        | ParagraphContentClass::BlockQuote
        | ParagraphContentClass::ListItem => (TextUnitContentClass::Unknown, class.as_str()),
        ParagraphContentClass::CodeBlock
        | ParagraphContentClass::Preformatted
        | ParagraphContentClass::Table => (TextUnitContentClass::NonProse, class.as_str()),
    }
}

fn is_non_prose_class(class: ParagraphContentClass) -> bool {
    matches!(
        class,
        ParagraphContentClass::CodeBlock
            | ParagraphContentClass::Preformatted
            | ParagraphContentClass::Table
    )
}

fn is_coarse_structural_class(class: ParagraphContentClass) -> bool {
    matches!(
        class,
        ParagraphContentClass::BlockQuote | ParagraphContentClass::ListItem
    )
}
