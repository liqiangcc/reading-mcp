use crate::application::ports::{ApplicationError, OcrEvidenceStore};
use async_trait::async_trait;
use std::path::PathBuf;

#[async_trait]
pub struct FileOcrEvidenceStore {
    root: PathBuf,
}

impl FileOcrEvidenceStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }
    fn path(&self, digest: &str) -> PathBuf {
        self.root.join(digest)
    }
}

#[async_trait]
impl OcrEvidenceStore for FileOcrEvidenceStore {
    async fn put_immutable(
        &self,
        identity: &str,
        bytes: &[u8],
    ) -> Result<String, ApplicationError> {
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        h.update(identity.as_bytes());
        h.update([0]);
        h.update(bytes);
        let digest = format!("sha256:{:x}", h.finalize());
        let target = self.path(&digest);
        tokio::fs::create_dir_all(&self.root)
            .await
            .map_err(|e| ApplicationError::CacheFailed(e.to_string()))?;
        if tokio::fs::try_exists(&target).await.unwrap_or(false) {
            let existing = tokio::fs::read(&target)
                .await
                .map_err(|e| ApplicationError::CacheFailed(e.to_string()))?;
            if existing != bytes {
                return Err(ApplicationError::CacheFailed("evidence collision".into()));
            }
            return Ok(digest);
        }
        let tmp = self.root.join(format!(
            ".{}.tmp-{}-{}",
            digest.replace(':', "-"),
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        use tokio::io::AsyncWriteExt;
        let mut file = tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)
            .await
            .map_err(|e| ApplicationError::CacheFailed(e.to_string()))?;
        file.write_all(bytes)
            .await
            .map_err(|e| ApplicationError::CacheFailed(e.to_string()))?;
        file.sync_all()
            .await
            .map_err(|e| ApplicationError::CacheFailed(e.to_string()))?;
        drop(file);
        if let Err(e) = tokio::fs::rename(&tmp, &target).await {
            let _ = tokio::fs::remove_file(&tmp).await;
            return Err(ApplicationError::CacheFailed(e.to_string()));
        }
        std::fs::File::open(&self.root)
            .map_err(|e| ApplicationError::CacheFailed(e.to_string()))?
            .sync_all()
            .map_err(|e| ApplicationError::CacheFailed(e.to_string()))?;
        Ok(digest)
    }
    async fn get(&self, digest: &str) -> Result<Option<Vec<u8>>, ApplicationError> {
        let path = self.path(digest);
        match tokio::fs::read(path).await {
            Ok(bytes) => Ok(Some(bytes)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(ApplicationError::CacheFailed(e.to_string())),
        }
    }
}
