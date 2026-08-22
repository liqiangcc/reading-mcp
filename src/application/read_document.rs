use std::sync::Arc;

use crate::application::ports::{ApplicationError, DocumentRepository};
use crate::application::read_cursor::{ReadCursorClaims, decode_read_cursor, encode_read_cursor};
use crate::application::reading_support::{
    SECTION_TREE_READ_MODE, SECTION_TREE_RENDERING_VERSION, SECTION_TREE_STREAM_COORDINATE_SPACE,
    content_response_limit, render_section_tree, slice_rendered_stream,
};
use crate::domain::{
    Document, DocumentId, DocumentSource, Location, NormalizedDocumentHash, NormalizedTextRange,
    Section, SectionId, TEXT_SEGMENTATION_VERSION, TextLocator,
};

pub const EXACT_TARGET_READ_MODE: &str = "exact_target";
pub const EXACT_TARGET_RENDERING_VERSION: &str = "exact-normalized-source/v1";
pub const EXACT_TARGET_STREAM_COORDINATE_SPACE: &str = "exact-target-unicode-scalar/v1";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReadSectionCommand {
    pub document_id: DocumentId,
    pub section_id: SectionId,
    pub max_chars: Option<usize>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContinueReadCommand {
    pub document_id: DocumentId,
    pub section_id: SectionId,
    pub cursor: String,
    pub max_chars: Option<usize>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReadExactTargetCommand {
    pub document_id: DocumentId,
    pub target_locator: TextLocator,
    pub max_chars: Option<usize>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContinueExactReadCommand {
    pub document_id: DocumentId,
    pub target_locator: TextLocator,
    pub cursor: String,
    pub max_chars: Option<usize>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReadStreamSegment {
    pub read_mode: String,
    pub rendering_version: String,
    pub coordinate_space: String,
    pub start_char: usize,
    pub end_char: usize,
    pub total_chars: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReadSectionResult {
    pub document_id: DocumentId,
    pub source: DocumentSource,
    pub section_id: SectionId,
    pub content: String,
    pub location: Location,
    pub truncated: bool,
    pub complete: bool,
    pub next_cursor: Option<String>,
    pub stream: ReadStreamSegment,
    pub resolved_target_locator: TextLocator,
    pub returned_locator: Option<TextLocator>,
}

pub struct ReadDocumentUseCase {
    repository: Arc<dyn DocumentRepository>,
}

impl ReadDocumentUseCase {
    pub fn new(repository: Arc<dyn DocumentRepository>) -> Self {
        Self { repository }
    }

    pub async fn execute(
        &self,
        command: ReadSectionCommand,
    ) -> Result<ReadSectionResult, ApplicationError> {
        let document = self.load_document(&command.document_id).await?;
        let section = document
            .find_section(&command.section_id)
            .ok_or(ApplicationError::SectionNotFound)?;
        let normalized_hash = document.normalized_document_hash();

        read_section_segment(&document, section, normalized_hash, 0, command.max_chars)
    }

    pub async fn continue_read(
        &self,
        command: ContinueReadCommand,
    ) -> Result<ReadSectionResult, ApplicationError> {
        validate_continuation_budget(command.max_chars)?;
        let claims = decode_read_cursor(&command.cursor)?;
        validate_cursor_target(&claims, &command.document_id, &command.section_id)?;
        validate_section_tree_cursor_contract(&claims)?;

        let document = self.load_document(&command.document_id).await?;
        validate_cursor_document_identity(&claims, &document)?;

        let normalized_hash = document.normalized_document_hash();
        let section = document
            .find_section(&command.section_id)
            .ok_or(ApplicationError::SectionNotFound)?;
        let rendered = render_section_tree(section);
        let total_chars = rendered.chars().count();
        validate_resumable_position(claims.next_char, total_chars)?;

        read_rendered_section_segment(
            &document,
            section,
            normalized_hash,
            rendered,
            claims.next_char,
            command.max_chars,
        )
    }

    pub async fn read_exact(
        &self,
        command: ReadExactTargetCommand,
    ) -> Result<ReadSectionResult, ApplicationError> {
        let document = self.load_document(&command.document_id).await?;
        let target = resolve_exact_target(&document, &command.target_locator)?;
        read_exact_segment(&document, target, 0, command.max_chars)
    }

    pub async fn continue_exact(
        &self,
        command: ContinueExactReadCommand,
    ) -> Result<ReadSectionResult, ApplicationError> {
        validate_continuation_budget(command.max_chars)?;
        let claims = decode_read_cursor(&command.cursor)?;
        validate_cursor_target(
            &claims,
            &command.document_id,
            &command.target_locator.owner_section_id,
        )?;
        validate_exact_cursor_contract(&claims)?;

        let document = self.load_document(&command.document_id).await?;
        validate_cursor_document_identity(&claims, &document)?;
        let target = resolve_exact_target(&document, &command.target_locator)?;
        validate_exact_cursor_binding(&claims, &target)?;

        let target_chars = target.range.len();
        validate_resumable_position(claims.next_char, target_chars)?;
        read_exact_segment(&document, target, claims.next_char, command.max_chars)
    }

    async fn load_document(&self, id: &DocumentId) -> Result<Document, ApplicationError> {
        self.repository
            .get(id)
            .await?
            .ok_or(ApplicationError::DocumentNotFound)
    }
}

fn read_section_segment(
    document: &Document,
    section: &Section,
    normalized_hash: NormalizedDocumentHash,
    start_char: usize,
    max_chars: Option<usize>,
) -> Result<ReadSectionResult, ApplicationError> {
    read_rendered_section_segment(
        document,
        section,
        normalized_hash,
        render_section_tree(section),
        start_char,
        max_chars,
    )
}

fn read_rendered_section_segment(
    document: &Document,
    section: &Section,
    normalized_hash: NormalizedDocumentHash,
    rendered: String,
    start_char: usize,
    max_chars: Option<usize>,
) -> Result<ReadSectionResult, ApplicationError> {
    let slice = slice_rendered_stream(&rendered, start_char, content_response_limit(max_chars));
    let next_cursor = if slice.complete {
        None
    } else {
        Some(encode_read_cursor(ReadCursorClaims::new(
            document.id.0.clone(),
            document.content_hash.0.clone(),
            normalized_hash.0,
            section.id.0.clone(),
            SECTION_TREE_READ_MODE,
            SECTION_TREE_RENDERING_VERSION,
            slice.end_char,
        ))?)
    };

    Ok(ReadSectionResult {
        document_id: document.id.clone(),
        source: document.source.clone(),
        section_id: section.id.clone(),
        content: slice.content,
        location: section.location.clone(),
        truncated: !slice.complete,
        complete: slice.complete,
        next_cursor,
        stream: ReadStreamSegment {
            read_mode: SECTION_TREE_READ_MODE.into(),
            rendering_version: SECTION_TREE_RENDERING_VERSION.into(),
            coordinate_space: SECTION_TREE_STREAM_COORDINATE_SPACE.into(),
            start_char: slice.start_char,
            end_char: slice.end_char,
            total_chars: slice.total_chars,
        },
        resolved_target_locator: TextLocator::for_section(document, section),
        returned_locator: None,
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ExactTargetKind {
    Section,
    CharacterRange,
    Paragraph,
    Sentence,
}

impl ExactTargetKind {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Section => "section",
            Self::CharacterRange => "character_range",
            Self::Paragraph => "paragraph",
            Self::Sentence => "sentence",
        }
    }
}

#[derive(Clone, Debug)]
struct ResolvedExactTarget {
    locator: TextLocator,
    kind: ExactTargetKind,
    range: NormalizedTextRange,
}

fn resolve_exact_target(
    document: &Document,
    locator: &TextLocator,
) -> Result<ResolvedExactTarget, ApplicationError> {
    if locator.document_id != document.id {
        return Err(ApplicationError::InvalidLocator(format!(
            "locator document {} does not match requested document {}",
            locator.document_id.0, document.id.0
        )));
    }
    if locator.content_hash != document.content_hash {
        return Err(ApplicationError::StaleLocator(format!(
            "raw content hash changed from {} to {}",
            locator.content_hash.0, document.content_hash.0
        )));
    }
    let normalized_hash = document.normalized_document_hash();
    if locator.normalized_document_hash != normalized_hash {
        return Err(ApplicationError::StaleLocator(format!(
            "normalized document hash changed from {} to {}",
            locator.normalized_document_hash.0, normalized_hash.0
        )));
    }

    let section = document
        .find_section(&locator.owner_section_id)
        .ok_or_else(|| {
            ApplicationError::InvalidLocator(format!(
                "locator owner section {} does not exist",
                locator.owner_section_id.0
            ))
        })?;

    match (
        locator.paragraph_index,
        locator.sentence_index,
        locator.normalized_range,
        locator.segmentation_version.as_deref(),
    ) {
        (None, None, None, None) => {
            let range = NormalizedTextRange::new(0, section.normalized_text_len())
                .expect("zero-to-length normalized range is valid");
            Ok(ResolvedExactTarget {
                locator: TextLocator::for_section(document, section),
                kind: ExactTargetKind::Section,
                range,
            })
        }
        (None, None, Some(range), None) => {
            section.validate_normalized_range(range).map_err(|error| {
                ApplicationError::InvalidLocator(format!("invalid character range: {error}"))
            })?;
            Ok(ResolvedExactTarget {
                locator: TextLocator::for_character_range(document, section, range),
                kind: ExactTargetKind::CharacterRange,
                range,
            })
        }
        (Some(paragraph_index), None, Some(range), Some(segmentation_version)) => {
            validate_segmentation_version(segmentation_version)?;
            if paragraph_index == 0 {
                return Err(ApplicationError::InvalidLocator(
                    "paragraph_index must be 1-based".into(),
                ));
            }
            let paragraph = document
                .paragraph_text_units()
                .units
                .into_iter()
                .find(|unit| {
                    unit.owner_section_id == locator.owner_section_id
                        && unit.paragraph_index == paragraph_index
                })
                .ok_or_else(|| {
                    ApplicationError::StaleLocator(format!(
                        "paragraph {paragraph_index} no longer exists in section {}",
                        locator.owner_section_id.0
                    ))
                })?;
            if paragraph.normalized_range != range {
                return Err(ApplicationError::StaleLocator(format!(
                    "paragraph {paragraph_index} normalized range changed"
                )));
            }
            Ok(ResolvedExactTarget {
                locator: TextLocator::for_paragraph(document, section, &paragraph),
                kind: ExactTargetKind::Paragraph,
                range,
            })
        }
        (Some(paragraph_index), Some(sentence_index), Some(range), Some(segmentation_version)) => {
            validate_segmentation_version(segmentation_version)?;
            if paragraph_index == 0 || sentence_index == 0 {
                return Err(ApplicationError::InvalidLocator(
                    "paragraph_index and sentence_index must be 1-based".into(),
                ));
            }
            let sentence = document
                .sentence_text_units()
                .units
                .into_iter()
                .find(|unit| {
                    unit.owner_section_id == locator.owner_section_id
                        && unit.paragraph_index == paragraph_index
                        && unit.sentence_index == sentence_index
                })
                .ok_or_else(|| {
                    ApplicationError::StaleLocator(format!(
                        "sentence {paragraph_index}.{sentence_index} no longer exists in section {}",
                        locator.owner_section_id.0
                    ))
                })?;
            if sentence.normalized_range != range {
                return Err(ApplicationError::StaleLocator(format!(
                    "sentence {paragraph_index}.{sentence_index} normalized range changed"
                )));
            }
            Ok(ResolvedExactTarget {
                locator: TextLocator::for_sentence(document, section, &sentence),
                kind: ExactTargetKind::Sentence,
                range,
            })
        }
        _ => Err(ApplicationError::InvalidLocator(
            "locator Section/CharacterRange/Paragraph/Sentence fields form an invalid shape".into(),
        )),
    }
}

fn read_exact_segment(
    document: &Document,
    target: ResolvedExactTarget,
    start_char: usize,
    max_chars: Option<usize>,
) -> Result<ReadSectionResult, ApplicationError> {
    let section = document
        .find_section(&target.locator.owner_section_id)
        .ok_or(ApplicationError::SectionNotFound)?;
    let target_text = section
        .normalized_text_slice(target.range)
        .map_err(|error| {
            ApplicationError::InvalidLocator(format!("resolved target range is invalid: {error}"))
        })?;
    let slice = slice_rendered_stream(target_text, start_char, content_response_limit(max_chars));
    let next_cursor = if slice.complete {
        None
    } else {
        Some(encode_read_cursor(exact_cursor_claims(
            document,
            &target,
            slice.end_char,
        ))?)
    };
    let returned_range = NormalizedTextRange::new(
        target.range.start() + slice.start_char,
        target.range.start() + slice.end_char,
    )
    .expect("segment offsets preserve normalized range ordering");

    Ok(ReadSectionResult {
        document_id: document.id.clone(),
        source: document.source.clone(),
        section_id: section.id.clone(),
        content: slice.content,
        location: section.location.clone(),
        truncated: !slice.complete,
        complete: slice.complete,
        next_cursor,
        stream: ReadStreamSegment {
            read_mode: EXACT_TARGET_READ_MODE.into(),
            rendering_version: EXACT_TARGET_RENDERING_VERSION.into(),
            coordinate_space: EXACT_TARGET_STREAM_COORDINATE_SPACE.into(),
            start_char: slice.start_char,
            end_char: slice.end_char,
            total_chars: slice.total_chars,
        },
        resolved_target_locator: target.locator,
        returned_locator: Some(TextLocator::for_character_range(
            document,
            section,
            returned_range,
        )),
    })
}

fn exact_cursor_claims(
    document: &Document,
    target: &ResolvedExactTarget,
    next_char: usize,
) -> ReadCursorClaims {
    ReadCursorClaims::new_exact(
        document.id.0.clone(),
        document.content_hash.0.clone(),
        document.normalized_document_hash().0,
        target.locator.owner_section_id.0.clone(),
        EXACT_TARGET_READ_MODE,
        EXACT_TARGET_RENDERING_VERSION,
        next_char,
        target.kind.as_str(),
        target.locator.paragraph_index,
        target.locator.sentence_index,
        target
            .locator
            .normalized_range
            .map(NormalizedTextRange::start),
        target
            .locator
            .normalized_range
            .map(NormalizedTextRange::end),
        target.locator.segmentation_version.clone(),
    )
}

fn validate_exact_cursor_binding(
    claims: &ReadCursorClaims,
    target: &ResolvedExactTarget,
) -> Result<(), ApplicationError> {
    let expected_start = target
        .locator
        .normalized_range
        .map(NormalizedTextRange::start);
    let expected_end = target
        .locator
        .normalized_range
        .map(NormalizedTextRange::end);
    if claims.target_kind.as_deref() != Some(target.kind.as_str())
        || claims.target_paragraph_index != target.locator.paragraph_index
        || claims.target_sentence_index != target.locator.sentence_index
        || claims.target_range_start != expected_start
        || claims.target_range_end != expected_end
        || claims.target_segmentation_version != target.locator.segmentation_version
    {
        return Err(ApplicationError::CursorTargetMismatch(
            "read cursor exact target does not match requested TextLocator".into(),
        ));
    }
    Ok(())
}

fn validate_segmentation_version(version: &str) -> Result<(), ApplicationError> {
    if version != TEXT_SEGMENTATION_VERSION {
        return Err(ApplicationError::StaleLocator(format!(
            "locator segmentation version {version} is incompatible with {TEXT_SEGMENTATION_VERSION}"
        )));
    }
    Ok(())
}

fn validate_continuation_budget(max_chars: Option<usize>) -> Result<(), ApplicationError> {
    if max_chars == Some(0) {
        return Err(ApplicationError::InvalidRequest(
            "continuation max_chars must be greater than zero".into(),
        ));
    }
    Ok(())
}

fn validate_cursor_target(
    claims: &ReadCursorClaims,
    document_id: &DocumentId,
    section_id: &SectionId,
) -> Result<(), ApplicationError> {
    if claims.document_id != document_id.0 {
        return Err(ApplicationError::CursorTargetMismatch(format!(
            "cursor document {} does not match requested document {}",
            claims.document_id, document_id.0
        )));
    }
    if claims.section_id != section_id.0 {
        return Err(ApplicationError::CursorTargetMismatch(format!(
            "cursor section {} does not match requested section {}",
            claims.section_id, section_id.0
        )));
    }
    Ok(())
}

fn validate_cursor_document_identity(
    claims: &ReadCursorClaims,
    document: &Document,
) -> Result<(), ApplicationError> {
    if document.content_hash.0 != claims.content_hash {
        return Err(ApplicationError::StaleCursor(format!(
            "raw content hash changed from {} to {}",
            claims.content_hash, document.content_hash.0
        )));
    }
    let normalized_hash = document.normalized_document_hash();
    if normalized_hash.as_ref() != claims.normalized_document_hash.as_str() {
        return Err(ApplicationError::StaleCursor(format!(
            "normalized document hash changed from {} to {normalized_hash}",
            claims.normalized_document_hash
        )));
    }
    Ok(())
}

fn validate_section_tree_cursor_contract(
    claims: &ReadCursorClaims,
) -> Result<(), ApplicationError> {
    if claims.read_mode != SECTION_TREE_READ_MODE {
        return Err(ApplicationError::StaleCursor(format!(
            "cursor read mode {} is incompatible with {SECTION_TREE_READ_MODE}",
            claims.read_mode
        )));
    }
    if claims.rendering_version != SECTION_TREE_RENDERING_VERSION {
        return Err(ApplicationError::StaleCursor(format!(
            "cursor rendering version {} is incompatible with {SECTION_TREE_RENDERING_VERSION}",
            claims.rendering_version
        )));
    }
    if claims.target_kind.is_some()
        || claims.target_paragraph_index.is_some()
        || claims.target_sentence_index.is_some()
        || claims.target_range_start.is_some()
        || claims.target_range_end.is_some()
        || claims.target_segmentation_version.is_some()
    {
        return Err(ApplicationError::InvalidCursor(
            "section-tree cursor contains exact-target bindings".into(),
        ));
    }
    Ok(())
}

fn validate_exact_cursor_contract(claims: &ReadCursorClaims) -> Result<(), ApplicationError> {
    if claims.read_mode != EXACT_TARGET_READ_MODE {
        return Err(ApplicationError::StaleCursor(format!(
            "cursor read mode {} is incompatible with {EXACT_TARGET_READ_MODE}",
            claims.read_mode
        )));
    }
    if claims.rendering_version != EXACT_TARGET_RENDERING_VERSION {
        return Err(ApplicationError::StaleCursor(format!(
            "cursor rendering version {} is incompatible with {EXACT_TARGET_RENDERING_VERSION}",
            claims.rendering_version
        )));
    }
    if claims.target_kind.is_none() {
        return Err(ApplicationError::InvalidCursor(
            "exact-target cursor is missing target bindings".into(),
        ));
    }
    Ok(())
}

fn validate_resumable_position(
    next_char: usize,
    total_chars: usize,
) -> Result<(), ApplicationError> {
    if next_char >= total_chars {
        return Err(ApplicationError::InvalidCursor(format!(
            "next stream position {next_char} is outside the resumable range 0..{total_chars}"
        )));
    }
    Ok(())
}
