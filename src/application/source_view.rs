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

#[cfg(test)]
mod locator_differential_tests {
    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;

    use super::*;
    use crate::application::cite_reading::CiteReadingUseCase;
    use crate::application::locator_resolution::ResolvedLocatorKind;
    use crate::application::open_document::{OpenDocumentCommand, OpenDocumentUseCase};
    use crate::application::ports::{Parser, SourcePolicy};
    use crate::application::short_handle::{
        DocumentHandle, SectionHandle, handle_checksum, paragraph_handle, sentence_handle,
    };
    use crate::application::text_unit_store::store_text_units;
    use crate::domain::{
        ContentHash, DocumentSource, Location, MediaType, NormalizedTextRange,
        OriginalSourceBinding, OriginalSourceBindingMap, OriginalSourceTarget, Section, SectionId,
    };
    use crate::infrastructure::{
        InMemorySearchIndex, SqliteDocumentRepository, SqliteTextUnitIndex,
    };
    use crate::parsing::ParserRouter;
    use crate::retrieval::{FileRetriever, LocalFileSourcePolicy};

    /// Compare the pre-change tree/binding-map source-view resolution with the
    /// indexed point-query resolution on every stored paragraph and sentence
    /// handle of every text-layer PDF fixture. Large measurement PDFs can be
    /// added locally through `READING_MCP_SOURCE_VIEW_DIFFERENTIAL_INPUTS`;
    /// CI need not download them.
    #[tokio::test]
    async fn indexed_source_view_resolution_matches_tree_resolution_for_every_unit_handle() {
        let mut inputs = Vec::new();
        collect_pdfs(Path::new("tests/fixtures"), &mut inputs);
        // The large public measurement PDFs are not committed fixtures. A
        // local differential run names them with this variable, separated by
        // `;`; CI still runs every committed fixture without downloading
        // anything.
        if let Ok(extra) = std::env::var("READING_MCP_SOURCE_VIEW_DIFFERENTIAL_INPUTS") {
            for input in extra.split(';').filter(|input| !input.is_empty()) {
                inputs.push(PathBuf::from(input));
            }
        }
        inputs.sort();
        assert!(inputs.len() >= 20, "fixture PDFs should be present");

        let state = tempfile::tempdir().expect("differential state");
        let database = state.path().join("store.db");
        let repository =
            Arc::new(SqliteDocumentRepository::open(&database).expect("differential repository"));
        let index = Arc::new(SqliteTextUnitIndex::open(&database).expect("differential index"));
        let mut roots = vec![PathBuf::from("tests/fixtures")];
        for input in &inputs {
            if let Some(parent) = input.parent()
                && !roots.iter().any(|root| root == parent)
            {
                roots.push(parent.to_path_buf());
            }
        }
        let policy: Arc<dyn SourcePolicy> = Arc::new(LocalFileSourcePolicy::allow_roots(roots));
        let retriever: Arc<dyn Retriever> = Arc::new(FileRetriever);
        let parser: Arc<dyn Parser> = Arc::new(ParserRouter::phase4());

        let mut opened_documents = 0usize;
        let mut compared = 0usize;
        let mut matching_refusals = 0usize;
        let mut mismatches = Vec::new();
        for input in inputs {
            let opened = OpenDocumentUseCase::with_text_unit_index(
                Arc::clone(&policy),
                Arc::clone(&retriever),
                Arc::clone(&parser),
                Arc::clone(&repository) as Arc<dyn DocumentRepository>,
                Arc::clone(&index) as Arc<dyn TextUnitIndex>,
                Arc::new(InMemorySearchIndex::default()),
            )
            .execute(OpenDocumentCommand {
                source: DocumentSource(input.to_string_lossy().into_owned()),
                options: RetrievalOptions::default(),
            })
            .await;
            let opened = match opened {
                Ok(opened) => opened,
                // Scanned fixtures without a text layer refuse open with
                // `ocr_required`; they have no sentence or paragraph handles
                // for this differential.
                Err(_) => continue,
            };
            opened_documents += 1;
            let Some((full_document, _)) = repository
                .get_rebuilt_with_hash(&opened.document_id, true)
                .await
                .expect("full document should load")
            else {
                continue;
            };
            let document_key = index
                .document_key(&opened.document_id)
                .await
                .expect("document key query")
                .expect("opened document should have a key");
            let document_handle = DocumentHandle {
                doc_key: document_key,
                checksum: handle_checksum(
                    &opened.document_id,
                    &opened.normalized_document_hash,
                    &full_document.media_type,
                ),
            };
            let mut sections = Vec::new();
            collect_sections(&full_document.root_sections, &mut sections);
            for section in sections {
                let Some(units) = index
                    .section_units(&opened.document_id, &section.id)
                    .await
                    .expect("section units query")
                else {
                    continue;
                };
                if units.paragraphs.is_empty() && units.sentences.is_empty() {
                    continue;
                }
                let section_key = index
                    .section_key(&opened.document_id, &section.id)
                    .await
                    .expect("section key query")
                    .expect("stored section should have a key");
                let section_handle = SectionHandle {
                    document: document_handle.clone(),
                    section_key,
                };
                let mut handles = Vec::new();
                for unit in &units.paragraphs {
                    handles.push(paragraph_handle(&section_handle, unit.paragraph_index));
                }
                for unit in &units.sentences {
                    handles.push(sentence_handle(
                        &section_handle,
                        unit.paragraph_index,
                        unit.sentence_index,
                    ));
                }
                for handle in handles {
                    compared += 1;
                    if let Err(mismatch) = compare_one_handle(
                        &handle,
                        &input,
                        repository.clone(),
                        index.clone(),
                        &opened.document_id,
                    )
                    .await
                    {
                        if mismatch.starts_with("matching refusal:") {
                            matching_refusals += 1;
                        } else {
                            mismatches.push(mismatch);
                        }
                    }
                }
            }
        }

        // Valid fixture handles usually render one page. Add one minimal
        // multi-page binding case whose paragraph and first sentence cross the
        // page boundary, so the refusal path is differentially exercised too.
        let cross_handles = save_cross_page_case(&repository, &index).await;
        for handle in cross_handles {
            compared += 1;
            if let Err(mismatch) = compare_one_handle(
                &handle,
                Path::new("memory:cross-page.pdf"),
                repository.clone(),
                index.clone(),
                &DocumentId("doc:cross-page".into()),
            )
            .await
            {
                if mismatch.starts_with("matching refusal:") {
                    matching_refusals += 1;
                } else {
                    mismatches.push(mismatch);
                }
            }
        }

        eprintln!(
            "source-view locator differential: documents={opened_documents} handles={compared} matching_refusals={matching_refusals}"
        );
        assert!(opened_documents >= 8, "text-layer fixtures should open");
        assert!(compared >= 100, "the fixtures should exercise many handles");
        assert!(
            matching_refusals >= 2,
            "the synthetic page-crossing paragraph and sentence must both refuse"
        );
        assert!(
            mismatches.is_empty(),
            "indexed/tree source-view resolution mismatches:\n{}",
            mismatches.join("\n")
        );
    }

    async fn compare_one_handle(
        handle: &str,
        input: &Path,
        repository: Arc<SqliteDocumentRepository>,
        index: Arc<SqliteTextUnitIndex>,
        document_id: &DocumentId,
    ) -> Result<(), String> {
        let cite = CiteReadingUseCase::new(
            repository.clone() as Arc<dyn DocumentRepository>,
            index.clone() as Arc<dyn TextUnitIndex>,
        );
        let locator = cite.source_locator(handle).await.map_err(|error| {
            format!(
                "{} {handle}: cite failed: {}",
                input.display(),
                error.detail
            )
        })?;

        let old = old_resolution(&repository, &index, document_id, &locator).await;
        let new = new_resolution(&repository, &index, document_id, &locator).await;
        match (old, new) {
            (Ok(old), Ok(new)) if old == new => Ok(()),
            (Err(old), Err(new)) if old == new => Err(format!("matching refusal: {old}")),
            (old, new) => Err(format!(
                "{} {handle}: old={:?} new={:?}",
                input.display(),
                old,
                new
            )),
        }
    }

    type ResolutionOutcome = (
        ResolvedLocatorKind,
        NormalizedTextRange,
        u32,
        String,
        TextLocator,
    );

    async fn old_resolution(
        repository: &Arc<SqliteDocumentRepository>,
        index: &Arc<SqliteTextUnitIndex>,
        document_id: &DocumentId,
        locator: &TextLocator,
    ) -> Result<ResolutionOutcome, String> {
        let result = async {
            let (document, hash) = repository
                .get_rebuilt_with_hash(document_id, false)
                .await?
                .ok_or(ApplicationError::DocumentNotFound)?;
            let mut texts = SectionTexts::from_repository(
                &document,
                repository.clone() as Arc<dyn DocumentRepository>,
            );
            let resolved =
                resolve_text_locator(&document, locator, &hash, index.as_ref(), &mut texts).await?;
            let binding_map =
                load_binding_map(repository.as_ref(), &document, true, &mut texts).await?;
            let (page, version) = resolve_original_page(
                &resolved.locator.owner_section_id,
                &resolved.section_location,
                resolved.range,
                binding_map.as_ref(),
            )?;
            Ok((
                resolved.kind,
                resolved.range,
                page,
                version,
                resolved.locator,
            ))
        }
        .await;
        result.map_err(|error: ApplicationError| error.to_string())
    }

    async fn new_resolution(
        repository: &Arc<SqliteDocumentRepository>,
        index: &Arc<SqliteTextUnitIndex>,
        document_id: &DocumentId,
        locator: &TextLocator,
    ) -> Result<ResolutionOutcome, String> {
        let result = async {
            let (view, hash) = DocumentView::load_with_hash(
                repository.clone() as Arc<dyn DocumentRepository>,
                document_id,
            )
            .await?
            .ok_or(ApplicationError::DocumentNotFound)?;
            let document = view.document();
            let mut texts = SectionTexts::from_repository(
                &document,
                repository.clone() as Arc<dyn DocumentRepository>,
            );
            let resolved = resolve_text_locator_at(
                &document,
                &view,
                locator,
                &hash,
                index.as_ref(),
                &mut texts,
            )
            .await?;
            let (page, version) = resolve_original_page_indexed(
                repository.as_ref(),
                document_id,
                &resolved.locator.owner_section_id,
                &resolved.section_location,
                resolved.range,
            )
            .await?;
            Ok((
                resolved.kind,
                resolved.range,
                page,
                version,
                resolved.locator,
            ))
        }
        .await;
        result.map_err(|error: ApplicationError| error.to_string())
    }

    async fn save_cross_page_case(
        repository: &Arc<SqliteDocumentRepository>,
        index: &Arc<SqliteTextUnitIndex>,
    ) -> Vec<String> {
        let section_id = SectionId("section://chapter".into());
        let content = "Alpha bravo charlie. Delta echo foxtrot.".to_owned();
        let boundary = 15;
        let mut document = Document {
            id: DocumentId("doc:cross-page".into()),
            source: DocumentSource("memory:cross-page.pdf".into()),
            title: "Cross page".into(),
            media_type: MediaType("application/pdf".into()),
            content_hash: ContentHash("sha256:cross-page".into()),
            metadata: BTreeMap::new(),
            root_sections: vec![Section {
                id: section_id.clone(),
                parent_id: None,
                title: "Chapter".into(),
                level: 1,
                content: content.clone(),
                location: Location {
                    page: Some(1),
                    native_location: Some("pdf:pages:1-2".into()),
                    ..Location::default()
                },
                children: vec![],
            }],
        };
        document
            .set_original_source_binding_map(OriginalSourceBindingMap::new(vec![
                OriginalSourceBinding {
                    owner_section_id: section_id.clone(),
                    normalized_range: NormalizedTextRange::new(0, boundary).expect("range"),
                    target: OriginalSourceTarget::Page { page_number: 1 },
                },
                OriginalSourceBinding {
                    owner_section_id: section_id.clone(),
                    normalized_range: NormalizedTextRange::new(boundary, content.chars().count())
                        .expect("range"),
                    target: OriginalSourceTarget::Page { page_number: 2 },
                },
            ]))
            .expect("binding map should validate");
        let normalized_document_hash = document.normalized_document_hash();
        repository
            .save(document.clone())
            .await
            .expect("cross-page document should save");
        store_text_units(index.as_ref(), &document)
            .await
            .expect("cross-page units should store");
        let document_handle = DocumentHandle {
            doc_key: index
                .document_key(&document.id)
                .await
                .expect("document key")
                .expect("stored document"),
            checksum: handle_checksum(
                &document.id,
                &normalized_document_hash,
                &document.media_type,
            ),
        };
        let section = SectionHandle {
            document: document_handle,
            section_key: index
                .section_key(&document.id, &section_id)
                .await
                .expect("section key")
                .expect("stored section"),
        };
        vec![
            paragraph_handle(&section, 1),
            sentence_handle(&section, 1, 1),
        ]
    }

    fn collect_pdfs(directory: &Path, output: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(directory).expect("fixture directory should read") {
            let path = entry.expect("fixture directory entry").path();
            if path.is_dir() {
                collect_pdfs(&path, output);
            } else if path.extension().is_some_and(|extension| extension == "pdf") {
                output.push(path);
            }
        }
    }

    fn collect_sections<'a>(sections: &'a [Section], output: &mut Vec<&'a Section>) {
        for section in sections {
            output.push(section);
            collect_sections(&section.children, output);
        }
    }
}
