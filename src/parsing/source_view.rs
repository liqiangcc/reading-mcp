use std::ffi::OsString;
use std::fs;
use std::io::{Cursor, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use image::{ImageFormat, RgbaImage};
use pdf_render::pdf_interpret::InterpreterSettings;
use pdf_render::pdf_syntax::{Pdf, PdfLoadLimits};
use serde::{Deserialize, Serialize};

use crate::application::ports::{
    ApplicationError, RenderedSourceView, SourceViewRenderOptions, SourceViewRenderer,
};
use crate::domain::MediaType;

const SOURCE_VIEW_WORKER_FLAG: &str = "--reading-mcp-source-view-worker";
const WORKER_POLL_INTERVAL: Duration = Duration::from_millis(5);
const MAX_WORKER_DIAGNOSTIC_CHARS: usize = 4_096;
static WORKER_TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

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
        if !is_pdf(&media_type) {
            return Err(ApplicationError::SourceViewFailed(format!(
                "unsupported source-view media type {}",
                media_type.0
            )));
        }

        let load_limits = PdfLoadLimits::new()
            .max_object_depth(64)
            .max_image_pixels(options.max_pixels)
            .max_stream_bytes(options.max_decoded_stream_bytes);
        let pdf = Pdf::new_with_limits(bytes, load_limits).map_err(|error| {
            ApplicationError::SourceViewFailed(format!("invalid PDF source view: {error:?}"))
        })?;
        let pages = pdf.pages();
        let page_count = pages.len();
        if page_count > options.max_pages {
            return Err(ApplicationError::ResourceLimitExceeded(format!(
                "PDF has {page_count} pages; source-view limit is {} pages",
                options.max_pages
            )));
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
        let width = checked_dimension(base_width, scale, options.max_width, "width")?;
        let height = checked_dimension(base_height, scale, options.max_height, "height")?;
        let pixels = u64::from(width)
            .checked_mul(u64::from(height))
            .ok_or_else(|| {
                ApplicationError::ResourceLimitExceeded("source-view pixel count overflowed".into())
            })?;
        if pixels > options.max_pixels {
            return Err(ApplicationError::ResourceLimitExceeded(format!(
                "source-view image has {pixels} pixels; limit is {}",
                options.max_pixels
            )));
        }

        let pixmap = pdf_render::render(
            source_page,
            &InterpreterSettings::default(),
            &pdf_render::RenderSettings {
                x_scale: scale,
                y_scale: scale,
                width: Some(u16::try_from(width).expect("checked source-view width")),
                height: Some(u16::try_from(height).expect("checked source-view height")),
                bg_color: pdf_render::vello_cpu::color::palette::css::WHITE,
                ..Default::default()
            },
        );

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
            return Err(ApplicationError::ResourceLimitExceeded(format!(
                "source-view image is {} bytes; limit is {} bytes",
                encoded.len(),
                options.max_image_bytes
            )));
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
        if !is_pdf(&media_type) {
            return Err(ApplicationError::SourceViewFailed(format!(
                "unsupported source-view media type {}",
                media_type.0
            )));
        }
        if self.timeout.is_zero() {
            return Err(ApplicationError::ResourceLimitExceeded(
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
                return Err(ApplicationError::ResourceLimitExceeded(format!(
                    "source view renderer exceeded {:?} timeout and was terminated",
                    self.timeout
                )));
            }
            thread::sleep(WORKER_POLL_INTERVAL);
        };

        let stdout = read_child_pipe(child.stdout.take(), "stdout")?;
        let stderr = read_child_pipe(child.stderr.take(), "stderr")?;
        if !status.success() {
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
        if encoded.len() > options.max_image_bytes || encoded.len() != metadata.image_bytes {
            return Err(ApplicationError::ResourceLimitExceeded(format!(
                "isolated source-view image size {} violates declared/maximum size",
                encoded.len()
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
    if mode != OsString::from(SOURCE_VIEW_WORKER_FLAG) {
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
    let max_decoded_stream_bytes =
        parse_worker_arg::<u64>(&mut args, "max decoded stream bytes")?;
    if args.next().is_some() {
        return Err("source-view worker received unexpected trailing arguments".into());
    }

    let mut bytes = Vec::new();
    std::io::stdin().read_to_end(&mut bytes)?;
    let rendered = PdfSourceViewRenderer.render(
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
    )?;
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
    if dimension > f64::from(limit) {
        return Err(ApplicationError::ResourceLimitExceeded(format!(
            "source-view image {label} is {dimension:.0}px; limit is {limit}px"
        )));
    }
    if dimension > f64::from(u16::MAX) {
        return Err(ApplicationError::ResourceLimitExceeded(format!(
            "source-view image {label} exceeds renderer dimensions"
        )));
    }
    u32::try_from(dimension as u64).map_err(|_| {
        ApplicationError::ResourceLimitExceeded(format!(
            "source-view image {label} exceeds supported dimensions"
        ))
    })
}

fn is_pdf(media_type: &MediaType) -> bool {
    media_type
        .0
        .split(';')
        .next()
        .is_some_and(|value| value.trim().eq_ignore_ascii_case("application/pdf"))
}
