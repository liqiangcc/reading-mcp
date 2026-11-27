//! Startup cleanup for temp files left behind by the caches' atomic writers.
//!
//! Every cache in this state directory publishes an entry the same way: write
//! a uniquely named temp file next to the target, then rename it into place
//! (#275). The rename is atomic, so a reader never sees a half-written entry,
//! but a crash — or a kill — between the write and the rename leaves the temp
//! file behind for good, and nothing ever removed it.
//!
//! The writers that share this shape, and the temp names they leave:
//!
//! | writer | directory | temp name |
//! | --- | --- | --- |
//! | `FileRawResourceCache::put` | `<state>/cache/raw` | `.<key>.bin.tmp-<pid>-<nanos>-<seq>` |
//! | `FileSourceViewCache::put` | `<state>/cache/source-view` | `.<key>.rsv.tmp-<pid>-<nanos>-<seq>` |
//! | `FileOcrCheckpointStore::write_atomic` | `<state>/ocr-checkpoints/<key>` | `page-NNNN.tmp-<pid>-<nanos>`, `meta.tmp-<pid>-<nanos>` |
//! | `FileOcrEvidenceStore::put_immutable` | `<state>/ocr-evidence` | `.<digest>.tmp-<pid>-<nanos>` |
//!
//! All four remove their stale temp files at startup. `FileParsedDocumentCache`
//! is not on this list: it overwrites its single file in place and never writes
//! a temp file.
//!
//! ## The age threshold
//!
//! A temp file can only be a leftover if no write could still be in progress,
//! and the state directory may be shared: two processes (two test binaries, or
//! two servers pointed at one `READING_MCP_STATE_DIR`) can be writing at the
//! same moment, so a blanket delete at startup could destroy the other one's
//! in-flight entry.
//!
//! [`STALE_TEMP_AGE`] is one hour. A temp file lives for exactly one `put` —
//! write, then rename — and the largest body the runtime accepts is
//! `READING_MCP_MAX_DOCUMENT_BYTES` (256 MiB by default), so even a slow disk
//! finishes a write in seconds. One hour is orders of magnitude above any
//! plausible in-flight write and is still short enough that a leftover is gone
//! by the next restart of the day. The comparison uses the file's modification
//! time, which the write itself sets: a temp file being written right now is
//! fresh and is kept.

use std::ffi::OsStr;
use std::io::ErrorKind;
use std::path::Path;
use std::time::{Duration, SystemTime};

use crate::application::ports::ApplicationError;

/// Temp files younger than this are kept: they may belong to a `put` that is
/// still in progress in another process sharing the state directory.
pub const STALE_TEMP_AGE: Duration = Duration::from_secs(60 * 60);

/// Removes the temp files in `directory` that are older than [`STALE_TEMP_AGE`]
/// and returns how many were removed. A missing directory is not an error.
pub fn remove_stale_temp_files(directory: &Path) -> Result<usize, ApplicationError> {
    remove_stale_temp_files_with_age(directory, STALE_TEMP_AGE, SystemTime::now())
}

/// [`remove_stale_temp_files`] for every directory directly under `root` — the
/// OCR checkpoint store keeps one directory per checkpoint key.
pub fn remove_stale_temp_files_in_children(root: &Path) -> Result<usize, ApplicationError> {
    let entries = match std::fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(cache_io_error(root, error)),
    };
    let mut removed = 0;
    for entry in entries.flatten() {
        if !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            continue;
        }
        removed += remove_stale_temp_files(&entry.path())?;
    }
    Ok(removed)
}

/// Every temp name above carries this marker, and no published entry does:
/// cache keys are hex digests, checkpoint files are `meta.json` and
/// `page-NNNN.json`, and evidence files are `sha256:<hex>`.
fn is_temp_name(name: &OsStr) -> bool {
    name.to_str().is_some_and(|name| name.contains(".tmp-"))
}

fn remove_stale_temp_files_with_age(
    directory: &Path,
    max_age: Duration,
    now: SystemTime,
) -> Result<usize, ApplicationError> {
    let entries = match std::fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(cache_io_error(directory, error)),
    };
    let mut removed = 0;
    for entry in entries.flatten() {
        if !is_temp_name(&entry.file_name()) {
            continue;
        }
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        let Ok(modified) = metadata.modified() else {
            // No timestamp to judge the age by: keep the file rather than risk
            // deleting a write in progress.
            continue;
        };
        // A timestamp in the future is a clock anomaly, not evidence of age.
        let is_stale = now.duration_since(modified).is_ok_and(|age| age > max_age);
        if !is_stale {
            continue;
        }
        match std::fs::remove_file(entry.path()) {
            Ok(()) => removed += 1,
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => return Err(cache_io_error(&entry.path(), error)),
        }
    }
    Ok(removed)
}

fn cache_io_error(path: &Path, error: std::io::Error) -> ApplicationError {
    ApplicationError::CacheFailed(format!("{}: {error}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn age(path: &Path, by: Duration) {
        let file = std::fs::File::options()
            .write(true)
            .open(path)
            .expect("temp file should open");
        file.set_times(std::fs::FileTimes::new().set_modified(SystemTime::now() - by))
            .expect("temp file mtime should be set");
    }

    #[test]
    fn only_temp_files_older_than_the_threshold_are_removed() {
        let directory = tempdir().expect("temporary directory should be created");
        let old_temp = directory.path().join(".entry.bin.tmp-1-2-3");
        let fresh_temp = directory.path().join(".entry.bin.tmp-4-5-6");
        let entry = directory.path().join("entry.bin");
        for path in [&old_temp, &fresh_temp, &entry] {
            std::fs::write(path, b"bytes").expect("file should be written");
        }
        age(&old_temp, STALE_TEMP_AGE * 2);

        assert_eq!(
            remove_stale_temp_files(directory.path()).expect("cleanup should run"),
            1
        );
        assert!(!old_temp.exists(), "an old temp file is removed");
        assert!(fresh_temp.exists(), "a fresh temp file may be a live write");
        assert!(entry.exists(), "a real entry is untouched");
    }

    #[test]
    fn a_missing_directory_is_not_an_error() {
        let directory = tempdir().expect("temporary directory should be created");
        assert_eq!(
            remove_stale_temp_files(&directory.path().join("absent")).expect("cleanup should run"),
            0
        );
        assert_eq!(
            remove_stale_temp_files_in_children(&directory.path().join("absent"))
                .expect("cleanup should run"),
            0
        );
    }

    #[test]
    fn checkpoint_temp_files_are_removed_in_child_directories() {
        let directory = tempdir().expect("temporary directory should be created");
        let key = directory.path().join("key");
        std::fs::create_dir_all(&key).expect("checkpoint directory should be created");
        let old_temp = key.join("page-0001.tmp-1-2");
        let kept = key.join("page-0001.json");
        std::fs::write(&old_temp, b"{}").expect("temp file should be written");
        std::fs::write(&kept, b"{}").expect("checkpoint should be written");
        age(&old_temp, STALE_TEMP_AGE * 2);

        assert_eq!(
            remove_stale_temp_files_in_children(directory.path()).expect("cleanup should run"),
            1
        );
        assert!(!old_temp.exists());
        assert!(kept.exists());
    }
}
