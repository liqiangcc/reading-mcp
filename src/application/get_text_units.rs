use std::collections::HashMap;
use std::sync::Arc;

use crate::application::locator_resolution::resolve_text_locator;
use crate::application::ports::{ApplicationError, DocumentRepository};
use crate::application::text_unit_cursor::{
    TextUnitCursorClaims, decode_text_unit_cursor, encode_text_unit_cursor,
};
use crate::domain::{
    Document, DocumentId, ParagraphContentClass, Section, SectionId, SentenceEligibility,
    SentenceParagraphCoverage, SentenceTextUnit, TEXT_SEGMENTATION_VERSION, TextLocator, TextUnit,
};

pub const DEFAULT_TEXT_UNIT_MAX_ITEMS: usize = 32;
pub const MAX_TEXT_UNIT_MAX_ITEMS: usize = 256;
pub const DEFAULT_TEXT_UNIT_MAX_CHARS: usize = 32 * 1024;
pub const MAX_TEXT_UNIT_MAX_CHARS: usize = 64 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RequestedTextUnitKind {
    Paragraph,
    Sentence,
}

impl RequestedTextUnitKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Paragraph => "paragraph",
            Self::Sentence => "sentence",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextUnitDirection {
    Forward,
    Backward,
}

impl TextUnitDirection {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Forward => "forward",
            Self::Backward => "backward",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextUnitCoveragePolicy {
    PreserveSource,
    EligibleOnly,
}

impl TextUnitCoveragePolicy {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::PreserveSource => "preserve_source",
            Self::EligibleOnly => "eligible_only",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EffectiveTextUnitKind {
    Paragraph,
    Sentence,
}

impl EffectiveTextUnitKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Paragraph => "paragraph",
            Self::Sentence => "sentence",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextUnitContentClass {
    Unknown,
    NonProse,
}

impl TextUnitContentClass {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::NonProse => "non_prose",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GetTextUnitsCommand {
    pub document_id: DocumentId,
    pub section_id: SectionId,
    pub requested_kind: RequestedTextUnitKind,
    pub direction: TextUnitDirection,
    pub coverage_policy: TextUnitCoveragePolicy,
    pub max_items: usize,
    pub max_chars: Option<usize>,
    pub cursor: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextUnitReadingItem {
    pub text: String,
    pub locator: TextLocator,
    pub effective_kind: EffectiveTextUnitKind,
    pub content_class: TextUnitContentClass,
    pub content_class_detail: String,
    pub degradation: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextUnitEnumerationCoverage {
    pub owner_chars: usize,
    pub section_separator_chars: usize,
    pub sentence_separator_chars: usize,
    pub paragraph_count: usize,
    pub sentence_eligible_paragraphs: usize,
    pub non_prose_paragraphs: usize,
    pub coarse_structural_paragraphs: usize,
    pub represented_paragraphs: usize,
    pub represented_sentences: usize,
    pub coarse_non_prose_items: usize,
    pub coarse_structural_items: usize,
    pub intentionally_skipped: usize,
    pub unsupported_gaps: usize,
    pub source_complete: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextUnitStreamSegment {
    pub direction: TextUnitDirection,
    pub start_index: usize,
    pub end_index: usize,
    pub total_items: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GetTextUnitsResult {
    pub document_id: DocumentId,
    pub target_section_locator: TextLocator,
    pub start_anchor_locator: Option<TextLocator>,
    pub requested_kind: RequestedTextUnitKind,
    pub direction: TextUnitDirection,
    pub coverage_policy: TextUnitCoveragePolicy,
    pub items: Vec<TextUnitReadingItem>,
    pub complete: bool,
    pub section_complete: bool,
    pub next_cursor: Option<String>,
    pub coverage: TextUnitEnumerationCoverage,
    pub stream: TextUnitStreamSegment,
}

pub struct GetTextUnitsUseCase {
    repository: Arc<dyn DocumentRepository>,
}

impl GetTextUnitsUseCase {
    pub fn new(repository: Arc<dyn DocumentRepository>) -> Self {
        Self { repository }
    }

    pub async fn execute(
        &self,
        command: GetTextUnitsCommand,
    ) -> Result<GetTextUnitsResult, ApplicationError> {
        self.execute_internal(command, None).await
    }

    pub async fn execute_from_anchor(
        &self,
        command: GetTextUnitsCommand,
        anchor_locator: TextLocator,
    ) -> Result<GetTextUnitsResult, ApplicationError> {
        if command.cursor.is_some() {
            return Err(ApplicationError::InvalidRequest(
                "get_text_units anchor_locator cannot be combined with cursor; continue an anchored traversal with cursor alone"
                    .into(),
            ));
        }
        self.execute_internal(command, Some(anchor_locator)).await
    }

    async fn execute_internal(
        &self,
        command: GetTextUnitsCommand,
        anchor_locator: Option<TextLocator>,
    ) -> Result<GetTextUnitsResult, ApplicationError> {
        validate_budget(command.max_items, command.max_chars)?;

        let cursor_claims = command
            .cursor
            .as_deref()
            .map(decode_text_unit_cursor)
            .transpose()?;
        if let Some(claims) = &cursor_claims {
            validate_cursor_scope(claims, &command)?;
        }

        let document = self
            .repository
            .get(&command.document_id)
            .await?
            .ok_or(ApplicationError::DocumentNotFound)?;
        let section = document
            .find_section(&command.section_id)
            .ok_or(ApplicationError::SectionNotFound)?;
        let normalized_hash = document.normalized_document_hash();

        if let Some(claims) = &cursor_claims {
            if claims.content_hash != document.content_hash.0 {
                return Err(ApplicationError::StaleCursor(format!(
                    "raw content hash changed from {} to {}",
                    claims.content_hash, document.content_hash.0
                )));
            }
            if claims.normalized_document_hash != normalized_hash.0 {
                return Err(ApplicationError::StaleCursor(format!(
                    "normalized document hash changed from {} to {}",
                    claims.normalized_document_hash, normalized_hash.0
                )));
            }
        }

        let (stream_items, coverage) = build_declared_stream(
            &document,
            section,
            command.requested_kind,
            command.coverage_policy,
        )?;
        let total_items = stream_items.len();

        let origin_anchor_index = if let Some(claims) = &cursor_claims {
            validate_cursor_origin_anchor(claims.origin_anchor_index, total_items)?;
            claims.origin_anchor_index
        } else if let Some(anchor_locator) = anchor_locator.as_ref() {
            Some(resolve_anchor_index(
                &document,
                section,
                &stream_items,
                anchor_locator,
                command.requested_kind,
                command.coverage_policy,
            )?)
        } else {
            None
        };

        let position = if let Some(claims) = &cursor_claims {
            if claims.total_items != total_items {
                return Err(ApplicationError::StaleCursor(format!(
                    "text-unit stream length changed from {} to {total_items}",
                    claims.total_items
                )));
            }
            validate_cursor_position(claims.next_index, total_items, command.direction)?;
            validate_anchored_cursor_position(
                claims.next_index,
                claims.origin_anchor_index,
                command.direction,
            )?;
            claims.next_index
        } else if let Some(anchor_index) = origin_anchor_index {
            match command.direction {
                TextUnitDirection::Forward => anchor_index + 1,
                TextUnitDirection::Backward => anchor_index,
            }
        } else {
            match command.direction {
                TextUnitDirection::Forward => 0,
                TextUnitDirection::Backward => total_items,
            }
        };

        let page = paginate(
            &stream_items,
            command.direction,
            position,
            command.max_items,
            command.max_chars.unwrap_or(DEFAULT_TEXT_UNIT_MAX_CHARS),
        )?;
        let next_cursor = if page.complete {
            None
        } else {
            Some(encode_text_unit_cursor(
                TextUnitCursorClaims::new(
                    document.id.0.clone(),
                    document.content_hash.0.clone(),
                    normalized_hash.0.clone(),
                    section.id.0.clone(),
                    TEXT_SEGMENTATION_VERSION,
                    command.requested_kind.as_str(),
                    command.direction.as_str(),
                    command.coverage_policy.as_str(),
                    page.next_index,
                    total_items,
                )
                .with_origin_anchor_index(origin_anchor_index),
            )?)
        };
        let section_complete =
            page.complete && coverage.source_complete && origin_anchor_index.is_none();
        let start_anchor_locator = origin_anchor_index
            .and_then(|index| stream_items.get(index))
            .map(|item| item.locator.clone());

        Ok(GetTextUnitsResult {
            document_id: document.id.clone(),
            target_section_locator: TextLocator::for_section(&document, section),
            start_anchor_locator,
            requested_kind: command.requested_kind,
            direction: command.direction,
            coverage_policy: command.coverage_policy,
            items: page.items,
            complete: page.complete,
            section_complete,
            next_cursor,
            coverage,
            stream: TextUnitStreamSegment {
                direction: command.direction,
                start_index: page.start_index,
                end_index: page.end_index,
                total_items,
            },
        })
    }
}

fn validate_budget(max_items: usize, max_chars: Option<usize>) -> Result<(), ApplicationError> {
    if max_items == 0 {
        return Err(ApplicationError::InvalidRequest(
            "get_text_units max_items must be greater than zero".into(),
        ));
    }
    if max_items > MAX_TEXT_UNIT_MAX_ITEMS {
        return Err(ApplicationError::ResourceLimitExceeded(format!(
            "get_text_units max_items {max_items} exceeds server limit {MAX_TEXT_UNIT_MAX_ITEMS}"
        )));
    }
    let max_chars = max_chars.unwrap_or(DEFAULT_TEXT_UNIT_MAX_CHARS);
    if max_chars == 0 {
        return Err(ApplicationError::InvalidRequest(
            "get_text_units max_chars must be greater than zero".into(),
        ));
    }
    if max_chars > MAX_TEXT_UNIT_MAX_CHARS {
        return Err(ApplicationError::ResourceLimitExceeded(format!(
            "get_text_units max_chars {max_chars} exceeds server limit {MAX_TEXT_UNIT_MAX_CHARS}"
        )));
    }
    Ok(())
}

fn validate_cursor_scope(
    claims: &TextUnitCursorClaims,
    command: &GetTextUnitsCommand,
) -> Result<(), ApplicationError> {
    if claims.document_id != command.document_id.0 {
        return Err(ApplicationError::CursorTargetMismatch(format!(
            "text-unit cursor document {} does not match requested document {}",
            claims.document_id, command.document_id.0
        )));
    }
    if claims.section_id != command.section_id.0 {
        return Err(ApplicationError::CursorTargetMismatch(format!(
            "text-unit cursor section {} does not match requested section {}",
            claims.section_id, command.section_id.0
        )));
    }
    if claims.segmentation_version != TEXT_SEGMENTATION_VERSION {
        return Err(ApplicationError::StaleCursor(format!(
            "text-unit cursor segmentation version {} is incompatible with {TEXT_SEGMENTATION_VERSION}",
            claims.segmentation_version
        )));
    }
    if claims.requested_kind != command.requested_kind.as_str()
        || claims.direction != command.direction.as_str()
        || claims.coverage_policy != command.coverage_policy.as_str()
    {
        return Err(ApplicationError::CursorTargetMismatch(
            "text-unit cursor stream contract does not match requested kind/direction/coverage policy"
                .into(),
        ));
    }
    Ok(())
}

fn validate_cursor_origin_anchor(
    origin_anchor_index: Option<usize>,
    total_items: usize,
) -> Result<(), ApplicationError> {
    if origin_anchor_index.is_some_and(|index| index >= total_items) {
        return Err(ApplicationError::InvalidCursor(format!(
            "text-unit cursor origin anchor is outside the current {total_items}-item stream"
        )));
    }
    Ok(())
}

fn validate_anchored_cursor_position(
    next_index: usize,
    origin_anchor_index: Option<usize>,
    direction: TextUnitDirection,
) -> Result<(), ApplicationError> {
    let Some(anchor_index) = origin_anchor_index else {
        return Ok(());
    };
    let valid = match direction {
        TextUnitDirection::Forward => next_index > anchor_index,
        TextUnitDirection::Backward => next_index <= anchor_index,
    };
    if !valid {
        return Err(ApplicationError::InvalidCursor(format!(
            "text-unit cursor position {next_index} crosses its origin anchor {anchor_index} in {} direction",
            direction.as_str()
        )));
    }
    Ok(())
}

fn validate_cursor_position(
    next_index: usize,
    total_items: usize,
    direction: TextUnitDirection,
) -> Result<(), ApplicationError> {
    let valid = match direction {
        TextUnitDirection::Forward => next_index < total_items,
        TextUnitDirection::Backward => next_index > 0 && next_index <= total_items,
    };
    if !valid {
        return Err(ApplicationError::InvalidCursor(format!(
            "text-unit cursor position {next_index} is not resumable for {total_items} items in {} direction",
            direction.as_str()
        )));
    }
    Ok(())
}

fn resolve_anchor_index(
    document: &Document,
    section: &Section,
    stream_items: &[TextUnitReadingItem],
    anchor_locator: &TextLocator,
    requested_kind: RequestedTextUnitKind,
    coverage_policy: TextUnitCoveragePolicy,
) -> Result<usize, ApplicationError> {
    let resolved = resolve_text_locator(document, anchor_locator)?;
    if resolved.locator.owner_section_id != section.id {
        return Err(ApplicationError::InvalidRequest(format!(
            "get_text_units anchor belongs to section {}, requested section is {}",
            resolved.locator.owner_section_id.0, section.id.0
        )));
    }
    stream_items
        .iter()
        .position(|item| item.locator == resolved.locator)
        .ok_or_else(|| {
            ApplicationError::InvalidRequest(format!(
                "{} anchor is not part of the declared {} / {} TextUnit stream",
                resolved.kind.as_str(),
                requested_kind.as_str(),
                coverage_policy.as_str()
            ))
        })
}

fn build_declared_stream(
    document: &Document,
    section: &Section,
    requested_kind: RequestedTextUnitKind,
    coverage_policy: TextUnitCoveragePolicy,
) -> Result<(Vec<TextUnitReadingItem>, TextUnitEnumerationCoverage), ApplicationError> {
    let paragraph_set = document.try_paragraph_text_units().map_err(|error| {
        ApplicationError::TextUnitIndexFailed(format!(
            "cannot materialize Paragraph TextUnits from persisted block evidence: {error}"
        ))
    })?;
    let sentence_set = document.try_sentence_text_units().map_err(|error| {
        ApplicationError::TextUnitIndexFailed(format!(
            "cannot materialize Sentence TextUnits from persisted block evidence: {error}"
        ))
    })?;
    let paragraphs = paragraph_set
        .units
        .iter()
        .filter(|unit| unit.owner_section_id == section.id)
        .collect::<Vec<_>>();
    let section_coverage = paragraph_set
        .coverage
        .iter()
        .find(|coverage| coverage.owner_section_id == section.id)
        .ok_or_else(|| {
            ApplicationError::InvalidRequest(format!(
                "paragraph coverage is unavailable for section {}",
                section.id.0
            ))
        })?;

    let coverage_by_paragraph = sentence_set
        .coverage
        .iter()
        .filter(|coverage| coverage.owner_section_id == section.id)
        .map(|coverage| (coverage.paragraph_index, coverage))
        .collect::<HashMap<_, _>>();
    let mut sentences_by_paragraph: HashMap<usize, Vec<&SentenceTextUnit>> = HashMap::new();
    for sentence in sentence_set
        .units
        .iter()
        .filter(|unit| unit.owner_section_id == section.id)
    {
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

    let mut items = Vec::new();
    let mut represented_paragraphs = 0usize;
    let mut represented_sentences = 0usize;
    let mut coarse_non_prose_items = 0usize;
    let mut coarse_structural_items = 0usize;
    let mut intentionally_skipped = 0usize;

    for paragraph in paragraphs {
        let paragraph_coverage = coverage_by_paragraph
            .get(&paragraph.paragraph_index)
            .copied()
            .ok_or_else(|| {
                ApplicationError::InvalidRequest(format!(
                    "sentence coverage is unavailable for paragraph {} in section {}",
                    paragraph.paragraph_index, section.id.0
                ))
            })?;
        let is_coarse = paragraph_coverage.eligibility == SentenceEligibility::CoarseParagraphOnly;

        match requested_kind {
            RequestedTextUnitKind::Paragraph => {
                if coverage_policy == TextUnitCoveragePolicy::EligibleOnly && is_coarse {
                    intentionally_skipped += 1;
                    continue;
                }
                items.push(paragraph_item(
                    document,
                    section,
                    paragraph,
                    paragraph_coverage,
                    None,
                ));
                represented_paragraphs += 1;
            }
            RequestedTextUnitKind::Sentence if is_coarse => {
                if coverage_policy == TextUnitCoveragePolicy::EligibleOnly {
                    intentionally_skipped += 1;
                    continue;
                }
                let degradation = if is_coarse_structural_class(paragraph_coverage.content_class) {
                    coarse_structural_items += 1;
                    "flat_native_container_no_nested_textunit_evidence"
                } else {
                    coarse_non_prose_items += 1;
                    "requested_sentence_but_non_prose_is_paragraph_only"
                };
                items.push(paragraph_item(
                    document,
                    section,
                    paragraph,
                    paragraph_coverage,
                    Some(degradation.into()),
                ));
                represented_paragraphs += 1;
            }
            RequestedTextUnitKind::Sentence => {
                for sentence in sentences_by_paragraph
                    .get(&paragraph.paragraph_index)
                    .into_iter()
                    .flatten()
                {
                    items.push(sentence_item(
                        document,
                        section,
                        sentence,
                        paragraph_coverage.content_class,
                    ));
                    represented_sentences += 1;
                }
            }
        }
    }

    let source_complete =
        coverage_policy == TextUnitCoveragePolicy::PreserveSource && intentionally_skipped == 0;
    Ok((
        items,
        TextUnitEnumerationCoverage {
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
            intentionally_skipped,
            unsupported_gaps: 0,
            source_complete,
        },
    ))
}

fn paragraph_item(
    document: &Document,
    section: &Section,
    paragraph: &TextUnit,
    coverage: &SentenceParagraphCoverage,
    degradation: Option<String>,
) -> TextUnitReadingItem {
    let (content_class, detail) = content_class(coverage.content_class);
    TextUnitReadingItem {
        text: paragraph.text.clone(),
        locator: TextLocator::for_paragraph(document, section, paragraph),
        effective_kind: EffectiveTextUnitKind::Paragraph,
        content_class,
        content_class_detail: detail.into(),
        degradation,
    }
}

fn sentence_item(
    document: &Document,
    section: &Section,
    sentence: &SentenceTextUnit,
    paragraph_class: ParagraphContentClass,
) -> TextUnitReadingItem {
    let (content_class, detail) = content_class(paragraph_class);
    TextUnitReadingItem {
        text: sentence.text.clone(),
        locator: TextLocator::for_sentence(document, section, sentence),
        effective_kind: EffectiveTextUnitKind::Sentence,
        content_class,
        content_class_detail: detail.into(),
        degradation: None,
    }
}

fn content_class(class: ParagraphContentClass) -> (TextUnitContentClass, &'static str) {
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

#[derive(Debug)]
struct Page {
    items: Vec<TextUnitReadingItem>,
    start_index: usize,
    end_index: usize,
    next_index: usize,
    complete: bool,
}

fn paginate(
    items: &[TextUnitReadingItem],
    direction: TextUnitDirection,
    position: usize,
    max_items: usize,
    max_chars: usize,
) -> Result<Page, ApplicationError> {
    match direction {
        TextUnitDirection::Forward => paginate_forward(items, position, max_items, max_chars),
        TextUnitDirection::Backward => paginate_backward(items, position, max_items, max_chars),
    }
}

fn paginate_forward(
    items: &[TextUnitReadingItem],
    start: usize,
    max_items: usize,
    max_chars: usize,
) -> Result<Page, ApplicationError> {
    if start > items.len() {
        return Err(ApplicationError::InvalidCursor(format!(
            "forward text-unit start {start} exceeds stream length {}",
            items.len()
        )));
    }
    let mut end = start;
    let mut chars = 0usize;
    while end < items.len() && end - start < max_items {
        let item_chars = items[end].text.chars().count();
        if end == start && item_chars > max_chars {
            return Err(ApplicationError::ResourceLimitExceeded(format!(
                "next text unit contains {item_chars} characters, exceeding max_chars {max_chars}"
            )));
        }
        if chars + item_chars > max_chars {
            break;
        }
        chars += item_chars;
        end += 1;
    }
    Ok(Page {
        items: items[start..end].to_vec(),
        start_index: start,
        end_index: end,
        next_index: end,
        complete: end == items.len(),
    })
}

fn paginate_backward(
    items: &[TextUnitReadingItem],
    end: usize,
    max_items: usize,
    max_chars: usize,
) -> Result<Page, ApplicationError> {
    if end > items.len() {
        return Err(ApplicationError::InvalidCursor(format!(
            "backward text-unit end {end} exceeds stream length {}",
            items.len()
        )));
    }
    let mut start = end;
    let mut chars = 0usize;
    while start > 0 && end - start < max_items {
        let candidate = start - 1;
        let item_chars = items[candidate].text.chars().count();
        if start == end && item_chars > max_chars {
            return Err(ApplicationError::ResourceLimitExceeded(format!(
                "next text unit contains {item_chars} characters, exceeding max_chars {max_chars}"
            )));
        }
        if chars + item_chars > max_chars {
            break;
        }
        chars += item_chars;
        start = candidate;
    }
    Ok(Page {
        items: items[start..end].to_vec(),
        start_index: start,
        end_index: end,
        next_index: start,
        complete: start == 0,
    })
}
