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

use crate::application::handle_recovery::HandleRecovery;
use crate::application::ports::{
    DocumentHeader, DocumentRepository, HandleRemap, HandleRemapUnit, INGEST_IN_PROGRESS_FINGERPRINT,
    OwnedStreamUnit, SectionRow, TextUnitIndex,
};
use crate::application::reading_error::ReadingError;
use crate::application::section_view::{DocumentView, carrier_section};
use crate::application::short_handle::{
    CursorUnit, DocumentHandle, ReadingCursor, ReadingHandle, SectionHandle, handle_checksum,
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

/// Resolves a handle's document to its header, verifying its identity checksum. When the server
/// carries a lazy recovery object, a row written by another ingest is re-ingested here before the
/// checksum is judged.
pub(crate) async fn resolve_header(
    index: &dyn TextUnitIndex,
    repository: &Arc<dyn DocumentRepository>,
    handle: &ReadingHandle,
    recovery: Option<&HandleRecovery>,
) -> Result<ResolvedHeader, ReadingError> {
    let document_handle = handle.document();
    // Read the current rows first. A fresh handle must keep exactly its old hot path: one key
    // lookup plus the document read, never a lazy parse merely because a deploy changed the
    // ingest fingerprint. Only a checksum mismatch enters the recovery path.
    let mut row = index.document_key_row(document_handle.doc_key).await?;
    let Some(initial_row) = row else {
        return Err(ReadingError::unknown_handle(format!(
            "doc_key {} is not stored in this server",
            document_handle.doc_key
        )));
    };
    let mut fingerprint = initial_row.ingest_fingerprint.clone();
    let mut document_id = initial_row.document_id;
    let (mut view, mut normalized_document_hash) =
        DocumentView::load_with_hash(Arc::clone(repository), &document_id)
            .await?
            .ok_or_else(ReadingError::not_opened)?;
    let mut current_checksum = handle_checksum(
        view.id(),
        &normalized_document_hash,
        &view.header().media_type,
    );
    if recovery.is_some_and(|recovery| !recovery.accepts(initial_row.ingest_fingerprint.as_deref()))
    {
        let recovery = recovery.expect("checked above");
        row = recovery.current_row(document_handle.doc_key).await?;
        let Some(recovered) = row else {
            return Err(ReadingError::unknown_handle(format!(
                "doc_key {} is not stored in this server",
                document_handle.doc_key
            )));
        };
        fingerprint = recovered.ingest_fingerprint.clone();
        document_id = recovered.document_id;
        (view, normalized_document_hash) =
            DocumentView::load_with_hash(Arc::clone(repository), &document_id)
                .await?
                .ok_or_else(ReadingError::not_opened)?;
        current_checksum = handle_checksum(
            view.id(),
            &normalized_document_hash,
            &view.header().media_type,
        );
    }
    // A document whose section rows are mid-replacement (or were left so by a crash) is not
    // readable, exactly as it would be had it never been ingested: `open` re-ingests it, and its
    // rows are never read half-written.
    if fingerprint.as_deref() == Some(INGEST_IN_PROGRESS_FINGERPRINT) {
        return Err(ReadingError::not_opened());
    }
    if current_checksum != document_handle.checksum {
        let detail = format!(
            "handle checksum {} does not match the current document identity",
            document_handle.checksum
        );
        let mut error = ReadingError::stale_handle(detail);
        if let Some(retry) =
            remap_reading_handle(index, document_handle.doc_key, &current_checksum, handle).await?
        {
            error = error.with_retry(handle.to_string(), retry);
        }
        return Err(error);
    }
    Ok(ResolvedHeader {
        view,
        normalized_document_hash,
        handle: document_handle.clone(),
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

/// The remap unit a parsed handle names.
fn remap_unit_of_handle(handle: &ReadingHandle) -> HandleRemapUnit {
    match handle {
        ReadingHandle::Document(_) => HandleRemapUnit::Document,
        ReadingHandle::Section(section) => HandleRemapUnit::Section {
            section_key: section.section_key,
        },
        ReadingHandle::Paragraph {
            section,
            paragraph_index,
        } => HandleRemapUnit::Paragraph {
            section_key: section.section_key,
            paragraph_index: *paragraph_index,
        },
        ReadingHandle::Sentence {
            section,
            paragraph_index,
            sentence_index,
        } => HandleRemapUnit::Sentence {
            section_key: section.section_key,
            paragraph_index: *paragraph_index,
            sentence_index: *sentence_index,
        },
    }
}

/// Renders a remap target as a handle of the same document key and the current identity checksum.
fn format_remap_target(doc_key: i64, current_checksum: &str, target: &HandleRemapUnit) -> String {
    let document = DocumentHandle {
        doc_key,
        checksum: current_checksum.to_owned(),
    };
    match target {
        HandleRemapUnit::Document => document.to_string(),
        HandleRemapUnit::Section { section_key } => SectionHandle {
            document,
            section_key: *section_key,
        }
        .to_string(),
        HandleRemapUnit::Paragraph {
            section_key,
            paragraph_index,
        } => super::short_handle::paragraph_handle(
            &SectionHandle {
                document,
                section_key: *section_key,
            },
            *paragraph_index,
        ),
        HandleRemapUnit::Sentence {
            section_key,
            paragraph_index,
            sentence_index,
        } => super::short_handle::sentence_handle(
            &SectionHandle {
                document,
                section_key: *section_key,
            },
            *paragraph_index,
            *sentence_index,
        ),
    }
}

/// Follows one already found remap step through chained re-ingests. A cycle or a missing step means
/// no proof and therefore no retry; there is deliberately no generation-count bound.
async fn follow_remap_step(
    index: &dyn TextUnitIndex,
    doc_key: i64,
    current_checksum: &str,
    first: HandleRemap,
) -> Result<Option<String>, ReadingError> {
    let mut checksum = first.target_checksum;
    let mut target = first.target;
    let mut seen = std::collections::HashSet::new();
    loop {
        if checksum == *current_checksum {
            return Ok(Some(format_remap_target(
                doc_key,
                current_checksum,
                &target,
            )));
        }
        if !seen.insert((checksum.clone(), target.clone())) {
            return Ok(None);
        }
        let Some(step) = index.handle_remap(doc_key, &checksum, &target).await? else {
            return Ok(None);
        };
        checksum = step.target_checksum;
        target = step.target;
    }
}

/// The proven current handle of one stale document, section, paragraph or sentence handle.
pub(crate) async fn remap_reading_handle(
    index: &dyn TextUnitIndex,
    doc_key: i64,
    current_checksum: &str,
    handle: &ReadingHandle,
) -> Result<Option<String>, ReadingError> {
    let source_checksum = &handle.document().checksum;
    if source_checksum == current_checksum {
        return Ok(None);
    }
    let unit = remap_unit_of_handle(handle);
    let Some(first) = index.handle_remap(doc_key, source_checksum, &unit).await? else {
        return Ok(None);
    };
    follow_remap_step(index, doc_key, current_checksum, first).await
}

/// The proven current handle of the first item a stale backward cursor would read. A cursor names
/// an exclusive end, so its item is `position - 1`; the returned handle is the exact backward-read
/// anchor of the current stream.
pub(crate) async fn remap_reading_cursor(
    index: &dyn TextUnitIndex,
    doc_key: i64,
    current_checksum: &str,
    cursor: &ReadingCursor,
) -> Result<Option<String>, ReadingError> {
    if cursor.position == 0 || cursor.section.document.checksum == *current_checksum {
        return Ok(None);
    }
    let kind = match cursor.unit {
        CursorUnit::Sentence => crate::application::get_text_units::RequestedTextUnitKind::Sentence,
        CursorUnit::Paragraph => {
            crate::application::get_text_units::RequestedTextUnitKind::Paragraph
        }
    };
    let Some(first) = index
        .handle_remap_at_stream_position(
            doc_key,
            &cursor.section.document.checksum,
            cursor.section.section_key,
            kind,
            cursor.position - 1,
        )
        .await?
    else {
        return Ok(None);
    };
    follow_remap_step(index, doc_key, current_checksum, first).await
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
