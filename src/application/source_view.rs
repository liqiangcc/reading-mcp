use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use sha2::{Digest, Sha256};
use tokio::task::spawn_blocking;

use crate::application::derived_text_units::DerivedTextUnits;
use crate::application::light_sections::SectionTexts;
use crate::application::locator_resolution::{resolve_text_locator, resolve_text_locator_at};
use crate::application::ports::{
    ApplicationError, DocumentRepository, HttpUrlPolicy, LocalSourceAccess,
    OriginalSourceRangePage, RawResourceCache, RenderedSourceView, RenderedSourceViewCache,
    ResolvedLocalSource, RetrievalOptions, Retriever, SOURCE_VIEW_JPEG_QUALITY,
    SOURCE_VIEW_RASTER_RULE_VERSION, SourceViewOutputFormat, SourceViewRenderOptions,
    SourceViewRenderer, TextUnitIndex, check_document_size_limit, scoped_cache_key,
};
use crate::application::section_view::DocumentView;
use crate::domain::{
    Document, DocumentId, DocumentSource, Location, NORMALIZED_DOCUMENT_HASH_VERSION,
    NormalizedDocumentHash, ORIGINAL_SOURCE_BINDING_METADATA_KEY,
    ORIGINAL_SOURCE_BINDING_MODEL_VERSION, OriginalSourceBindingError, OriginalSourceBindingMap,
    OriginalSourceTarget, SectionId, TextLocator,
};

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
    pub format: crate::application::ports::SourceViewOutputFormat,
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
            format: crate::application::ports::SourceViewOutputFormat::Auto,
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
    pub normalized_document_hash_version: String,
    pub source_binding_version: String,
    pub target_locator: TextLocator,
    pub representation: SourceViewRepresentation,
    pub page_number: u32,
    pub page_count: usize,
    pub dpi: u32,
    pub view: RenderedSourceView,
}

/// The collaborators of the cached URL path: the raw cache the retriever
/// stores through, the URL-level HTTP policy check, and the document size
/// limit `READING_MCP_MAX_DOCUMENT_BYTES` configures.
struct UrlSourceCache {
    raw_cache: Arc<dyn RawResourceCache>,
    http_policy: Arc<dyn HttpUrlPolicy>,
    max_document_bytes: usize,
}

pub struct SourceViewUseCase {
    repository: Arc<dyn DocumentRepository>,
    retriever: Arc<dyn Retriever>,
    renderer: Arc<dyn SourceViewRenderer>,
    limits: SourceViewLimits,
    text_unit_index: Option<Arc<dyn TextUnitIndex>>,
    local_sources: Option<Arc<dyn LocalSourceAccess>>,
    url_cache: Option<UrlSourceCache>,
    render_cache: Option<Arc<dyn RenderedSourceViewCache>>,
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
            text_unit_index: None,
            local_sources: None,
            url_cache: None,
            render_cache: None,
        }
    }

    pub fn with_text_unit_index(mut self, index: Arc<dyn TextUnitIndex>) -> Self {
        self.text_unit_index = Some(index);
        self
    }

    /// The local-file resolution path: with `resolver` set, a document whose
    /// source is a local file is rendered from the file itself — the worker
    /// maps it and checks its sha256 — instead of the parent retrieving and
    /// staging a copy. Without it every source keeps the buffered path.
    pub fn with_local_source_access(mut self, resolver: Arc<dyn LocalSourceAccess>) -> Self {
        self.local_sources = Some(resolver);
        self
    }

    /// The rendered-page cache. It is consulted only after locator, policy,
    /// source and size resolution; a mapped-source hit additionally requires
    /// the cache's file freshness evidence, and every miss still runs the
    /// renderer's sha256 identity check before rendering.
    pub fn with_render_cache(mut self, cache: Arc<dyn RenderedSourceViewCache>) -> Self {
        self.render_cache = Some(cache);
        self
    }

    /// The URL-source cache path: a document whose source is HTTP(S) is
    /// rendered from its raw-cache body — the worker maps it and checks its
    /// sha256 exactly as for a local file — without contacting the origin,
    /// whenever a usable entry exists. `raw_cache` is the same cache the
    /// retriever stores through, `http_policy` supplies only the URL-level
    /// check (never DNS), and `max_document_bytes` is
    /// `READING_MCP_MAX_DOCUMENT_BYTES`. Without it every URL source keeps the
    /// buffered path.
    pub fn with_url_source_cache(
        mut self,
        raw_cache: Arc<dyn RawResourceCache>,
        http_policy: Arc<dyn HttpUrlPolicy>,
        max_document_bytes: usize,
    ) -> Self {
        self.url_cache = Some(UrlSourceCache {
            raw_cache,
            http_policy,
            max_document_bytes,
        });
        self
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

        // With a stored text-unit index, resolve through the same point-query
        // document view `cite` uses: one header, one owner-section row, one
        // text-unit row and one source-binding row. A warm rendered-page hit
        // must not rebuild a whole book (or validate every binding in it)
        // before reaching the cache. Without a stored index the fallback units
        // segment the full document, so that path keeps its whole-document load.
        let (document, normalized_document_hash, resolved) = match &self.text_unit_index {
            Some(index) => {
                let (view, normalized_document_hash) = DocumentView::load_with_hash(
                    Arc::clone(&self.repository),
                    &command.document_id,
                )
                .await?
                .ok_or(ApplicationError::DocumentNotFound)?;
                let document = Arc::new(view.document());
                let mut texts =
                    SectionTexts::from_repository(&document, Arc::clone(&self.repository));
                let resolved = resolve_text_locator_at(
                    &document,
                    &view,
                    &command.target_locator,
                    &normalized_document_hash,
                    index.as_ref(),
                    &mut texts,
                )
                .await?;
                (document, normalized_document_hash, resolved)
            }
            None => {
                let (document, normalized_document_hash) = self
                    .repository
                    .get_rebuilt_with_hash(&command.document_id, true)
                    .await?
                    .ok_or(ApplicationError::DocumentNotFound)?;
                let units: Arc<dyn TextUnitIndex> =
                    Arc::new(DerivedTextUnits::new(Arc::clone(&document)));
                let mut texts = SectionTexts::from_document(&document);
                let resolved = resolve_text_locator(
                    &document,
                    &command.target_locator,
                    &normalized_document_hash,
                    units.as_ref(),
                    &mut texts,
                )
                .await?;
                (document, normalized_document_hash, resolved)
            }
        };
        let (page_number, source_binding_version) = if self.text_unit_index.is_some() {
            resolve_original_page_indexed(
                self.repository.as_ref(),
                &document.id,
                &resolved.locator.owner_section_id,
                &resolved.section_location,
                resolved.range,
            )
            .await?
        } else {
            let mut texts = SectionTexts::from_document(&document);
            let binding_map =
                load_binding_map(self.repository.as_ref(), &document, false, &mut texts).await?;
            resolve_original_page(
                &resolved.locator.owner_section_id,
                &resolved.section_location,
                resolved.range,
                binding_map.as_ref(),
            )?
        };

        if !document.media_type.is_pdf() {
            return Err(ApplicationError::InvalidRequest(format!(
                "original source view is not available for media type {}",
                document.media_type.0
            )));
        }

        // A local source resolves to the file itself under the same policy the
        // retrieval chain enforces, so the worker maps it instead of the parent
        // holding buffered and staged copies. A URL source resolves to its
        // raw-cache body when the stored entry is usable — the same mapped
        // worker path without a network round trip. Anything else keeps the
        // buffered retrieval path; `None` resolver means the buffered path for
        // all.
        let mapped_source: Option<ResolvedLocalSource> = if document.source.is_http() {
            self.cached_url_source(&document).await?
        } else {
            match &self.local_sources {
                Some(resolver) => Some(resolver.resolve(&document.source).await?),
                None => None,
            }
        };

        let renderer = Arc::clone(&self.renderer);
        let options = SourceViewRenderOptions {
            dpi,
            max_pages: self.limits.max_pages,
            max_width: self.limits.max_width,
            max_height: self.limits.max_height,
            max_pixels: self.limits.max_pixels,
            max_image_bytes: self.limits.max_image_bytes,
            max_decoded_stream_bytes: self.limits.max_decoded_stream_bytes,
            format: self.limits.format,
        };
        let cache_keys = RenderCacheKeys::new(
            &document.content_hash.0,
            page_number,
            &options,
            renderer.cache_namespace(),
        );
        let render = match mapped_source {
            Some(resolved) => {
                if resolved.final_source != document.source {
                    return Err(ApplicationError::StaleLocator(format!(
                        "source changed from {} to {}",
                        document.source.0, resolved.final_source.0
                    )));
                }
                let expected = document.content_hash.0.clone();
                let cache = self.render_cache.clone();
                spawn_blocking(move || {
                    let fingerprint = cache
                        .as_ref()
                        .and_then(|cache| cache.fingerprint(&resolved.path).ok());
                    if let (Some(cache), Some(fingerprint)) = (&cache, &fingerprint)
                        && let Some(hit) = cache.get(cache_keys.probes(), Some(fingerprint))?
                    {
                        return Ok(hit);
                    }

                    let before = fingerprint;
                    let rendered = renderer.render_local_file(
                        &resolved.path,
                        &expected,
                        resolved.media_type.clone(),
                        page_number,
                        options,
                    )?;
                    if let Some(cache) = &cache {
                        // Use freshness evidence from after the render. If it
                        // differs from the pre-render evidence the file was
                        // replaced or touched concurrently, so do not publish
                        // an entry whose hit-time validation would be wrong.
                        let after = cache.fingerprint(&resolved.path).ok();
                        let valid = match (&before, &after) {
                            (Some(before), Some(after)) if before == after => Some(after.clone()),
                            _ => None,
                        };
                        if let Some(fingerprint) = valid {
                            cache.put(
                                cache_keys.for_media_type(&rendered.media_type.0),
                                &rendered,
                                Some(fingerprint),
                            )?;
                        }
                    }
                    Ok(rendered)
                })
                .await
                .map_err(|error| {
                    ApplicationError::SourceViewFailed(format!(
                        "source view worker failed: {error}"
                    ))
                })??
            }
            None => {
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

                let media_type = resource.media_type.clone();
                let cache = self.render_cache.clone();
                spawn_blocking(move || {
                    // The buffered path retrieved and hashed the exact bytes
                    // before this lookup, so no file freshness evidence is
                    // needed: the content hash in the key is the validation.
                    if let Some(cache) = &cache
                        && let Some(hit) = cache.get(cache_keys.probes(), None)?
                    {
                        return Ok(hit);
                    }
                    let rendered =
                        renderer.render(resource.bytes, media_type, page_number, options)?;
                    if let Some(cache) = &cache {
                        cache.put(
                            cache_keys.for_media_type(&rendered.media_type.0),
                            &rendered,
                            None,
                        )?;
                    }
                    Ok(rendered)
                })
                .await
                .map_err(|error| {
                    ApplicationError::SourceViewFailed(format!(
                        "source view worker failed: {error}"
                    ))
                })??
            }
        };

        Ok(SourceViewResult {
            document_id: document.id.clone(),
            source: document.source.clone(),
            content_hash: document.content_hash.0.clone(),
            normalized_document_hash,
            normalized_document_hash_version: NORMALIZED_DOCUMENT_HASH_VERSION.into(),
            source_binding_version,
            target_locator: resolved.locator,
            representation: command.representation,
            page_number,
            page_count: render.page_count,
            dpi,
            view: render,
        })
    }

    /// The raw-cache body of a URL document source, located without reading
    /// the body and without any network I/O — the same key the retrieval chain
    /// stores under, no auth profile — kept only while it may stand in for the
    /// origin: clean metadata, `final_source` equal to the document's source,
    /// a PDF media type, and a body within `max_document_bytes`. The HTTP
    /// policy's URL-level check runs first, before the cache is touched, so a
    /// URL it refuses answers the policy's error whatever the cache holds. A
    /// body over the limit fails the way the retrieval path fails; everything
    /// else short of usable answers `None` and the caller falls back to
    /// today's buffered retrieval unchanged.
    async fn cached_url_source(
        &self,
        document: &Document,
    ) -> Result<Option<ResolvedLocalSource>, ApplicationError> {
        let Some(cache) = &self.url_cache else {
            return Ok(None);
        };
        cache.http_policy.check_url(&document.source)?;
        let key = scoped_cache_key(&document.source, None);
        let Some(entry) = cache.raw_cache.locate(&key).await? else {
            return Ok(None);
        };
        if entry.final_source != document.source || !entry.media_type.is_pdf() {
            return Ok(None);
        }
        check_document_size_limit(entry.body_len, cache.max_document_bytes)?;
        Ok(Some(ResolvedLocalSource {
            path: entry.body_path,
            final_source: entry.final_source,
            media_type: entry.media_type,
        }))
    }
}

/// Both cache keys a render can occupy in `auto` mode. Probing the actual
/// format names keeps the chosen format inside the key without requiring a
/// pre-render raster detection pass: at most one of the two entries can exist
/// for one deterministic renderer/rule version.
struct RenderCacheKeys {
    png: String,
    jpeg: String,
    probes: Vec<String>,
}

impl RenderCacheKeys {
    fn new(
        content_hash: &str,
        page: u32,
        options: &SourceViewRenderOptions,
        renderer_namespace: &str,
    ) -> Self {
        let png = rendered_cache_key(content_hash, page, options, renderer_namespace, "png");
        let jpeg = rendered_cache_key(content_hash, page, options, renderer_namespace, "jpeg");
        let probes = match options.format {
            SourceViewOutputFormat::Auto => vec![jpeg.clone(), png.clone()],
            SourceViewOutputFormat::Png => vec![png.clone()],
        };
        Self { png, jpeg, probes }
    }

    fn probes(&self) -> &[String] {
        &self.probes
    }

    fn for_media_type(&self, media_type: &str) -> &str {
        if media_type == "image/jpeg" {
            &self.jpeg
        } else {
            &self.png
        }
    }
}

/// The cache key is a sha256 over explicit length-neutral components. It covers
/// the exact document bytes through `content_hash`, the page, every render
/// input that can change output bytes, the renderer engine/version namespace,
/// the raster rule and JPEG quality constants, the configured format mode, and
/// the actual format of the stored entry.
fn rendered_cache_key(
    content_hash: &str,
    page: u32,
    options: &SourceViewRenderOptions,
    renderer_namespace: &str,
    chosen_format: &str,
) -> String {
    let mut input = String::new();
    let mut component = |name: &str, value: &str| {
        input.push_str(name);
        input.push('\x1e');
        input.push_str(value);
        input.push('\x1f');
    };
    component("cache", "rendered-source-view/v1");
    component("content_hash", content_hash);
    component("page", &page.to_string());
    component("dpi", &options.dpi.to_string());
    component("max_pages", &options.max_pages.to_string());
    component("max_width", &options.max_width.to_string());
    component("max_height", &options.max_height.to_string());
    component("max_pixels", &options.max_pixels.to_string());
    component("max_image_bytes", &options.max_image_bytes.to_string());
    component(
        "max_decoded_stream_bytes",
        &options.max_decoded_stream_bytes.to_string(),
    );
    component("renderer", renderer_namespace);
    component("raster_rule", SOURCE_VIEW_RASTER_RULE_VERSION);
    component("jpeg_quality", &SOURCE_VIEW_JPEG_QUALITY.to_string());
    component("format_mode", options.format.as_str());
    component("chosen_format", chosen_format);
    format!("{:x}", Sha256::digest(input.as_bytes()))
}

/// The persisted original-source binding map, deserialized and validated exactly as the full
/// document's `original_source_binding_map` does. A light document carries no section text, so the
/// lengths the range check needs come from the repository, one `section_layout_v2` row per owner.
async fn load_binding_map(
    repository: &dyn DocumentRepository,
    document: &Document,
    light: bool,
    texts: &mut SectionTexts<'_>,
) -> Result<Option<OriginalSourceBindingMap>, ApplicationError> {
    if !light {
        return document
            .original_source_binding_map()
            .map_err(source_binding_error);
    }
    let Some(json) = repository
        .load_metadata(&document.id, ORIGINAL_SOURCE_BINDING_METADATA_KEY)
        .await?
    else {
        return Ok(None);
    };
    let map = serde_json::from_str::<OriginalSourceBindingMap>(&json).map_err(|error| {
        source_binding_error(OriginalSourceBindingError::InvalidJson(error.to_string()))
    })?;
    let mut lengths = HashMap::new();
    for binding in &map.bindings {
        let section_id = &binding.owner_section_id;
        if lengths.contains_key(section_id) || document.find_section(section_id).is_none() {
            // An owner that does not exist is reported by the validation below, with its own error.
            continue;
        }
        lengths.insert(section_id.clone(), texts.char_len(section_id).await?);
    }
    map.validate_against_section_lengths(|section_id| lengths.get(section_id).copied())
        .map_err(source_binding_error)?;
    Ok(Some(map))
}

fn resolve_original_page(
    section_id: &SectionId,
    section_location: &Location,
    range: crate::domain::NormalizedTextRange,
    binding_map: Option<&OriginalSourceBindingMap>,
) -> Result<(u32, String), ApplicationError> {
    let lookup = if let Some(binding_map) = binding_map {
        match binding_map.target_for_range(section_id, range) {
            Ok(Some(OriginalSourceTarget::Page { page_number })) => {
                OriginalSourceRangePage::Page(page_number)
            }
            Ok(None) => OriginalSourceRangePage::NoCoveringBinding,
            Err(error) => return Err(source_binding_error(error)),
        }
    } else {
        OriginalSourceRangePage::NoBindings
    };
    resolve_original_page_lookup(section_location, lookup)
}

/// One point-query binding lookup. SQLite answers it from indexed binding
/// rows; validating every row of a book on every `source_view` call would cost
/// more than the rendered-page cache hit it protects.
async fn resolve_original_page_indexed(
    repository: &dyn DocumentRepository,
    document_id: &DocumentId,
    section_id: &SectionId,
    section_location: &Location,
    range: crate::domain::NormalizedTextRange,
) -> Result<(u32, String), ApplicationError> {
    let lookup = repository
        .original_source_page_for_range(document_id, section_id, range)
        .await?;
    resolve_original_page_lookup(section_location, lookup)
}

fn resolve_original_page_lookup(
    section_location: &Location,
    lookup: OriginalSourceRangePage,
) -> Result<(u32, String), ApplicationError> {
    match lookup {
        OriginalSourceRangePage::Page(page_number) => {
            Ok((page_number, ORIGINAL_SOURCE_BINDING_MODEL_VERSION.into()))
        }
        OriginalSourceRangePage::NoCoveringBinding => Err(ApplicationError::InvalidLocator(
            "target locator has no precise original source binding".into(),
        )),
        OriginalSourceRangePage::Ambiguous => Err(source_binding_error(
            OriginalSourceBindingError::AmbiguousTarget,
        )),
        OriginalSourceRangePage::NoBindings => legacy_single_page_binding(section_location)
            .map(|page| (page, "legacy-single-page-section/v1".into()))
            .ok_or_else(|| {
                ApplicationError::StaleLocator(
                    "persisted PDF lacks precise page binding evidence for this locator; reopen the document with the current parser"
                        .into(),
                )
            }),
    }
}

fn legacy_single_page_binding(location: &Location) -> Option<u32> {
    let page = location.page?;
    let expected = format!("pdf:page:{page}");
    (location.native_location.as_deref() == Some(expected.as_str())).then_some(page)
}

fn source_binding_error(error: OriginalSourceBindingError) -> ApplicationError {
    match error {
        OriginalSourceBindingError::AmbiguousTarget => ApplicationError::InvalidRequest(
            "target locator spans multiple original source pages; use a narrower Paragraph/Sentence locator"
                .into(),
        ),
        other => ApplicationError::SourceViewFailed(format!(
            "invalid persisted original source binding evidence: {other}"
        )),
    }
}
