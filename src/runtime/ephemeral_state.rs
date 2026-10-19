use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT_EPHEMERAL_ID: AtomicUsize = AtomicUsize::new(0);

const PREFIX: &str = "reading-mcp-ephemeral-";

#[derive(Debug)]
pub(crate) struct EphemeralStateDir {
    path: PathBuf,
}

impl EphemeralStateDir {
    pub(crate) fn new() -> std::io::Result<Self> {
        cleanup_stale_directories();
        let id = NEXT_EPHEMERAL_ID.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("{PREFIX}{}-{id}", std::process::id()));
        std::fs::create_dir_all(&path)?;
        Ok(Self { path })
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for EphemeralStateDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

#[cfg(target_os = "linux")]
fn cleanup_stale_directories() {
    let Ok(entries) = std::fs::read_dir(std::env::temp_dir()) else {
        return;
    };
    let current_pid = std::process::id();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        let Some(rest) = name.strip_prefix(PREFIX) else {
            continue;
        };
        let Some((pid, sequence)) = rest.split_once('-') else {
            continue;
        };
        let (Ok(pid), Ok(_)) = (pid.parse::<u32>(), sequence.parse::<u64>()) else {
            continue;
        };
        if pid == current_pid {
            continue;
        }
        if Path::new(&format!("/proc/{pid}")).exists() {
            continue;
        }
        let _ = std::fs::remove_dir_all(entry.path());
    }
}

#[cfg(not(target_os = "linux"))]
fn cleanup_stale_directories() {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guards_use_distinct_existing_directories_and_remove_them_on_drop() {
        let first = EphemeralStateDir::new().unwrap();
        let second = EphemeralStateDir::new().unwrap();
        assert_ne!(first.path(), second.path());
        assert!(first.path().starts_with(std::env::temp_dir()));
        assert!(second.path().starts_with(std::env::temp_dir()));
        assert!(first.path().is_dir());
        assert!(second.path().is_dir());
        let first_path = first.path().to_path_buf();
        drop(first);
        assert!(!first_path.exists());
        assert!(second.path().is_dir());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn stale_directories_are_removed_but_other_names_are_kept() {
        let temp = std::env::temp_dir();
        let stale = temp.join("reading-mcp-ephemeral-999999999-0");
        let unrelated = temp.join("reading-mcp-ephemeral-unrelated-test-directory");
        std::fs::create_dir_all(&stale).unwrap();
        std::fs::create_dir_all(&unrelated).unwrap();

        let guard = EphemeralStateDir::new().unwrap();
        assert!(!stale.exists());
        assert!(unrelated.exists());

        drop(guard);
        let _ = std::fs::remove_dir_all(&unrelated);
    }
}
