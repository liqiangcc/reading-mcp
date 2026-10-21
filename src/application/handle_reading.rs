//! Shared resolution of v2 short handles into the v1 canonical objects.
//!
//! `outline`, `search`, `cite` and `source_view` all start from a handle and need the same three
//! steps: the document behind its `doc_key`, the section behind its `section_key`, and the v1
//! `TextLocator` of a paragraph or sentence. Keeping them in one place means a handle fails the
//! same way (`unknown_handle`, `stale_handle`) in every tool.

use std::sync::Arc;

use crate::application::ports::{DocumentRepository, OwnedStreamUnit, TextUnitIndex};
use crate::application::reading_error::ReadingError;
use crate::application::short_handle::{
    DocumentHandle, ReadingHandle, SectionHandle, handle_checksum,
};
use crate::domain::{
    Document, NormalizedDocumentHash, Section, SentenceTextUnit, TextLocator, TextUnit,
};

/// One document resolved from a document handle, with the identity the handle is bound to.
#[derive(Clone)]
pub struct ResolvedDocument {
    pub document: Arc<Document>,
    pub normalized_document_hash: NormalizedDocumentHash,
    pub handle: DocumentHandle,
}

/// Resolves a document handle to the stored document, verifying its identity checksum.
pub async fn resolve_document(
    index: &dyn TextUnitIndex,
    repository: &dyn DocumentRepository,
    handle: &DocumentHandle,
) -> Result<ResolvedDocument, ReadingError> {
    let document_id = index
        .document_id_for_key(handle.doc_key)
        .await?
        .ok_or_else(|| {
            ReadingError::unknown_handle(format!(
                "doc_key {} is not stored in this server",
                handle.doc_key
            ))
        })?;
    // Every caller (`outline`, `search`, `cite`, the `source_view` locator) reads the section tree,
    // the titles and the locators, never a section's text, so the light document is enough.
    let (document, normalized_document_hash) = repository
        .get_light_with_hash(&document_id)
        .await?
        .ok_or_else(ReadingError::not_opened)?;
    if handle_checksum(&document.id, &normalized_document_hash) != handle.checksum {
        return Err(ReadingError::stale_handle(format!(
            "handle checksum {} does not match the current document identity",
            handle.checksum
        )));
    }
    Ok(ResolvedDocument {
        document,
        normalized_document_hash,
        handle: handle.clone(),
    })
}

/// Resolves a section handle to its section, inside an already resolved document.
pub async fn resolve_section(
    index: &dyn TextUnitIndex,
    resolved: &ResolvedDocument,
    handle: &SectionHandle,
) -> Result<Section, ReadingError> {
    let section_id = index
        .section_id_for_key(&resolved.document.id, handle.section_key)
        .await?
        .ok_or_else(|| {
            ReadingError::unknown_handle(format!(
                "section key {} is not stored for this document",
                handle.section_key
            ))
        })?;
    resolved
        .document
        .find_section(&section_id)
        .cloned()
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
pub async fn resolve_locator(
    index: &dyn TextUnitIndex,
    resolved: &ResolvedDocument,
    handle: &ReadingHandle,
) -> Result<Option<TextLocator>, ReadingError> {
    match handle {
        ReadingHandle::Document(_) => Ok(None),
        ReadingHandle::Section(section) => {
            let section = resolve_section(index, resolved, section).await?;
            Ok(Some(TextLocator::for_section_with_hash(
                &resolved.document,
                &section,
                &resolved.normalized_document_hash,
            )))
        }
        ReadingHandle::Paragraph {
            section,
            paragraph_index,
        } => {
            let section = resolve_section(index, resolved, section).await?;
            let unit = index
                .section_unit(
                    &resolved.document.id,
                    &section.id,
                    crate::application::get_text_units::EffectiveTextUnitKind::Paragraph,
                    *paragraph_index,
                    None,
                )
                .await?
                .ok_or_else(|| {
                    ReadingError::unknown_handle(format!(
                        "paragraph {paragraph_index} of section {} is not readable",
                        section.id.0
                    ))
                })?;
            let OwnedStreamUnit::Paragraph(unit) = unit else {
                return Err(ReadingError::unknown_handle(format!(
                    "paragraph {paragraph_index} of section {} is not stored as a paragraph",
                    section.id.0
                )));
            };
            Ok(Some(paragraph_locator(&resolved.document, &section, &unit)))
        }
        ReadingHandle::Sentence {
            section,
            paragraph_index,
            sentence_index,
        } => {
            let section = resolve_section(index, resolved, section).await?;
            let unit = index
                .section_unit(
                    &resolved.document.id,
                    &section.id,
                    crate::application::get_text_units::EffectiveTextUnitKind::Sentence,
                    *paragraph_index,
                    Some(*sentence_index),
                )
                .await?
                .ok_or_else(|| {
                    ReadingError::unknown_handle(format!(
                        "sentence {paragraph_index}.{sentence_index} of section {} is not readable",
                        section.id.0
                    ))
                })?;
            let OwnedStreamUnit::Sentence(unit) = unit else {
                return Err(ReadingError::unknown_handle(format!(
                    "sentence {paragraph_index}.{sentence_index} of section {} is not stored as a sentence",
                    section.id.0
                )));
            };
            Ok(Some(sentence_locator(&resolved.document, &section, &unit)))
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
