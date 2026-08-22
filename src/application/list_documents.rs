use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::application::discovery_cursor::{
    DiscoveryCursorClaims, decode_discovery_cursor, encode_discovery_cursor, manifest_hash,
};
use crate::application::ports::ApplicationError;

pub const DEFAULT_DISCOVERY_MAX_RESULTS: usize = 100;
pub const MAX_DISCOVERY_RESULTS: usize = 1_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ListDocumentsCommand {
    pub path: Option<String>,
    pub recursive: bool,
    pub max_results: usize,
    pub cursor: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ListedDocument {
    pub path: String,
    pub name: String,
    pub media_type: String,
    pub size_bytes: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ListDocumentsResult {
    pub documents: Vec<ListedDocument>,
    pub complete: bool,
    pub next_cursor: Option<String>,
}

pub struct ListDocumentsUseCase {
    allowed_roots: Vec<PathBuf>,
}

impl ListDocumentsUseCase {
    pub fn new(allowed_roots: Vec<PathBuf>) -> Self {
        Self { allowed_roots }
    }

    pub async fn execute(
        &self,
        command: ListDocumentsCommand,
    ) -> Result<ListDocumentsResult, ApplicationError> {
        let max_results = effective_max_results(command.max_results)?;
        let cursor = command
            .cursor
            .as_deref()
            .map(decode_discovery_cursor)
            .transpose()?;
        let scope = resolve_scope(&self.allowed_roots, &command, cursor.as_ref()).await?;

        if scope.allowed_roots.is_empty() {
            return Ok(ListDocumentsResult {
                documents: Vec::new(),
                complete: true,
                next_cursor: None,
            });
        }

        let candidates = scan_scope(&scope).await?;
        let candidate_manifest_hash = manifest_hash(&candidates)?;
        let start_index = if let Some(cursor) = &cursor {
            if cursor.candidate_manifest_hash != candidate_manifest_hash
                || cursor.total_candidates != candidates.len()
            {
                return Err(ApplicationError::StaleCursor(
                    "discovery candidate manifest changed during continuation".into(),
                ));
            }
            if cursor.next_index >= candidates.len() {
                return Err(ApplicationError::InvalidCursor(format!(
                    "discovery cursor position {} is not resumable for {} candidates",
                    cursor.next_index,
                    candidates.len()
                )));
            }
            cursor.next_index
        } else {
            0
        };

        let end_index = start_index
            .saturating_add(max_results)
            .min(candidates.len());
        let complete = end_index == candidates.len();
        let next_cursor = if complete {
            None
        } else {
            Some(encode_discovery_cursor(DiscoveryCursorClaims::new(
                scope.allowed_roots.clone(),
                scope.requested_path.clone(),
                scope.recursive,
                candidate_manifest_hash,
                candidates.len(),
                end_index,
            ))?)
        };

        Ok(ListDocumentsResult {
            documents: candidates[start_index..end_index].to_vec(),
            complete,
            next_cursor,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct DiscoveryScope {
    allowed_roots: Vec<String>,
    search_roots: Vec<PathBuf>,
    requested_path: Option<String>,
    recursive: bool,
}

async fn resolve_scope(
    configured_roots: &[PathBuf],
    command: &ListDocumentsCommand,
    cursor: Option<&DiscoveryCursorClaims>,
) -> Result<DiscoveryScope, ApplicationError> {
    let allowed_roots = canonical_roots(configured_roots).await;
    if allowed_roots.is_empty() && command.path.is_some() {
        return Err(ApplicationError::BlockedSource(
            "local file discovery is disabled; configure an allowed root explicitly".into(),
        ));
    }

    let requested_path = match command.path.as_deref() {
        Some(path) => {
            let path = PathBuf::from(path.strip_prefix("file://").unwrap_or(path));
            let canonical = tokio::fs::canonicalize(&path).await.map_err(|error| {
                ApplicationError::RetrievalFailed(format!("{}: {error}", path.display()))
            })?;
            if !allowed_roots
                .iter()
                .any(|root| canonical.starts_with(Path::new(root)))
            {
                return Err(ApplicationError::BlockedSource(format!(
                    "document listing path is outside configured roots: {}",
                    canonical.display()
                )));
            }
            let metadata = tokio::fs::metadata(&canonical).await.map_err(|error| {
                ApplicationError::RetrievalFailed(format!("{}: {error}", canonical.display()))
            })?;
            if !metadata.is_dir() {
                return Err(ApplicationError::InvalidRequest(format!(
                    "document listing path is not a directory: {}",
                    canonical.display()
                )));
            }
            Some(canonical.to_string_lossy().into_owned())
        }
        None => None,
    };

    if let Some(cursor) = cursor {
        if cursor.recursive != command.recursive {
            return Err(ApplicationError::CursorTargetMismatch(format!(
                "discovery cursor recursive={} does not match requested recursive={}",
                cursor.recursive, command.recursive
            )));
        }
        if cursor.requested_path != requested_path {
            return Err(ApplicationError::CursorTargetMismatch(
                "discovery cursor path does not match requested path".into(),
            ));
        }
        if cursor.allowed_roots != allowed_roots {
            return Err(ApplicationError::StaleCursor(
                "configured discovery roots changed during continuation".into(),
            ));
        }
    }

    let search_roots = match &requested_path {
        Some(path) => vec![PathBuf::from(path)],
        None => allowed_roots.iter().map(PathBuf::from).collect(),
    };

    Ok(DiscoveryScope {
        allowed_roots,
        search_roots,
        requested_path,
        recursive: command.recursive,
    })
}

async fn canonical_roots(allowed_roots: &[PathBuf]) -> Vec<String> {
    let mut roots = Vec::new();
    for root in allowed_roots {
        if let Ok(canonical) = tokio::fs::canonicalize(root).await {
            roots.push(canonical);
        }
    }
    roots.sort();
    roots.dedup();
    roots
        .into_iter()
        .map(|root| root.to_string_lossy().into_owned())
        .collect()
}

async fn scan_scope(scope: &DiscoveryScope) -> Result<Vec<ListedDocument>, ApplicationError> {
    let mut documents = BTreeMap::new();
    for root in &scope.search_roots {
        collect_documents(root, &scope.allowed_roots, scope.recursive, &mut documents).await?;
    }
    Ok(documents.into_values().collect())
}

async fn collect_documents(
    directory: &Path,
    allowed_roots: &[String],
    recursive: bool,
    output: &mut BTreeMap<String, ListedDocument>,
) -> Result<(), ApplicationError> {
    let mut directories = vec![directory.to_path_buf()];
    while let Some(current) = directories.pop() {
        let mut entries = tokio::fs::read_dir(&current).await.map_err(|error| {
            ApplicationError::RetrievalFailed(format!("{}: {error}", current.display()))
        })?;

        while let Some(entry) = entries.next_entry().await.map_err(|error| {
            ApplicationError::RetrievalFailed(format!("{}: {error}", current.display()))
        })? {
            let file_type = entry.file_type().await.map_err(|error| {
                ApplicationError::RetrievalFailed(format!("{}: {error}", entry.path().display()))
            })?;
            let path = entry.path();

            if file_type.is_dir() {
                if recursive {
                    directories.push(path);
                }
                continue;
            }
            if !file_type.is_file() || !is_supported_document(&path) {
                continue;
            }

            let canonical = tokio::fs::canonicalize(&path).await.map_err(|error| {
                ApplicationError::RetrievalFailed(format!("{}: {error}", path.display()))
            })?;
            if !allowed_roots
                .iter()
                .any(|root| canonical.starts_with(Path::new(root)))
            {
                continue;
            }

            let metadata = tokio::fs::metadata(&canonical).await.map_err(|error| {
                ApplicationError::RetrievalFailed(format!("{}: {error}", canonical.display()))
            })?;
            let path = canonical.to_string_lossy().into_owned();
            let name = canonical
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap_or_default()
                .to_owned();
            output.insert(
                path.clone(),
                ListedDocument {
                    path,
                    name,
                    media_type: media_type_for_path(&canonical).to_owned(),
                    size_bytes: metadata.len(),
                },
            );
        }
    }

    Ok(())
}

fn effective_max_results(requested: usize) -> Result<usize, ApplicationError> {
    if requested == 0 {
        return Err(ApplicationError::InvalidRequest(
            "max_results must be greater than zero".into(),
        ));
    }
    Ok(requested.min(MAX_DISCOVERY_RESULTS))
}

fn is_supported_document(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|value| value.to_str())
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some("md")
            | Some("markdown")
            | Some("txt")
            | Some("text")
            | Some("html")
            | Some("htm")
            | Some("pdf")
            | Some("epub")
            | Some("docx")
            | Some("json")
            | Some("yaml")
            | Some("yml")
    )
}

fn media_type_for_path(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|value| value.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("md") | Some("markdown") => "text/markdown",
        Some("txt") | Some("text") => "text/plain",
        Some("html") | Some("htm") => "text/html",
        Some("pdf") => "application/pdf",
        Some("epub") => "application/epub+zip",
        Some("docx") => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        Some("json") => "application/json",
        Some("yaml") | Some("yml") => "application/yaml",
        _ => "application/octet-stream",
    }
}
