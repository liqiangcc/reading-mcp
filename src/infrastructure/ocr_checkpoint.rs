//! Durable per-page OCR checkpoints for bounded resumable ingestion.
//!
//! The store only holds intermediate worker progress bound to an exact
//! source+runtime identity key. It never contains a canonical document and
//! is discarded on success or definitive failure; a retryable timeout is the
//! only outcome that retains it. Page files are written via tmp+rename so a
//! kill mid-write can never produce a partially written checkpoint.
use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::application::ports::ApplicationError;

pub const CHECKPOINT_SCHEMA: &str = "ocr-page-checkpoint/v1";

/// One durable checkpoint per (raw source, runtime identity, projection
/// namespace) triple. Changing any input changes the key, so incompatible
/// progress is never even looked up — fail-closed by construction.
pub fn checkpoint_key(
    original_sha256: &str,
    runtime_identity_sha256: &str,
    layout_namespace: &str,
) -> String {
    let digest = Sha256::digest(
        [
            b"ocr-checkpoint/v1".as_slice(),
            original_sha256.as_bytes(),
            runtime_identity_sha256.as_bytes(),
            layout_namespace.as_bytes(),
        ]
        .concat(),
    );
    format!("{digest:x}")
}

#[derive(Debug)]
pub struct CheckpointLoad {
    /// Validated page checkpoints keyed by 1-based page number; each value is
    /// the worker's `ocr-page-checkpoint/v1` payload returned verbatim.
    pub pages: BTreeMap<u32, serde_json::Value>,
    pub required_pages: Option<Vec<u32>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CheckpointMeta {
    schema: String,
    original_sha256: String,
    runtime_identity_sha256: String,
    layout_namespace: String,
    #[serde(default)]
    required_pages: Option<Vec<u32>>,
}

/// Persisted page files must match the worker's emitted record exactly: any
/// unknown field fails closed rather than being silently trusted later.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PageEntry {
    schema: String,
    page: u32,
    original_sha256: String,
    runtime_identity_sha256: String,
    page_raster_sha256: String,
    boxes: Vec<serde_json::Value>,
    ocr_retry_diagnostic: serde_json::Value,
    #[serde(default)]
    observation: Option<serde_json::Value>,
}

pub struct FileOcrCheckpointStore {
    root: PathBuf,
}

impl FileOcrCheckpointStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    fn directory(&self, key: &str) -> PathBuf {
        self.root.join(key)
    }

    fn meta_is_current(meta: &CheckpointMeta, expected: &MetaIdentity) -> bool {
        meta.schema == "ocr-checkpoint-meta/v1"
            && meta.original_sha256 == expected.original_sha256
            && meta.runtime_identity_sha256 == expected.runtime_identity_sha256
            && meta.layout_namespace == expected.layout_namespace
    }

    /// Union of all valid page files; an unreadable or identity-mismatched
    /// store degrades to "no checkpoint" rather than wrong progress.
    pub async fn load(
        &self,
        key: &str,
        expected: &MetaIdentity,
    ) -> Result<CheckpointLoad, ApplicationError> {
        let directory = self.directory(key);
        let meta = match tokio::fs::read(directory.join("meta.json")).await {
            Ok(bytes) => match serde_json::from_slice::<CheckpointMeta>(&bytes) {
                Ok(meta) if Self::meta_is_current(&meta, expected) => meta,
                _ => {
                    return Ok(CheckpointLoad {
                        pages: BTreeMap::new(),
                        required_pages: None,
                    });
                }
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(CheckpointLoad {
                    pages: BTreeMap::new(),
                    required_pages: None,
                });
            }
            Err(error) => return Err(ApplicationError::CacheFailed(error.to_string())),
        };
        let mut pages = BTreeMap::new();
        let mut entries = match tokio::fs::read_dir(&directory).await {
            Ok(entries) => entries,
            Err(_) => {
                return Ok(CheckpointLoad {
                    pages,
                    required_pages: meta.required_pages,
                });
            }
        };
        while let Some(entry) = entries
            .next_entry()
            .await
            .map_err(|e| ApplicationError::CacheFailed(e.to_string()))?
        {
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            let Some(number) = name
                .strip_prefix("page-")
                .and_then(|rest| rest.strip_suffix(".json"))
                .and_then(|rest| rest.parse::<u32>().ok())
            else {
                continue;
            };
            let Ok(bytes) = tokio::fs::read(entry.path()).await else {
                continue;
            };
            let Ok(page) = serde_json::from_slice::<PageEntry>(&bytes) else {
                continue;
            };
            if page.schema != CHECKPOINT_SCHEMA
                || page.page != number
                || page.page == 0
                || page.original_sha256 != expected.original_sha256
                || page.runtime_identity_sha256 != expected.runtime_identity_sha256
                || page.page_raster_sha256.len() != 64
                || page.boxes.len() > 10_000
                || !page.ocr_retry_diagnostic.is_object()
                || page
                    .observation
                    .as_ref()
                    .is_some_and(|value| !value.is_object())
            {
                continue;
            }
            let Ok(payload) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
                continue;
            };
            pages.insert(number, payload);
        }
        Ok(CheckpointLoad {
            pages,
            required_pages: meta.required_pages,
        })
    }

    /// Persist one completed page plus the current meta identity. The write is
    /// atomic per file; concurrent writers merge by union on read.
    pub async fn put_page(
        &self,
        key: &str,
        meta: &MetaIdentity,
        required_pages: Option<&[u32]>,
        page: &serde_json::Value,
    ) -> Result<(), ApplicationError> {
        let directory = self.directory(key);
        tokio::fs::create_dir_all(&directory)
            .await
            .map_err(|e| ApplicationError::CacheFailed(e.to_string()))?;
        let number = page
            .get("page")
            .and_then(serde_json::Value::as_u64)
            .ok_or_else(|| ApplicationError::CacheFailed("checkpoint page missing".into()))?;
        let meta_value = serde_json::json!({
            "schema": "ocr-checkpoint-meta/v1",
            "original_sha256": meta.original_sha256,
            "runtime_identity_sha256": meta.runtime_identity_sha256,
            "layout_namespace": meta.layout_namespace,
            "required_pages": required_pages,
        });
        write_atomic(
            &directory.join("meta.json"),
            &meta_value.to_string().into_bytes(),
        )
        .await?;
        write_atomic(
            &directory.join(format!("page-{number:04}.json")),
            &serde_json::to_vec(page).map_err(|e| ApplicationError::CacheFailed(e.to_string()))?,
        )
        .await
    }

    pub async fn record_required(
        &self,
        key: &str,
        meta: &MetaIdentity,
        required_pages: &[u32],
    ) -> Result<(), ApplicationError> {
        let directory = self.directory(key);
        tokio::fs::create_dir_all(&directory)
            .await
            .map_err(|e| ApplicationError::CacheFailed(e.to_string()))?;
        let meta_value = serde_json::json!({
            "schema": "ocr-checkpoint-meta/v1",
            "original_sha256": meta.original_sha256,
            "runtime_identity_sha256": meta.runtime_identity_sha256,
            "layout_namespace": meta.layout_namespace,
            "required_pages": required_pages,
        });
        write_atomic(
            &directory.join("meta.json"),
            &meta_value.to_string().into_bytes(),
        )
        .await
    }

    /// Success and definitive failures discard progress; only retryable
    /// timeout/cancellation paths retain the directory.
    pub async fn remove(&self, key: &str) -> Result<(), ApplicationError> {
        match tokio::fs::remove_dir_all(self.directory(key)).await {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(ApplicationError::CacheFailed(error.to_string())),
        }
    }
}

#[derive(Clone)]
pub struct MetaIdentity {
    pub original_sha256: String,
    pub runtime_identity_sha256: String,
    pub layout_namespace: String,
}

async fn write_atomic(path: &PathBuf, bytes: &[u8]) -> Result<(), ApplicationError> {
    let tmp = path.with_extension(format!(
        "tmp-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    tokio::fs::write(&tmp, bytes)
        .await
        .map_err(|e| ApplicationError::CacheFailed(e.to_string()))?;
    tokio::fs::rename(&tmp, path)
        .await
        .map_err(|e| ApplicationError::CacheFailed(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta() -> MetaIdentity {
        MetaIdentity {
            original_sha256: "a".repeat(64),
            runtime_identity_sha256: "b".repeat(64),
            layout_namespace: "ns/v1".into(),
        }
    }

    fn page(number: u32, meta: &MetaIdentity) -> serde_json::Value {
        serde_json::json!({
            "schema": CHECKPOINT_SCHEMA,
            "page": number,
            "original_sha256": meta.original_sha256,
            "runtime_identity_sha256": meta.runtime_identity_sha256,
            "page_raster_sha256": "c".repeat(64),
            "boxes": [],
            "ocr_retry_diagnostic": {"schema": "ocr-regional-observations/v3"},
        })
    }

    #[tokio::test]
    async fn checkpoint_roundtrips_and_rejects_identity_drift() {
        let directory = tempfile::tempdir().unwrap();
        let store = FileOcrCheckpointStore::new(directory.path());
        let key = checkpoint_key(&meta().original_sha256, "b".repeat(64).as_str(), "ns/v1");
        let meta = meta();
        store
            .put_page(&key, &meta, Some(&[1, 2]), &page(1, &meta))
            .await
            .unwrap();
        store
            .put_page(&key, &meta, Some(&[1, 2]), &page(2, &meta))
            .await
            .unwrap();
        let loaded = store.load(&key, &meta).await.unwrap();
        assert_eq!(loaded.pages.len(), 2);
        assert_eq!(loaded.required_pages, Some(vec![1, 2]));
        let other = MetaIdentity {
            original_sha256: "d".repeat(64),
            runtime_identity_sha256: "b".repeat(64),
            layout_namespace: "ns/v1".into(),
        };
        let drifted = store.load(&key, &other).await.unwrap();
        assert!(drifted.pages.is_empty());
        store.remove(&key).await.unwrap();
        assert!(store.load(&key, &meta).await.unwrap().pages.is_empty());
    }

    #[tokio::test]
    async fn corrupt_page_files_are_ignored_not_loaded() {
        let directory = tempfile::tempdir().unwrap();
        let store = FileOcrCheckpointStore::new(directory.path());
        let meta = meta();
        let key = checkpoint_key(
            &meta.original_sha256,
            &meta.runtime_identity_sha256,
            "ns/v1",
        );
        store
            .put_page(&key, &meta, None, &page(1, &meta))
            .await
            .unwrap();
        let bad = directory.path().join(&key).join("page-0002.json");
        tokio::fs::write(&bad, b"{not json").await.unwrap();
        let mut mismatched = page(3, &meta);
        mismatched["original_sha256"] = serde_json::json!("e".repeat(64));
        tokio::fs::write(
            directory.path().join(&key).join("page-0003.json"),
            serde_json::to_vec(&mismatched).unwrap(),
        )
        .await
        .unwrap();
        let loaded = store.load(&key, &meta).await.unwrap();
        assert_eq!(loaded.pages.keys().copied().collect::<Vec<_>>(), vec![1]);
    }
}
