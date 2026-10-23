//! The v2 `cite` tool: the v1 canonical locator and document identity of one or more handles.
//!
//! `cite` is the only v2 tool that returns long fields on purpose: a caller asking for a citation
//! wants the exact locator, the original source position and the identity it is bound to. Every
//! other tool stays compact and points here when a citation is needed.

use std::sync::Arc;

use crate::application::handle_reading::{resolve_header, resolve_locator};
use crate::application::ports::{DocumentRepository, TextUnitIndex};
use crate::application::reading_error::ReadingError;
use crate::application::section_view::carrier_section;
use crate::application::short_handle::parse_handle;
use crate::domain::{
    NORMALIZATION_VERSION, NORMALIZED_DOCUMENT_HASH_VERSION, TEXT_SEGMENTATION_VERSION, TextLocator,
};

/// One cited handle.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CiteReadingItem {
    pub at: String,
    /// The full v1 locator, absent only for a document handle.
    pub locator: Option<TextLocator>,
    /// The original source position the section carries (`epub:<file>#<anchor>` or `pdf:page:<n>`).
    pub source_position: Option<String>,
    /// The PDF page number, when the source position names one.
    pub page: Option<u32>,
    pub document_id: String,
    pub content_hash: String,
    pub normalized_document_hash: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CiteReadingResult {
    pub items: Vec<CiteReadingItem>,
}

pub struct CiteReadingUseCase {
    repository: Arc<dyn DocumentRepository>,
    text_unit_index: Arc<dyn TextUnitIndex>,
}

impl CiteReadingUseCase {
    pub fn new(
        repository: Arc<dyn DocumentRepository>,
        text_unit_index: Arc<dyn TextUnitIndex>,
    ) -> Self {
        Self {
            repository,
            text_unit_index,
        }
    }

    pub async fn execute(&self, handles: &[String]) -> Result<CiteReadingResult, ReadingError> {
        if handles.is_empty() {
            return Err(ReadingError::invalid(
                "at",
                "at must name at least one handle",
            ));
        }
        let mut items = Vec::with_capacity(handles.len());
        for at in handles {
            let handle = parse_handle(at).map_err(|error| {
                ReadingError::invalid("at", format!("at must be a handle: {}", error.message))
            })?;
            let resolved = resolve_header(
                self.text_unit_index.as_ref(),
                &self.repository,
                handle.document(),
            )
            .await?;
            let locator =
                resolve_locator(self.text_unit_index.as_ref(), &resolved, &handle).await?;
            let source_position = locator
                .as_ref()
                .and_then(|locator| locator.native_location.clone());
            let page = source_position.as_deref().and_then(page_of);
            items.push(CiteReadingItem {
                at: at.clone(),
                locator,
                source_position,
                page,
                document_id: resolved.id().0.clone(),
                content_hash: resolved.header().content_hash.0.clone(),
                normalized_document_hash: resolved.normalized_document_hash.0.clone(),
            });
        }
        Ok(CiteReadingResult { items })
    }

    /// The v1 locator `source_view` renders for a handle.
    ///
    /// A document handle has no locator of its own, so it renders the first section in body order.
    pub async fn source_locator(&self, at: &str) -> Result<TextLocator, ReadingError> {
        let handle = parse_handle(at).map_err(|error| {
            ReadingError::invalid("at", format!("at must be a handle: {}", error.message))
        })?;
        let resolved = resolve_header(
            self.text_unit_index.as_ref(),
            &self.repository,
            handle.document(),
        )
        .await?;
        if !resolved.header().media_type.is_pdf() {
            return Err(ReadingError::invalid(
                "at",
                "source view renders an original page, which only a PDF source has",
            ));
        }
        if let Some(locator) =
            resolve_locator(self.text_unit_index.as_ref(), &resolved, &handle).await?
        {
            return Ok(locator);
        }
        // The first section in body order is body-order position 0: one point query, not the
        // whole order.
        let Some(section_key) = self.repository.body_order_at(resolved.id(), 0).await? else {
            return Err(ReadingError::unknown_handle("the document has no sections"));
        };
        let row = resolved
            .view
            .section_by_key(section_key)
            .await?
            .ok_or_else(|| ReadingError::unknown_handle("the first section disappeared"))?;
        Ok(TextLocator::for_section_with_hash(
            &resolved.document(),
            &carrier_section(&row),
            &resolved.normalized_document_hash,
        ))
    }
}

/// The versions a citation is bound to.
pub const CITE_VERSIONS: (&str, &str, &str) = (
    NORMALIZATION_VERSION,
    NORMALIZED_DOCUMENT_HASH_VERSION,
    TEXT_SEGMENTATION_VERSION,
);

/// The page a `pdf:page:<n>` or `pdf:pages:<a>-<b>` native location names.
fn page_of(native_location: &str) -> Option<u32> {
    if let Some(page) = native_location.strip_prefix("pdf:page:") {
        return page.parse().ok();
    }
    if let Some(range) = native_location.strip_prefix("pdf:pages:") {
        return range.split('-').next().and_then(|start| start.parse().ok());
    }
    None
}
