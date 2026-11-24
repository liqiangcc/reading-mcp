use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::application::ports::{
    ApplicationError, RenderedSourceView, SourceViewLimitDetail, SourceViewLimitKind,
    SourceViewRenderOptions, SourceViewRenderer,
};
use crate::domain::MediaType;

use super::PdfSourceViewRenderer;
use super::source_view::{report_worker_limit, worker_limit_detail};

const FILE_SOURCE_VIEW_WORKER_FLAG: &str = "--reading-mcp-source-view-file-worker";
const WORKER_POLL_INTERVAL: Duration = Duration::from_millis(5);
const MAX_WORKER_DIAGNOSTIC_CHARS: usize = 4_096;
static WORKER_TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug)]
pub struct FileProcessIsolatedPdfSourceViewRenderer {
    executable: PathBuf,
    timeout: Duration,
    pymupdf_python: Option<PathBuf>,
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
            pymupdf_python: None,
        })
    }

    pub fn with_executable(executable: PathBuf, timeout: Duration) -> Self {
        Self {
            executable,
            timeout,
            pymupdf_python: None,
        }
    }
    pub fn with_pymupdf(mut self, python: PathBuf) -> Self {
        self.pymupdf_python = Some(python);
        self
    }
}

impl FileProcessIsolatedPdfSourceViewRenderer {
    /// Spawns the file worker on `input_path` — the staged copy of the buffered
    /// path or the local source's own file — and collects its PNG. The worker
    /// records the sha256 of the bytes it renders as `source_sha256` in its
    /// metadata; anything other than `expected_source_sha256` (the document's
    /// `content_hash`) is a stale source and the PNG is discarded.
    fn run_worker(
        &self,
        input_path: &Path,
        expected_source_sha256: &str,
        temp: &WorkerTempDir,
        page: u32,
        options: &SourceViewRenderOptions,
    ) -> Result<RenderedSourceView, ApplicationError> {
        let output_path = temp.path.join("page.png");
        let metadata_path = temp.path.join("metadata.json");
        let stderr_path = temp.path.join("stderr.log");
        let stderr = fs::File::create(&stderr_path).map_err(|error| {
            ApplicationError::SourceViewFailed(format!(
                "failed to create source-view worker stderr file: {error}"
            ))
        })?;

        let started = Instant::now();
        let mut command = if let Some(python) = &self.pymupdf_python {
            let mut command = Command::new(python);
            command.args(["-I", "-c", include_str!("pdf_source_view_worker.py")]);
            command
        } else {
            Command::new(&self.executable)
        };
        let mut child = command
            .arg(FILE_SOURCE_VIEW_WORKER_FLAG)
            .arg(input_path)
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
            .arg(expected_source_sha256)
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

        if !status.success() {
            // A worker that stopped early on a hash mismatch still records what
            // it saw: the parent — not the exit status — decides stale vs. failed.
            if let Some(actual) = reported_source_sha256(&metadata_path)
                && actual != expected_source_sha256
            {
                return Err(ApplicationError::StaleLocator(format!(
                    "raw content hash changed from {expected_source_sha256} to {actual}"
                )));
            }
            let diagnostic = fs::read(&stderr_path).unwrap_or_default();
            if let Some(detail) = worker_limit_detail(&diagnostic, page) {
                return Err(ApplicationError::SourceViewLimitExceeded(detail));
            }
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
        // A render the worker never hashed proves nothing about identity — a
        // missing `source_sha256` is a worker failure, never a pass.
        let Some(actual_sha256) = &metadata.source_sha256 else {
            return Err(ApplicationError::SourceViewFailed(
                "isolated source-view metadata omits source_sha256".into(),
            ));
        };
        if actual_sha256 != expected_source_sha256 {
            return Err(ApplicationError::StaleLocator(format!(
                "raw content hash changed from {expected_source_sha256} to {actual_sha256}"
            )));
        }
        validate_metadata(&metadata, options, page)?;

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

impl SourceViewRenderer for FileProcessIsolatedPdfSourceViewRenderer {
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
        let input_path = temp.path.join("source.pdf");
        fs::write(&input_path, &bytes).map_err(|error| {
            ApplicationError::SourceViewFailed(format!(
                "failed to stage source-view PDF bytes: {error}"
            ))
        })?;
        // The staged file is the worker's copy: verify it end-to-end and release
        // the heap buffer before the worker runs instead of holding two copies.
        let expected_source_sha256 = format!("sha256:{:x}", Sha256::digest(&bytes));
        drop(bytes);
        self.run_worker(&input_path, &expected_source_sha256, &temp, page, &options)
    }

    fn render_local_file(
        &self,
        path: &Path,
        expected_source_sha256: &str,
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

        // No staging at all: the worker maps `path` itself — the only file copy
        // it ever touches — so a large local PDF costs no heap buffer and no
        // tmpfs copy in the parent.
        let temp = WorkerTempDir::create()?;
        self.run_worker(path, expected_source_sha256, &temp, page, &options)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct WorkerMetadata {
    width: u32,
    height: u32,
    page_count: usize,
    image_bytes: usize,
    #[serde(default)]
    source_sha256: Option<String>,
}

/// The `source_sha256` a worker recorded in its metadata file, when it wrote
/// one — the hash-mismatch evidence of a worker that stopped before parsing,
/// or the identity proof of one that rendered.
fn reported_source_sha256(metadata_path: &Path) -> Option<String> {
    #[derive(Deserialize)]
    struct SourceSha256Probe {
        source_sha256: Option<String>,
    }
    let bytes = fs::read(metadata_path).ok()?;
    serde_json::from_slice::<SourceSha256Probe>(&bytes)
        .ok()?
        .source_sha256
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
    // The expected hash is optional in the protocol: the parent always passes
    // it, but an older caller without it still gets the hash recorded — the
    // parent's own compare is the authoritative check either way.
    let expected_source_sha256 = args.next().and_then(|value| value.into_string().ok());
    if args.next().is_some() {
        return Err("source-view worker received unexpected trailing arguments".into());
    }

    // The input is mapped read-only, never read into a heap buffer: its pages
    // stay clean and file-backed, so memory pressure drops them instead of
    // swapping them. A concurrent truncation of the file would fault the map —
    // a loud failure, not a wrong render.
    let file = fs::File::open(&input_path)?;
    let map = unsafe { memmap2::Mmap::map(&file) }?;
    let source_sha256 = format!("sha256:{:x}", Sha256::digest(&map[..]));
    // Residual window: a file modified in place between this hash and the
    // parser's lazy reads can produce one render of mixed bytes — the map holds
    // no snapshot. The next call hashes the file again and fails closed.
    if let Some(expected) = &expected_source_sha256
        && source_sha256 != *expected
    {
        // Stop before parsing and leave the observed hash for the parent, which
        // turns it into the fail-closed stale-source answer.
        fs::write(
            &metadata_path,
            serde_json::to_vec(&serde_json::json!({ "source_sha256": source_sha256 }))?,
        )?;
        return Err(format!(
            "source-view input sha256 {source_sha256} does not match expected {expected:?}"
        )
        .into());
    }
    let rendered = PdfSourceViewRenderer
        .render_data(
            std::sync::Arc::new(map),
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
        source_sha256: Some(source_sha256),
    };
    fs::write(&metadata_path, serde_json::to_vec(&metadata)?)?;
    Ok(true)
}

fn validate_metadata(
    metadata: &WorkerMetadata,
    options: &SourceViewRenderOptions,
    page: u32,
) -> Result<(), ApplicationError> {
    let limit = |kind, measured, limit| {
        ApplicationError::SourceViewLimitExceeded(SourceViewLimitDetail::new(
            page,
            kind,
            Some(measured),
            Some(limit),
        ))
    };
    if metadata.page_count > options.max_pages {
        return Err(limit(
            SourceViewLimitKind::PageCount,
            metadata.page_count as u64,
            options.max_pages as u64,
        ));
    }
    if metadata.width == 0 || metadata.height == 0 {
        return Err(ApplicationError::SourceViewFailed(format!(
            "isolated source-view metadata declares invalid dimensions {}x{}",
            metadata.width, metadata.height
        )));
    }
    if metadata.width > options.max_width {
        return Err(limit(
            SourceViewLimitKind::PageDimensions,
            u64::from(metadata.width),
            u64::from(options.max_width),
        ));
    }
    if metadata.height > options.max_height {
        return Err(limit(
            SourceViewLimitKind::PageDimensions,
            u64::from(metadata.height),
            u64::from(options.max_height),
        ));
    }
    let pixels = u64::from(metadata.width)
        .checked_mul(u64::from(metadata.height))
        .ok_or_else(|| {
            ApplicationError::SourceViewLimitExceeded(SourceViewLimitDetail::new(
                page,
                SourceViewLimitKind::RenderedPixels,
                None,
                Some(options.max_pixels),
            ))
        })?;
    if pixels > options.max_pixels {
        return Err(limit(
            SourceViewLimitKind::RenderedPixels,
            pixels,
            options.max_pixels,
        ));
    }
    if metadata.image_bytes > options.max_image_bytes {
        return Err(limit(
            SourceViewLimitKind::ImageBytes,
            metadata.image_bytes as u64,
            options.max_image_bytes as u64,
        ));
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
