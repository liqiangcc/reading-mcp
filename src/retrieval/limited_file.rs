use std::path::PathBuf;

use async_trait::async_trait;

use crate::application::ports::{ApplicationError, RetrievalOptions, RetrievedResource, Retriever};
use crate::domain::DocumentSource;

use super::FileRetriever;

pub struct LimitedFileRetriever {
    inner: FileRetriever,
    max_bytes: usize,
}

impl LimitedFileRetriever {
    pub fn new(max_bytes: usize) -> Self {
        Self {
            inner: FileRetriever,
            max_bytes,
        }
    }
}

#[async_trait]
impl Retriever for LimitedFileRetriever {
    async fn retrieve(
        &self,
        source: &DocumentSource,
        options: &RetrievalOptions,
    ) -> Result<RetrievedResource, ApplicationError> {
        let path = source_to_path(source)?;
        let canonical = tokio::fs::canonicalize(&path).await.map_err(|error| {
            ApplicationError::RetrievalFailed(format!("{}: {error}", path.display()))
        })?;
        let metadata = tokio::fs::metadata(&canonical).await.map_err(|error| {
            ApplicationError::RetrievalFailed(format!("{}: {error}", canonical.display()))
        })?;
        if metadata.len() > self.max_bytes as u64 {
            return Err(ApplicationError::ResourceLimitExceeded(format!(
                "local file is {} bytes; limit is {} bytes",
                metadata.len(),
                self.max_bytes
            )));
        }
        self.inner.retrieve(source, options).await
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
