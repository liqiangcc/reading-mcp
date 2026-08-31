use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::application::directory_cursor::{
    DirectoryCursorClaims, decode_directory_cursor, directory_manifest_hash,
    encode_directory_cursor,
};
use crate::application::list_documents::{is_supported_document, media_type_for_path};
use crate::application::local_path::{
    canonical_roots, canonicalize_authorized_directory, is_within_any_root,
};
use crate::application::ports::ApplicationError;

pub const DEFAULT_DIRECTORY_MAX_RESULTS: usize = 100;
pub const MAX_DIRECTORY_RESULTS: usize = 1_000;
const MAX_DIRECTORY_ENTRIES: usize = 100_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ListDirectoryCommand {
    pub path: Option<String>,
    pub max_results: usize,
    pub cursor: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DirectoryEntryKind {
    Directory,
    Document,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ListedDirectoryEntry {
    pub kind: DirectoryEntryKind,
    pub path: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub media_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ListDirectoryResult {
    pub entries: Vec<ListedDirectoryEntry>,
    pub complete: bool,
    pub next_cursor: Option<String>,
}

pub struct ListDirectoryUseCase {
    allowed_roots: Vec<PathBuf>,
}

impl ListDirectoryUseCase {
    pub fn new(allowed_roots: Vec<PathBuf>) -> Self {
        Self { allowed_roots }
    }

    pub async fn execute(
        &self,
        command: ListDirectoryCommand,
    ) -> Result<ListDirectoryResult, ApplicationError> {
        let max_results = effective_max_results(command.max_results)?;
        let cursor = command
            .cursor
            .as_deref()
            .map(decode_directory_cursor)
            .transpose()?;
        let scope = resolve_scope(&self.allowed_roots, &command, cursor.as_ref()).await?;

        if scope.allowed_roots.is_empty() {
            return Ok(ListDirectoryResult {
                entries: Vec::new(),
                complete: true,
                next_cursor: None,
            });
        }

        let entries = scan_scope(&scope).await?;
        let entry_manifest_hash = directory_manifest_hash(&entries)?;
        let start_index = if let Some(cursor) = &cursor {
            if cursor.entry_manifest_hash != entry_manifest_hash
                || cursor.total_entries != entries.len()
            {
                return Err(ApplicationError::StaleCursor(
                    "directory entry manifest changed during continuation".into(),
                ));
            }
            if cursor.next_index >= entries.len() {
                return Err(ApplicationError::InvalidCursor(format!(
                    "directory cursor position {} is not resumable for {} entries",
                    cursor.next_index,
                    entries.len()
                )));
            }
            cursor.next_index
        } else {
            0
        };

        let end_index = start_index.saturating_add(max_results).min(entries.len());
        let complete = end_index == entries.len();
        let next_cursor = if complete {
            None
        } else {
            Some(encode_directory_cursor(DirectoryCursorClaims::new(
                scope.allowed_roots.clone(),
                scope.requested_path.clone(),
                entry_manifest_hash,
                entries.len(),
                end_index,
            ))?)
        };

        Ok(ListDirectoryResult {
            entries: entries[start_index..end_index].to_vec(),
            complete,
            next_cursor,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct DirectoryScope {
    allowed_roots: Vec<String>,
    requested_path: Option<String>,
}

async fn resolve_scope(
    configured_roots: &[PathBuf],
    command: &ListDirectoryCommand,
    cursor: Option<&DirectoryCursorClaims>,
) -> Result<DirectoryScope, ApplicationError> {
    let allowed_roots = canonical_roots(configured_roots).await;
    if allowed_roots.is_empty() && command.path.is_some() {
        return Err(ApplicationError::BlockedSource(
            "local directory discovery is disabled; configure an allowed root explicitly".into(),
        ));
    }

    let requested_path = match command.path.as_deref() {
        Some(path) => {
            let canonical =
                canonicalize_authorized_directory(path, &allowed_roots, "directory listing")
                    .await?;
            Some(canonical.to_string_lossy().into_owned())
        }
        None => None,
    };

    if let Some(cursor) = cursor {
        if cursor.requested_path != requested_path {
            return Err(ApplicationError::CursorTargetMismatch(
                "directory cursor path does not match requested path".into(),
            ));
        }
        if cursor.allowed_roots != allowed_roots {
            return Err(ApplicationError::StaleCursor(
                "configured directory roots changed during continuation".into(),
            ));
        }
    }

    Ok(DirectoryScope {
        allowed_roots,
        requested_path,
    })
}

async fn scan_scope(scope: &DirectoryScope) -> Result<Vec<ListedDirectoryEntry>, ApplicationError> {
    if let Some(path) = &scope.requested_path {
        return collect_direct_children(Path::new(path), &scope.allowed_roots).await;
    }

    let mut entries = BTreeMap::new();
    for root in &scope.allowed_roots {
        let path = PathBuf::from(root);
        let metadata = tokio::fs::metadata(&path).await.map_err(|error| {
            ApplicationError::RetrievalFailed(format!("{}: {error}", path.display()))
        })?;
        if !metadata.is_dir() {
            continue;
        }
        insert_directory(&path, &scope.allowed_roots, &mut entries).await?;
        if entries.len() > MAX_DIRECTORY_ENTRIES {
            return Err(ApplicationError::ResourceLimitExceeded(format!(
                "configured roots contain more than {MAX_DIRECTORY_ENTRIES} entries"
            )));
        }
    }
    Ok(entries.into_values().collect())
}

async fn collect_direct_children(
    directory: &Path,
    allowed_roots: &[String],
) -> Result<Vec<ListedDirectoryEntry>, ApplicationError> {
    let canonical_directory = tokio::fs::canonicalize(directory).await.map_err(|error| {
        ApplicationError::RetrievalFailed(format!("{}: {error}", directory.display()))
    })?;
    if !is_within_any_root(allowed_roots, &canonical_directory) {
        return Err(ApplicationError::BlockedSource(format!(
            "directory listing path is outside configured roots: {}",
            canonical_directory.display()
        )));
    }

    let mut entries_by_path = BTreeMap::new();
    let mut entries = tokio::fs::read_dir(&canonical_directory)
        .await
        .map_err(|error| {
            ApplicationError::RetrievalFailed(format!("{}: {error}", canonical_directory.display()))
        })?;
    while let Some(entry) = entries.next_entry().await.map_err(|error| {
        ApplicationError::RetrievalFailed(format!("{}: {error}", canonical_directory.display()))
    })? {
        let path = entry.path();
        let file_type = entry.file_type().await.map_err(|error| {
            ApplicationError::RetrievalFailed(format!("{}: {error}", path.display()))
        })?;
        let canonical = tokio::fs::canonicalize(&path).await.map_err(|error| {
            ApplicationError::RetrievalFailed(format!("{}: {error}", path.display()))
        })?;
        if !is_within_any_root(allowed_roots, &canonical) {
            continue;
        }
        if file_type.is_symlink() {
            continue;
        }
        let metadata = tokio::fs::metadata(&canonical).await.map_err(|error| {
            ApplicationError::RetrievalFailed(format!("{}: {error}", canonical.display()))
        })?;
        let Some(name) = canonical.file_name().and_then(|value| value.to_str()) else {
            continue;
        };
        if metadata.is_dir() {
            entries_by_path.insert(
                canonical.to_string_lossy().into_owned(),
                ListedDirectoryEntry {
                    kind: DirectoryEntryKind::Directory,
                    path: canonical.to_string_lossy().into_owned(),
                    name: name.to_owned(),
                    media_type: None,
                    size_bytes: None,
                },
            );
        } else if metadata.is_file() && is_supported_document(&canonical) {
            entries_by_path.insert(
                canonical.to_string_lossy().into_owned(),
                ListedDirectoryEntry {
                    kind: DirectoryEntryKind::Document,
                    path: canonical.to_string_lossy().into_owned(),
                    name: name.to_owned(),
                    media_type: Some(media_type_for_path(&canonical).to_owned()),
                    size_bytes: Some(metadata.len()),
                },
            );
        }
        if entries_by_path.len() > MAX_DIRECTORY_ENTRIES {
            return Err(ApplicationError::ResourceLimitExceeded(format!(
                "directory contains more than {MAX_DIRECTORY_ENTRIES} discoverable entries"
            )));
        }
    }
    Ok(entries_by_path.into_values().collect())
}

async fn insert_directory(
    path: &Path,
    allowed_roots: &[String],
    output: &mut BTreeMap<String, ListedDirectoryEntry>,
) -> Result<(), ApplicationError> {
    let canonical = tokio::fs::canonicalize(path).await.map_err(|error| {
        ApplicationError::RetrievalFailed(format!("{}: {error}", path.display()))
    })?;
    if !is_within_any_root(allowed_roots, &canonical) {
        return Ok(());
    }
    let Some(name) = canonical.file_name().and_then(|value| value.to_str()) else {
        return Ok(());
    };
    output.insert(
        canonical.to_string_lossy().into_owned(),
        ListedDirectoryEntry {
            kind: DirectoryEntryKind::Directory,
            path: canonical.to_string_lossy().into_owned(),
            name: name.to_owned(),
            media_type: None,
            size_bytes: None,
        },
    );
    Ok(())
}

fn effective_max_results(requested: usize) -> Result<usize, ApplicationError> {
    if requested == 0 {
        return Err(ApplicationError::InvalidRequest(
            "max_results must be greater than zero".into(),
        ));
    }
    Ok(requested.min(MAX_DIRECTORY_RESULTS))
}

#[cfg(test)]
mod tests {
    use super::effective_max_results;
    use crate::application::ports::ApplicationError;

    #[test]
    fn directory_page_limit_is_bounded() {
        assert_eq!(effective_max_results(2_000).unwrap(), 1_000);
        assert!(matches!(
            effective_max_results(0),
            Err(ApplicationError::InvalidRequest(_))
        ));
    }
}
