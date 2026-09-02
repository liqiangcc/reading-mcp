use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::application::ports::{
    ApplicationError, RenderedSourceView, SourceViewRenderOptions, SourceViewRenderer,
};
use crate::domain::MediaType;

use super::PdfSourceViewRenderer;

const FILE_SOURCE_VIEW_WORKER_FLAG: &str = "--reading-mcp-source-view-file-worker";
const WORKER_POLL_INTERVAL: Duration = Duration::from_millis(5);
const MAX_WORKER_DIAGNOSTIC_CHARS: usize = 4_096;
static WORKER_TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug)]
pub struct FileProcessIsolatedPdfSourceViewRenderer {
    executable: PathBuf,
    timeout: Duration,
}

impl FileProcessIsolatedPdfSourceViewRenderer {
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

impl SourceViewRenderer for FileProcessIsolatedPdfSourceViewRenderer {
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
        let input_path = temp.path.join("source.pdf");
        let output_path = temp.path.join("page.png");
        let metadata_path = temp.path.join("metadata.json");
        let stderr_path = temp.path.join("stderr.log");
        fs::write(&input_path, &bytes).map_err(|error| {
            ApplicationError::SourceViewFailed(format!(
                "failed to stage source-view PDF bytes: {error}"
            ))
        })?;
        let stderr = fs::File::create(&stderr_path).map_err(|error| {
            ApplicationError::SourceViewFailed(format!(
                "failed to create source-view worker stderr file: {error}"
            ))
        })?;

        let started = Instant::now();
        let mut child = Command::new(&self.executable)
            .arg(FILE_SOURCE_VIEW_WORKER_FLAG)
            .arg(&input_path)
            .arg(&output_path)
            .arg(&metadata_path)
            .arg(page.to_string())
            .arg(options.dpi.to_string())
            .arg(options.max_pages.to_string())
            .arg(options.max_width.to_string())
            .arg(options.max_height.to_string())
            .arg(options.max_pixels.to_string())
            .arg(options.max_image_bytes.to_string())
            .arg(options.max_decoded_stream_bytes.to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::from(stderr))
            .spawn()
            .map_err(|error| {
                ApplicationError::SourceViewFailed(format!(
                    "failed to start isolated source-view worker: {error}"
                ))
            })?;

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

        if !status.success() {
            let diagnostic = fs::read(&stderr_path).unwrap_or_default();
            return Err(ApplicationError::SourceViewFailed(format!(
                "isolated source-view worker exited with {status}: {}",
                bounded_diagnostic(&diagnostic)
            )));
        }

        let metadata_bytes = fs::read(&metadata_path).map_err(|error| {
            ApplicationError::SourceViewFailed(format!(
                "failed to read isolated source-view metadata: {error}"
            ))
        })?;
        let metadata: WorkerMetadata =
            serde_json::from_slice(&metadata_bytes).map_err(|error| {
                ApplicationError::SourceViewFailed(format!(
                    "invalid source-view worker metadata: {error}"
                ))
            })?;
        validate_metadata(&metadata, &options)?;

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

pub fn run_file_source_view_worker_if_requested() -> Result<bool, Box<dyn std::error::Error>> {
    let mut args = std::env::args_os();
    let _executable = args.next();
    let Some(mode) = args.next() else {
        return Ok(false);
    };
    if mode != FILE_SOURCE_VIEW_WORKER_FLAG {
        return Ok(false);
    }

    let input_path = PathBuf::from(next_worker_arg(&mut args, "input path")?);
    let output_path = PathBuf::from(next_worker_arg(&mut args, "output path")?);
    let metadata_path = PathBuf::from(next_worker_arg(&mut args, "metadata path")?);
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

    let bytes = fs::read(&input_path)?;
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
    fs::write(&metadata_path, serde_json::to_vec(&metadata)?)?;
    Ok(true)
}

fn validate_metadata(
    metadata: &WorkerMetadata,
    options: &SourceViewRenderOptions,
) -> Result<(), ApplicationError> {
    if metadata.page_count > options.max_pages {
        return Err(ApplicationError::ResourceLimitExceeded(format!(
            "isolated source-view page count {} exceeds limit {}",
            metadata.page_count, options.max_pages
        )));
    }
    if metadata.width == 0
        || metadata.height == 0
        || metadata.width > options.max_width
        || metadata.height > options.max_height
    {
        return Err(ApplicationError::ResourceLimitExceeded(format!(
            "isolated source-view dimensions {}x{} violate configured limits",
            metadata.width, metadata.height
        )));
    }
    let pixels = u64::from(metadata.width)
        .checked_mul(u64::from(metadata.height))
        .ok_or_else(|| {
            ApplicationError::ResourceLimitExceeded(
                "isolated source-view pixel count overflowed".into(),
            )
        })?;
    if pixels > options.max_pixels {
        return Err(ApplicationError::ResourceLimitExceeded(format!(
            "isolated source-view image has {pixels} pixels; limit is {}",
            options.max_pixels
        )));
    }
    if metadata.image_bytes > options.max_image_bytes {
        return Err(ApplicationError::ResourceLimitExceeded(format!(
            "isolated source-view metadata declares {} image bytes; limit is {}",
            metadata.image_bytes, options.max_image_bytes
        )));
    }
    Ok(())
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
                "reading-mcp-source-view-file-{}-{nanos}-{counter}",
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

fn is_pdf(media_type: &MediaType) -> bool {
    media_type
        .0
        .split(';')
        .next()
        .is_some_and(|value| value.trim().eq_ignore_ascii_case("application/pdf"))
}
