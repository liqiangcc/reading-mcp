//! The v2 `cite` tool: the v1 canonical locator and document identity of one or more handles.
//!
//! `cite` is the only v2 tool that returns long fields on purpose: a caller asking for a citation
//! wants the exact locator, the original source position and the identity it is bound to. Every
//! other tool stays compact and points here when a citation is needed.

use std::sync::Arc;

use crate::application::handle_reading::{ResolvedHeader, resolve_header, resolve_locator};
use crate::application::ports::{DocumentRepository, TextUnitIndex};
use crate::application::reading_error::ReadingError;
use crate::application::section_view::carrier_section;
use crate::application::short_handle::{ReadingHandle, parse_handle};
use crate::application::text_unit_stream::coarse_degradation;
use crate::domain::{
    DocumentId, NORMALIZATION_VERSION, NORMALIZED_DOCUMENT_HASH_VERSION, SectionId,
    SentenceEligibility, TEXT_SEGMENTATION_VERSION, TextLocator,
};

/// One cited handle.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CiteReadingItem {
    pub at: String,
    /// The full v1 locator, absent only for a document handle.
    pub locator: Option<TextLocator>,
    /// The v1 `effective_kind` of a paragraph handle sentence mode cannot split, so a caller sees
    /// the unit is a paragraph without having to read it. `None` for a sentence and for a
    /// paragraph that splits into sentences.
    pub effective_kind: Option<&'static str>,
    /// The v1 degradation code that says why that paragraph is whole.
    pub degradation: Option<&'static str>,
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
            let (effective_kind, degradation) = match (&handle, &locator) {
                (
                    ReadingHandle::Paragraph {
                        paragraph_index, ..
                    },
                    Some(locator),
                ) => {
                    unsplittable_paragraph(
                        self.text_unit_index.as_ref(),
                        resolved.id(),
                        &locator.owner_section_id,
                        *paragraph_index,
                    )
                    .await?
                }
                _ => (None, None),
            };
            let source_position = locator
                .as_ref()
                .and_then(|locator| locator.native_location.clone());
            let page = self
                .cite_page(&resolved, locator.as_ref(), source_position.as_deref())
                .await?;
            items.push(CiteReadingItem {
                at: at.clone(),
                locator,
                effective_kind,
                degradation,
                source_position,
                page,
                document_id: resolved.id().0.clone(),
                content_hash: resolved.header().content_hash.0.clone(),
                normalized_document_hash: resolved.normalized_document_hash.0.clone(),
            });
        }
        Ok(CiteReadingResult { items })
    }

    /// The page one cited handle sits on.
    ///
    /// A paragraph or sentence handle of a PDF reports the page of the unit's first character,
    /// read from the owner section's original-source bindings — the page `source_view` renders
    /// for the unit when it sits inside one page, and still its first page when the unit crosses
    /// a page break. A section or document handle, and every handle of a non-PDF document, keeps
    /// the page its `src` names.
    async fn cite_page(
        &self,
        resolved: &ResolvedHeader,
        locator: Option<&TextLocator>,
        source_position: Option<&str>,
    ) -> Result<Option<u32>, ReadingError> {
        let Some(locator) = locator else {
            return Ok(None);
        };
        let Some(range) = locator.normalized_range else {
            return Ok(source_position.and_then(page_of));
        };
        if !resolved.header().media_type.is_pdf() {
            return Ok(source_position.and_then(page_of));
        }
        if let Some(page) = self
            .repository
            .original_source_page_at(resolved.id(), &locator.owner_section_id, range.start())
            .await?
        {
            return Ok(Some(page));
        }
        // `stale_handle` only when reopening fixes it: the document keeps no binding rows at
        // all and its stored open result predates this store version. A document ingested
        // fresh that lacks a covering row cannot gain one by reopening, so it falls through.
        if self
            .repository
            .original_source_bindings_stale(resolved.id())
            .await?
        {
            return Err(ReadingError::stale_handle(
                "the stored PDF keeps no original-source bindings; open the document again",
            ));
        }
        match source_position {
            // A one-page section needs no binding row to name its page.
            Some(position) if position.starts_with("pdf:page:") => Ok(page_of(position)),
            // A unit no stored binding covers reports no page: the section's start page would
            // be the wrong page.
            _ => Ok(None),
        }
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

/// The v1 `effective_kind` and degradation code of a paragraph handle, when sentence mode cannot
/// split that paragraph: the paragraph is coarse, so a caller citing it is citing a whole paragraph
/// and not a sentence. `(None, None)` for a paragraph that splits into sentences.
async fn unsplittable_paragraph(
    index: &dyn TextUnitIndex,
    document_id: &DocumentId,
    section_id: &SectionId,
    paragraph_index: usize,
) -> Result<(Option<&'static str>, Option<&'static str>), ReadingError> {
    let Some(units) = index.section_units(document_id, section_id).await? else {
        return Err(ReadingError::stale_handle(format!(
            "section {} has no stored text units",
            section_id.0
        )));
    };
    let coverage = units
        .sentence_coverage
        .iter()
        .find(|coverage| coverage.paragraph_index == paragraph_index);
    match coverage {
        Some(coverage) if coverage.eligibility == SentenceEligibility::CoarseParagraphOnly => Ok((
            Some(crate::application::text_unit_stream::PARAGRAPH_KIND),
            Some(coarse_degradation(coverage.content_class)),
        )),
        _ => Ok((None, None)),
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
