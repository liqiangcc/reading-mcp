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
///
/// v11 (SEC-3c) changes the `document_sections_v1` row shape: the location is a compact positional
/// blob, the title a tagged raw/zlib blob, and the id lookup index is `(document_id, id_hash)`.
pub const TEXT_UNIT_STORE_VERSION: &str = "text-unit-store/v11";

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

/// A paragraph unit reduced to the fields the planner needs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ParagraphDesc {
    pub paragraph_index: usize,
    pub char_count: usize,
}

/// A sentence unit reduced to the fields the planner needs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SentenceDesc {
    pub paragraph_index: usize,
    pub sentence_index: usize,
    pub char_count: usize,
}

/// One sentence-coverage record of the section, reduced to the fields the planner needs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ParagraphCoverageDesc {
    pub paragraph_index: usize,
    pub content_class: ParagraphContentClass,
    pub eligibility: SentenceEligibility,
    pub separator_chars: usize,
}

/// The section-level coverage reduced to the fields the planner needs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SectionCoverageDesc {
    pub owner_chars: usize,
    pub separator_chars: usize,
    pub paragraph_count: usize,
}

/// A unit of a descriptor stream: an index into the paragraphs or sentences slice.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnitRef {
    Paragraph(usize),
    Sentence(usize),
}

/// One item of a descriptor stream, with everything the read paths need except the text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StreamEntryDesc {
    pub unit: UnitRef,
    /// Class of the owning paragraph's coverage.
    pub content_class: ParagraphContentClass,
    pub degradation: Option<&'static str>,
    /// The normalized char length of the unit.
    pub char_count: usize,
    /// `false` exactly for the coarse items that EligibleOnly skips.
    pub eligible: bool,
}

/// The PreserveSource stream of one section, described without unit text or ids.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SectionStreamPlanDesc {
    pub entries: Vec<StreamEntryDesc>,
    pub preserve_source: TextUnitEnumerationCoverage,
    pub eligible_only: TextUnitEnumerationCoverage,
}

/// Plans the section stream for `requested_kind`.
///
/// Both coverages equal what the enumeration loop used to compute for each policy: the EligibleOnly
/// stream is exactly the PreserveSource entries filtered by `eligible`.
///
/// This is a thin adapter over [`plan_section_stream_desc`]: it reads `char_count` from the unit text,
/// exactly as the enumeration loop always has.
pub fn plan_section_stream<'a>(
    section_id: &SectionId,
    paragraphs: &[&'a TextUnit],
    paragraph_coverage: Option<&ParagraphSectionCoverage>,
    sentences: &[&'a SentenceTextUnit],
    sentence_coverage: &[&SentenceParagraphCoverage],
    requested_kind: RequestedTextUnitKind,
) -> Result<SectionStreamPlan<'a>, ApplicationError> {
    let paragraph_descs = paragraphs
        .iter()
        .map(|unit| ParagraphDesc {
            paragraph_index: unit.paragraph_index,
            char_count: unit.text.chars().count(),
        })
        .collect::<Vec<_>>();
    let sentence_descs = sentences
        .iter()
        .map(|unit| SentenceDesc {
            paragraph_index: unit.paragraph_index,
            sentence_index: unit.sentence_index,
            char_count: unit.text.chars().count(),
        })
        .collect::<Vec<_>>();
    let coverage_descs = sentence_coverage
        .iter()
        .map(|coverage| ParagraphCoverageDesc {
            paragraph_index: coverage.paragraph_index,
            content_class: coverage.content_class,
            eligibility: coverage.eligibility,
            separator_chars: coverage.separator_chars,
        })
        .collect::<Vec<_>>();
    let section_coverage_desc = paragraph_coverage.map(|coverage| SectionCoverageDesc {
        owner_chars: coverage.owner_chars,
        separator_chars: coverage.separator_chars,
        paragraph_count: coverage.paragraph_count,
    });

    let plan = plan_section_stream_desc(
        section_id,
        &paragraph_descs,
        section_coverage_desc.as_ref(),
        &sentence_descs,
        &coverage_descs,
        requested_kind,
    )?;

    Ok(SectionStreamPlan {
        entries: plan
            .entries
            .into_iter()
            .map(|entry| StreamEntry {
                unit: match entry.unit {
                    UnitRef::Paragraph(index) => StreamUnit::Paragraph(paragraphs[index]),
                    UnitRef::Sentence(index) => StreamUnit::Sentence(sentences[index]),
                },
                content_class: entry.content_class,
                degradation: entry.degradation,
                char_count: entry.char_count,
                eligible: entry.eligible,
            })
            .collect(),
        preserve_source: plan.preserve_source,
        eligible_only: plan.eligible_only,
    })
}

/// Plans the section stream from unit descriptors, without touching unit text or ids.
///
/// The decision logic, error messages, coverage counters and degradation strings are the same as
/// [`plan_section_stream`]; only the unit identity is an index into the given slices.
pub fn plan_section_stream_desc(
    section_id: &SectionId,
    paragraphs: &[ParagraphDesc],
    section_coverage: Option<&SectionCoverageDesc>,
    sentences: &[SentenceDesc],
    coverage: &[ParagraphCoverageDesc],
    requested_kind: RequestedTextUnitKind,
) -> Result<SectionStreamPlanDesc, ApplicationError> {
    let mut entries = Vec::new();
    let counts = walk_section_stream(
        section_id,
        paragraphs,
        section_coverage,
        sentences,
        coverage,
        requested_kind,
        &mut |entry| entries.push(entry),
    )?;
    Ok(SectionStreamPlanDesc {
        entries,
        preserve_source: counts.preserve_source,
        eligible_only: counts.eligible_only,
    })
}

/// The summary side of one stream kind: both coverages and both totals, accumulated in one pass over
/// the same entries the planner emits. Ingest uses this so it never materializes a sentence stream.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SectionStreamCounts {
    pub total_items_preserve: usize,
    pub total_items_eligible: usize,
    pub preserve_source: TextUnitEnumerationCoverage,
    pub eligible_only: TextUnitEnumerationCoverage,
}

/// Walks the section stream of `requested_kind` once, emitting each entry in planner order and
/// accumulating the summary counters the planner reports. This is the single implementation of the
/// stream order and of the coverage counters: [`plan_section_stream_desc`] collects the entries and
/// the store counts them while cutting chunks.
#[allow(clippy::too_many_arguments)]
pub(crate) fn walk_section_stream(
    section_id: &SectionId,
    paragraphs: &[ParagraphDesc],
    section_coverage: Option<&SectionCoverageDesc>,
    sentences: &[SentenceDesc],
    coverage: &[ParagraphCoverageDesc],
    requested_kind: RequestedTextUnitKind,
    emit: &mut dyn FnMut(StreamEntryDesc),
) -> Result<SectionStreamCounts, ApplicationError> {
    let section_coverage = section_coverage.ok_or_else(|| {
        ApplicationError::InvalidRequest(format!(
            "paragraph coverage is unavailable for section {}",
            section_id.0
        ))
    })?;

    let coverage_by_paragraph = coverage
        .iter()
        .map(|coverage| (coverage.paragraph_index, coverage))
        .collect::<HashMap<_, _>>();

    let mut sentences_by_paragraph: HashMap<usize, Vec<usize>> = HashMap::new();
    for (index, sentence) in sentences.iter().enumerate() {
        sentences_by_paragraph
            .entry(sentence.paragraph_index)
            .or_default()
            .push(index);
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

    let mut total = 0usize;
    let mut eligible_total = 0usize;
    let mut represented_paragraphs = 0usize;
    let mut represented_sentences = 0usize;
    let mut eligible_represented_paragraphs = 0usize;
    let mut eligible_represented_sentences = 0usize;
    let mut coarse_non_prose_items = 0usize;
    let mut coarse_structural_items = 0usize;
    let mut intentionally_skipped = 0usize;

    for (paragraph_index, paragraph) in paragraphs.iter().enumerate() {
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

        let mut emit_entry = |unit: UnitRef, degradation: Option<&'static str>, char_count| {
            let entry = StreamEntryDesc {
                unit,
                content_class: paragraph_coverage.content_class,
                degradation,
                char_count,
                eligible: match requested_kind {
                    RequestedTextUnitKind::Paragraph => !is_coarse,
                    RequestedTextUnitKind::Sentence if is_coarse => false,
                    RequestedTextUnitKind::Sentence => true,
                },
            };
            total += 1;
            match entry.unit {
                UnitRef::Paragraph(_) => represented_paragraphs += 1,
                UnitRef::Sentence(_) => represented_sentences += 1,
            }
            if entry.eligible {
                eligible_total += 1;
                match entry.unit {
                    UnitRef::Paragraph(_) => eligible_represented_paragraphs += 1,
                    UnitRef::Sentence(_) => eligible_represented_sentences += 1,
                }
            } else {
                intentionally_skipped += 1;
                if requested_kind == RequestedTextUnitKind::Sentence {
                    if is_coarse_structural_class(entry.content_class) {
                        coarse_structural_items += 1;
                    } else {
                        coarse_non_prose_items += 1;
                    }
                }
            }
            emit(entry);
        };

        match requested_kind {
            RequestedTextUnitKind::Paragraph => {
                emit_entry(
                    UnitRef::Paragraph(paragraph_index),
                    None,
                    paragraph.char_count,
                );
            }
            RequestedTextUnitKind::Sentence if is_coarse => {
                emit_entry(
                    UnitRef::Paragraph(paragraph_index),
                    Some(coarse_degradation(paragraph_coverage.content_class)),
                    paragraph.char_count,
                );
            }
            RequestedTextUnitKind::Sentence => {
                for sentence_index in sentences_by_paragraph
                    .get(&paragraph.paragraph_index)
                    .into_iter()
                    .flatten()
                {
                    let sentence = &sentences[*sentence_index];
                    emit_entry(
                        UnitRef::Sentence(*sentence_index),
                        None,
                        sentence.char_count,
                    );
                }
            }
        }
    }

    let preserve_source = TextUnitEnumerationCoverage {
        owner_chars: section_coverage.owner_chars,
        section_separator_chars: section_coverage.separator_chars,
        sentence_separator_chars,
        paragraph_count: section_coverage.paragraph_count,
        sentence_eligible_paragraphs,
        non_prose_paragraphs,
        coarse_structural_paragraphs,
        represented_paragraphs,
        represented_sentences,
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

    Ok(SectionStreamCounts {
        total_items_preserve: total,
        total_items_eligible: eligible_total,
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

/// The v2 `kind` of a coarse or non-prose item: its content class, or `None` for prose.
pub(crate) fn item_kind(class: ParagraphContentClass) -> Option<&'static str> {
    (is_non_prose_class(class) || is_coarse_structural_class(class)).then_some(class.as_str())
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
