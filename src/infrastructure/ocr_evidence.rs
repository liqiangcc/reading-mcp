use crate::application::ports::{ApplicationError, OcrEvidenceStore};
use async_trait::async_trait;
use std::path::PathBuf;

const MAGIC: &[u8] = b"reading-mcp-ocr-evidence/v1\0";

pub struct FileOcrEvidenceStore {
    root: PathBuf,
}

struct TempFileGuard(Option<PathBuf>);

impl TempFileGuard {
    fn new(path: PathBuf) -> Self {
        Self(Some(path))
    }
    fn disarm(&mut self) {
        self.0 = None;
    }
}

impl Drop for TempFileGuard {
    fn drop(&mut self) {
        if let Some(path) = self.0.take() {
            let _ = std::fs::remove_file(path);
        }
    }
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
        let identity_bytes = identity.as_bytes();
        let identity_len = u32::try_from(identity_bytes.len())
            .map_err(|_| ApplicationError::CacheFailed("evidence identity is too large".into()))?;
        let mut envelope = Vec::with_capacity(MAGIC.len() + 4 + identity_bytes.len() + bytes.len());
        envelope.extend_from_slice(MAGIC);
        envelope.extend_from_slice(&identity_len.to_be_bytes());
        envelope.extend_from_slice(identity_bytes);
        envelope.extend_from_slice(bytes);
        use sha2::{Digest, Sha256};
        let digest = format!("sha256:{:x}", Sha256::digest(&envelope));
        let target = self.path(&digest);
        tokio::fs::create_dir_all(&self.root)
            .await
            .map_err(|e| ApplicationError::CacheFailed(e.to_string()))?;
        if tokio::fs::try_exists(&target).await.unwrap_or(false) {
            let existing = tokio::fs::read(&target)
                .await
                .map_err(|e| ApplicationError::CacheFailed(e.to_string()))?;
            if existing != envelope {
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
        let mut temp_guard = TempFileGuard::new(tmp.clone());
        use tokio::io::AsyncWriteExt;
        let mut file = tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)
            .await
            .map_err(|e| ApplicationError::CacheFailed(e.to_string()))?;
        file.write_all(&envelope)
            .await
            .map_err(|e| ApplicationError::CacheFailed(e.to_string()))?;
        file.sync_all()
            .await
            .map_err(|e| ApplicationError::CacheFailed(e.to_string()))?;
        drop(file);
        match tokio::fs::hard_link(&tmp, &target).await {
            Ok(()) => {
                tokio::fs::remove_file(&tmp)
                    .await
                    .map_err(|e| ApplicationError::CacheFailed(e.to_string()))?;
                temp_guard.disarm();
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                let existing = tokio::fs::read(&target)
                    .await
                    .map_err(|read_error| ApplicationError::CacheFailed(read_error.to_string()))?;
                if existing != envelope {
                    return Err(ApplicationError::CacheFailed("evidence collision".into()));
                }
                tokio::fs::remove_file(&tmp).await.map_err(|remove_error| {
                    ApplicationError::CacheFailed(remove_error.to_string())
                })?;
                temp_guard.disarm();
            }
            Err(e) => return Err(ApplicationError::CacheFailed(e.to_string())),
        }
        sync_directory(&self.root)?;
        if let Some(parent) = self.root.parent() {
            if parent != self.root {
                sync_directory(parent)?;
            }
        }
        Ok(digest)
    }
    async fn get(&self, digest: &str) -> Result<Option<Vec<u8>>, ApplicationError> {
        let valid_digest = digest.len() == 71
            && digest.starts_with("sha256:")
            && digest[7..]
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase());
        if !valid_digest {
            return Err(ApplicationError::CacheFailed(
                "invalid evidence digest".into(),
            ));
        }
        let path = self.path(digest);
        match tokio::fs::read(path).await {
            Ok(blob) => {
                let header = MAGIC.len() + 4;
                if blob.len() < header || !blob.starts_with(MAGIC) {
                    return Err(ApplicationError::CacheFailed(
                        "invalid evidence blob".into(),
                    ));
                }
                let identity_len = u32::from_be_bytes(
                    blob[MAGIC.len()..header]
                        .try_into()
                        .expect("fixed-size evidence header"),
                ) as usize;
                let payload_start = header.checked_add(identity_len).ok_or_else(|| {
                    ApplicationError::CacheFailed("invalid evidence identity length".into())
                })?;
                if payload_start > blob.len() {
                    return Err(ApplicationError::CacheFailed(
                        "truncated evidence blob".into(),
                    ));
                }
                let payload = &blob[payload_start..];
                use sha2::{Digest, Sha256};
                if format!("sha256:{:x}", Sha256::digest(&blob)) != digest {
                    return Err(ApplicationError::CacheFailed(
                        "evidence digest mismatch".into(),
                    ));
                }
                Ok(Some(payload.to_vec()))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(ApplicationError::CacheFailed(e.to_string())),
        }
    }
}

fn sync_directory(path: &std::path::Path) -> Result<(), ApplicationError> {
    std::fs::File::open(path)
        .map_err(|e| ApplicationError::CacheFailed(e.to_string()))?
        .sync_all()
        .map_err(|e| ApplicationError::CacheFailed(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::ports::OcrEvidenceStore;

    #[tokio::test]
    async fn immutable_blob_round_trips_and_rejects_invalid_digest() {
        let directory = tempfile::tempdir().unwrap();
        let store = FileOcrEvidenceStore::new(directory.path());
        let digest = store
            .put_immutable("identity-v1", b"payload")
            .await
            .unwrap();
        assert_eq!(store.get(&digest).await.unwrap(), Some(b"payload".to_vec()));
        assert!(store.get("sha256:../escape").await.is_err());
        assert!(
            store
                .get("sha256:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA")
                .await
                .is_err()
        );
        assert!(
            store
                .get("sha256:0000000000000000000000000000000000000000000000000000000000000000")
                .await
                .is_ok()
        );
    }

    #[tokio::test]
    async fn tampering_blob_is_reported() {
        let directory = tempfile::tempdir().unwrap();
        let store = FileOcrEvidenceStore::new(directory.path());
        let digest = store
            .put_immutable("identity-v1", b"payload")
            .await
            .unwrap();
        let path = store.path(&digest);
        let mut blob = tokio::fs::read(&path).await.unwrap();
        *blob.last_mut().unwrap() ^= 1;
        tokio::fs::write(path, blob).await.unwrap();
        let error = store.get(&digest).await.unwrap_err();
        assert!(error.to_string().contains("digest mismatch"));
    }

    #[tokio::test]
    async fn identity_and_payload_separator_are_unambiguous() {
        let directory = tempfile::tempdir().unwrap();
        let store = FileOcrEvidenceStore::new(directory.path());
        let first = store.put_immutable("a", b"\0b").await.unwrap();
        let second = store.put_immutable("a\0", b"b").await.unwrap();
        assert_ne!(first, second);
        assert_eq!(store.get(&first).await.unwrap(), Some(b"\0b".to_vec()));
        assert_eq!(store.get(&second).await.unwrap(), Some(b"b".to_vec()));
    }

    #[tokio::test]
    async fn concurrent_put_has_one_final_blob_and_no_temporary_files() {
        let directory = tempfile::tempdir().unwrap();
        let store = std::sync::Arc::new(FileOcrEvidenceStore::new(directory.path()));
        let tasks = (0..8).map(|_| {
            let store = store.clone();
            tokio::spawn(async move { store.put_immutable("same", b"payload").await.unwrap() })
        });
        let digests = futures_join(tasks).await;
        assert!(digests.iter().all(|digest| digest == &digests[0]));
        let mut entries = tokio::fs::read_dir(directory.path()).await.unwrap();
        let mut names = Vec::new();
        while let Some(entry) = entries.next_entry().await.unwrap() {
            names.push(entry.file_name());
        }
        assert_eq!(names.len(), 1);
        assert!(!names[0].to_string_lossy().starts_with('.'));
    }

    async fn futures_join<T>(
        tasks: impl IntoIterator<Item = tokio::task::JoinHandle<T>>,
    ) -> Vec<T> {
        let mut values = Vec::new();
        for task in tasks {
            values.push(task.await.unwrap());
        }
        values
    }
}
