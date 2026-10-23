//! Shared resolution of v2 short handles into the v1 canonical objects.
//!
//! `outline`, `search`, `cite` and `source_view` all start from a handle and need the same three
//! steps: the document behind its `doc_key`, the section behind its `section_key`, and the v1
//! `TextLocator` of a paragraph or sentence. Keeping them in one place means a handle fails the
//! same way (`unknown_handle`, `stale_handle`) in every tool.
//!
//! `outline`, `search` and `cite` read the header and one section row at a time
//! ([`resolve_header`], [`resolve_section`]). `source_view` resolves its handle itself and keeps the
//! light document, because its page binding renders the section tree.

use std::sync::Arc;

use crate::application::ports::{
    DocumentHeader, DocumentRepository, OwnedStreamUnit, SectionRow, TextUnitIndex,
};
use crate::application::reading_error::ReadingError;
use crate::application::section_view::{DocumentView, carrier_section};
use crate::application::short_handle::{
    DocumentHandle, ReadingHandle, SectionHandle, handle_checksum,
};
use crate::domain::{
    Document, DocumentId, NormalizedDocumentHash, Section, SentenceTextUnit, TextLocator, TextUnit,
};

/// One document resolved from a document handle as its header plus point queries: what a reader
/// that needs one section at a time pays, instead of the whole tree.
pub(crate) struct ResolvedHeader {
    pub view: DocumentView,
    pub normalized_document_hash: NormalizedDocumentHash,
    pub handle: DocumentHandle,
}

impl ResolvedHeader {
    pub(crate) fn id(&self) -> &DocumentId {
        self.view.id()
    }

    pub(crate) fn header(&self) -> &DocumentHeader {
        self.view.header()
    }

    /// The identity-only `Document` the locator constructors take.
    pub(crate) fn document(&self) -> Document {
        self.view.document()
    }
}

/// Resolves a document handle to its header, verifying its identity checksum.
pub(crate) async fn resolve_header(
    index: &dyn TextUnitIndex,
    repository: &Arc<dyn DocumentRepository>,
    handle: &DocumentHandle,
) -> Result<ResolvedHeader, ReadingError> {
    let document_id = document_id_for(index, handle).await?;
    let (view, normalized_document_hash) =
        DocumentView::load_with_hash(Arc::clone(repository), &document_id)
            .await?
            .ok_or_else(ReadingError::not_opened)?;
    if handle_checksum(view.id(), &normalized_document_hash) != handle.checksum {
        return Err(ReadingError::stale_handle(format!(
            "handle checksum {} does not match the current document identity",
            handle.checksum
        )));
    }
    Ok(ResolvedHeader {
        view,
        normalized_document_hash,
        handle: handle.clone(),
    })
}

/// The document a `doc_key` names, or the `unknown_handle` every tool reports for a key this server
/// does not store.
async fn document_id_for(
    index: &dyn TextUnitIndex,
    handle: &DocumentHandle,
) -> Result<DocumentId, ReadingError> {
    index
        .document_id_for_key(handle.doc_key)
        .await?
        .ok_or_else(|| {
            ReadingError::unknown_handle(format!(
                "doc_key {} is not stored in this server",
                handle.doc_key
            ))
        })
}

/// Resolves a section handle to its row, inside an already resolved header.
pub(crate) async fn resolve_section(
    index: &dyn TextUnitIndex,
    resolved: &ResolvedHeader,
    handle: &SectionHandle,
) -> Result<SectionRow, ReadingError> {
    let section_id = index
        .section_id_for_key(resolved.id(), handle.section_key)
        .await?
        .ok_or_else(|| {
            ReadingError::unknown_handle(format!(
                "section key {} is not stored for this document",
                handle.section_key
            ))
        })?;
    resolved
        .view
        .section_by_id(&section_id)
        .await?
        .ok_or_else(|| {
            ReadingError::unknown_handle(format!(
                "section {} is not part of this document",
                handle.section_key
            ))
        })
}

/// The v1 `TextLocator` of a handle, or `None` for a document handle.
///
/// Paragraph and sentence locators carry the stored unit's normalized range and segmentation
/// version, so they are byte-identical to the locator the v1 enumeration returns for that unit.
pub(crate) async fn resolve_locator(
    index: &dyn TextUnitIndex,
    resolved: &ResolvedHeader,
    handle: &ReadingHandle,
) -> Result<Option<TextLocator>, ReadingError> {
    match handle {
        ReadingHandle::Document(_) => Ok(None),
        ReadingHandle::Section(section) => {
            let row = resolve_section(index, resolved, section).await?;
            Ok(Some(TextLocator::for_section_with_hash(
                &resolved.document(),
                &carrier_section(&row),
                &resolved.normalized_document_hash,
            )))
        }
        ReadingHandle::Paragraph {
            section,
            paragraph_index,
        } => {
            let row = resolve_section(index, resolved, section).await?;
            let unit = index
                .section_unit(
                    resolved.id(),
                    &row.id,
                    crate::application::get_text_units::EffectiveTextUnitKind::Paragraph,
                    *paragraph_index,
                    None,
                )
                .await?
                .ok_or_else(|| {
                    ReadingError::unknown_handle(format!(
                        "paragraph {paragraph_index} of section {} is not readable",
                        row.id.0
                    ))
                })?;
            let OwnedStreamUnit::Paragraph(unit) = unit else {
                return Err(ReadingError::unknown_handle(format!(
                    "paragraph {paragraph_index} of section {} is not stored as a paragraph",
                    row.id.0
                )));
            };
            Ok(Some(paragraph_locator(
                &resolved.document(),
                &carrier_section(&row),
                &unit,
            )))
        }
        ReadingHandle::Sentence {
            section,
            paragraph_index,
            sentence_index,
        } => {
            let row = resolve_section(index, resolved, section).await?;
            let unit = index
                .section_unit(
                    resolved.id(),
                    &row.id,
                    crate::application::get_text_units::EffectiveTextUnitKind::Sentence,
                    *paragraph_index,
                    Some(*sentence_index),
                )
                .await?
                .ok_or_else(|| {
                    ReadingError::unknown_handle(format!(
                        "sentence {paragraph_index}.{sentence_index} of section {} is not readable",
                        row.id.0
                    ))
                })?;
            let OwnedStreamUnit::Sentence(unit) = unit else {
                return Err(ReadingError::unknown_handle(format!(
                    "sentence {paragraph_index}.{sentence_index} of section {} is not stored as a sentence",
                    row.id.0
                )));
            };
            Ok(Some(sentence_locator(
                &resolved.document(),
                &carrier_section(&row),
                &unit,
            )))
        }
    }
}

fn paragraph_locator(document: &Document, section: &Section, unit: &TextUnit) -> TextLocator {
    TextLocator::for_paragraph(document, section, unit)
}

fn sentence_locator(
    document: &Document,
    section: &Section,
    unit: &SentenceTextUnit,
) -> TextLocator {
    TextLocator::for_sentence(document, section, unit)
}
