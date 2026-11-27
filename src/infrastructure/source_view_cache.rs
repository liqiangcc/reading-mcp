use std::fs;
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::application::ports::{
    ApplicationError, RenderedSourceView, RenderedSourceViewCache, SourceViewCacheFileFingerprint,
};
use crate::domain::MediaType;

const MAGIC: &[u8; 8] = b"RSVIMG01";
const MAX_METADATA_BYTES: usize = 16 * 1024;
static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// A single-file-per-page rendered source-view cache. Every entry carries its
/// metadata and image bytes in one atomic rename, so readers never observe a
/// half-written pair of files.
#[derive(Clone, Debug)]
pub struct FileSourceViewCache {
    directory: PathBuf,
    cap_bytes: u64,
    /// Single-entry write-through memory front cache. It is validated against
    /// the cache file's own metadata before every use, so deleting or
    /// rewriting the on-disk entry still falls through to the disk reader.
    memory: Arc<Mutex<Option<MemoryEntry>>>,
}

#[derive(Debug)]
struct MemoryEntry {
    key: String,
    view: RenderedSourceView,
    source_fingerprint: Option<SourceViewCacheFileFingerprint>,
    cache_file_fingerprint: SourceViewCacheFileFingerprint,
}

impl FileSourceViewCache {
    /// Opens the cache directory, removes temp files left by an interrupted
    /// writer, and enforces the configured cap once at startup. `cap_mib == 0`
    /// disables reads and writes; cleanup still runs so an operator flipping
    /// the knob does not inherit stale temp files.
    pub fn open(directory: impl Into<PathBuf>, cap_mib: u64) -> Result<Self, ApplicationError> {
        let cache = Self {
            directory: directory.into(),
            cap_bytes: cap_mib.saturating_mul(1024 * 1024),
            memory: Arc::new(Mutex::new(None)),
        };
        fs::create_dir_all(&cache.directory)
            .map_err(|error| cache_io_error(&cache.directory, error))?;
        cache.cleanup_temp_files()?;
        cache.prune()?;
        Ok(cache)
    }

    pub fn directory(&self) -> &Path {
        &self.directory
    }

    fn entry_path(&self, key: &str) -> PathBuf {
        self.directory.join(format!("{key}.rsv"))
    }

    /// Removes the temp files an interrupted writer left behind. Only files
    /// older than the shared threshold go, so a second process sharing the
    /// state directory keeps its in-flight write.
    fn cleanup_temp_files(&self) -> Result<(), ApplicationError> {
        crate::infrastructure::temp_cleanup::remove_stale_temp_files(&self.directory)?;
        Ok(())
    }

    /// Evicts least-recently-used entries until the cache fits its cap. The
    /// filesystem access time is the recency evidence reads maintain; where
    /// the mount does not update it on every read it still orders hits after
    /// writes closely enough for a bounded cache.
    fn prune(&self) -> Result<(), ApplicationError> {
        if self.cap_bytes == 0 {
            return Ok(());
        }
        let entries = match fs::read_dir(&self.directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(cache_io_error(&self.directory, error)),
        };
        let mut files = Vec::new();
        let mut total = 0_u64;
        for entry in entries.flatten() {
            if entry
                .path()
                .extension()
                .is_some_and(|extension| extension == "rsv")
                && let Ok(metadata) = entry.metadata()
            {
                total = total.saturating_add(metadata.len());
                let accessed = metadata
                    .accessed()
                    .ok()
                    .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
                    .map(|duration| duration.as_nanos())
                    .unwrap_or(0);
                let modified = metadata
                    .modified()
                    .ok()
                    .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
                    .map(|duration| duration.as_nanos())
                    .unwrap_or(0);
                files.push((entry.path(), metadata.len(), accessed.max(modified)));
            }
        }
        files.sort_by_key(|(_, _, recency)| *recency);
        for (path, len, _) in files {
            if total <= self.cap_bytes {
                break;
            }
            match fs::remove_file(&path) {
                Ok(()) => total = total.saturating_sub(len),
                Err(error) if error.kind() == ErrorKind::NotFound => {
                    total = total.saturating_sub(len);
                }
                Err(error) => return Err(cache_io_error(&path, error)),
            }
        }
        Ok(())
    }
    fn memory_hit(
        &self,
        key: &str,
        fingerprint: Option<&SourceViewCacheFileFingerprint>,
    ) -> Option<RenderedSourceView> {
        let path = self.entry_path(key);
        let cache_file_fingerprint = source_view_cache_file_fingerprint(&path).ok()?;
        let memory = memory_lock(&self.memory);
        let entry = memory.as_ref()?;
        if entry.key != key
            || entry.cache_file_fingerprint != cache_file_fingerprint
            || entry.source_fingerprint.as_ref() != fingerprint
        {
            return None;
        }
        // Keep the disk recency evidence hot as well: eviction is file-time
        // based, so a memory-only hit must still count as use.
        if let Ok(file) = fs::File::open(&path) {
            let times = fs::FileTimes::new().set_accessed(SystemTime::now());
            let _ = file.set_times(times);
        }
        Some(entry.view.clone())
    }

    fn remember(
        &self,
        key: &str,
        view: &RenderedSourceView,
        source_fingerprint: Option<SourceViewCacheFileFingerprint>,
    ) {
        let path = self.entry_path(key);
        let Ok(cache_file_fingerprint) = source_view_cache_file_fingerprint(&path) else {
            return;
        };
        *memory_lock(&self.memory) = Some(MemoryEntry {
            key: key.to_owned(),
            view: view.clone(),
            source_fingerprint,
            cache_file_fingerprint,
        });
    }
}

fn memory_lock(memory: &Arc<Mutex<Option<MemoryEntry>>>) -> MutexGuard<'_, Option<MemoryEntry>> {
    memory
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl RenderedSourceViewCache for FileSourceViewCache {
    fn fingerprint(&self, path: &Path) -> Result<SourceViewCacheFileFingerprint, ApplicationError> {
        source_view_cache_file_fingerprint(path)
    }

    fn get(
        &self,
        keys: &[String],
        fingerprint: Option<&SourceViewCacheFileFingerprint>,
    ) -> Result<Option<RenderedSourceView>, ApplicationError> {
        if self.cap_bytes == 0 {
            return Ok(None);
        }
        for key in keys {
            if let Some(hit) = self.memory_hit(key, fingerprint) {
                return Ok(Some(hit));
            }
            let path = self.entry_path(key);
            let encoded = match fs::read(&path) {
                Ok(encoded) => encoded,
                Err(error) if error.kind() == ErrorKind::NotFound => continue,
                Err(error) => return Err(cache_io_error(&path, error)),
            };
            let Some((metadata, body)) = decode_entry(&encoded) else {
                // A corrupt entry is cache noise, never image evidence. Remove
                // it and keep looking; the caller can still render the page.
                let _ = fs::remove_file(&path);
                continue;
            };
            if metadata.image_bytes != body.len()
                || (metadata.media_type != "image/png" && metadata.media_type != "image/jpeg")
                || fingerprint
                    .is_some_and(|expected| metadata.fingerprint.as_ref() != Some(expected))
            {
                continue;
            }
            // Best-effort LRU maintenance. A filesystem or platform that does
            // not expose per-file times still returns a correct hit.
            if let Ok(file) = fs::File::open(&path) {
                let times = fs::FileTimes::new().set_accessed(SystemTime::now());
                let _ = file.set_times(times);
            }
            let view = RenderedSourceView {
                media_type: MediaType(metadata.media_type),
                bytes: body,
                width: metadata.width,
                height: metadata.height,
                page_count: metadata.page_count,
            };
            self.remember(key, &view, fingerprint.cloned());
            return Ok(Some(view));
        }
        Ok(None)
    }

    fn put(
        &self,
        key: &str,
        view: &RenderedSourceView,
        fingerprint: Option<SourceViewCacheFileFingerprint>,
    ) -> Result<(), ApplicationError> {
        if self.cap_bytes == 0 {
            return Ok(());
        }
        fs::create_dir_all(&self.directory)
            .map_err(|error| cache_io_error(&self.directory, error))?;
        let metadata = CacheEntryMetadata {
            width: view.width,
            height: view.height,
            page_count: view.page_count,
            image_bytes: view.bytes.len(),
            media_type: view.media_type.0.clone(),
            fingerprint: fingerprint.clone(),
        };
        let metadata = serde_json::to_vec(&metadata).map_err(|error| {
            ApplicationError::CacheFailed(format!(
                "failed to encode source-view cache metadata: {error}"
            ))
        })?;
        let mut encoded = Vec::with_capacity(8 + 4 + metadata.len() + view.bytes.len());
        encoded.extend_from_slice(MAGIC);
        encoded.extend_from_slice(
            &u32::try_from(metadata.len())
                .map_err(|_| {
                    ApplicationError::CacheFailed("source-view cache metadata is too large".into())
                })?
                .to_le_bytes(),
        );
        encoded.extend_from_slice(&metadata);
        encoded.extend_from_slice(&view.bytes);

        let target = self.entry_path(key);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let sequence = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let temp = target.with_file_name(format!(
            ".{}.tmp-{}-{nanos:x}-{sequence}",
            target
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("entry.rsv"),
            std::process::id()
        ));
        let write_result = (|| {
            let mut file = fs::File::create(&temp).map_err(|error| cache_io_error(&temp, error))?;
            file.write_all(&encoded)
                .and_then(|()| file.sync_all())
                .map_err(|error| cache_io_error(&temp, error))?;
            drop(file);
            fs::rename(&temp, &target).map_err(|error| cache_io_error(&target, error))
        })();
        if write_result.is_err() {
            let _ = fs::remove_file(&temp);
        }
        write_result?;
        self.remember(key, view, fingerprint);
        self.prune()
    }
}

/// Cheap mapped-file freshness evidence. It includes the inode/device and the
/// POSIX change time where available, so replacing the file or rewriting it
/// without preserving timestamps cannot reuse an entry rendered from another
/// file. A miss still runs the worker's sha256 check before rendering.
pub fn source_view_cache_file_fingerprint(
    path: &Path,
) -> Result<SourceViewCacheFileFingerprint, ApplicationError> {
    let metadata = fs::metadata(path).map_err(|error| cache_io_error(path, error))?;
    let modified_ns = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_nanos().min(u128::from(u64::MAX)) as u64)
        .unwrap_or(0);
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Ok(SourceViewCacheFileFingerprint {
            len: metadata.len(),
            modified_ns,
            changed_ns: Some(metadata.ctime_nsec()),
            device: Some(metadata.dev()),
            inode: Some(metadata.ino()),
        })
    }
    #[cfg(not(unix))]
    {
        Ok(SourceViewCacheFileFingerprint {
            len: metadata.len(),
            modified_ns,
            changed_ns: None,
            device: None,
            inode: None,
        })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct CacheEntryMetadata {
    width: u32,
    height: u32,
    page_count: usize,
    image_bytes: usize,
    media_type: String,
    fingerprint: Option<SourceViewCacheFileFingerprint>,
}

fn decode_entry(encoded: &[u8]) -> Option<(DecodedCacheMetadata, Vec<u8>)> {
    let header_len = encoded.len().checked_sub(12)?;
    if encoded.get(..8)? != MAGIC {
        return None;
    }
    let metadata_len = u32::from_le_bytes(encoded.get(8..12)?.try_into().ok()?) as usize;
    if metadata_len > MAX_METADATA_BYTES || metadata_len > header_len {
        return None;
    }
    let metadata: CacheEntryMetadata =
        serde_json::from_slice(encoded.get(12..12 + metadata_len)?).ok()?;
    let body = encoded.get(12 + metadata_len..)?.to_vec();
    Some((
        DecodedCacheMetadata {
            width: metadata.width,
            height: metadata.height,
            page_count: metadata.page_count,
            image_bytes: metadata.image_bytes,
            media_type: metadata.media_type,
            fingerprint: metadata.fingerprint,
        },
        body,
    ))
}

struct DecodedCacheMetadata {
    width: u32,
    height: u32,
    page_count: usize,
    image_bytes: usize,
    media_type: String,
    fingerprint: Option<SourceViewCacheFileFingerprint>,
}

fn cache_io_error(path: &Path, error: std::io::Error) -> ApplicationError {
    ApplicationError::CacheFailed(format!("{}: {error}", path.display()))
}
