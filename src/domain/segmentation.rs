//! One document's text segmentation as compact boundaries: no unit text, no ids, no hashes.
//!
//! Every consumer at ingest needs boundaries only — the store rebuilds ids and text on read, the
//! reading profile and the PDF inspector need coverage, the EPUB validator needs ranges, indices and
//! orders. Unit structs are materialized from these boundaries where a caller really needs them.

use std::sync::atomic::{AtomicUsize, Ordering};

use thiserror::Error;

use super::{
    Document, NormalizedBlockKind, NormalizedBlockMap, NormalizedBlockMapError,
    NormalizedDocumentHash, NormalizedTextRange, ParagraphContentClass, ParagraphSectionCoverage,
    ParagraphTextUnitSet, Section, SectionId, SentenceEligibility, SentenceParagraphCoverage,
    SentenceTextUnit, SentenceTextUnitSet, TEXT_SEGMENTATION_VERSION, TextUnit, TextUnitKind,
    paragraph_text_unit_id, sentence_text_unit_id,
};

/// Why a document cannot be segmented into boundaries.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum SegmentationError {
    /// The persisted block evidence is invalid.
    #[error(transparent)]
    BlockMap(#[from] NormalizedBlockMapError),
    /// A boundary offset does not fit the compact 32-bit form.
    #[error("text segmentation boundary {field} = {value} does not fit the compact 32-bit form")]
    BoundaryOverflow { field: &'static str, value: usize },
}

/// A normalized text range as two 32-bit offsets.
///
/// `READING_MCP_MAX_NORMALIZED_CHARS` caps a document far below `u32::MAX`, so the compact form is
/// exact for every document that can be opened; a value that does not fit is an error at
/// construction, never a truncation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct CompactRange {
    start: u32,
    end: u32,
}

impl CompactRange {
    fn try_new(field: &'static str, range: NormalizedTextRange) -> Result<Self, SegmentationError> {
        Ok(Self {
            start: compact_offset(field, range.start())?,
            end: compact_offset(field, range.end())?,
        })
    }

    fn range(self) -> NormalizedTextRange {
        NormalizedTextRange::new(self.start as usize, self.end as usize)
            .expect("a compact range keeps the order it was built from")
    }
}

fn compact_offset(field: &'static str, value: usize) -> Result<u32, SegmentationError> {
    u32::try_from(value).map_err(|_| SegmentationError::BoundaryOverflow { field, value })
}

/// One section's segmentation as boundaries.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SectionSegmentation {
    pub section_id: SectionId,
    /// Exactly the record `collect_section_paragraphs` pushes today.
    pub paragraph_coverage: ParagraphSectionCoverage,
    pub paragraphs: Vec<ParagraphBoundary>,
    pub sentences: Vec<SentenceBoundary>,
    /// One per paragraph, in paragraph order.
    pub sentence_coverage: Vec<SentenceCoverageBoundary>,
}

/// One paragraph boundary in compact form: three 32-bit fields instead of four machine words.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ParagraphBoundary {
    paragraph_index: u32,
    source_order: u32,
    range: CompactRange,
}

impl ParagraphBoundary {
    pub fn new(
        paragraph_index: usize,
        source_order: usize,
        range: NormalizedTextRange,
    ) -> Result<Self, SegmentationError> {
        Ok(Self {
            paragraph_index: compact_offset("paragraph.paragraph_index", paragraph_index)?,
            source_order: compact_offset("paragraph.source_order", source_order)?,
            range: CompactRange::try_new("paragraph.range", range)?,
        })
    }

    pub fn paragraph_index(&self) -> usize {
        self.paragraph_index as usize
    }

    pub fn source_order(&self) -> usize {
        self.source_order as usize
    }

    pub fn range(&self) -> NormalizedTextRange {
        self.range.range()
    }
}

/// One sentence boundary in compact form: five 32-bit fields, 20 bytes.
///
/// The parent paragraph range is not stored: it is always the range of the paragraph with
/// `paragraph_index` in the same section, so `SectionSegmentation::paragraph_range` derives it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SentenceBoundary {
    paragraph_index: u32,
    sentence_index: u32,
    source_order: u32,
    range: CompactRange,
}

impl SentenceBoundary {
    pub fn new(
        paragraph_index: usize,
        sentence_index: usize,
        source_order: usize,
        range: NormalizedTextRange,
    ) -> Result<Self, SegmentationError> {
        Ok(Self {
            paragraph_index: compact_offset("sentence.paragraph_index", paragraph_index)?,
            sentence_index: compact_offset("sentence.sentence_index", sentence_index)?,
            source_order: compact_offset("sentence.source_order", source_order)?,
            range: CompactRange::try_new("sentence.range", range)?,
        })
    }

    pub fn paragraph_index(&self) -> usize {
        self.paragraph_index as usize
    }

    pub fn sentence_index(&self) -> usize {
        self.sentence_index as usize
    }

    pub fn source_order(&self) -> usize {
        self.source_order as usize
    }

    pub fn range(&self) -> NormalizedTextRange {
        self.range.range()
    }
}

/// `SentenceParagraphCoverage` without the owner id and the paragraph `TextUnitId`: the paragraph
/// range identifies the paragraph. Every field is 32 bits wide.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SentenceCoverageBoundary {
    paragraph_index: u32,
    paragraph_range: CompactRange,
    content_class: ParagraphContentClass,
    eligibility: SentenceEligibility,
    paragraph_chars: u32,
    sentence_chars: u32,
    separator_chars: u32,
    coarse_only_chars: u32,
    sentence_count: u32,
}

impl SentenceCoverageBoundary {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        paragraph_index: usize,
        paragraph_range: NormalizedTextRange,
        content_class: ParagraphContentClass,
        eligibility: SentenceEligibility,
        paragraph_chars: usize,
        sentence_chars: usize,
        separator_chars: usize,
        coarse_only_chars: usize,
        sentence_count: usize,
    ) -> Result<Self, SegmentationError> {
        Ok(Self {
            paragraph_index: compact_offset("coverage.paragraph_index", paragraph_index)?,
            paragraph_range: CompactRange::try_new("coverage.paragraph_range", paragraph_range)?,
            content_class,
            eligibility,
            paragraph_chars: compact_offset("coverage.paragraph_chars", paragraph_chars)?,
            sentence_chars: compact_offset("coverage.sentence_chars", sentence_chars)?,
            separator_chars: compact_offset("coverage.separator_chars", separator_chars)?,
            coarse_only_chars: compact_offset("coverage.coarse_only_chars", coarse_only_chars)?,
            sentence_count: compact_offset("coverage.sentence_count", sentence_count)?,
        })
    }

    pub fn paragraph_index(&self) -> usize {
        self.paragraph_index as usize
    }

    pub fn paragraph_range(&self) -> NormalizedTextRange {
        self.paragraph_range.range()
    }

    pub fn content_class(&self) -> ParagraphContentClass {
        self.content_class
    }

    pub fn eligibility(&self) -> SentenceEligibility {
        self.eligibility
    }

    pub fn paragraph_chars(&self) -> usize {
        self.paragraph_chars as usize
    }

    pub fn sentence_chars(&self) -> usize {
        self.sentence_chars as usize
    }

    pub fn separator_chars(&self) -> usize {
        self.separator_chars as usize
    }

    pub fn coarse_only_chars(&self) -> usize {
        self.coarse_only_chars as usize
    }

    pub fn sentence_count(&self) -> usize {
        self.sentence_count as usize
    }
}

/// All sections, depth-first pre-order (the order `collect_section_paragraphs` walks).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DocumentSegmentation {
    pub sections: Vec<SectionSegmentation>,
}

impl SectionSegmentation {
    /// The range of the paragraph at the 1-based `paragraph_index` in this section.
    ///
    /// Paragraphs are pushed in index order starting at 1, so the slot is `paragraph_index - 1`;
    /// the index check keeps the answer honest if that ever changes.
    pub fn paragraph_range(&self, paragraph_index: usize) -> Option<NormalizedTextRange> {
        self.paragraphs
            .get(paragraph_index.checked_sub(1)?)
            .filter(|paragraph| paragraph.paragraph_index() == paragraph_index)
            .map(ParagraphBoundary::range)
    }
}

impl DocumentSegmentation {
    pub fn section(&self, section_id: &SectionId) -> Option<&SectionSegmentation> {
        self.sections
            .iter()
            .find(|section| section.section_id == *section_id)
    }

    /// Rebuilds today's unit sets from the boundaries: each unit's text is its slice and its ids come
    /// from the document hash.
    pub fn materialize(
        &self,
        document: &Document,
        normalized_document_hash: &NormalizedDocumentHash,
    ) -> (ParagraphTextUnitSet, SentenceTextUnitSet) {
        let mut paragraph_set = ParagraphTextUnitSet {
            normalized_document_hash: normalized_document_hash.clone(),
            units: Vec::new(),
            coverage: Vec::new(),
        };
        let mut sentence_set = SentenceTextUnitSet {
            normalized_document_hash: normalized_document_hash.clone(),
            units: Vec::new(),
            coverage: Vec::new(),
        };

        for section in &self.sections {
            let source = document
                .find_section(&section.section_id)
                .expect("segmented section must exist in its canonical document");
            let index = source.normalized_text_index();
            paragraph_set
                .coverage
                .push(section.paragraph_coverage.clone());

            for boundary in &section.paragraphs {
                let text = index
                    .slice(boundary.range())
                    .expect("generated paragraph range must be a valid owner slice")
                    .to_string();
                paragraph_set.units.push(TextUnit {
                    id: paragraph_text_unit_id(
                        &document.id,
                        normalized_document_hash,
                        &section.section_id,
                        boundary.paragraph_index(),
                        boundary.range(),
                    ),
                    document_id: document.id.clone(),
                    content_hash: document.content_hash.clone(),
                    normalized_document_hash: normalized_document_hash.clone(),
                    owner_section_id: section.section_id.clone(),
                    kind: TextUnitKind::Paragraph,
                    paragraph_index: boundary.paragraph_index(),
                    source_order: boundary.source_order(),
                    normalized_range: boundary.range(),
                    text,
                    segmentation_version: TEXT_SEGMENTATION_VERSION.into(),
                });
            }

            for boundary in &section.sentences {
                let text = index
                    .slice(boundary.range())
                    .expect("generated sentence range must be a valid owner slice")
                    .to_string();
                let parent_range = section
                    .paragraph_range(boundary.paragraph_index())
                    .expect("generated sentence must have its parent paragraph boundary");
                sentence_set.units.push(SentenceTextUnit {
                    id: sentence_text_unit_id(
                        &document.id,
                        normalized_document_hash,
                        &section.section_id,
                        boundary.paragraph_index(),
                        boundary.sentence_index(),
                        boundary.range(),
                    ),
                    document_id: document.id.clone(),
                    content_hash: document.content_hash.clone(),
                    normalized_document_hash: normalized_document_hash.clone(),
                    owner_section_id: section.section_id.clone(),
                    paragraph_index: boundary.paragraph_index(),
                    sentence_index: boundary.sentence_index(),
                    parent_paragraph_id: paragraph_text_unit_id(
                        &document.id,
                        normalized_document_hash,
                        &section.section_id,
                        boundary.paragraph_index(),
                        parent_range,
                    ),
                    source_order: boundary.source_order(),
                    normalized_range: boundary.range(),
                    text,
                    segmentation_version: TEXT_SEGMENTATION_VERSION.into(),
                });
            }

            for coverage in &section.sentence_coverage {
                sentence_set.coverage.push(SentenceParagraphCoverage {
                    owner_section_id: section.section_id.clone(),
                    paragraph_id: paragraph_text_unit_id(
                        &document.id,
                        normalized_document_hash,
                        &section.section_id,
                        coverage.paragraph_index(),
                        coverage.paragraph_range(),
                    ),
                    paragraph_index: coverage.paragraph_index(),
                    content_class: coverage.content_class(),
                    eligibility: coverage.eligibility(),
                    paragraph_chars: coverage.paragraph_chars(),
                    sentence_chars: coverage.sentence_chars(),
                    separator_chars: coverage.separator_chars(),
                    coarse_only_chars: coverage.coarse_only_chars(),
                    sentence_count: coverage.sentence_count(),
                });
            }
        }

        (paragraph_set, sentence_set)
    }
}

impl Document {
    /// Decodes the persisted block evidence once and segments the document into boundaries.
    pub fn try_segmentation(&self) -> Result<DocumentSegmentation, SegmentationError> {
        let block_map = self.normalized_block_map()?;
        self.build_segmentation(block_map.as_ref())
    }

    pub fn segmentation(&self) -> DocumentSegmentation {
        self.try_segmentation()
            .expect("canonical normalized block map must be valid for text segmentation")
    }

    fn build_segmentation(
        &self,
        block_map: Option<&NormalizedBlockMap>,
    ) -> Result<DocumentSegmentation, SegmentationError> {
        SEGMENTATION_BUILDS.fetch_add(1, Ordering::Relaxed);
        let mut sections = Vec::new();
        let mut paragraph_source_order = 0usize;
        let mut sentence_source_order = 0usize;

        for section in &self.root_sections {
            collect_section_segmentation(
                block_map,
                section,
                &mut sections,
                &mut paragraph_source_order,
                &mut sentence_source_order,
            )?;
        }

        Ok(DocumentSegmentation { sections })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ParagraphBoundarySource {
    NativeParagraph,
    NativeBlockQuote,
    NativeListItem,
    NativePreformatted,
    NativeTable,
    Fallback,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ParagraphCandidate {
    range: NormalizedTextRange,
    source: ParagraphBoundarySource,
}

fn collect_section_segmentation(
    block_map: Option<&NormalizedBlockMap>,
    section: &Section,
    out: &mut Vec<SectionSegmentation>,
    paragraph_source_order: &mut usize,
    sentence_source_order: &mut usize,
) -> Result<(), SegmentationError> {
    let candidates = section_paragraph_candidates(section, block_map);
    let index = section.normalized_text_index();
    let paragraph_chars = candidates
        .iter()
        .map(|candidate| candidate.range.len())
        .sum::<usize>();
    let owner_chars = index.char_len();
    let native_paragraph_chars = candidates
        .iter()
        .filter(|candidate| candidate.source == ParagraphBoundarySource::NativeParagraph)
        .map(|candidate| candidate.range.len())
        .sum::<usize>();
    let native_structural_container_chars = candidates
        .iter()
        .filter(|candidate| {
            matches!(
                candidate.source,
                ParagraphBoundarySource::NativeBlockQuote | ParagraphBoundarySource::NativeListItem
            )
        })
        .map(|candidate| candidate.range.len())
        .sum::<usize>();
    let native_non_prose_chars = candidates
        .iter()
        .filter(|candidate| {
            matches!(
                candidate.source,
                ParagraphBoundarySource::NativePreformatted | ParagraphBoundarySource::NativeTable
            )
        })
        .map(|candidate| candidate.range.len())
        .sum::<usize>();
    let fallback_chars = candidates
        .iter()
        .filter(|candidate| candidate.source == ParagraphBoundarySource::Fallback)
        .map(|candidate| candidate.range.len())
        .sum::<usize>();

    let paragraph_coverage = ParagraphSectionCoverage {
        owner_section_id: section.id.clone(),
        owner_chars,
        paragraph_chars,
        separator_chars: owner_chars
            .checked_sub(paragraph_chars)
            .expect("paragraph candidates must remain inside the owner Section"),
        paragraph_count: candidates.len(),
        native_paragraph_chars,
        native_structural_container_chars,
        native_non_prose_chars,
        fallback_chars,
    };

    let mut paragraphs = Vec::with_capacity(candidates.len());
    let mut sentences = Vec::new();
    let mut sentence_coverage = Vec::with_capacity(candidates.len());

    for (offset, candidate) in candidates.into_iter().enumerate() {
        let paragraph_index = offset + 1;
        let source_order = *paragraph_source_order;
        *paragraph_source_order += 1;
        let text = index
            .slice(candidate.range)
            .expect("generated paragraph range must be a valid owner slice");
        let content_class = paragraph_content_class(block_map, &section.id, candidate.range, text);
        let mut eligibility = match content_class {
            ParagraphContentClass::ProseOrUnknown | ParagraphContentClass::NativeParagraph => {
                SentenceEligibility::Eligible
            }
            ParagraphContentClass::BlockQuote
            | ParagraphContentClass::ListItem
            | ParagraphContentClass::CodeBlock
            | ParagraphContentClass::Preformatted
            | ParagraphContentClass::Table => SentenceEligibility::CoarseParagraphOnly,
        };

        // Fallback-only guard: a ProseOrUnknown paragraph has no parser-native
        // provenance. When its text shows objective merge/corruption evidence
        // (missing-space glue, doubled characters, a bibliographic masthead run),
        // terminal-punctuation splitting would produce a fake clean sentence that
        // hides front matter or extra body text inside one unit. Keep it coarse so
        // strict callers can fail closed instead of trusting the over-claim.
        if eligibility == SentenceEligibility::Eligible
            && content_class == ParagraphContentClass::ProseOrUnknown
            && looks_like_unreliable_fallback_text(text)
        {
            eligibility = SentenceEligibility::CoarseParagraphOnly;
        }

        if eligibility == SentenceEligibility::CoarseParagraphOnly {
            sentence_coverage.push(SentenceCoverageBoundary::new(
                paragraph_index,
                candidate.range,
                content_class,
                eligibility,
                candidate.range.len(),
                0,
                0,
                candidate.range.len(),
                0,
            )?);
        } else {
            let ranges = sentence_ranges(text, candidate.range);
            let sentence_chars = ranges.iter().map(|range| range.len()).sum::<usize>();

            for (sentence_offset, range) in ranges.iter().copied().enumerate() {
                sentences.push(SentenceBoundary::new(
                    paragraph_index,
                    sentence_offset + 1,
                    *sentence_source_order,
                    range,
                )?);
                *sentence_source_order += 1;
            }

            let separator_chars =
                candidate.range.len().checked_sub(sentence_chars).expect(
                    "generated sentence ranges must remain inside the containing Paragraph",
                );
            sentence_coverage.push(SentenceCoverageBoundary::new(
                paragraph_index,
                candidate.range,
                content_class,
                eligibility,
                candidate.range.len(),
                sentence_chars,
                separator_chars,
                0,
                ranges.len(),
            )?);
        }

        paragraphs.push(ParagraphBoundary::new(
            paragraph_index,
            source_order,
            candidate.range,
        )?);
    }

    // The sentence count is only known once every paragraph is scanned, so the Vec grew by
    // doubling. Release the overshoot once per section instead of segmenting a second time.
    sentences.shrink_to_fit();

    out.push(SectionSegmentation {
        section_id: section.id.clone(),
        paragraph_coverage,
        paragraphs,
        sentences,
        sentence_coverage,
    });

    for child in &section.children {
        collect_section_segmentation(
            block_map,
            child,
            out,
            paragraph_source_order,
            sentence_source_order,
        )?;
    }

    Ok(())
}

fn section_paragraph_candidates(
    section: &Section,
    block_map: Option<&NormalizedBlockMap>,
) -> Vec<ParagraphCandidate> {
    let Some(block_map) = block_map else {
        return fallback_paragraph_candidates(
            &section.content,
            NormalizedTextRange::new(0, section.normalized_text_len())
                .expect("whole Section range must be ordered"),
        );
    };

    let blocks = block_map
        .blocks
        .iter()
        .filter(|block| block.owner_section_id == section.id)
        .collect::<Vec<_>>();
    if blocks.is_empty() {
        return fallback_paragraph_candidates(
            &section.content,
            NormalizedTextRange::new(0, section.normalized_text_len())
                .expect("whole Section range must be ordered"),
        );
    }

    let mut candidates = Vec::new();
    let mut cursor = 0usize;
    for block in blocks {
        if cursor < block.normalized_range.start() {
            let gap = NormalizedTextRange::new(cursor, block.normalized_range.start())
                .expect("validated block gap must be ordered");
            candidates.extend(fallback_paragraph_candidates(&section.content, gap));
        }
        candidates.push(ParagraphCandidate {
            range: block.normalized_range,
            source: match block.kind {
                NormalizedBlockKind::Paragraph => ParagraphBoundarySource::NativeParagraph,
                NormalizedBlockKind::BlockQuote => ParagraphBoundarySource::NativeBlockQuote,
                NormalizedBlockKind::ListItem => ParagraphBoundarySource::NativeListItem,
                NormalizedBlockKind::Preformatted => ParagraphBoundarySource::NativePreformatted,
                NormalizedBlockKind::Table => ParagraphBoundarySource::NativeTable,
            },
        });
        cursor = block.normalized_range.end();
    }

    if cursor < section.normalized_text_len() {
        let tail = NormalizedTextRange::new(cursor, section.normalized_text_len())
            .expect("validated trailing gap must be ordered");
        candidates.extend(fallback_paragraph_candidates(&section.content, tail));
    }

    candidates
}

fn fallback_paragraph_candidates(
    content: &str,
    owner_range: NormalizedTextRange,
) -> Vec<ParagraphCandidate> {
    let slice = owner_range
        .slice(content)
        .expect("fallback range must be a valid Section slice");
    paragraph_ranges(slice)
        .into_iter()
        .map(|range| {
            let range = NormalizedTextRange::new(
                owner_range.start() + range.start(),
                owner_range.start() + range.end(),
            )
            .expect("offset fallback paragraph range must be ordered");
            ParagraphCandidate {
                range,
                source: ParagraphBoundarySource::Fallback,
            }
        })
        .collect()
}

fn paragraph_content_class(
    block_map: Option<&NormalizedBlockMap>,
    owner_section_id: &SectionId,
    range: NormalizedTextRange,
    text: &str,
) -> ParagraphContentClass {
    if let Some(block) = block_map.and_then(|map| {
        map.blocks.iter().find(|block| {
            block.owner_section_id == *owner_section_id && block.normalized_range == range
        })
    }) {
        return match block.kind {
            NormalizedBlockKind::Paragraph => ParagraphContentClass::NativeParagraph,
            NormalizedBlockKind::BlockQuote => ParagraphContentClass::BlockQuote,
            NormalizedBlockKind::ListItem => ParagraphContentClass::ListItem,
            NormalizedBlockKind::Preformatted => ParagraphContentClass::Preformatted,
            NormalizedBlockKind::Table => ParagraphContentClass::Table,
        };
    }

    classify_fallback_paragraph_content(text)
}

fn paragraph_ranges(content: &str) -> Vec<NormalizedTextRange> {
    let mut ranges = Vec::new();
    let mut paragraph_start = None;
    let mut paragraph_end = 0usize;
    let mut scalar_offset = 0usize;

    for line in content.split_inclusive('\n') {
        let line_chars = line.chars().count();
        let line_body = strip_line_ending(line);
        let line_body_chars = line_body.chars().count();

        if line_body.trim().is_empty() {
            if let Some(start) = paragraph_start.take() {
                ranges.push(
                    NormalizedTextRange::new(start, paragraph_end)
                        .expect("paragraph boundaries must be ordered"),
                );
            }
        } else {
            paragraph_start.get_or_insert(scalar_offset);
            paragraph_end = scalar_offset + line_body_chars;
        }

        scalar_offset += line_chars;
    }

    if let Some(start) = paragraph_start {
        ranges.push(
            NormalizedTextRange::new(start, paragraph_end)
                .expect("paragraph boundaries must be ordered"),
        );
    }

    ranges
}

fn sentence_ranges(text: &str, paragraph_range: NormalizedTextRange) -> Vec<NormalizedTextRange> {
    let chars = text.chars().collect::<Vec<_>>();
    let mut ranges = Vec::new();
    let mut start = skip_whitespace(&chars, 0);
    let mut cursor = start;

    while cursor < chars.len() {
        if let Some(end) = sentence_boundary_end(&chars, cursor) {
            if start < end {
                ranges.push(
                    NormalizedTextRange::new(
                        paragraph_range.start() + start,
                        paragraph_range.start() + end,
                    )
                    .expect("sentence boundaries must be ordered"),
                );
            }
            start = skip_whitespace(&chars, end);
            cursor = start;
        } else {
            cursor += 1;
        }
    }

    let tail_end = trim_trailing_whitespace(&chars, chars.len());
    if start < tail_end {
        ranges.push(
            NormalizedTextRange::new(
                paragraph_range.start() + start,
                paragraph_range.start() + tail_end,
            )
            .expect("sentence tail boundaries must be ordered"),
        );
    }

    ranges
}

fn sentence_boundary_end(chars: &[char], index: usize) -> Option<usize> {
    match chars[index] {
        '。' | '！' | '？' => Some(extend_terminal_cluster(chars, index)),
        '!' | '?' if ascii_terminal_is_terminal(chars, index) => {
            Some(extend_terminal_cluster(chars, index))
        }
        '.' if ascii_period_is_terminal(chars, index) => {
            Some(extend_terminal_cluster(chars, index))
        }
        _ => None,
    }
}

fn ascii_terminal_is_terminal(chars: &[char], index: usize) -> bool {
    let boundary_end = extend_terminal_cluster(chars, index);
    boundary_end == chars.len() || chars[boundary_end].is_whitespace()
}

fn ascii_period_is_terminal(chars: &[char], index: usize) -> bool {
    if chars.get(index + 1) == Some(&'.') {
        return false;
    }

    let previous = index
        .checked_sub(1)
        .and_then(|position| chars.get(position));
    let next = chars.get(index + 1);

    if previous.is_some_and(|ch| ch.is_ascii_digit()) && next.is_some_and(|ch| ch.is_ascii_digit())
    {
        return false;
    }

    if previous.is_some_and(|ch| is_identifier_char(*ch))
        && next.is_some_and(|ch| is_identifier_char(*ch))
    {
        return false;
    }

    if next.is_some_and(|ch| matches!(ch, '/' | '\\')) {
        return false;
    }

    let token = preceding_period_token(chars, index).to_ascii_lowercase();
    if !token.is_empty() && is_protected_abbreviation(&token) && index + 1 < chars.len() {
        // A list-ending abbreviation can also end a sentence. Preserve interior
        // uses ("etc., including" / "etc. and") and title/example abbreviations.
        let terminal_etc = token == "etc"
            && ascii_terminal_is_terminal(chars, index)
            && next_non_whitespace(chars, extend_terminal_cluster(chars, index))
                .is_some_and(|ch| ch.is_uppercase());
        if !terminal_etc {
            return false;
        }
    }

    if token.chars().count() == 1
        && token.chars().all(|ch| ch.is_ascii_alphabetic())
        && next_non_whitespace(chars, index + 1).is_some_and(|ch| ch.is_uppercase())
    {
        return false;
    }

    ascii_terminal_is_terminal(chars, index)
}

fn extend_terminal_cluster(chars: &[char], index: usize) -> usize {
    let mut end = index + 1;
    while end < chars.len() && is_terminal_mark(chars[end]) {
        end += 1;
    }
    while end < chars.len() && is_sentence_closer(chars[end]) {
        end += 1;
    }
    end
}

fn is_terminal_mark(ch: char) -> bool {
    matches!(ch, '.' | '!' | '?' | '。' | '！' | '？')
}

fn is_sentence_closer(ch: char) -> bool {
    matches!(
        ch,
        '"' | '\'' | '”' | '’' | ')' | ']' | '}' | '》' | '」' | '』' | '】'
    )
}

fn is_identifier_char(ch: char) -> bool {
    ch.is_alphanumeric() || matches!(ch, '_' | '-' | '$')
}

fn preceding_period_token(chars: &[char], index: usize) -> String {
    let mut start = index;
    while start > 0 {
        let ch = chars[start - 1];
        if ch.is_alphanumeric() || matches!(ch, '.' | '_' | '-') {
            start -= 1;
        } else {
            break;
        }
    }
    chars[start..index].iter().collect()
}

fn is_protected_abbreviation(token: &str) -> bool {
    matches!(
        token,
        "e.g"
            | "i.e"
            | "etc"
            | "vs"
            | "mr"
            | "mrs"
            | "ms"
            | "dr"
            | "prof"
            | "sr"
            | "jr"
            | "fig"
            | "eq"
            | "no"
            | "inc"
            | "ltd"
            | "u.s"
            | "u.k"
    )
}

fn next_non_whitespace(chars: &[char], mut index: usize) -> Option<char> {
    while index < chars.len() {
        if !chars[index].is_whitespace() {
            return Some(chars[index]);
        }
        index += 1;
    }
    None
}

fn skip_whitespace(chars: &[char], mut index: usize) -> usize {
    while index < chars.len() && chars[index].is_whitespace() {
        index += 1;
    }
    index
}

fn trim_trailing_whitespace(chars: &[char], mut end: usize) -> usize {
    while end > 0 && chars[end - 1].is_whitespace() {
        end -= 1;
    }
    end
}

fn classify_fallback_paragraph_content(text: &str) -> ParagraphContentClass {
    if looks_like_fenced_code(text) || looks_like_indented_code(text) {
        ParagraphContentClass::CodeBlock
    } else if looks_like_markdown_table(text) {
        ParagraphContentClass::Table
    } else {
        ParagraphContentClass::ProseOrUnknown
    }
}

const MIN_MISSING_SPACE_GLUE_EVENTS: usize = 3;
const MIN_DOUBLED_RUN_CHARS: usize = 6;
const MIN_BIBLIOGRAPHIC_MARKERS: usize = 4;
const BIBLIOGRAPHIC_WINDOW_WORDS: usize = 10;

/// A fallback paragraph that merges heterogeneous fragments (masthead, title, byline,
/// bibliographic metadata) or carries extraction corruption (missing spaces, doubled
/// characters) cannot be trusted to produce honest sentences from terminal punctuation.
/// Detection is limited to objective, document-agnostic evidence.
fn looks_like_unreliable_fallback_text(text: &str) -> bool {
    contains_doubled_character_token(text)
        || missing_space_glue_events(text) >= MIN_MISSING_SPACE_GLUE_EVENTS
        || has_bibliographic_metadata_run(text)
}

/// Missing-space concatenation evidence: `StrongInference`, `JohnR.Platt`,
/// `3642SCIENCE`, `,NewSeries`, `others.John`, `)provided`.
fn missing_space_glue_events(text: &str) -> usize {
    let chars = text.chars().collect::<Vec<_>>();
    let mut events = 0usize;
    for index in 1..chars.len() {
        let previous = chars[index - 1];
        let current = chars[index];
        let glued = (previous.is_ascii_digit() && current.is_uppercase())
            || (matches!(previous, ',' | ';' | ':' | '.' | '!' | '?') && current.is_uppercase())
            || (previous == ')' && current.is_alphabetic())
            // An all-caps run of 3+ immediately continued by a lowercase tail inside
            // one token ("SCIENCEStrong") means two fragments were merged.
            || (previous.is_uppercase()
                && current.is_lowercase()
                && uppercase_run_ending_at(&chars, index - 1) >= 3)
            // Interior lowercase->uppercase junction only: single-letter brand
            // prefixes ("iPhone") and surname patterns ("McDonald") are not glue.
            || (previous.is_lowercase()
                && current.is_uppercase()
                && chars
                    .get(index.wrapping_sub(2))
                    .is_some_and(|before| before.is_alphabetic())
                && !is_surname_junction(&chars, index));
        events += usize::from(glued);
    }
    events
}

fn is_surname_junction(chars: &[char], index: usize) -> bool {
    // chars[index - 1] is the lowercase side of the junction.
    if chars[index - 1] != 'c' {
        return false;
    }
    (index >= 2 && chars[index - 2] == 'M')
        || (index >= 3 && chars[index - 3] == 'M' && chars[index - 2] == 'a')
}

fn uppercase_run_ending_at(chars: &[char], end: usize) -> usize {
    let mut run = 0usize;
    let mut cursor = end;
    loop {
        if chars[cursor].is_uppercase() {
            run += 1;
        } else {
            break;
        }
        if cursor == 0 {
            break;
        }
        cursor -= 1;
    }
    run
}

/// Extraction corruption that doubles every character ("NNoottee", "WWhhiillee"):
/// an alphabetic token whose characters are almost entirely pairwise-doubled runs.
fn contains_doubled_character_token(text: &str) -> bool {
    let chars = text.chars().collect::<Vec<_>>();
    let mut cursor = 0usize;
    while cursor < chars.len() {
        if !chars[cursor].is_alphabetic() {
            cursor += 1;
            continue;
        }
        let start = cursor;
        while cursor < chars.len() && chars[cursor].is_alphabetic() {
            cursor += 1;
        }
        if cursor - start >= MIN_DOUBLED_RUN_CHARS
            && token_is_pairwise_doubled(&chars[start..cursor])
        {
            return true;
        }
    }
    false
}

fn token_is_pairwise_doubled(token: &[char]) -> bool {
    let mut doubled_runs = 0usize;
    let mut doubled_chars = 0usize;
    let mut index = 0usize;
    while index < token.len() {
        let mut run = 1usize;
        while index + run < token.len() && token[index + run] == token[index] {
            run += 1;
        }
        if run == 2 {
            doubled_runs += 1;
            doubled_chars += 2;
        }
        index += run;
    }
    // Real words contain scattered doubles ("bookkeeping", "committee"); systematic
    // corruption doubles nearly every character.
    doubled_runs >= 3 && doubled_chars * 5 >= token.len() * 4
}

/// Journal-masthead / bibliographic metadata evidence. Citation markers (Vol/No/pp +
/// digits), dates, links, and rights markers also appear inside legitimate prose, so
/// the signal is *density*: a masthead packs several markers into a handful of words
/// while citation-dense prose spreads them across sentences.
fn has_bibliographic_metadata_run(text: &str) -> bool {
    let markers = bibliographic_marker_counts(text);
    if markers.len() < MIN_BIBLIOGRAPHIC_MARKERS {
        return false;
    }
    let window = BIBLIOGRAPHIC_WINDOW_WORDS.min(markers.len());
    let mut best = 0usize;
    let mut current: usize = markers[..window].iter().sum();
    best = best.max(current);
    for index in window..markers.len() {
        current += markers[index];
        current -= markers[index - window];
        best = best.max(current);
    }
    best >= MIN_BIBLIOGRAPHIC_MARKERS
}

/// Per-word bibliographic marker counts used for the density check.
fn bibliographic_marker_counts(text: &str) -> Vec<usize> {
    let words = text
        .split(|ch: char| !ch.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>();
    let mut counts = Vec::with_capacity(words.len());
    for (index, word) in words.iter().enumerate() {
        let lower = word.to_lowercase();
        let next_is_number = words
            .get(index + 1)
            .is_some_and(|next| next.chars().next().is_some_and(|ch| ch.is_ascii_digit()));
        let mut count = 0usize;
        if matches!(
            lower.as_str(),
            "vol" | "volume" | "no" | "number" | "issue" | "series" | "pp" | "p" | "pages"
        ) && next_is_number
        {
            count += 1;
        }
        if is_month_word(&lower) && nearby_number(&words, index) {
            count += 1;
        }
        if word.len() == 4
            && word.bytes().all(|byte| byte.is_ascii_digit())
            && let Ok(year) = word.parse::<u32>()
            && (1800..=2099).contains(&year)
        {
            count += 1;
        }
        if lower.contains("http")
            || lower.contains("www.")
            || lower.contains("url")
            || lower == "doi"
        {
            count += 1;
        }
        if matches!(lower.as_str(), "copyright" | "isbn" | "issn") {
            count += 1;
        }
        counts.push(count);
    }
    counts
}

fn is_month_word(word: &str) -> bool {
    matches!(
        word.get(..3),
        Some(
            "jan"
                | "feb"
                | "mar"
                | "apr"
                | "may"
                | "jun"
                | "jul"
                | "aug"
                | "sep"
                | "oct"
                | "nov"
                | "dec"
        )
    ) && word.chars().all(|ch| ch.is_alphabetic())
}

fn nearby_number(words: &[&str], index: usize) -> bool {
    let lo = index.saturating_sub(2);
    let hi = (index + 3).min(words.len());
    words[lo..hi].iter().enumerate().any(|(offset, word)| {
        lo + offset != index && word.chars().next().is_some_and(|ch| ch.is_ascii_digit())
    })
}

fn looks_like_fenced_code(text: &str) -> bool {
    let trimmed = text.trim();
    let mut lines = trimmed.lines().filter(|line| !line.trim().is_empty());
    let Some(first) = lines.next() else {
        return false;
    };
    let marker = if first.trim_start().starts_with("```") {
        "```"
    } else if first.trim_start().starts_with("~~~") {
        "~~~"
    } else {
        return false;
    };

    trimmed
        .lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .is_some_and(|line| line.trim_start().starts_with(marker))
}

fn looks_like_indented_code(text: &str) -> bool {
    let mut saw_line = false;
    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        saw_line = true;
        if !(line.starts_with('\t') || line.starts_with("    ")) {
            return false;
        }
    }
    saw_line
}

fn looks_like_markdown_table(text: &str) -> bool {
    let lines = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect::<Vec<_>>();
    if lines.len() < 2 || !lines[0].contains('|') || !lines[1].contains('|') {
        return false;
    }

    let delimiter_cells = lines[1]
        .trim()
        .trim_matches('|')
        .split('|')
        .map(str::trim)
        .collect::<Vec<_>>();

    delimiter_cells.len() >= 2
        && delimiter_cells
            .iter()
            .all(|cell| is_table_delimiter_cell(cell))
}

fn is_table_delimiter_cell(cell: &str) -> bool {
    let cell = cell.strip_prefix(':').unwrap_or(cell);
    let cell = cell.strip_suffix(':').unwrap_or(cell);
    cell.len() >= 3 && cell.chars().all(|ch| ch == '-')
}

fn strip_line_ending(line: &str) -> &str {
    let line = line.strip_suffix('\n').unwrap_or(line);
    line.strip_suffix('\r').unwrap_or(line)
}

/// Test hook: how many times any document has been segmented in this process. The open path must
/// segment each document once, and `tests/epub_open_segmentation_once.rs` reads this counter to
/// prove it; the cost is one relaxed increment per build.
#[doc(hidden)]
pub static SEGMENTATION_BUILDS: AtomicUsize = AtomicUsize::new(0);

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, HashMap};

    use super::*;
    use crate::domain::{ContentHash, DocumentId, DocumentSource, Location, MediaType, TextUnitId};

    fn section(
        id: &str,
        parent_id: Option<&str>,
        content: &str,
        children: Vec<Section>,
    ) -> Section {
        Section {
            id: SectionId(id.into()),
            parent_id: parent_id.map(|id| SectionId(id.into())),
            title: id.into(),
            level: 1,
            content: content.into(),
            location: Location::default(),
            children,
        }
    }

    fn document(root_sections: Vec<Section>) -> Document {
        let source = DocumentSource("fixture://compact-boundaries".into());
        let content_hash = ContentHash::of_bytes(b"compact-boundaries");
        Document {
            id: DocumentId::derive(&source, &content_hash),
            source,
            title: "Compact boundaries".into(),
            media_type: MediaType("application/epub+zip".into()),
            content_hash,
            metadata: BTreeMap::new(),
            root_sections,
        }
    }

    /// One expected paragraph: its text, its content class and its sentence texts.
    type ExpectedParagraph = (&'static str, ParagraphContentClass, Vec<&'static str>);
    /// One expected section: its id and its paragraphs.
    type ExpectedSection = (&'static str, Vec<ExpectedParagraph>);

    /// Two prose paragraphs (ASCII sentences), one fenced code paragraph, and a nested CJK
    /// section whose source orders continue its parent's.
    fn fixtures() -> Vec<(&'static str, Document)> {
        vec![
            (
                "mixed",
                document(vec![section(
                    "s1",
                    None,
                    "Alpha one. Alpha two.\n\nBeta three! Beta four?\n\n```\ncode block\n```\n",
                    vec![section("s2", Some("s1"), "第一句。第二句！\n", Vec::new())],
                )]),
            ),
            (
                "single",
                document(vec![section(
                    "only",
                    None,
                    "Only one sentence here.\n",
                    Vec::new(),
                )]),
            ),
        ]
    }

    /// Every field the pre-compaction `ParagraphBoundary`, `SentenceBoundary` and
    /// `SentenceCoverageBoundary` stored, read back through the compact accessors.
    #[test]
    fn compact_boundaries_round_trip_every_field() {
        let (label, document) = fixtures().remove(0);
        let segmentation = document.segmentation();

        let expected: Vec<ExpectedSection> = vec![
            (
                "s1",
                vec![
                    (
                        "Alpha one. Alpha two.",
                        ParagraphContentClass::ProseOrUnknown,
                        vec!["Alpha one.", "Alpha two."],
                    ),
                    (
                        "Beta three! Beta four?",
                        ParagraphContentClass::ProseOrUnknown,
                        vec!["Beta three!", "Beta four?"],
                    ),
                    (
                        "```\ncode block\n```",
                        ParagraphContentClass::CodeBlock,
                        vec![],
                    ),
                ],
            ),
            (
                "s2",
                vec![(
                    "第一句。第二句！",
                    ParagraphContentClass::ProseOrUnknown,
                    vec!["第一句。", "第二句！"],
                )],
            ),
        ];

        assert_eq!(label, "mixed");
        assert_eq!(segmentation.sections.len(), expected.len());
        let mut paragraph_source_order = 0usize;
        let mut sentence_source_order = 0usize;

        for (section, (section_id, paragraphs)) in segmentation.sections.iter().zip(expected) {
            assert_eq!(section.section_id.0, section_id);
            let owner = document
                .find_section(&section.section_id)
                .expect("segmented section must exist in its document");
            let index = owner.normalized_text_index();

            assert_eq!(section.paragraphs.len(), paragraphs.len());
            assert_eq!(
                section.sentences.len(),
                paragraphs
                    .iter()
                    .map(|(_, _, sentences)| sentences.len())
                    .sum::<usize>()
            );
            assert_eq!(section.sentence_coverage.len(), paragraphs.len());

            let mut sentence_offset = 0usize;
            for (offset, (boundary, (text, content_class, expected_sentences))) in
                section.paragraphs.iter().zip(paragraphs).enumerate()
            {
                assert_eq!(boundary.paragraph_index(), offset + 1);
                assert_eq!(boundary.source_order(), paragraph_source_order);
                assert_eq!(
                    index
                        .slice(boundary.range())
                        .expect("paragraph range must be an owner slice"),
                    text
                );

                let paragraph_range = boundary.range();
                let coverage = &section.sentence_coverage[offset];
                assert_eq!(coverage.paragraph_index(), offset + 1);
                assert_eq!(coverage.paragraph_range(), paragraph_range);
                assert_eq!(coverage.content_class(), content_class);
                assert_eq!(coverage.sentence_count(), expected_sentences.len());

                let mut sentence_chars = 0usize;
                for (sentence_index, sentence_text) in expected_sentences.iter().enumerate() {
                    let sentence = &section.sentences[sentence_offset];
                    assert_eq!(sentence.paragraph_index(), offset + 1);
                    assert_eq!(sentence.sentence_index(), sentence_index + 1);
                    assert_eq!(sentence.source_order(), sentence_source_order);
                    assert_eq!(
                        index
                            .slice(sentence.range())
                            .expect("sentence range must be an owner slice"),
                        *sentence_text
                    );
                    // The dropped `parent_range`, derived back from the paragraph.
                    assert_eq!(
                        section.paragraph_range(sentence.paragraph_index()),
                        Some(paragraph_range)
                    );
                    sentence_chars += sentence.range().len();
                    sentence_offset += 1;
                    sentence_source_order += 1;
                }

                let paragraph_chars = text.chars().count();
                assert_eq!(coverage.paragraph_chars(), paragraph_chars);
                assert_eq!(coverage.sentence_chars(), sentence_chars);
                // A coarse paragraph keeps its whole text as coarse-only chars and reports no
                // separator chars of its own.
                assert_eq!(
                    coverage.separator_chars(),
                    if expected_sentences.is_empty() {
                        0
                    } else {
                        paragraph_chars - sentence_chars
                    }
                );
                assert_eq!(
                    coverage.coarse_only_chars(),
                    if expected_sentences.is_empty() {
                        paragraph_chars
                    } else {
                        0
                    }
                );
                assert_eq!(
                    coverage.eligibility(),
                    if expected_sentences.is_empty() {
                        SentenceEligibility::CoarseParagraphOnly
                    } else {
                        SentenceEligibility::Eligible
                    }
                );
                paragraph_source_order += 1;
            }
            assert_eq!(sentence_offset, section.sentences.len());
        }

        assert_eq!(paragraph_source_order, 4);
        assert_eq!(sentence_source_order, 6);
    }

    /// The parent range is no longer stored, so it must be exactly what the paragraph of the same
    /// section says it is — for every sentence of every fixture.
    #[test]
    fn derived_parent_range_equals_the_parent_paragraph_for_every_sentence() {
        for (label, document) in fixtures() {
            let segmentation = document.segmentation();
            let hash = document.normalized_document_hash();
            let (_, sentence_units) = segmentation.materialize(&document, &hash);
            let coverage_ids: HashMap<(String, usize), TextUnitId> = sentence_units
                .coverage
                .iter()
                .map(|coverage| {
                    (
                        (
                            coverage.owner_section_id.0.clone(),
                            coverage.paragraph_index,
                        ),
                        coverage.paragraph_id.clone(),
                    )
                })
                .collect();

            let mut sentences = 0usize;
            for section in &segmentation.sections {
                for sentence in &section.sentences {
                    let parent = section
                        .paragraph_range(sentence.paragraph_index())
                        .unwrap_or_else(|| {
                            panic!("fixture {label}: sentence has no parent paragraph")
                        });
                    // Exactly one paragraph contains the sentence text, and it is the one the
                    // sentence names: that range is the value the old struct stored.
                    let containing = section
                        .paragraphs
                        .iter()
                        .filter(|paragraph| {
                            paragraph.range().start() <= sentence.range().start()
                                && sentence.range().end() <= paragraph.range().end()
                        })
                        .collect::<Vec<_>>();
                    assert_eq!(containing.len(), 1, "fixture {label}");
                    assert_eq!(
                        containing[0].paragraph_index(),
                        sentence.paragraph_index(),
                        "fixture {label}"
                    );
                    assert_eq!(containing[0].range(), parent, "fixture {label}");
                    sentences += 1;
                }
            }
            assert!(sentences > 0, "fixture {label} has no sentences");

            // Materialization builds the parent id from the derived range and the coverage id from
            // the paragraph's own range: equal ids prove both ranges are the same value.
            for unit in &sentence_units.units {
                let coverage_id = coverage_ids
                    .get(&(unit.owner_section_id.0.clone(), unit.paragraph_index))
                    .unwrap_or_else(|| {
                        panic!("fixture {label}: sentence paragraph has no coverage")
                    });
                assert_eq!(&unit.parent_paragraph_id, coverage_id, "fixture {label}");
            }
        }
    }

    #[cfg(target_pointer_width = "64")]
    #[test]
    fn a_boundary_that_does_not_fit_32_bits_is_an_error() {
        let range = NormalizedTextRange::new(0, 8).expect("ordered fixture range");
        assert!(SentenceBoundary::new(1, 1, 0, range).is_ok());
        assert!(ParagraphBoundary::new(1, 0, range).is_ok());
        assert_eq!(
            SentenceBoundary::new(usize::MAX, 1, 0, range),
            Err(SegmentationError::BoundaryOverflow {
                field: "sentence.paragraph_index",
                value: usize::MAX,
            })
        );
        assert_eq!(
            ParagraphBoundary::new(1, usize::MAX, range),
            Err(SegmentationError::BoundaryOverflow {
                field: "paragraph.source_order",
                value: usize::MAX,
            })
        );
        assert_eq!(
            SentenceCoverageBoundary::new(
                1,
                range,
                ParagraphContentClass::ProseOrUnknown,
                SentenceEligibility::Eligible,
                8,
                8,
                0,
                0,
                usize::MAX,
            ),
            Err(SegmentationError::BoundaryOverflow {
                field: "coverage.sentence_count",
                value: usize::MAX,
            })
        );
    }
}
