use std::collections::BTreeMap;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::fs;

use crate::application::ports::{
    ApplicationError, ParsedCacheKey, ParsedDocumentCache, RawResourceCache, RetrievedResource,
};
use crate::domain::{
    ContentHash, Document, DocumentId, DocumentSource, Location, MediaType, Section, SectionId,
};

#[derive(Clone, Debug)]
pub struct FileRawResourceCache {
    root: PathBuf,
}

impl FileRawResourceCache {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    fn directory(&self) -> PathBuf {
        self.root.join("raw")
    }
}

#[async_trait]
impl RawResourceCache for FileRawResourceCache {
    async fn get(
        &self,
        source: &DocumentSource,
    ) -> Result<Option<RetrievedResource>, ApplicationError> {
        let key = digest_key(source.0.as_bytes());
        let directory = self.directory();
        let metadata_path = directory.join(format!("{key}.json"));
        let body_path = directory.join(format!("{key}.bin"));

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

        let body_path = directory.join(format!("{key}.bin"));
        let metadata_path = directory.join(format!("{key}.json"));
        let metadata = RawResourceMetadata::from_resource(&resource);
        let metadata_bytes = serde_json::to_vec(&metadata).map_err(|error| {
            ApplicationError::CacheFailed(format!("failed to encode raw cache metadata: {error}"))
        })?;

        // Write the body first and metadata last. A missing metadata file therefore
        // means an interrupted entry is treated as a cache miss, not as valid data.
        fs::write(&body_path, &resource.bytes)
            .await
            .map_err(|error| cache_io_error(&body_path, error))?;
        fs::write(&metadata_path, metadata_bytes)
            .await
            .map_err(|error| cache_io_error(&metadata_path, error))?;
        Ok(())
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
        self.root.join("parsed")
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

fn parsed_key(key: &ParsedCacheKey) -> String {
    let mut input = key.final_source.0.as_bytes().to_vec();
    input.push(0);
    input.extend_from_slice(key.raw_sha256.as_bytes());
    input.push(0);
    input.extend_from_slice(key.normalization_version.as_bytes());
    digest_key(&input)
}

fn digest_key(value: &[u8]) -> String {
    format!("{:x}", Sha256::digest(value))
}

fn cache_io_error(path: &Path, error: std::io::Error) -> ApplicationError {
    ApplicationError::CacheFailed(format!("{}: {error}", path.display()))
}
