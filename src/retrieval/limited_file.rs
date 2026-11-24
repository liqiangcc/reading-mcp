use std::path::Path;

use async_trait::async_trait;

use crate::application::ports::{
    ApplicationError, LocalSourceAccess, ResolvedLocalSource, RetrievalOptions, RetrievedResource,
    Retriever,
};
use crate::domain::DocumentSource;

use super::file::{media_type_for_path, source_to_path};
use super::{FileRetriever, LocalFileSourcePolicy};

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
        check_local_size_limit(&canonical, self.max_bytes).await?;
        self.inner.retrieve(source, options).await
    }
}

/// The size limit `LimitedFileRetriever` applies to a canonical local path,
/// shared with the source-view resolver so a refused file fails the same way
/// whether it is read for parsing or mapped for rendering.
async fn check_local_size_limit(
    canonical: &Path,
    max_bytes: usize,
) -> Result<(), ApplicationError> {
    let metadata = tokio::fs::metadata(canonical).await.map_err(|error| {
        ApplicationError::RetrievalFailed(format!("{}: {error}", canonical.display()))
    })?;
    if metadata.len() > max_bytes as u64 {
        return Err(ApplicationError::ResourceLimitExceeded(format!(
            "local file is {} bytes; limit is {} bytes",
            metadata.len(),
            max_bytes
        )));
    }
    Ok(())
}

/// Resolves a local-file document source to the canonical path a source-view
/// worker reads itself, enforcing exactly what the retrieval chain enforces:
/// `LocalFileSourcePolicy`'s roots and symlink rules, then
/// `LimitedFileRetriever`'s `READING_MCP_MAX_DOCUMENT_BYTES` check — without
/// reading the file. A refused source fails with the same error the matching
/// check reports today.
pub struct LocalFileSourceAccess {
    policy: LocalFileSourcePolicy,
    max_bytes: usize,
}

impl LocalFileSourceAccess {
    pub fn new(policy: LocalFileSourcePolicy, max_bytes: usize) -> Self {
        Self { policy, max_bytes }
    }
}

#[async_trait]
impl LocalSourceAccess for LocalFileSourceAccess {
    async fn resolve(
        &self,
        source: &DocumentSource,
    ) -> Result<ResolvedLocalSource, ApplicationError> {
        let canonical = self.policy.resolve_canonical(source).await?;
        check_local_size_limit(&canonical, self.max_bytes).await?;
        Ok(ResolvedLocalSource {
            path: canonical.clone(),
            final_source: DocumentSource(format!("file://{}", canonical.to_string_lossy())),
            media_type: media_type_for_path(&canonical),
        })
    }
}
