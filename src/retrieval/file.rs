use std::collections::BTreeMap;
use std::path::PathBuf;

use async_trait::async_trait;

use crate::application::ports::{
    ApplicationError, RetrievalOptions, RetrievedResource, Retriever, SourcePolicy,
};
use crate::domain::{DocumentSource, MediaType};

#[derive(Clone, Debug, Default)]
pub struct LocalFileSourcePolicy {
    allowed_roots: Vec<PathBuf>,
}

impl LocalFileSourcePolicy {
    pub fn disabled() -> Self {
        Self::default()
    }

    pub fn allow_roots<I, P>(roots: I) -> Self
    where
        I: IntoIterator<Item = P>,
        P: Into<PathBuf>,
    {
        Self {
            allowed_roots: roots.into_iter().map(Into::into).collect(),
        }
    }
}

#[async_trait]
impl SourcePolicy for LocalFileSourcePolicy {
    async fn validate(&self, source: &DocumentSource) -> Result<(), ApplicationError> {
        let value = source.0.trim();
        if value.is_empty() {
            return Err(ApplicationError::InvalidRequest(
                "document source must not be empty".into(),
            ));
        }

        if value.contains("://") && !value.starts_with("file://") {
            return Err(ApplicationError::BlockedSource(format!(
                "local file mode does not allow source: {value}"
            )));
        }

        if self.allowed_roots.is_empty() {
            return Err(ApplicationError::BlockedSource(
                "local file access is disabled; configure an allowed root explicitly".into(),
            ));
        }

        let path = source_to_path(source)?;
        let canonical = tokio::fs::canonicalize(&path).await.map_err(|error| {
            ApplicationError::RetrievalFailed(format!("{}: {error}", path.display()))
        })?;

        for root in &self.allowed_roots {
            let Ok(canonical_root) = tokio::fs::canonicalize(root).await else {
                continue;
            };
            if canonical.starts_with(&canonical_root) {
                return Ok(());
            }
        }

        Err(ApplicationError::BlockedSource(format!(
            "local file is outside configured roots: {}",
            canonical.display()
        )))
    }
}

#[derive(Default)]
pub struct FileRetriever;

#[async_trait]
impl Retriever for FileRetriever {
    async fn retrieve(
        &self,
        source: &DocumentSource,
        options: &RetrievalOptions,
    ) -> Result<RetrievedResource, ApplicationError> {
        if options.auth_profile.is_some() {
            return Err(ApplicationError::InvalidRequest(
                "auth_profile is not supported for local files".into(),
            ));
        }

        let path = source_to_path(source)?;
        let canonical = tokio::fs::canonicalize(&path).await.map_err(|error| {
            ApplicationError::RetrievalFailed(format!("{}: {error}", path.display()))
        })?;
        let bytes = tokio::fs::read(&canonical).await.map_err(|error| {
            ApplicationError::RetrievalFailed(format!("{}: {error}", canonical.display()))
        })?;

        let media_type = media_type_for_path(&canonical);
        let mut metadata = BTreeMap::new();
        if let Some(name) = canonical.file_name().and_then(|value| value.to_str()) {
            metadata.insert("file_name".into(), name.into());
        }
        if let Some(stem) = canonical.file_stem().and_then(|value| value.to_str()) {
            metadata.insert("file_stem".into(), stem.into());
        }

        Ok(RetrievedResource {
            source: source.clone(),
            final_source: DocumentSource(format!("file://{}", canonical.to_string_lossy())),
            media_type,
            bytes,
            etag: None,
            last_modified: None,
            metadata,
        })
    }
}

fn source_to_path(source: &DocumentSource) -> Result<PathBuf, ApplicationError> {
    let value = source.0.trim();
    let path = value.strip_prefix("file://").unwrap_or(value);
    if path.is_empty() {
        return Err(ApplicationError::InvalidRequest(
            "file source must contain a path".into(),
        ));
    }
    Ok(PathBuf::from(path))
}

fn media_type_for_path(path: &std::path::Path) -> MediaType {
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .map(str::to_ascii_lowercase);

    match extension.as_deref() {
        Some("md") | Some("markdown") => MediaType("text/markdown".into()),
        Some("txt") | Some("text") => MediaType("text/plain".into()),
        Some("html") | Some("htm") => MediaType("text/html".into()),
        Some("pdf") => MediaType("application/pdf".into()),
        _ => MediaType("application/octet-stream".into()),
    }
}
