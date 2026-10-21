use std::sync::Arc;

use crate::application::light_sections::SectionTexts;
use crate::application::locator_resolution::{ResolvedLocatorKind, resolve_text_locator};
use crate::application::ports::{
    ApplicationError, DocumentRepository, OwnedStreamUnit, StoredStreamItem, TextUnitIndex,
    TextUnitStreamKey,
};
use crate::application::text_unit_cursor::{
    TextUnitCursorClaims, decode_text_unit_cursor, encode_text_unit_cursor,
};
use crate::application::text_unit_stream::content_class;
use crate::domain::{
    Document, DocumentId, NormalizedDocumentHash, ParagraphContentClass, Section, SectionId,
    SentenceTextUnit, TEXT_SEGMENTATION_VERSION, TextLocator, TextUnit,
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
    text_unit_index: Arc<dyn TextUnitIndex>,
}

impl GetTextUnitsUseCase {
    pub fn new(
        repository: Arc<dyn DocumentRepository>,
        text_unit_index: Arc<dyn TextUnitIndex>,
    ) -> Self {
        Self {
            repository,
            text_unit_index,
        }
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

        let (document, normalized_hash) = self
            .repository
            .get_light_with_hash(&command.document_id)
            .await?
            .ok_or(ApplicationError::DocumentNotFound)?;
        let section = document
            .find_section(&command.section_id)
            .ok_or(ApplicationError::SectionNotFound)?;

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

        let summary = self
            .text_unit_index
            .section_stream_summary(TextUnitStreamKey {
                document_id: &document.id,
                section_id: &section.id,
                kind: command.requested_kind,
                policy: command.coverage_policy,
            })
            .await?
            .ok_or_else(|| stale_text_units(&document))?;
        let total_items = summary.total_items;
        let coverage = summary.coverage;

        let (origin_anchor_index, start_anchor_locator) = if let Some(claims) = &cursor_claims {
            validate_cursor_origin_anchor(claims.origin_anchor_index, total_items)?;
            let locator = match claims.origin_anchor_index {
                Some(index) => {
                    self.stream_item_locator(&document, section, &command, index)
                        .await?
                }
                None => None,
            };
            (claims.origin_anchor_index, locator)
        } else if let Some(anchor_locator) = anchor_locator.as_ref() {
            let mut texts = SectionTexts::from_repository(&document, self.repository.as_ref());
            let (index, locator) = self
                .resolve_anchor_index(
                    &document,
                    section,
                    anchor_locator,
                    command.requested_kind,
                    command.coverage_policy,
                    &normalized_hash,
                    &mut texts,
                )
                .await?;
            (Some(index), Some(locator))
        } else {
            (None, None)
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

        let page = self
            .page(&document, section, &command, position, total_items)
            .await?;
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

        Ok(GetTextUnitsResult {
            document_id: document.id.clone(),
            target_section_locator: TextLocator::for_section_with_hash(
                &document,
                section,
                &normalized_hash,
            ),
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

    /// The locator of the single stream item at `index`, or `None` when it is not readable.
    async fn stream_item_locator(
        &self,
        document: &Document,
        section: &Section,
        command: &GetTextUnitsCommand,
        index: usize,
    ) -> Result<Option<TextLocator>, ApplicationError> {
        let rows = self
            .text_unit_index
            .section_stream_items(
                TextUnitStreamKey {
                    document_id: &document.id,
                    section_id: &section.id,
                    kind: command.requested_kind,
                    policy: command.coverage_policy,
                },
                TextUnitDirection::Forward,
                index,
                1,
            )
            .await?;
        Ok(rows
            .into_iter()
            .next()
            .map(|row| reading_item(document, section, row).locator))
    }

    #[allow(clippy::too_many_arguments)]
    async fn resolve_anchor_index(
        &self,
        document: &Document,
        section: &Section,
        anchor_locator: &TextLocator,
        requested_kind: RequestedTextUnitKind,
        coverage_policy: TextUnitCoveragePolicy,
        normalized_document_hash: &NormalizedDocumentHash,
        texts: &mut SectionTexts<'_>,
    ) -> Result<(usize, TextLocator), ApplicationError> {
        let resolved = resolve_text_locator(
            document,
            anchor_locator,
            normalized_document_hash,
            self.text_unit_index.as_ref(),
            texts,
        )
        .await?;
        if resolved.locator.owner_section_id != section.id {
            return Err(ApplicationError::InvalidRequest(format!(
                "get_text_units anchor belongs to section {}, requested section is {}",
                resolved.locator.owner_section_id.0, section.id.0
            )));
        }
        fn not_in_stream(
            kind: ResolvedLocatorKind,
            requested_kind: RequestedTextUnitKind,
            coverage_policy: TextUnitCoveragePolicy,
        ) -> ApplicationError {
            ApplicationError::InvalidRequest(format!(
                "{} anchor is not part of the declared {} / {} TextUnit stream",
                kind.as_str(),
                requested_kind.as_str(),
                coverage_policy.as_str()
            ))
        }
        let unit_kind = match resolved.kind {
            ResolvedLocatorKind::Paragraph => EffectiveTextUnitKind::Paragraph,
            ResolvedLocatorKind::Sentence => EffectiveTextUnitKind::Sentence,
            ResolvedLocatorKind::Section | ResolvedLocatorKind::CharacterRange => {
                return Err(not_in_stream(
                    resolved.kind,
                    requested_kind,
                    coverage_policy,
                ));
            }
        };
        let paragraph_index = resolved
            .locator
            .paragraph_index
            .ok_or_else(|| not_in_stream(resolved.kind, requested_kind, coverage_policy))?;
        let position = self
            .text_unit_index
            .section_stream_position(
                TextUnitStreamKey {
                    document_id: &document.id,
                    section_id: &section.id,
                    kind: requested_kind,
                    policy: coverage_policy,
                },
                unit_kind,
                paragraph_index,
                resolved.locator.sentence_index,
            )
            .await?
            .ok_or_else(|| not_in_stream(resolved.kind, requested_kind, coverage_policy))?;
        let rows = self
            .text_unit_index
            .section_stream_items(
                TextUnitStreamKey {
                    document_id: &document.id,
                    section_id: &section.id,
                    kind: requested_kind,
                    policy: coverage_policy,
                },
                TextUnitDirection::Forward,
                position,
                1,
            )
            .await?;
        let row = rows
            .into_iter()
            .next()
            .ok_or_else(|| not_in_stream(resolved.kind, requested_kind, coverage_policy))?;
        let locator = reading_item(document, section, row).locator;
        if locator != resolved.locator {
            return Err(not_in_stream(
                resolved.kind,
                requested_kind,
                coverage_policy,
            ));
        }
        Ok((position, locator))
    }

    async fn page(
        &self,
        document: &Document,
        section: &Section,
        command: &GetTextUnitsCommand,
        position: usize,
        total_items: usize,
    ) -> Result<Page, ApplicationError> {
        let max_chars = command.max_chars.unwrap_or(DEFAULT_TEXT_UNIT_MAX_CHARS);
        if command.direction == TextUnitDirection::Forward && position > total_items {
            return Err(ApplicationError::InvalidCursor(format!(
                "forward text-unit start {position} exceeds stream length {total_items}"
            )));
        }
        if command.direction == TextUnitDirection::Backward && position > total_items {
            return Err(ApplicationError::InvalidCursor(format!(
                "backward text-unit end {position} exceeds stream length {total_items}"
            )));
        }
        let rows = self
            .text_unit_index
            .section_stream_items(
                TextUnitStreamKey {
                    document_id: &document.id,
                    section_id: &section.id,
                    kind: command.requested_kind,
                    policy: command.coverage_policy,
                },
                command.direction,
                position,
                command.max_items,
            )
            .await?;
        let char_counts = rows.iter().map(|row| row.char_count).collect::<Vec<_>>();

        match command.direction {
            TextUnitDirection::Forward => {
                let kept = forward_page_len(&char_counts, max_chars)?;
                let items = rows
                    .into_iter()
                    .take(kept)
                    .map(|row| reading_item(document, section, row))
                    .collect();
                let end_index = position + kept;
                Ok(Page {
                    items,
                    start_index: position,
                    end_index,
                    next_index: end_index,
                    complete: end_index == total_items,
                })
            }
            TextUnitDirection::Backward => {
                let kept = backward_page_len(&char_counts, max_chars)?;
                let start = char_counts.len() - kept;
                let items = rows
                    .into_iter()
                    .skip(start)
                    .map(|row| reading_item(document, section, row))
                    .collect();
                let start_index = position - kept;
                Ok(Page {
                    items,
                    start_index,
                    end_index: position,
                    next_index: start_index,
                    complete: start_index == 0,
                })
            }
        }
    }
}

fn stale_text_units(document: &Document) -> ApplicationError {
    ApplicationError::StaleDocument(format!(
        "text units of document {} are not stored for {TEXT_SEGMENTATION_VERSION}; open the document again",
        document.id.0
    ))
}

/// How many fetched rows the forward char budget keeps, from the start.
fn forward_page_len(char_counts: &[usize], max_chars: usize) -> Result<usize, ApplicationError> {
    let mut chars = 0usize;
    let mut kept = 0usize;
    for (index, item_chars) in char_counts.iter().enumerate() {
        if index == 0 && *item_chars > max_chars {
            return Err(ApplicationError::ResourceLimitExceeded(format!(
                "next text unit contains {item_chars} characters, exceeding max_chars {max_chars}"
            )));
        }
        if chars + *item_chars > max_chars {
            break;
        }
        chars += *item_chars;
        kept += 1;
    }
    Ok(kept)
}

/// How many fetched rows the backward char budget keeps, counting from the end.
fn backward_page_len(char_counts: &[usize], max_chars: usize) -> Result<usize, ApplicationError> {
    let mut chars = 0usize;
    let mut kept = 0usize;
    for (index, item_chars) in char_counts.iter().rev().enumerate() {
        if index == 0 && *item_chars > max_chars {
            return Err(ApplicationError::ResourceLimitExceeded(format!(
                "next text unit contains {item_chars} characters, exceeding max_chars {max_chars}"
            )));
        }
        if chars + *item_chars > max_chars {
            break;
        }
        chars += *item_chars;
        kept += 1;
    }
    Ok(kept)
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

fn reading_item(
    document: &Document,
    section: &Section,
    row: StoredStreamItem,
) -> TextUnitReadingItem {
    match row.unit {
        OwnedStreamUnit::Paragraph(unit) => {
            paragraph_item(document, section, &unit, row.content_class, row.degradation)
        }
        OwnedStreamUnit::Sentence(unit) => {
            sentence_item(document, section, &unit, row.content_class)
        }
    }
}

fn paragraph_item(
    document: &Document,
    section: &Section,
    paragraph: &TextUnit,
    paragraph_class: ParagraphContentClass,
    degradation: Option<String>,
) -> TextUnitReadingItem {
    let (content_class, detail) = content_class(paragraph_class);
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

#[derive(Debug)]
struct Page {
    items: Vec<TextUnitReadingItem>,
    start_index: usize,
    end_index: usize,
    next_index: usize,
    complete: bool,
}
