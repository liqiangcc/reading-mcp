use std::io::{Read, Seek};

use zip::ZipArchive;

use crate::application::ports::ApplicationError;

#[derive(Clone, Debug)]
pub struct ArchiveLimits {
    pub max_entries: usize,
    pub max_entry_bytes: usize,
    pub max_total_bytes: usize,
}

impl Default for ArchiveLimits {
    fn default() -> Self {
        Self {
            max_entries: 10_000,
            max_entry_bytes: 16 * 1024 * 1024,
            max_total_bytes: 64 * 1024 * 1024,
        }
    }
}

pub(crate) fn validate_archive_entries<R: Read + Seek>(
    archive: &ZipArchive<R>,
    limits: &ArchiveLimits,
) -> Result<(), ApplicationError> {
    if archive.len() > limits.max_entries {
        return Err(ApplicationError::ResourceLimitExceeded(format!(
            "archive contains {} entries; limit is {}",
            archive.len(),
            limits.max_entries
        )));
    }
    Ok(())
}

pub(crate) fn read_entry<R: Read + Seek>(
    archive: &mut ZipArchive<R>,
    name: &str,
    limits: &ArchiveLimits,
    total_read: &mut usize,
) -> Result<Vec<u8>, ApplicationError> {
    let file = archive.by_name(name).map_err(|error| {
        ApplicationError::ParseFailed(format!("archive entry {name:?} is unavailable: {error}"))
    })?;
    let declared = usize::try_from(file.size()).unwrap_or(usize::MAX);
    if declared > limits.max_entry_bytes {
        return Err(ApplicationError::ResourceLimitExceeded(format!(
            "archive entry {name:?} is {declared} bytes; per-entry limit is {}",
            limits.max_entry_bytes
        )));
    }
    if total_read.saturating_add(declared) > limits.max_total_bytes {
        return Err(ApplicationError::ResourceLimitExceeded(format!(
            "archive decompressed data exceeds {} bytes",
            limits.max_total_bytes
        )));
    }

    let mut bytes = Vec::with_capacity(declared.min(limits.max_entry_bytes));
    file.take((limits.max_entry_bytes + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|error| {
            ApplicationError::ParseFailed(format!("failed reading archive entry {name:?}: {error}"))
        })?;
    if bytes.len() > limits.max_entry_bytes {
        return Err(ApplicationError::ResourceLimitExceeded(format!(
            "archive entry {name:?} exceeds {} bytes after decompression",
            limits.max_entry_bytes
        )));
    }
    *total_read = total_read.saturating_add(bytes.len());
    if *total_read > limits.max_total_bytes {
        return Err(ApplicationError::ResourceLimitExceeded(format!(
            "archive decompressed data exceeds {} bytes",
            limits.max_total_bytes
        )));
    }
    Ok(bytes)
}

pub(crate) fn read_optional_entry<R: Read + Seek>(
    archive: &mut ZipArchive<R>,
    name: &str,
    limits: &ArchiveLimits,
    total_read: &mut usize,
) -> Result<Option<Vec<u8>>, ApplicationError> {
    if archive.index_for_name(name).is_none() {
        return Ok(None);
    }
    read_entry(archive, name, limits, total_read).map(Some)
}

pub(crate) fn utf8_entry(bytes: Vec<u8>, name: &str) -> Result<String, ApplicationError> {
    String::from_utf8(bytes).map_err(|error| {
        ApplicationError::ParseFailed(format!("archive XML/text entry {name:?} is not UTF-8: {error}"))
    })
}
