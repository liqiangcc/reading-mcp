use crate::application::ports::{ApplicationError, OcrEvidenceStore};
use async_trait::async_trait;
use std::path::PathBuf;

const MAGIC: &[u8] = b"reading-mcp-ocr-evidence/v1\0";

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
        let identity_bytes = identity.as_bytes();
        let mut envelope = Vec::with_capacity(MAGIC.len() + 4 + identity_bytes.len() + bytes.len());
        envelope.extend_from_slice(MAGIC);
        envelope.extend_from_slice(&(identity_bytes.len() as u32).to_be_bytes());
        envelope.extend_from_slice(identity_bytes);
        envelope.extend_from_slice(bytes);
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
                let identity = &blob[header..payload_start];
                let payload = &blob[payload_start..];
                use sha2::{Digest, Sha256};
                let mut hasher = Sha256::new();
                hasher.update(identity);
                hasher.update([0]);
                hasher.update(payload);
                if format!("sha256:{:x}", hasher.finalize()) != digest {
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
        tokio::fs::write(store.path(&digest), b"tampered")
            .await
            .unwrap();
        let error = store.get(&digest).await.unwrap_err();
        assert!(error.to_string().contains("evidence"));
    }
}
