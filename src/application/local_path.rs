use std::path::{Component, Path, PathBuf};

use crate::application::ports::ApplicationError;

pub(crate) fn path_from_input(value: &str) -> Result<PathBuf, ApplicationError> {
    let value = value.trim();
    let path = value.strip_prefix("file://").unwrap_or(value);
    if path.is_empty() {
        return Err(ApplicationError::InvalidRequest(
            "local path must not be empty".into(),
        ));
    }
    let path = PathBuf::from(path);
    if path
        .components()
        .any(|component| component == Component::ParentDir)
    {
        return Err(ApplicationError::InvalidRequest(
            "local path must not contain '..' traversal components".into(),
        ));
    }
    Ok(path)
}

pub(crate) fn is_contained_by(root: &Path, candidate: &Path) -> bool {
    candidate == root || candidate.strip_prefix(root).is_ok()
}

pub(crate) fn is_within_any_root(roots: &[String], candidate: &Path) -> bool {
    roots
        .iter()
        .any(|root| is_contained_by(Path::new(root), candidate))
}

pub(crate) async fn canonical_roots(configured_roots: &[PathBuf]) -> Vec<String> {
    let mut roots = Vec::new();
    for root in configured_roots {
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

pub(crate) async fn canonicalize_authorized_directory(
    value: &str,
    allowed_roots: &[String],
    action: &str,
) -> Result<PathBuf, ApplicationError> {
    let path = path_from_input(value)?;
    let canonical = tokio::fs::canonicalize(&path).await.map_err(|error| {
        ApplicationError::RetrievalFailed(format!("{}: {error}", path.display()))
    })?;
    if !is_within_any_root(allowed_roots, &canonical) {
        return Err(ApplicationError::BlockedSource(format!(
            "{action} path is outside configured roots: {}",
            canonical.display()
        )));
    }
    let metadata = tokio::fs::metadata(&canonical).await.map_err(|error| {
        ApplicationError::RetrievalFailed(format!("{}: {error}", canonical.display()))
    })?;
    if !metadata.is_dir() {
        return Err(ApplicationError::InvalidRequest(format!(
            "{action} path is not a directory: {}",
            canonical.display()
        )));
    }
    Ok(canonical)
}
