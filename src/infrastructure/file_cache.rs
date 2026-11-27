use std::collections::BTreeMap;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::fs;

use crate::application::ports::{
    ApplicationError, LocatedRawResource, ParsedCacheKey, ParsedDocumentCache, RawResourceCache,
    RetrievedResource,
};
use crate::domain::{
    ContentHash, Document, DocumentId, DocumentSource, Location, MediaType, Section, SectionId,
};

const MIB: u64 = 1024 * 1024;

/// A file-per-entry raw resource cache: one `<key>.bin` body and one
/// `<key>.json` metadata file per URL source, both replaced by rename (#275).
///
/// The cache is bounded by a size cap that evicts whole entries — body and
/// metadata together — least recently used first, and it removes the temp files
/// an interrupted writer leaves behind at startup.
#[derive(Clone, Debug)]
pub struct FileRawResourceCache {
    root: PathBuf,
    cap_bytes: u64,
}

impl FileRawResourceCache {
    /// `READING_MCP_RAW_CACHE_MIB`'s default cap, in MiB.
    pub const DEFAULT_CAP_MIB: u64 = 1024;

    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self::with_cap_mib(root, Self::DEFAULT_CAP_MIB)
    }

    /// Opens the cache with `cap_mib` as its size cap (`0` = no cap, which is
    /// the behaviour before the cap existed), then tidies once: leftover temp
    /// files are removed and an inherited over-cap cache is evicted down to the
    /// cap, so a cache that grew before the cap was configured shrinks at
    /// startup rather than at the next write. Both steps are best effort —
    /// hygiene must never stop the server from serving.
    pub fn with_cap_mib(root: impl Into<PathBuf>, cap_mib: u64) -> Self {
        let cache = Self {
            root: root.into(),
            cap_bytes: cap_mib.saturating_mul(MIB),
        };
        if let Err(error) =
            crate::infrastructure::temp_cleanup::remove_stale_temp_files(&cache.directory())
        {
            eprintln!("warning: failed to clean raw cache temp files: {error}");
        }
        if let Err(error) = cache.prune() {
            eprintln!("warning: failed to prune the raw cache: {error}");
        }
        cache
    }

    fn directory(&self) -> PathBuf {
        self.root.join("raw")
    }

    fn body_path(&self, key: &str) -> PathBuf {
        self.directory().join(format!("{key}.bin"))
    }

    fn metadata_path(&self, key: &str) -> PathBuf {
        self.directory().join(format!("{key}.json"))
    }

    /// Evicts whole entries until the cache fits `cap_bytes`. `0` disables the
    /// cap, which is today's unbounded behaviour.
    ///
    /// Recency is the newest of the entry's two files' access and modification
    /// times. Reads refresh the access time explicitly (see [`mark_used`]), so
    /// the order is "last used", not "last written", and it survives mounts
    /// that never update access times implicitly (`noatime`, `relatime`); where
    /// a platform refuses the update the modification time still orders an
    /// entry after everything written before it. A body that was written but
    /// whose metadata never landed is not an entry (the reader treats it as a
    /// miss), so it is garbage and goes before any whole entry.
    ///
    /// Each eviction removes the metadata first: its presence is what makes an
    /// entry exist, so a reader that races the eviction reads a missing
    /// metadata file (clean miss) or the missing body (also a clean miss) and
    /// never a body that claims metadata it does not have. A reader that
    /// already opened or mapped the body keeps its inode and bytes.
    fn prune(&self) -> Result<(), ApplicationError> {
        if self.cap_bytes == 0 {
            return Ok(());
        }
        let directory = self.directory();
        let entries = match std::fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(cache_io_error(&directory, error)),
        };
        let mut candidates: BTreeMap<String, Candidate> = BTreeMap::new();
        let mut total = 0_u64;
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            // Temp files are transient — startup cleanup owns them — and are
            // not part of any entry's size.
            if name.contains(".tmp-") {
                continue;
            }
            let (key, is_body) = match name.strip_suffix(".bin") {
                Some(key) => (key, true),
                None => match name.strip_suffix(".json") {
                    Some(key) => (key, false),
                    None => continue,
                },
            };
            let Ok(metadata) = entry.metadata() else {
                continue;
            };
            let len = metadata.len();
            total = total.saturating_add(len);
            let candidate = candidates.entry(key.to_owned()).or_default();
            candidate.recency = candidate.recency.max(recency(&metadata));
            let file = FileRef {
                path: entry.path(),
                len,
            };
            if is_body {
                candidate.body = Some(file);
            } else {
                candidate.metadata = Some(file);
            }
        }
        let mut candidates: Vec<Candidate> = candidates.into_values().collect();
        // Incomplete entries first (garbage), then least recently used.
        candidates.sort_by_key(|candidate| (candidate.is_complete(), candidate.recency));
        for candidate in candidates {
            if total <= self.cap_bytes {
                break;
            }
            for file in candidate.metadata.iter().chain(candidate.body.iter()) {
                match std::fs::remove_file(&file.path) {
                    Ok(()) => total = total.saturating_sub(file.len),
                    Err(error) if error.kind() == ErrorKind::NotFound => {
                        total = total.saturating_sub(file.len)
                    }
                    Err(error) => return Err(cache_io_error(&file.path, error)),
                }
            }
        }
        Ok(())
    }
}

#[derive(Default)]
struct Candidate {
    body: Option<FileRef>,
    metadata: Option<FileRef>,
    recency: u128,
}

impl Candidate {
    fn is_complete(&self) -> bool {
        self.body.is_some() && self.metadata.is_some()
    }
}

struct FileRef {
    path: PathBuf,
    len: u64,
}

/// The newest timestamp the file carries, in nanoseconds since the epoch.
fn recency(metadata: &std::fs::Metadata) -> u128 {
    let nanos = |time: std::io::Result<SystemTime>| {
        time.ok()
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .map(|duration| duration.as_nanos())
            .unwrap_or(0)
    };
    nanos(metadata.accessed()).max(nanos(metadata.modified()))
}

/// Records that the entry's files were used now, which is what eviction orders
/// by. Best effort: a filesystem that refuses the update leaves the entry
/// ordered by its write time.
fn mark_used(path: &Path) {
    if let Ok(file) = std::fs::File::open(path) {
        let times = std::fs::FileTimes::new().set_accessed(SystemTime::now());
        let _ = file.set_times(times);
    }
}

fn mark_entry_used(metadata_path: &Path, body_path: &Path) {
    mark_used(metadata_path);
    mark_used(body_path);
}

#[async_trait]
impl RawResourceCache for FileRawResourceCache {
    async fn get(
        &self,
        source: &DocumentSource,
    ) -> Result<Option<RetrievedResource>, ApplicationError> {
        let key = digest_key(source.0.as_bytes());
        let metadata_path = self.metadata_path(&key);
        let body_path = self.body_path(&key);

        let metadata_bytes = match fs::read(&metadata_path).await {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(cache_io_error(&metadata_path, error)),
        };
        let body = match fs::read(&body_path).await {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(cache_io_error(&body_path, error)),
        };
        let metadata: RawResourceMetadata =
            serde_json::from_slice(&metadata_bytes).map_err(|error| {
                ApplicationError::CacheFailed(format!(
                    "failed to decode {}: {error}",
                    metadata_path.display()
                ))
            })?;

        // A hit is a use: this is what keeps a re-read document out of the
        // eviction order's front.
        mark_entry_used(&metadata_path, &body_path);
        Ok(Some(metadata.into_resource(body)))
    }

    async fn put(
        &self,
        source: &DocumentSource,
        resource: RetrievedResource,
    ) -> Result<(), ApplicationError> {
        let key = digest_key(source.0.as_bytes());
        let directory = self.directory();
        fs::create_dir_all(&directory)
            .await
            .map_err(|error| cache_io_error(&directory, error))?;

        let body_path = self.body_path(&key);
        let metadata_path = self.metadata_path(&key);
        let metadata = RawResourceMetadata::from_resource(&resource);
        let metadata_bytes = serde_json::to_vec(&metadata).map_err(|error| {
            ApplicationError::CacheFailed(format!("failed to encode raw cache metadata: {error}"))
        })?;

        // Write each file to a unique temp name in the same directory, then
        // rename it into place — body first, metadata last as today. A worker
        // that mapped the old body keeps its inode and bytes, and a missing
        // metadata file still means an interrupted entry is treated as a
        // cache miss, not as valid data.
        write_replacing(&body_path, &resource.bytes).await?;
        write_replacing(&metadata_path, &metadata_bytes).await?;
        // Enforce the cap on the write path, so a long-running server stays
        // bounded without waiting for the next start. An entry bigger than the
        // whole cap is not retained.
        self.prune()
    }

    async fn locate(
        &self,
        source: &DocumentSource,
    ) -> Result<Option<LocatedRawResource>, ApplicationError> {
        let key = digest_key(source.0.as_bytes());
        let metadata_path = self.metadata_path(&key);
        let body_path = self.body_path(&key);

        // Anything short of a clean locate is "no usable entry": the caller's
        // fallback reads the entry through `get`, which reports a corrupt or
        // unreadable file exactly as the retrieval path does.
        let Ok(metadata_bytes) = fs::read(&metadata_path).await else {
            return Ok(None);
        };
        let Ok(metadata) = serde_json::from_slice::<RawResourceMetadata>(&metadata_bytes) else {
            return Ok(None);
        };
        let Ok(body) = fs::metadata(&body_path).await else {
            return Ok(None);
        };
        // `locate` is how `source_view` reaches a cached URL body without
        // reading it, so a clean locate is a use even though the bytes are not
        // read here; a view of a document must not age it out of the cache.
        mark_entry_used(&metadata_path, &body_path);
        Ok(Some(LocatedRawResource {
            body_path,
            final_source: DocumentSource(metadata.final_source),
            media_type: MediaType(metadata.media_type),
            body_len: body.len(),
        }))
    }
}

#[derive(Clone, Debug)]
pub struct FileParsedDocumentCache {
    root: PathBuf,
}

impl FileParsedDocumentCache {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    fn directory(&self) -> PathBuf {
        self.root.join("parsed-v2")
    }
}

#[async_trait]
impl ParsedDocumentCache for FileParsedDocumentCache {
    async fn get(&self, key: &ParsedCacheKey) -> Result<Option<Document>, ApplicationError> {
        let file_key = parsed_key(key);
        let path = self.directory().join(format!("{file_key}.json"));
        let bytes = match fs::read(&path).await {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(cache_io_error(&path, error)),
        };
        let document: CachedDocument = serde_json::from_slice(&bytes).map_err(|error| {
            ApplicationError::CacheFailed(format!("failed to decode {}: {error}", path.display()))
        })?;
        Ok(Some(document.into_document()))
    }

    async fn put(&self, key: ParsedCacheKey, document: Document) -> Result<(), ApplicationError> {
        let directory = self.directory();
        fs::create_dir_all(&directory)
            .await
            .map_err(|error| cache_io_error(&directory, error))?;
        let file_key = parsed_key(&key);
        let path = directory.join(format!("{file_key}.json"));
        let bytes =
            serde_json::to_vec(&CachedDocument::from_document(&document)).map_err(|error| {
                ApplicationError::CacheFailed(format!(
                    "failed to encode parsed cache entry: {error}"
                ))
            })?;
        fs::write(&path, bytes)
            .await
            .map_err(|error| cache_io_error(&path, error))?;
        Ok(())
    }
}

#[derive(Serialize, Deserialize)]
struct RawResourceMetadata {
    source: String,
    final_source: String,
    media_type: String,
    etag: Option<String>,
    last_modified: Option<String>,
    metadata: BTreeMap<String, String>,
}

impl RawResourceMetadata {
    fn from_resource(resource: &RetrievedResource) -> Self {
        Self {
            source: resource.source.0.clone(),
            final_source: resource.final_source.0.clone(),
            media_type: resource.media_type.0.clone(),
            etag: resource.etag.clone(),
            last_modified: resource.last_modified.clone(),
            metadata: resource.metadata.clone(),
        }
    }

    fn into_resource(self, bytes: Vec<u8>) -> RetrievedResource {
        RetrievedResource {
            source: DocumentSource(self.source),
            final_source: DocumentSource(self.final_source),
            media_type: MediaType(self.media_type),
            bytes,
            etag: self.etag,
            last_modified: self.last_modified,
            metadata: self.metadata,
        }
    }
}

#[derive(Serialize, Deserialize)]
struct CachedDocument {
    id: String,
    source: String,
    title: String,
    media_type: String,
    content_hash: String,
    metadata: BTreeMap<String, String>,
    root_sections: Vec<CachedSection>,
}

impl CachedDocument {
    fn from_document(document: &Document) -> Self {
        Self {
            id: document.id.0.clone(),
            source: document.source.0.clone(),
            title: document.title.clone(),
            media_type: document.media_type.0.clone(),
            content_hash: document.content_hash.0.clone(),
            metadata: document.metadata.clone(),
            root_sections: document
                .root_sections
                .iter()
                .map(CachedSection::from_section)
                .collect(),
        }
    }

    fn into_document(self) -> Document {
        Document {
            id: DocumentId(self.id),
            source: DocumentSource(self.source),
            title: self.title,
            media_type: MediaType(self.media_type),
            content_hash: ContentHash(self.content_hash),
            metadata: self.metadata,
            root_sections: self
                .root_sections
                .into_iter()
                .map(CachedSection::into_section)
                .collect(),
        }
    }
}

#[derive(Serialize, Deserialize)]
struct CachedSection {
    id: String,
    parent_id: Option<String>,
    title: String,
    level: u8,
    content: String,
    location: CachedLocation,
    children: Vec<CachedSection>,
}

impl CachedSection {
    fn from_section(section: &Section) -> Self {
        Self {
            id: section.id.0.clone(),
            parent_id: section.parent_id.as_ref().map(|id| id.0.clone()),
            title: section.title.clone(),
            level: section.level,
            content: section.content.clone(),
            location: CachedLocation::from_location(&section.location),
            children: section
                .children
                .iter()
                .map(CachedSection::from_section)
                .collect(),
        }
    }

    fn into_section(self) -> Section {
        Section {
            id: SectionId(self.id),
            parent_id: self.parent_id.map(SectionId),
            title: self.title,
            level: self.level,
            content: self.content,
            location: self.location.into_location(),
            children: self
                .children
                .into_iter()
                .map(CachedSection::into_section)
                .collect(),
        }
    }
}

#[derive(Serialize, Deserialize)]
struct CachedLocation {
    page: Option<u32>,
    chapter: Option<String>,
    section_path: Vec<String>,
    anchor: Option<String>,
    paragraph: Option<u32>,
    char_start: Option<usize>,
    char_end: Option<usize>,
    native_location: Option<String>,
}

impl CachedLocation {
    fn from_location(location: &Location) -> Self {
        Self {
            page: location.page,
            chapter: location.chapter.clone(),
            section_path: location.section_path.clone(),
            anchor: location.anchor.clone(),
            paragraph: location.paragraph,
            char_start: location.char_start,
            char_end: location.char_end,
            native_location: location.native_location.clone(),
        }
    }

    fn into_location(self) -> Location {
        Location {
            page: self.page,
            chapter: self.chapter,
            section_path: self.section_path,
            anchor: self.anchor,
            paragraph: self.paragraph,
            char_start: self.char_start,
            char_end: self.char_end,
            native_location: self.native_location,
        }
    }
}

/// Writes `bytes` to a unique temporary file next to `target`, then renames it
/// into place: a reader that still holds `target`'s old file keeps its inode
/// and bytes. The temporary file is removed when either step fails.
async fn write_replacing(target: &Path, bytes: &[u8]) -> Result<(), ApplicationError> {
    static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let sequence = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let name = target
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("entry");
    let temp = target.with_file_name(format!(
        ".{name}.tmp-{}-{nanos:x}-{sequence}",
        std::process::id()
    ));
    if let Err(error) = fs::write(&temp, bytes).await {
        let _ = fs::remove_file(&temp).await;
        return Err(cache_io_error(&temp, error));
    }
    if let Err(error) = fs::rename(&temp, target).await {
        let _ = fs::remove_file(&temp).await;
        return Err(cache_io_error(target, error));
    }
    Ok(())
}

fn parsed_key(key: &ParsedCacheKey) -> String {
    let mut input = key.final_source.0.as_bytes().to_vec();
    input.push(0);
    input.extend_from_slice(key.raw_sha256.as_bytes());
    input.push(0);
    input.extend_from_slice(key.normalization_version.as_bytes());
    input.push(0);
    input.extend_from_slice(key.ocr_fingerprint.as_bytes());
    digest_key(&input)
}

fn digest_key(value: &[u8]) -> String {
    format!("{:x}", Sha256::digest(value))
}

fn cache_io_error(path: &Path, error: std::io::Error) -> ApplicationError {
    ApplicationError::CacheFailed(format!("{}: {error}", path.display()))
}
