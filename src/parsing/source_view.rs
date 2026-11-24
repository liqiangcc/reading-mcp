use std::collections::HashSet;
use std::ffi::OsString;
use std::fs;
use std::io::{Cursor, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use image::{ImageFormat, RgbaImage};
use pdf_render::pdf_interpret::{InterpreterSettings, InterpreterWarning};
use pdf_render::pdf_syntax::content::TypedIter;
use pdf_render::pdf_syntax::content::ops::TypedInstruction;
use pdf_render::pdf_syntax::object::dict::keys::{
    FORM, H, HEIGHT, IMAGE, MASK, RESOURCES, SMASK, SUBTYPE, W, WIDTH,
};
use pdf_render::pdf_syntax::object::{Dict, Name, ObjectIdentifier, Stream};
use pdf_render::pdf_syntax::page::{Page, Resources};
use pdf_render::pdf_syntax::{self, LoadPdfError, Pdf, PdfLoadLimits};
use serde::{Deserialize, Serialize};

use crate::application::ports::{
    ApplicationError, RenderedSourceView, SourceViewLimitDetail, SourceViewLimitKind,
    SourceViewRenderOptions, SourceViewRenderer,
};
use crate::domain::MediaType;

const SOURCE_VIEW_WORKER_FLAG: &str = "--reading-mcp-source-view-worker";
const WORKER_POLL_INTERVAL: Duration = Duration::from_millis(5);
const MAX_WORKER_DIAGNOSTIC_CHARS: usize = 4_096;
static WORKER_TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// The marker prefix of the JSON limit record a source-view worker prints on
/// stderr when it rejects a page over a configured limit. Both workers — the
/// pinned PyMuPDF script and this crate's `--reading-mcp-source-view-*-worker`
/// modes — use the same record, so the parent reads `kind`, `measured` and
/// `limit` as data and never matches on message text. Any other stderr is a
/// crash or an invalid PDF and stays `SourceViewFailed`.
pub(crate) const WORKER_LIMIT_STDERR_MARKER: &str = "READING-MCP-SOURCE-VIEW-LIMIT ";

/// The JSON record a worker emits behind [`WORKER_LIMIT_STDERR_MARKER`]. The
/// PyMuPDF worker writes the same fields by name.
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub(crate) struct WorkerLimitRecord {
    pub kind: WorkerLimitKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub measured: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u64>,
}

/// The wire names of [`SourceViewLimitKind`]; keep in sync with
/// `pdf_source_view_worker.py`.
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum WorkerLimitKind {
    DecodedStreamBytes,
    EmbeddedImagePixels,
    PageDimensions,
    RenderedPixels,
    ImageBytes,
    PageCount,
    DocumentObjects,
    Timeout,
}

impl From<WorkerLimitKind> for SourceViewLimitKind {
    fn from(kind: WorkerLimitKind) -> Self {
        match kind {
            WorkerLimitKind::DecodedStreamBytes => Self::DecodedStreamBytes,
            WorkerLimitKind::EmbeddedImagePixels => Self::EmbeddedImagePixels,
            WorkerLimitKind::PageDimensions => Self::PageDimensions,
            WorkerLimitKind::RenderedPixels => Self::RenderedPixels,
            WorkerLimitKind::ImageBytes => Self::ImageBytes,
            WorkerLimitKind::PageCount => Self::PageCount,
            WorkerLimitKind::DocumentObjects => Self::DocumentObjects,
            WorkerLimitKind::Timeout => Self::Timeout,
        }
    }
}

impl From<SourceViewLimitKind> for WorkerLimitKind {
    fn from(kind: SourceViewLimitKind) -> Self {
        match kind {
            SourceViewLimitKind::DecodedStreamBytes => Self::DecodedStreamBytes,
            SourceViewLimitKind::EmbeddedImagePixels => Self::EmbeddedImagePixels,
            SourceViewLimitKind::PageDimensions => Self::PageDimensions,
            SourceViewLimitKind::RenderedPixels => Self::RenderedPixels,
            SourceViewLimitKind::ImageBytes => Self::ImageBytes,
            SourceViewLimitKind::PageCount => Self::PageCount,
            SourceViewLimitKind::DocumentObjects => Self::DocumentObjects,
            SourceViewLimitKind::Timeout => Self::Timeout,
        }
    }
}

/// Parses the worker's stderr for the limit record and turns it into the
/// detail the parent reports: the record carries the kind and the numbers; the
/// parent adds the page it asked for. `None` means the worker failed for a
/// non-limit reason (crash, invalid PDF, bad arguments).
pub(crate) fn worker_limit_detail(stderr: &[u8], page: u32) -> Option<SourceViewLimitDetail> {
    let text = String::from_utf8_lossy(stderr);
    for line in text.lines() {
        let Some(record) = line.strip_prefix(WORKER_LIMIT_STDERR_MARKER) else {
            continue;
        };
        let record: WorkerLimitRecord = serde_json::from_str(record).ok()?;
        return Some(SourceViewLimitDetail::new(
            page,
            record.kind.into(),
            record.measured,
            record.limit,
        ));
    }
    None
}

/// Prints the worker's limit record on stderr and passes the error through as
/// the exit the `main` wrapper reports. A worker reports a limit only this
/// way; every other error keeps propagating as before.
pub(crate) fn report_worker_limit(error: ApplicationError) -> Box<dyn std::error::Error> {
    if let ApplicationError::SourceViewLimitExceeded(detail) = &error {
        let record = WorkerLimitRecord {
            kind: detail.kind.into(),
            measured: detail.measured,
            limit: detail.limit,
        };
        if let Ok(json) = serde_json::to_string(&record) {
            eprintln!("{WORKER_LIMIT_STDERR_MARKER}{json}");
        }
    }
    error.into()
}

#[derive(Default)]
pub struct PdfSourceViewRenderer;

impl SourceViewRenderer for PdfSourceViewRenderer {
    fn render(
        &self,
        bytes: Vec<u8>,
        media_type: MediaType,
        page: u32,
        options: SourceViewRenderOptions,
    ) -> Result<RenderedSourceView, ApplicationError> {
        if !media_type.is_pdf() {
            return Err(ApplicationError::SourceViewFailed(format!(
                "unsupported source-view media type {}",
                media_type.0
            )));
        }

        let load_limits = PdfLoadLimits::new()
            .max_object_depth(64)
            .max_image_pixels(options.max_pixels)
            .max_stream_bytes(options.max_decoded_stream_bytes);
        let pdf = Pdf::new_with_limits(bytes, load_limits).map_err(|error| match error {
            // `TooLarge` is the only `LoadPdfError` that is a limit rejection
            // (the parser's object/page caps); `Invalid` and `Decryption` mean
            // a corrupt or encrypted PDF and stay `internal`.
            LoadPdfError::TooLarge(objects, pages) => {
                let (kind, measured, limit) = if objects > pdf_syntax::MAX_OBJECTS {
                    (
                        SourceViewLimitKind::DocumentObjects,
                        objects as u64,
                        pdf_syntax::MAX_OBJECTS as u64,
                    )
                } else {
                    (
                        SourceViewLimitKind::PageCount,
                        pages as u64,
                        pdf_syntax::MAX_PAGES as u64,
                    )
                };
                ApplicationError::SourceViewLimitExceeded(SourceViewLimitDetail::new(
                    page,
                    kind,
                    Some(measured),
                    Some(limit),
                ))
            }
            LoadPdfError::Invalid | LoadPdfError::Decryption(_) => {
                ApplicationError::SourceViewFailed(format!("invalid PDF source view: {error:?}"))
            }
        })?;
        let pages = pdf.pages();
        let page_count = pages.len();
        if page_count > options.max_pages {
            return Err(ApplicationError::SourceViewLimitExceeded(
                SourceViewLimitDetail::new(
                    page,
                    SourceViewLimitKind::PageCount,
                    Some(page_count as u64),
                    Some(options.max_pages as u64),
                ),
            ));
        }

        let page_index = page
            .checked_sub(1)
            .and_then(|value| usize::try_from(value).ok());
        let page_index = page_index.ok_or_else(|| {
            ApplicationError::InvalidRequest("source-view page must be 1-based".into())
        })?;
        let source_page = pages.get(page_index).ok_or_else(|| {
            ApplicationError::InvalidRequest(format!(
                "source-view page {page} is outside the PDF page range 1..={page_count}"
            ))
        })?;

        let (base_width, base_height) = source_page.render_dimensions();
        let scale = options.dpi as f32 / 72.0;
        let width = checked_dimension(base_width, scale, options.max_width, "width", page)?;
        let height = checked_dimension(base_height, scale, options.max_height, "height", page)?;
        let pixels = u64::from(width)
            .checked_mul(u64::from(height))
            .ok_or_else(|| {
                ApplicationError::SourceViewLimitExceeded(SourceViewLimitDetail::new(
                    page,
                    SourceViewLimitKind::RenderedPixels,
                    None,
                    Some(options.max_pixels),
                ))
            })?;
        if pixels > options.max_pixels {
            return Err(ApplicationError::SourceViewLimitExceeded(
                SourceViewLimitDetail::new(
                    page,
                    SourceViewLimitKind::RenderedPixels,
                    Some(pixels),
                    Some(options.max_pixels),
                ),
            ));
        }

        // A drawn embedded image over `max_pixels` reaches the interpreter as
        // the generic `ImageDecodeFailure` warning — indistinguishable from a
        // corrupt image — so the render pass alone cannot tell "over the limit"
        // from "broken". Check every drawn image's `Width × Height` against the
        // same bound `pdf-syntax` applies in `decoded_image` before rendering:
        // the first oversized one rejects the page instead of silently missing
        // from the PNG (for a scanned page, the image *is* the page).
        if let Some(measured) = first_oversized_drawn_image(source_page, options.max_pixels) {
            return Err(ApplicationError::SourceViewLimitExceeded(
                SourceViewLimitDetail::new(
                    page,
                    SourceViewLimitKind::EmbeddedImagePixels,
                    Some(measured),
                    Some(options.max_pixels),
                ),
            ));
        }

        // A stream over `max_decoded_stream_bytes` reaches the interpreter as
        // the typed `StreamTooLarge` warning, never as a render error, so the
        // sink is the only place the rejection is observable — capture the
        // first one and reject after the pass instead of returning an image
        // with the oversized stream silently missing.
        let stream_limit = std::sync::Arc::new(std::sync::Mutex::new(None));
        let warning_slot = std::sync::Arc::clone(&stream_limit);
        let interpreter = InterpreterSettings {
            warning_sink: std::sync::Arc::new(move |warning| {
                if let InterpreterWarning::StreamTooLarge { observed, limit } = warning
                    && let Ok(mut slot) = warning_slot.lock()
                    && slot.is_none()
                {
                    *slot = Some((observed, limit));
                }
            }),
            ..Default::default()
        };
        let pixmap = pdf_render::render(
            source_page,
            &interpreter,
            &pdf_render::RenderSettings {
                x_scale: scale,
                y_scale: scale,
                width: Some(u16::try_from(width).expect("checked source-view width")),
                height: Some(u16::try_from(height).expect("checked source-view height")),
                bg_color: pdf_render::vello_cpu::color::palette::css::WHITE,
                ..Default::default()
            },
        );

        let stream_hit = stream_limit.lock().ok().and_then(|slot| *slot);
        if let Some((observed, limit)) = stream_hit {
            return Err(ApplicationError::SourceViewLimitExceeded(
                SourceViewLimitDetail::new(
                    page,
                    SourceViewLimitKind::DecodedStreamBytes,
                    Some(observed),
                    Some(limit),
                ),
            ));
        }

        let image = RgbaImage::from_raw(
            u32::from(pixmap.width()),
            u32::from(pixmap.height()),
            pixmap.data_as_u8_slice().to_vec(),
        )
        .ok_or_else(|| {
            ApplicationError::SourceViewFailed(
                "PDF renderer returned an invalid RGBA image buffer".into(),
            )
        })?;
        let mut encoded = Cursor::new(Vec::new());
        image
            .write_to(&mut encoded, ImageFormat::Png)
            .map_err(|error| {
                ApplicationError::SourceViewFailed(format!(
                    "failed to encode source-view PNG: {error}"
                ))
            })?;
        let encoded = encoded.into_inner();
        if encoded.len() > options.max_image_bytes {
            return Err(ApplicationError::SourceViewLimitExceeded(
                SourceViewLimitDetail::new(
                    page,
                    SourceViewLimitKind::ImageBytes,
                    Some(encoded.len() as u64),
                    Some(options.max_image_bytes as u64),
                ),
            ));
        }

        Ok(RenderedSourceView {
            media_type: MediaType("image/png".into()),
            bytes: encoded,
            width: u32::from(pixmap.width()),
            height: u32::from(pixmap.height()),
            page_count,
        })
    }
}

/// Form-XObject descent bound for the drawn-image walk. `pdf-interpret` 0.5.8
/// has no dedicated form-nesting cap: `interpret` recurses through
/// `draw_form_xobject` for every `Do` until the operator stream ends, so the
/// walk borrows the object-depth bound this renderer already configures on
/// `PdfLoadLimits` (`.max_object_depth(64)` above). A form nested deeper is
/// left for the interpreter, whose decode path keeps today's behaviour.
const DRAWN_IMAGE_MAX_FORM_DEPTH: u32 = 64;

/// Walks the content a page will draw — the page's own operators and, through
/// each `Do`, the form XObjects the interpreter descends into — and returns the
/// `Width × Height` of the first drawn image stream over `max_pixels`, the same
/// bound `pdf-syntax` enforces in `Stream::decoded_image` when the interpreter
/// later decodes it. `None` means every drawn image fits (or the page has no
/// decodable content stream, in which case rendering behaves as it does today).
///
/// The walk follows `Do` operators, never the resource dictionary, so an image
/// a shared `/Resources` names but the page never draws cannot reject the page.
/// It reads only dictionaries and content streams — image data is never
/// decoded — and skips any content branch it cannot decode or parse.
///
/// What the walk does not see; an oversized image reached only through these
/// paths still drops silently the way it does today:
/// - tiling patterns;
/// - Type3 glyph procedures;
/// - soft-mask groups in an ExtGState;
/// - annotation appearance streams — `pdf-render` does draw them
///   (`interpret_page` resolves `/AP /N` forms when `render_annotations` is on,
///   which `InterpreterSettings::default()` enables), but they sit outside the
///   page's content walk.
///
/// Inline images (`BI … ID … EI`) are skipped for a different reason:
/// pdf-syntax 0.5.6 builds an inline image's `Stream` on
/// `ReaderContext::dummy_in_content_stream`, whose `PdfLoadLimits::default()`
/// carries no caps, so `decoded_image` never applies `max_image_pixels` to it.
/// Checking an inline image here would reject a page the interpreter draws in
/// full.
fn first_oversized_drawn_image(page: &Page<'_>, max_pixels: u64) -> Option<u64> {
    let mut walk = DrawnImageWalk::default();
    walk.content(page.page_stream()?, page.resources(), max_pixels, 0)
}

#[derive(Default)]
struct DrawnImageWalk {
    /// Each form's content is walked once, keyed by object reference; the set
    /// also breaks form cycles (`/Fm0 Do` inside `/Fm0`'s own stream).
    walked_forms: HashSet<ObjectIdentifier>,
    /// Each image is measured once, keyed by object reference — a page can
    /// draw thousands of images.
    checked_images: HashSet<ObjectIdentifier>,
}

impl DrawnImageWalk {
    fn content(
        &mut self,
        content: &[u8],
        resources: &Resources<'_>,
        max_pixels: u64,
        depth: u32,
    ) -> Option<u64> {
        for instruction in TypedIter::new(content) {
            let TypedInstruction::XObject(x_object) = instruction else {
                continue;
            };
            // `get_x_object` resolves the name exactly the way the interpreter
            // does: the level's own `/XObject` dict, then the parent chain.
            let Some(stream) = resources.get_x_object(x_object.0) else {
                continue;
            };
            let dict = stream.dict();
            match dict.get::<Name>(SUBTYPE).as_deref() {
                Some(IMAGE) => {
                    if let Some(obj_id) = stream.obj_id()
                        && !self.checked_images.insert(obj_id)
                    {
                        continue;
                    }
                    if let Some(pixels) = self.oversized_image(dict, max_pixels) {
                        return Some(pixels);
                    }
                }
                Some(FORM) => {
                    if depth >= DRAWN_IMAGE_MAX_FORM_DEPTH {
                        continue;
                    }
                    if let Some(obj_id) = stream.obj_id()
                        && !self.walked_forms.insert(obj_id)
                    {
                        continue;
                    }
                    // The same decode `FormXObject::new` performs through
                    // `decode_or_warn`: a form stream that cannot be decoded
                    // (including one over `max_decoded_stream_bytes`) is
                    // skipped here; the `StreamTooLarge` warning sink still
                    // reports that path during the render pass itself.
                    let Ok(decoded) = stream.decoded() else {
                        continue;
                    };
                    // `Resources::from_parent` mirrors `draw_form_xobject`: the
                    // form's own `/Resources` over the chain in effect at the
                    // `Do` that reached it.
                    let form_resources = Resources::from_parent(
                        dict.get::<Dict<'_>>(RESOURCES).unwrap_or_default(),
                        resources.clone(),
                    );
                    if let Some(pixels) =
                        self.content(&decoded, &form_resources, max_pixels, depth + 1)
                    {
                        return Some(pixels);
                    }
                }
                _ => {}
            }
        }
        None
    }

    /// The pixel count of the first oversized raster among a drawn image's own
    /// data and its `/SMask` and `/Mask` image streams — `pdf-interpret`
    /// decodes both masks through the same pixel-limited `decoded_image` path
    /// (`x_object.rs`), so each is measured the same way.
    fn oversized_image(&mut self, dict: &Dict<'_>, max_pixels: u64) -> Option<u64> {
        if let Some(pixels) = image_pixels(dict)
            && pixels > max_pixels
        {
            return Some(pixels);
        }
        for key in [SMASK, MASK] {
            let Some(mask) = dict.get::<Stream<'_>>(key) else {
                continue;
            };
            let mask_dict = mask.dict();
            if mask_dict.get::<Name>(SUBTYPE).as_deref() != Some(IMAGE) {
                continue;
            }
            if let Some(obj_id) = mask.obj_id()
                && !self.checked_images.insert(obj_id)
            {
                continue;
            }
            if let Some(pixels) = image_pixels(mask_dict)
                && pixels > max_pixels
            {
                return Some(pixels);
            }
        }
        None
    }
}

/// `Width × Height` of an image dict the way `ImageXObject::new` reads them —
/// `/W` then `/Width`, `/H` then `/Height`, both `u32` — with the saturating
/// multiplication `decoded_image` uses. `None` covers the cases that path
/// skips too: a missing, non-integer or zero dimension (a zero-size image is
/// never drawn, so it is never decoded either).
fn image_pixels(dict: &Dict<'_>) -> Option<u64> {
    let width = dict.get::<u32>(W).or_else(|| dict.get::<u32>(WIDTH))?;
    let height = dict.get::<u32>(H).or_else(|| dict.get::<u32>(HEIGHT))?;
    if width == 0 || height == 0 {
        return None;
    }
    Some(u64::from(width).saturating_mul(u64::from(height)))
}

#[derive(Clone, Debug)]
pub struct ProcessIsolatedPdfSourceViewRenderer {
    executable: PathBuf,
    timeout: Duration,
}

impl ProcessIsolatedPdfSourceViewRenderer {
    pub fn current_executable(timeout: Duration) -> Result<Self, ApplicationError> {
        let executable = std::env::current_exe().map_err(|error| {
            ApplicationError::SourceViewFailed(format!(
                "cannot resolve source-view worker executable: {error}"
            ))
        })?;
        Ok(Self {
            executable,
            timeout,
        })
    }

    pub fn with_executable(executable: PathBuf, timeout: Duration) -> Self {
        Self {
            executable,
            timeout,
        }
    }
}

impl SourceViewRenderer for ProcessIsolatedPdfSourceViewRenderer {
    fn render(
        &self,
        bytes: Vec<u8>,
        media_type: MediaType,
        page: u32,
        options: SourceViewRenderOptions,
    ) -> Result<RenderedSourceView, ApplicationError> {
        if !media_type.is_pdf() {
            return Err(ApplicationError::SourceViewFailed(format!(
                "unsupported source-view media type {}",
                media_type.0
            )));
        }
        if self.timeout.is_zero() {
            return Err(ApplicationError::SourceViewFailed(
                "source-view worker timeout must be greater than zero".into(),
            ));
        }

        let temp = WorkerTempDir::create()?;
        let output_path = temp.path.join("page.png");
        let mut child = Command::new(&self.executable)
            .arg(SOURCE_VIEW_WORKER_FLAG)
            .arg(&output_path)
            .arg(page.to_string())
            .arg(options.dpi.to_string())
            .arg(options.max_pages.to_string())
            .arg(options.max_width.to_string())
            .arg(options.max_height.to_string())
            .arg(options.max_pixels.to_string())
            .arg(options.max_image_bytes.to_string())
            .arg(options.max_decoded_stream_bytes.to_string())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| {
                ApplicationError::SourceViewFailed(format!(
                    "failed to start isolated source-view worker: {error}"
                ))
            })?;

        let mut stdin = child.stdin.take().ok_or_else(|| {
            ApplicationError::SourceViewFailed(
                "isolated source-view worker stdin was not available".into(),
            )
        })?;
        if let Err(error) = stdin.write_all(&bytes) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(ApplicationError::SourceViewFailed(format!(
                "failed to send PDF bytes to source-view worker: {error}"
            )));
        }
        drop(stdin);

        let started = Instant::now();
        let status = loop {
            if let Some(status) = child.try_wait().map_err(|error| {
                ApplicationError::SourceViewFailed(format!(
                    "failed while waiting for source-view worker: {error}"
                ))
            })? {
                break status;
            }
            if started.elapsed() >= self.timeout {
                let _ = child.kill();
                let _ = child.wait();
                return Err(ApplicationError::SourceViewLimitExceeded(
                    SourceViewLimitDetail::new(
                        page,
                        SourceViewLimitKind::Timeout,
                        Some(started.elapsed().as_millis() as u64),
                        Some(self.timeout.as_millis() as u64),
                    ),
                ));
            }
            thread::sleep(WORKER_POLL_INTERVAL);
        };

        let stdout = read_child_pipe(child.stdout.take(), "stdout")?;
        let stderr = read_child_pipe(child.stderr.take(), "stderr")?;
        if !status.success() {
            if let Some(detail) = worker_limit_detail(&stderr, page) {
                return Err(ApplicationError::SourceViewLimitExceeded(detail));
            }
            return Err(ApplicationError::SourceViewFailed(format!(
                "isolated source-view worker exited with {status}: {}",
                bounded_diagnostic(&stderr)
            )));
        }

        let metadata: WorkerMetadata = serde_json::from_slice(&stdout).map_err(|error| {
            ApplicationError::SourceViewFailed(format!(
                "invalid source-view worker metadata: {error}"
            ))
        })?;
        let encoded = fs::read(&output_path).map_err(|error| {
            ApplicationError::SourceViewFailed(format!(
                "failed to read isolated source-view image: {error}"
            ))
        })?;
        if encoded.len() > options.max_image_bytes {
            return Err(ApplicationError::SourceViewLimitExceeded(
                SourceViewLimitDetail::new(
                    page,
                    SourceViewLimitKind::ImageBytes,
                    Some(encoded.len() as u64),
                    Some(options.max_image_bytes as u64),
                ),
            ));
        }
        if encoded.len() != metadata.image_bytes {
            return Err(ApplicationError::SourceViewFailed(format!(
                "isolated source-view image size {} violates declared size {}",
                encoded.len(),
                metadata.image_bytes
            )));
        }

        Ok(RenderedSourceView {
            media_type: MediaType("image/png".into()),
            bytes: encoded,
            width: metadata.width,
            height: metadata.height,
            page_count: metadata.page_count,
        })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct WorkerMetadata {
    width: u32,
    height: u32,
    page_count: usize,
    image_bytes: usize,
}

pub fn run_source_view_worker_if_requested() -> Result<bool, Box<dyn std::error::Error>> {
    let mut args = std::env::args_os();
    let _executable = args.next();
    let Some(mode) = args.next() else {
        return Ok(false);
    };
    if mode != SOURCE_VIEW_WORKER_FLAG {
        return Ok(false);
    }

    let output_path = PathBuf::from(next_worker_arg(&mut args, "output path")?);
    let page = parse_worker_arg::<u32>(&mut args, "page")?;
    let dpi = parse_worker_arg::<u32>(&mut args, "dpi")?;
    let max_pages = parse_worker_arg::<usize>(&mut args, "max pages")?;
    let max_width = parse_worker_arg::<u32>(&mut args, "max width")?;
    let max_height = parse_worker_arg::<u32>(&mut args, "max height")?;
    let max_pixels = parse_worker_arg::<u64>(&mut args, "max pixels")?;
    let max_image_bytes = parse_worker_arg::<usize>(&mut args, "max image bytes")?;
    let max_decoded_stream_bytes = parse_worker_arg::<u64>(&mut args, "max decoded stream bytes")?;
    if args.next().is_some() {
        return Err("source-view worker received unexpected trailing arguments".into());
    }

    let mut bytes = Vec::new();
    std::io::stdin().read_to_end(&mut bytes)?;
    let rendered = PdfSourceViewRenderer
        .render(
            bytes,
            MediaType("application/pdf".into()),
            page,
            SourceViewRenderOptions {
                dpi,
                max_pages,
                max_width,
                max_height,
                max_pixels,
                max_image_bytes,
                max_decoded_stream_bytes,
            },
        )
        .map_err(report_worker_limit)?;
    fs::write(&output_path, &rendered.bytes)?;
    let metadata = WorkerMetadata {
        width: rendered.width,
        height: rendered.height,
        page_count: rendered.page_count,
        image_bytes: rendered.bytes.len(),
    };
    print!("{}", serde_json::to_string(&metadata)?);
    Ok(true)
}

fn next_worker_arg(
    args: &mut impl Iterator<Item = OsString>,
    name: &str,
) -> Result<OsString, Box<dyn std::error::Error>> {
    args.next()
        .ok_or_else(|| format!("source-view worker is missing {name}").into())
}

fn parse_worker_arg<T>(
    args: &mut impl Iterator<Item = OsString>,
    name: &str,
) -> Result<T, Box<dyn std::error::Error>>
where
    T: std::str::FromStr,
    T::Err: std::fmt::Display,
{
    let value = next_worker_arg(args, name)?;
    let value = value
        .to_str()
        .ok_or_else(|| format!("source-view worker {name} is not valid UTF-8"))?;
    value
        .parse::<T>()
        .map_err(|error| format!("invalid source-view worker {name}: {error}").into())
}

fn read_child_pipe<R: Read>(pipe: Option<R>, label: &str) -> Result<Vec<u8>, ApplicationError> {
    let mut pipe = pipe.ok_or_else(|| {
        ApplicationError::SourceViewFailed(format!(
            "isolated source-view worker {label} was not available"
        ))
    })?;
    let mut bytes = Vec::new();
    pipe.read_to_end(&mut bytes).map_err(|error| {
        ApplicationError::SourceViewFailed(format!(
            "failed to read isolated source-view worker {label}: {error}"
        ))
    })?;
    Ok(bytes)
}

fn bounded_diagnostic(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes)
        .chars()
        .take(MAX_WORKER_DIAGNOSTIC_CHARS)
        .collect()
}

struct WorkerTempDir {
    path: PathBuf,
}

impl WorkerTempDir {
    fn create() -> Result<Self, ApplicationError> {
        let base = std::env::temp_dir();
        for _ in 0..16 {
            let counter = WORKER_TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            let path = base.join(format!(
                "reading-mcp-source-view-{}-{nanos}-{counter}",
                std::process::id()
            ));
            match fs::create_dir(&path) {
                Ok(()) => {
                    restrict_temp_permissions(&path)?;
                    return Ok(Self { path });
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => {
                    return Err(ApplicationError::SourceViewFailed(format!(
                        "failed to create source-view worker temp directory: {error}"
                    )));
                }
            }
        }
        Err(ApplicationError::SourceViewFailed(
            "failed to allocate a unique source-view worker temp directory".into(),
        ))
    }
}

impl Drop for WorkerTempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

#[cfg(unix)]
fn restrict_temp_permissions(path: &Path) -> Result<(), ApplicationError> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(|error| {
        ApplicationError::SourceViewFailed(format!(
            "failed to restrict source-view worker temp directory: {error}"
        ))
    })
}

#[cfg(not(unix))]
fn restrict_temp_permissions(_path: &Path) -> Result<(), ApplicationError> {
    Ok(())
}

fn checked_dimension(
    base: f32,
    scale: f32,
    limit: u32,
    label: &str,
    page: u32,
) -> Result<u32, ApplicationError> {
    let scaled = f64::from(base) * f64::from(scale);
    if !scaled.is_finite() || scaled <= 0.0 {
        return Err(ApplicationError::SourceViewFailed(format!(
            "PDF page has invalid {label} dimension"
        )));
    }
    let dimension = scaled.round();
    if dimension < 1.0 {
        return Err(ApplicationError::SourceViewFailed(format!(
            "PDF page has an unusable {label} dimension"
        )));
    }
    let measured = dimension as u64;
    if dimension > f64::from(limit) {
        return Err(ApplicationError::SourceViewLimitExceeded(
            SourceViewLimitDetail::new(
                page,
                SourceViewLimitKind::PageDimensions,
                Some(measured),
                Some(u64::from(limit)),
            ),
        ));
    }
    if dimension > f64::from(u16::MAX) {
        return Err(ApplicationError::SourceViewLimitExceeded(
            SourceViewLimitDetail::new(
                page,
                SourceViewLimitKind::PageDimensions,
                Some(measured),
                Some(u64::from(u16::MAX)),
            ),
        ));
    }
    u32::try_from(measured).map_err(|_| {
        ApplicationError::SourceViewLimitExceeded(SourceViewLimitDetail::new(
            page,
            SourceViewLimitKind::PageDimensions,
            Some(measured),
            Some(u64::from(u32::MAX)),
        ))
    })
}
