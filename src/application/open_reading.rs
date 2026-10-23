//! The v2 `open` tool: the v1 open use case plus a short document handle and a book overview.

use std::sync::Arc;

use crate::application::open_document::{OpenDocumentCommand, OpenDocumentUseCase};
use crate::application::ports::{DocumentRepository, RetrievalOptions, TextUnitIndex};
use crate::application::reading_error::{ReadingError, ReadingErrorCode};
use crate::application::section_view::DocumentView;
use crate::application::short_handle::{DocumentHandle, SectionHandle, handle_checksum};
use crate::domain::{DocumentSource, MediaType};

/// How many root sections `open` lists before `more_top`.
pub const MAX_TOP_SECTIONS: usize = 10;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenReadingCommand {
    pub source: DocumentSource,
    /// Maps to the v1 `force_refresh`: re-ingest even when the bytes are already cached.
    pub refresh: bool,
}

/// The compact `open` answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenReadingResult {
    pub handle: String,
    pub title: Option<String>,
    pub format: &'static str,
    pub sections: usize,
    pub chars: usize,
    pub top: Vec<(String, String)>,
    pub more_top: usize,
}

/// The document format `open` reports, from the media type.
pub fn document_format(media_type: &MediaType) -> &'static str {
    match media_type.essence().to_ascii_lowercase().as_str() {
        "application/epub+zip" => "epub",
        "application/pdf" => "pdf",
        "text/html" | "application/xhtml+xml" => "html",
        "text/markdown" | "text/x-markdown" => "markdown",
        _ => "text",
    }
}

pub struct OpenReadingUseCase {
    open_document: Arc<OpenDocumentUseCase>,
    repository: Arc<dyn DocumentRepository>,
    text_unit_index: Arc<dyn TextUnitIndex>,
}

impl OpenReadingUseCase {
    pub fn new(
        open_document: Arc<OpenDocumentUseCase>,
        repository: Arc<dyn DocumentRepository>,
        text_unit_index: Arc<dyn TextUnitIndex>,
    ) -> Self {
        Self {
            open_document,
            repository,
            text_unit_index,
        }
    }

    pub async fn execute(
        &self,
        command: OpenReadingCommand,
    ) -> Result<OpenReadingResult, ReadingError> {
        let opened = self
            .open_document
            .execute(OpenDocumentCommand {
                source: command.source,
                options: RetrievalOptions {
                    auth_profile: None,
                    force_refresh: command.refresh,
                },
            })
            .await?;

        let document_key = self
            .text_unit_index
            .document_key(&opened.document_id)
            .await?
            .ok_or_else(|| {
                ReadingError::new(
                    ReadingErrorCode::NotOpened,
                    format!(
                        "document {} was opened but is not stored in the text unit index",
                        opened.document_id.0
                    ),
                )
            })?;
        let handle = DocumentHandle {
            doc_key: document_key,
            checksum: handle_checksum(&opened.document_id, &opened.normalized_document_hash),
        };

        // The header and the root rows: `open` reads no section text and no tree.
        let view = DocumentView::load(Arc::clone(&self.repository), &opened.document_id)
            .await?
            .ok_or_else(ReadingError::not_opened)?;
        let top = self.top_sections(&view, &handle).await?;
        let more_top = view.header().root_count.saturating_sub(top.len());

        Ok(OpenReadingResult {
            handle: handle.to_string(),
            title: (!opened.title.is_empty()).then_some(opened.title),
            format: document_format(&opened.media_type),
            sections: opened.section_count,
            chars: opened.reading_profile.canonical_text_coverage.owner_chars,
            top,
            more_top,
        })
    }

    /// The root section handles and titles, from the first `MAX_TOP_SECTIONS` root rows.
    async fn top_sections(
        &self,
        view: &DocumentView,
        handle: &DocumentHandle,
    ) -> Result<Vec<(String, String)>, ReadingError> {
        let mut top = Vec::new();
        for row in view.children(None, MAX_TOP_SECTIONS).await? {
            let Some(section_key) = self.text_unit_index.section_key(view.id(), &row.id).await?
            else {
                return Err(ReadingError::new(
                    ReadingErrorCode::NotOpened,
                    format!(
                        "section {} of document {} has no stored section key",
                        row.id.0,
                        view.id().0
                    ),
                ));
            };
            let section_handle = SectionHandle {
                document: handle.clone(),
                section_key,
            };
            top.push((section_handle.to_string(), row.title));
        }
        Ok(top)
    }
}
