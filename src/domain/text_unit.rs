use sha2::{Digest, Sha256};

use super::{
    ContentHash, Document, DocumentId, NormalizedBlockKind, NormalizedBlockMap,
    NormalizedBlockMapError, NormalizedDocumentHash, NormalizedTextRange, Section, SectionId,
};

pub const TEXT_SEGMENTATION_VERSION: &str = "text-segmentation/v3";
pub const TEXT_UNIT_ID_VERSION: &str = "text-unit-id/v1";

const TEXT_UNIT_ID_DOMAIN: &[u8] = b"reading-mcp/text-unit-id/v1\0";

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct TextUnitId(pub String);

impl AsRef<str> for TextUnitId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TextUnitKind {
    Paragraph,
}

impl TextUnitKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Paragraph => "paragraph",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextUnit {
    pub id: TextUnitId,
    pub document_id: DocumentId,
    pub content_hash: ContentHash,
    pub normalized_document_hash: NormalizedDocumentHash,
    pub owner_section_id: SectionId,
    pub kind: TextUnitKind,
    /// Human-facing, 1-based Paragraph ordinal within the owner Section.
    pub paragraph_index: usize,
    /// Global deterministic traversal order within the normalized Document.
    pub source_order: usize,
    pub normalized_range: NormalizedTextRange,
    pub text: String,
    pub segmentation_version: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ParagraphContentClass {
    /// Persisted text has no strong native/non-prose signal. This is intentionally not a claim of
    /// parser-native prose provenance; it only enables deterministic fallback segmentation.
    ProseOrUnknown,
    NativeParagraph,
    /// Flat `normalized-block-model/v1` may suppress nested leaf blocks, so BlockQuote remains
    /// coarse until stronger nested/leaf evidence is persisted.
    BlockQuote,
    /// Flat `normalized-block-model/v1` may suppress nested leaf blocks, so ListItem remains
    /// coarse until stronger nested/leaf evidence is persisted.
    ListItem,
    CodeBlock,
    Preformatted,
    Table,
}

impl ParagraphContentClass {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ProseOrUnknown => "prose_or_unknown",
            Self::NativeParagraph => "native_paragraph",
            Self::BlockQuote => "blockquote",
            Self::ListItem => "list_item",
            Self::CodeBlock => "code_block",
            Self::Preformatted => "preformatted",
            Self::Table => "table",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SentenceEligibility {
    Eligible,
    CoarseParagraphOnly,
}

impl SentenceEligibility {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Eligible => "eligible",
            Self::CoarseParagraphOnly => "coarse_paragraph_only",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SentenceTextUnit {
    pub id: TextUnitId,
    pub document_id: DocumentId,
    pub content_hash: ContentHash,
    pub normalized_document_hash: NormalizedDocumentHash,
    pub owner_section_id: SectionId,
    /// Human-facing, 1-based containing Paragraph ordinal within the owner Section.
    pub paragraph_index: usize,
    /// Human-facing, 1-based Sentence ordinal within the containing Paragraph.
    pub sentence_index: usize,
    /// Stable containing Paragraph TextUnit handle for container handoff.
    pub parent_paragraph_id: TextUnitId,
    /// Deterministic order within the document's Sentence stream.
    pub source_order: usize,
    pub normalized_range: NormalizedTextRange,
    pub text: String,
    pub segmentation_version: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParagraphSectionCoverage {
    pub owner_section_id: SectionId,
    pub owner_chars: usize,
    pub paragraph_chars: usize,
    pub separator_chars: usize,
    pub paragraph_count: usize,
    pub native_paragraph_chars: usize,
    pub native_structural_container_chars: usize,
    pub native_non_prose_chars: usize,
    pub fallback_chars: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParagraphTextUnitSet {
    pub normalized_document_hash: NormalizedDocumentHash,
    pub units: Vec<TextUnit>,
    pub coverage: Vec<ParagraphSectionCoverage>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SentenceParagraphCoverage {
    pub owner_section_id: SectionId,
    pub paragraph_id: TextUnitId,
    pub paragraph_index: usize,
    pub content_class: ParagraphContentClass,
    pub eligibility: SentenceEligibility,
    pub paragraph_chars: usize,
    pub sentence_chars: usize,
    pub separator_chars: usize,
    pub coarse_only_chars: usize,
    pub sentence_count: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SentenceTextUnitSet {
    pub normalized_document_hash: NormalizedDocumentHash,
    pub units: Vec<SentenceTextUnit>,
    pub coverage: Vec<SentenceParagraphCoverage>,
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

impl Document {
    pub fn try_paragraph_text_units(
        &self,
    ) -> Result<ParagraphTextUnitSet, NormalizedBlockMapError> {
        let block_map = self.normalized_block_map()?;
        Ok(self.build_paragraph_text_units(block_map.as_ref()))
    }

    pub fn paragraph_text_units(&self) -> ParagraphTextUnitSet {
        self.try_paragraph_text_units()
            .expect("canonical normalized block map must be valid for TextUnit materialization")
    }

    pub fn try_sentence_text_units(&self) -> Result<SentenceTextUnitSet, NormalizedBlockMapError> {
        let block_map = self.normalized_block_map()?;
        let paragraph_set = self.build_paragraph_text_units(block_map.as_ref());
        Ok(self.build_sentence_text_units(&paragraph_set, block_map.as_ref()))
    }

    pub fn sentence_text_units(&self) -> SentenceTextUnitSet {
        self.try_sentence_text_units()
            .expect("canonical normalized block map must be valid for Sentence materialization")
    }

    fn build_paragraph_text_units(
        &self,
        block_map: Option<&NormalizedBlockMap>,
    ) -> ParagraphTextUnitSet {
        let normalized_document_hash = self.normalized_document_hash();
        let mut units = Vec::new();
        let mut coverage = Vec::new();

        for section in &self.root_sections {
            collect_section_paragraphs(
                self,
                &normalized_document_hash,
                block_map,
                section,
                &mut units,
                &mut coverage,
            );
        }

        ParagraphTextUnitSet {
            normalized_document_hash,
            units,
            coverage,
        }
    }

    fn build_sentence_text_units(
        &self,
        paragraph_set: &ParagraphTextUnitSet,
        block_map: Option<&NormalizedBlockMap>,
    ) -> SentenceTextUnitSet {
        let normalized_document_hash = paragraph_set.normalized_document_hash.clone();
        let mut units = Vec::new();
        let mut coverage = Vec::with_capacity(paragraph_set.units.len());

        for paragraph in &paragraph_set.units {
            let content_class = paragraph_content_class(block_map, paragraph);
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
                && looks_like_unreliable_fallback_text(&paragraph.text)
            {
                eligibility = SentenceEligibility::CoarseParagraphOnly;
            }

            if eligibility == SentenceEligibility::CoarseParagraphOnly {
                coverage.push(SentenceParagraphCoverage {
                    owner_section_id: paragraph.owner_section_id.clone(),
                    paragraph_id: paragraph.id.clone(),
                    paragraph_index: paragraph.paragraph_index,
                    content_class,
                    eligibility,
                    paragraph_chars: paragraph.normalized_range.len(),
                    sentence_chars: 0,
                    separator_chars: 0,
                    coarse_only_chars: paragraph.normalized_range.len(),
                    sentence_count: 0,
                });
                continue;
            }

            let ranges = sentence_ranges(paragraph);
            let sentence_chars = ranges.iter().map(|range| range.len()).sum::<usize>();
            let section = self
                .find_section(&paragraph.owner_section_id)
                .expect("paragraph owner section must exist in its canonical document");

            for (offset, range) in ranges.iter().copied().enumerate() {
                let sentence_index = offset + 1;
                let source_order = units.len();
                let text = section
                    .normalized_text_slice(range)
                    .expect("generated sentence range must be a valid owner slice")
                    .to_string();
                let id = sentence_text_unit_id(
                    &self.id,
                    &normalized_document_hash,
                    &paragraph.owner_section_id,
                    paragraph.paragraph_index,
                    sentence_index,
                    range,
                );

                units.push(SentenceTextUnit {
                    id,
                    document_id: self.id.clone(),
                    content_hash: self.content_hash.clone(),
                    normalized_document_hash: normalized_document_hash.clone(),
                    owner_section_id: paragraph.owner_section_id.clone(),
                    paragraph_index: paragraph.paragraph_index,
                    sentence_index,
                    parent_paragraph_id: paragraph.id.clone(),
                    source_order,
                    normalized_range: range,
                    text,
                    segmentation_version: TEXT_SEGMENTATION_VERSION.into(),
                });
            }

            let paragraph_chars = paragraph.normalized_range.len();
            let separator_chars = paragraph_chars
                .checked_sub(sentence_chars)
                .expect("generated sentence ranges must remain inside the containing Paragraph");
            coverage.push(SentenceParagraphCoverage {
                owner_section_id: paragraph.owner_section_id.clone(),
                paragraph_id: paragraph.id.clone(),
                paragraph_index: paragraph.paragraph_index,
                content_class,
                eligibility,
                paragraph_chars,
                sentence_chars,
                separator_chars,
                coarse_only_chars: 0,
                sentence_count: ranges.len(),
            });
        }

        SentenceTextUnitSet {
            normalized_document_hash,
            units,
            coverage,
        }
    }
}

fn collect_section_paragraphs(
    document: &Document,
    normalized_document_hash: &NormalizedDocumentHash,
    block_map: Option<&NormalizedBlockMap>,
    section: &Section,
    units: &mut Vec<TextUnit>,
    coverage: &mut Vec<ParagraphSectionCoverage>,
) {
    let candidates = section_paragraph_candidates(section, block_map);
    let paragraph_chars = candidates
        .iter()
        .map(|candidate| candidate.range.len())
        .sum::<usize>();
    let owner_chars = section.normalized_text_len();
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

    coverage.push(ParagraphSectionCoverage {
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
    });

    for (offset, candidate) in candidates.into_iter().enumerate() {
        let paragraph_index = offset + 1;
        let source_order = units.len();
        let text = section
            .normalized_text_slice(candidate.range)
            .expect("generated paragraph range must be a valid owner slice")
            .to_string();
        let id = paragraph_text_unit_id(
            &document.id,
            normalized_document_hash,
            &section.id,
            paragraph_index,
            candidate.range,
        );

        units.push(TextUnit {
            id,
            document_id: document.id.clone(),
            content_hash: document.content_hash.clone(),
            normalized_document_hash: normalized_document_hash.clone(),
            owner_section_id: section.id.clone(),
            kind: TextUnitKind::Paragraph,
            paragraph_index,
            source_order,
            normalized_range: candidate.range,
            text,
            segmentation_version: TEXT_SEGMENTATION_VERSION.into(),
        });
    }

    for child in &section.children {
        collect_section_paragraphs(
            document,
            normalized_document_hash,
            block_map,
            child,
            units,
            coverage,
        );
    }
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
    paragraph: &TextUnit,
) -> ParagraphContentClass {
    if let Some(block) = block_map.and_then(|map| {
        map.blocks.iter().find(|block| {
            block.owner_section_id == paragraph.owner_section_id
                && block.normalized_range == paragraph.normalized_range
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

    classify_fallback_paragraph_content(&paragraph.text)
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

fn sentence_ranges(paragraph: &TextUnit) -> Vec<NormalizedTextRange> {
    let chars = paragraph.text.chars().collect::<Vec<_>>();
    let mut ranges = Vec::new();
    let mut start = skip_whitespace(&chars, 0);
    let mut cursor = start;

    while cursor < chars.len() {
        if let Some(end) = sentence_boundary_end(&chars, cursor) {
            if start < end {
                ranges.push(
                    NormalizedTextRange::new(
                        paragraph.normalized_range.start() + start,
                        paragraph.normalized_range.start() + end,
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
                paragraph.normalized_range.start() + start,
                paragraph.normalized_range.start() + tail_end,
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

fn paragraph_text_unit_id(
    document_id: &DocumentId,
    normalized_document_hash: &NormalizedDocumentHash,
    owner_section_id: &SectionId,
    paragraph_index: usize,
    range: NormalizedTextRange,
) -> TextUnitId {
    let mut hasher = Sha256::new();
    hasher.update(TEXT_UNIT_ID_DOMAIN);
    hash_text(&mut hasher, document_id.0.as_str());
    hash_text(&mut hasher, normalized_document_hash.as_ref());
    hash_text(&mut hasher, owner_section_id.0.as_str());
    hash_text(&mut hasher, TextUnitKind::Paragraph.as_str());
    hash_usize(&mut hasher, paragraph_index);
    hash_usize(&mut hasher, range.start());
    hash_usize(&mut hasher, range.end());
    hash_text(&mut hasher, TEXT_SEGMENTATION_VERSION);
    TextUnitId(format!("tu1:{:x}", hasher.finalize()))
}

fn sentence_text_unit_id(
    document_id: &DocumentId,
    normalized_document_hash: &NormalizedDocumentHash,
    owner_section_id: &SectionId,
    paragraph_index: usize,
    sentence_index: usize,
    range: NormalizedTextRange,
) -> TextUnitId {
    let mut hasher = Sha256::new();
    hasher.update(TEXT_UNIT_ID_DOMAIN);
    hash_text(&mut hasher, document_id.0.as_str());
    hash_text(&mut hasher, normalized_document_hash.as_ref());
    hash_text(&mut hasher, owner_section_id.0.as_str());
    hash_text(&mut hasher, "sentence");
    hash_usize(&mut hasher, paragraph_index);
    hash_usize(&mut hasher, sentence_index);
    hash_usize(&mut hasher, range.start());
    hash_usize(&mut hasher, range.end());
    hash_text(&mut hasher, TEXT_SEGMENTATION_VERSION);
    TextUnitId(format!("tu1:{:x}", hasher.finalize()))
}

fn hash_text(hasher: &mut Sha256, value: &str) {
    hash_usize(hasher, value.len());
    hasher.update(value.as_bytes());
}

fn hash_usize(hasher: &mut Sha256, value: usize) {
    let value = u64::try_from(value).expect("text-unit identity values must fit in u64");
    hasher.update(value.to_be_bytes());
}
