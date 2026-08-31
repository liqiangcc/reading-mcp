use std::sync::Arc;
use std::time::Duration;

use sha2::{Digest, Sha256};
use tokio::task::spawn_blocking;
use tokio::time::timeout;

use crate::application::locator_resolution::resolve_text_locator;
use crate::application::ports::{
    ApplicationError, DocumentRepository, RenderedSourceView, RetrievalOptions, Retriever,
    SourceViewRenderOptions, SourceViewRenderer,
};
use crate::domain::{DocumentId, DocumentSource, MediaType, NormalizedDocumentHash, TextLocator};

pub const DEFAULT_SOURCE_VIEW_DPI: u32 = 144;
pub const DEFAULT_SOURCE_VIEW_MAX_PAGES: usize = 2_000;
pub const DEFAULT_SOURCE_VIEW_MAX_WIDTH: u32 = 2_400;
pub const DEFAULT_SOURCE_VIEW_MAX_HEIGHT: u32 = 3_200;
pub const DEFAULT_SOURCE_VIEW_MAX_PIXELS: u64 = 4_000_000;
pub const DEFAULT_SOURCE_VIEW_MAX_IMAGE_BYTES: usize = 8 * 1024 * 1024;
pub const DEFAULT_SOURCE_VIEW_MAX_DECODED_STREAM_BYTES: u64 = 16 * 1024 * 1024;
pub const DEFAULT_SOURCE_VIEW_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceViewLimits {
    pub max_dpi: u32,
    pub max_pages: usize,
    pub max_width: u32,
    pub max_height: u32,
    pub max_pixels: u64,
    pub max_image_bytes: usize,
    pub max_decoded_stream_bytes: u64,
    pub timeout: Duration,
}

impl Default for SourceViewLimits {
    fn default() -> Self {
        Self {
            max_dpi: DEFAULT_SOURCE_VIEW_DPI,
            max_pages: DEFAULT_SOURCE_VIEW_MAX_PAGES,
            max_width: DEFAULT_SOURCE_VIEW_MAX_WIDTH,
            max_height: DEFAULT_SOURCE_VIEW_MAX_HEIGHT,
            max_pixels: DEFAULT_SOURCE_VIEW_MAX_PIXELS,
            max_image_bytes: DEFAULT_SOURCE_VIEW_MAX_IMAGE_BYTES,
            max_decoded_stream_bytes: DEFAULT_SOURCE_VIEW_MAX_DECODED_STREAM_BYTES,
            timeout: DEFAULT_SOURCE_VIEW_TIMEOUT,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SourceViewRepresentation {
    Original,
}

impl SourceViewRepresentation {
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Original => "original",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GetSourceViewCommand {
    pub document_id: DocumentId,
    pub target_locator: TextLocator,
    pub representation: SourceViewRepresentation,
    pub dpi: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceViewResult {
    pub document_id: DocumentId,
    pub source: DocumentSource,
    pub content_hash: String,
    pub normalized_document_hash: NormalizedDocumentHash,
    pub target_locator: TextLocator,
    pub representation: SourceViewRepresentation,
    pub page_number: u32,
    pub page_count: usize,
    pub dpi: u32,
    pub view: RenderedSourceView,
}

pub struct SourceViewUseCase {
    repository: Arc<dyn DocumentRepository>,
    retriever: Arc<dyn Retriever>,
    renderer: Arc<dyn SourceViewRenderer>,
    limits: SourceViewLimits,
}

impl SourceViewUseCase {
    pub fn new(
        repository: Arc<dyn DocumentRepository>,
        retriever: Arc<dyn Retriever>,
        renderer: Arc<dyn SourceViewRenderer>,
        limits: SourceViewLimits,
    ) -> Self {
        Self {
            repository,
            retriever,
            renderer,
            limits,
        }
    }

    pub async fn execute(
        &self,
        command: GetSourceViewCommand,
    ) -> Result<SourceViewResult, ApplicationError> {
        let dpi = command.dpi.unwrap_or(DEFAULT_SOURCE_VIEW_DPI);
        if dpi == 0 || dpi > self.limits.max_dpi {
            return Err(ApplicationError::InvalidRequest(format!(
                "source view dpi must be between 1 and {}",
                self.limits.max_dpi
            )));
        }

        let document = self
            .repository
            .get(&command.document_id)
            .await?
            .ok_or(ApplicationError::DocumentNotFound)?;
        let resolved = resolve_text_locator(&document, &command.target_locator)?;
        let section = document
            .find_section(&resolved.locator.owner_section_id)
            .ok_or(ApplicationError::InvalidLocator(
                "resolved source-view section disappeared".into(),
            ))?;
        let page_number = section.location.page.ok_or_else(|| {
            ApplicationError::InvalidRequest(
                "target locator has no original source page binding".into(),
            )
        })?;

        if !is_pdf(&document.media_type) {
            return Err(ApplicationError::InvalidRequest(format!(
                "original source view is not available for media type {}",
                document.media_type.0
            )));
        }

        let resource = self
            .retriever
            .retrieve(&document.source, &RetrievalOptions::default())
            .await?;
        if resource.final_source != document.source {
            return Err(ApplicationError::StaleLocator(format!(
                "source changed from {} to {}",
                document.source.0, resource.final_source.0
            )));
        }

        let actual_hash = format!("sha256:{:x}", Sha256::digest(&resource.bytes));
        if actual_hash != document.content_hash.0 {
            return Err(ApplicationError::StaleLocator(format!(
                "raw content hash changed from {} to {}",
                document.content_hash.0, actual_hash
            )));
        }

        let renderer = Arc::clone(&self.renderer);
        let media_type = resource.media_type.clone();
        let options = SourceViewRenderOptions {
            dpi,
            max_pages: self.limits.max_pages,
            max_width: self.limits.max_width,
            max_height: self.limits.max_height,
            max_pixels: self.limits.max_pixels,
            max_image_bytes: self.limits.max_image_bytes,
            max_decoded_stream_bytes: self.limits.max_decoded_stream_bytes,
        };
        let render = timeout(
            self.limits.timeout,
            spawn_blocking(move || {
                renderer.render(resource.bytes, media_type, page_number, options)
            }),
        )
        .await
        .map_err(|_| {
            ApplicationError::ResourceLimitExceeded(format!(
                "source view renderer exceeded {:?} timeout",
                self.limits.timeout
            ))
        })?
        .map_err(|error| {
            ApplicationError::SourceViewFailed(format!("source view worker failed: {error}"))
        })??;

        let normalized_document_hash = document.normalized_document_hash();

        Ok(SourceViewResult {
            document_id: document.id,
            source: document.source,
            content_hash: document.content_hash.0,
            normalized_document_hash,
            target_locator: resolved.locator,
            representation: command.representation,
            page_number,
            page_count: render.page_count,
            dpi,
            view: render,
        })
    }
}

fn is_pdf(media_type: &MediaType) -> bool {
    media_type
        .0
        .split(';')
        .next()
        .is_some_and(|value| value.trim().eq_ignore_ascii_case("application/pdf"))
}
