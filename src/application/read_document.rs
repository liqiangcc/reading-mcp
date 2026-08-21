use std::sync::Arc;

use crate::application::ports::{ApplicationError, DocumentRepository};
use crate::application::read_cursor::{ReadCursorClaims, decode_read_cursor, encode_read_cursor};
use crate::application::reading_support::{
    SECTION_TREE_READ_MODE, SECTION_TREE_RENDERING_VERSION, SECTION_TREE_STREAM_COORDINATE_SPACE,
    content_response_limit, render_section_tree, slice_rendered_stream,
};
use crate::domain::{
    Document, DocumentId, DocumentSource, Location, NormalizedDocumentHash, Section, SectionId,
};

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

        read_segment(&document, section, normalized_hash, 0, command.max_chars)
    }

    pub async fn continue_read(
        &self,
        command: ContinueReadCommand,
    ) -> Result<ReadSectionResult, ApplicationError> {
        validate_continuation_budget(command.max_chars)?;
        let claims = decode_read_cursor(&command.cursor)?;
        validate_cursor_target(&claims, &command.document_id, &command.section_id)?;
        validate_cursor_stream_contract(&claims)?;

        let document = self.load_document(&command.document_id).await?;
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

        let section = document
            .find_section(&command.section_id)
            .ok_or(ApplicationError::SectionNotFound)?;
        let rendered = render_section_tree(section);
        let total_chars = rendered.chars().count();
        if claims.next_char >= total_chars {
            return Err(ApplicationError::InvalidCursor(format!(
                "next stream position {} is outside the resumable range 0..{total_chars}",
                claims.next_char
            )));
        }

        read_rendered_segment(
            &document,
            section,
            normalized_hash,
            rendered,
            claims.next_char,
            command.max_chars,
        )
    }

    async fn load_document(&self, id: &DocumentId) -> Result<Document, ApplicationError> {
        self.repository
            .get(id)
            .await?
            .ok_or(ApplicationError::DocumentNotFound)
    }
}

fn read_segment(
    document: &Document,
    section: &Section,
    normalized_hash: NormalizedDocumentHash,
    start_char: usize,
    max_chars: Option<usize>,
) -> Result<ReadSectionResult, ApplicationError> {
    read_rendered_segment(
        document,
        section,
        normalized_hash,
        render_section_tree(section),
        start_char,
        max_chars,
    )
}

fn read_rendered_segment(
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
    })
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

fn validate_cursor_stream_contract(claims: &ReadCursorClaims) -> Result<(), ApplicationError> {
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
    Ok(())
}
