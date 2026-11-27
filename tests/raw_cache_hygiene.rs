//! Raw-cache hygiene: startup temp-file cleanup and the size cap with LRU
//! eviction (`READING_MCP_RAW_CACHE_MIB`).
//!
//! Recency is the newest of an entry's two files' access and modification
//! times, refreshed on every hit, so these tests separate writes with a short
//! sleep: the filesystem timestamps are the evidence the cache orders by.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use reading_mcp::application::ports::{RawResourceCache, RetrievedResource};
use reading_mcp::domain::{DocumentSource, MediaType};
use reading_mcp::infrastructure::FileRawResourceCache;
use sha2::{Digest, Sha256};
use tempfile::tempdir;

const MIB: u64 = 1024 * 1024;

fn key(source: &DocumentSource) -> String {
    format!("{:x}", Sha256::digest(source.0.as_bytes()))
}

fn resource(source: &DocumentSource, bytes: Vec<u8>) -> RetrievedResource {
    RetrievedResource {
        source: source.clone(),
        final_source: source.clone(),
        media_type: MediaType("application/pdf".into()),
        bytes,
        etag: Some("\"v1\"".into()),
        last_modified: None,
        metadata: BTreeMap::new(),
    }
}

fn entry_paths(root: &Path, source: &DocumentSource) -> (PathBuf, PathBuf) {
    let key = key(source);
    (
        root.join("raw").join(format!("{key}.bin")),
        root.join("raw").join(format!("{key}.json")),
    )
}

fn directory_bytes(root: &Path) -> u64 {
    std::fs::read_dir(root.join("raw"))
        .map(|entries| {
            entries
                .flatten()
                .filter_map(|entry| entry.metadata().ok())
                .map(|metadata| metadata.len())
                .sum()
        })
        .unwrap_or(0)
}

fn backdate(path: &Path, by: Duration) {
    let file = std::fs::File::options()
        .write(true)
        .open(path)
        .expect("file should open");
    file.set_times(std::fs::FileTimes::new().set_modified(SystemTime::now() - by))
        .expect("mtime should be set");
}

/// #275 left temp files behind on a crash; startup removes the ones no write
/// can still own (older than an hour) and keeps everything else.
#[tokio::test]
async fn startup_removes_only_stale_temp_files_and_keeps_the_entries() {
    let directory = tempdir().expect("temporary directory should be created");
    let root = directory.path();
    let source = DocumentSource("https://example.test/kept.pdf".into());
    let cache = FileRawResourceCache::with_cap_mib(root, 0);
    cache
        .put(&source, resource(&source, b"kept body".to_vec()))
        .await
        .expect("entry should be written");
    let (body, metadata) = entry_paths(root, &source);

    let stale = root.join("raw").join(".abandoned.bin.tmp-1-2-3");
    let fresh = root.join("raw").join(".in-flight.bin.tmp-4-5-6");
    std::fs::write(&stale, b"half a body").expect("stale temp file should be written");
    std::fs::write(&fresh, b"half a body").expect("fresh temp file should be written");
    backdate(&stale, Duration::from_secs(2 * 60 * 60));

    let reopened = FileRawResourceCache::with_cap_mib(root, 0);

    assert!(!stale.exists(), "an old temp file is removed at startup");
    assert!(
        fresh.exists(),
        "a fresh temp file may belong to a write in progress"
    );
    assert!(body.exists() && metadata.exists(), "the entry is untouched");
    assert_eq!(
        reopened.get(&source).await.expect("entry should read"),
        Some(resource(&source, b"kept body".to_vec())),
        "the entry still reads after cleanup"
    );
}

/// Eviction is by last *use*: a hit moves an entry to the back of the order.
#[tokio::test]
async fn eviction_order_follows_the_last_use() {
    let directory = tempdir().expect("temporary directory should be created");
    let root = directory.path();
    let cache = FileRawResourceCache::with_cap_mib(root, 1);
    let a = DocumentSource("https://example.test/a.pdf".into());
    let b = DocumentSource("https://example.test/b.pdf".into());
    let c = DocumentSource("https://example.test/c.pdf".into());

    cache
        .put(&a, resource(&a, vec![b'a'; 400_000]))
        .await
        .expect("a should be written");
    tokio::time::sleep(Duration::from_millis(10)).await;
    cache
        .put(&b, resource(&b, vec![b'b'; 400_000]))
        .await
        .expect("b should be written");
    tokio::time::sleep(Duration::from_millis(10)).await;
    cache.get(&a).await.expect("a should read");
    tokio::time::sleep(Duration::from_millis(10)).await;
    // Three 400 KiB entries exceed the 1 MiB cap; the least recently used one
    // goes, and that is b, not a.
    cache
        .put(&c, resource(&c, vec![b'c'; 400_000]))
        .await
        .expect("c should be written");

    assert!(
        cache.get(&b).await.expect("b should read").is_none(),
        "the least recently used entry is evicted"
    );
    assert!(
        cache.get(&a).await.expect("a should read").is_some(),
        "a read entry is newer than one only written before it"
    );
    assert!(
        cache.get(&c).await.expect("c should read").is_some(),
        "the newest entry stays"
    );
}

/// The cap holds after every `put`, not just at startup.
#[tokio::test]
async fn the_cap_is_respected_after_each_put() {
    let directory = tempdir().expect("temporary directory should be created");
    let root = directory.path();
    let cache = FileRawResourceCache::with_cap_mib(root, 1);

    for index in 0..6 {
        let source = DocumentSource(format!("https://example.test/{index}.pdf"));
        cache
            .put(&source, resource(&source, vec![b'x'; 400_000]))
            .await
            .expect("entry should be written");
        tokio::time::sleep(Duration::from_millis(10)).await;
        assert!(
            directory_bytes(root) <= MIB,
            "the cache stays under its 1 MiB cap: {} bytes",
            directory_bytes(root)
        );
    }
}

/// A body never outlives its metadata or the other way round.
#[tokio::test]
async fn eviction_takes_the_body_and_the_metadata_together() {
    let directory = tempdir().expect("temporary directory should be created");
    let root = directory.path();
    let cache = FileRawResourceCache::with_cap_mib(root, 1);
    let evicted = DocumentSource("https://example.test/evicted.pdf".into());
    let kept = DocumentSource("https://example.test/kept.pdf".into());

    cache
        .put(&evicted, resource(&evicted, vec![b'e'; 400_000]))
        .await
        .expect("the entry should be written");
    tokio::time::sleep(Duration::from_millis(10)).await;
    cache
        .put(&kept, resource(&kept, vec![b'k'; 400_000]))
        .await
        .expect("the entry should be written");
    tokio::time::sleep(Duration::from_millis(10)).await;
    let filler = DocumentSource("https://example.test/filler.pdf".into());
    cache
        .put(&filler, resource(&filler, vec![b'f'; 400_000]))
        .await
        .expect("the entry should be written");

    let (body, metadata) = entry_paths(root, &evicted);
    assert!(
        !body.exists() && !metadata.exists(),
        "an evicted entry leaves neither half behind"
    );
    let mut names: Vec<String> = std::fs::read_dir(root.join("raw"))
        .expect("raw cache directory should exist")
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    for name in &names {
        let half = if name.ends_with(".bin") {
            ".json"
        } else {
            ".bin"
        };
        let other = name
            .strip_suffix(if name.ends_with(".bin") {
                ".bin"
            } else {
                ".json"
            })
            .expect("every cache file has an extension");
        assert!(
            root.join("raw").join(format!("{other}{half}")).exists(),
            "every surviving file keeps its pair: {names:?}"
        );
    }
    assert_eq!(names.len(), 4, "two whole entries survive: {names:?}");
}

/// `0` is today's behaviour: no cap, nothing evicted.
#[tokio::test]
async fn a_zero_cap_disables_eviction() {
    let directory = tempdir().expect("temporary directory should be created");
    let root = directory.path();
    let cache = FileRawResourceCache::with_cap_mib(root, 0);
    let mut sources = Vec::new();

    for index in 0..4 {
        let source = DocumentSource(format!("https://example.test/{index}.pdf"));
        cache
            .put(&source, resource(&source, vec![b'z'; 400_000]))
            .await
            .expect("entry should be written");
        sources.push(source);
    }
    for source in &sources {
        assert!(
            cache
                .get(source)
                .await
                .expect("entry should read")
                .is_some(),
            "an uncapped cache keeps every entry"
        );
    }
    assert!(directory_bytes(root) > MIB, "no eviction happened");
}

/// A reader racing an eviction sees the whole entry or a clean miss — never a
/// body without the metadata that describes it, and never an error.
///
/// The cache the reader uses has room for the 5 MiB target; the cache the
/// writer uses caps at 1 MiB, so the target alone exceeds that cap and its
/// prune must take the target whatever order the entries are in. The read of
/// the 5 MiB body and the eviction of that same entry therefore overlap, and
/// every outcome must be the whole entry or a clean miss.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_reader_racing_eviction_sees_a_whole_entry_or_a_clean_miss() {
    let directory = tempdir().expect("temporary directory should be created");
    let root = directory.path().to_path_buf();
    let cache = Arc::new(FileRawResourceCache::with_cap_mib(&root, 8));
    let evicting = Arc::new(FileRawResourceCache::with_cap_mib(&root, 1));
    let target = DocumentSource("https://example.test/target.pdf".into());
    let bytes = vec![b't'; 5 * 1024 * 1024];
    let churn = DocumentSource("https://example.test/churn.pdf".into());
    let mut hits = 0;
    let mut misses = 0;

    for round in 0..10 {
        for entry in std::fs::read_dir(root.join("raw")).into_iter().flatten() {
            let _ = std::fs::remove_file(entry.expect("entry should list").path());
        }
        cache
            .put(&target, resource(&target, bytes.clone()))
            .await
            .expect("the target entry should be written");
        for index in 0..2 {
            let newer = DocumentSource(format!("https://example.test/newer-{index}.pdf"));
            cache
                .put(&newer, resource(&newer, vec![b'n'; 600_000]))
                .await
                .expect("a newer entry should be written");
        }

        let write = {
            let evicting = Arc::clone(&evicting);
            let churn = churn.clone();
            tokio::spawn(async move {
                evicting
                    .put(&churn, resource(&churn, vec![b'c'; 600_000]))
                    .await
                    .expect("the churn entry should be written")
            })
        };
        let read = {
            let cache = Arc::clone(&cache);
            let target = target.clone();
            let bytes = bytes.clone();
            tokio::spawn(async move {
                cache
                    .get(&target)
                    .await
                    .expect("a race is never an error")
                    .map(|entry| {
                        assert_eq!(entry.bytes, bytes, "a hit is the whole entry");
                    })
            })
        };
        match read.await.expect("the reader should finish") {
            Some(()) => hits += 1,
            None => misses += 1,
        }
        write.await.expect("the writer should finish");
        let (body, metadata) = entry_paths(&root, &target);
        assert!(
            !body.exists() && !metadata.exists(),
            "round {round}: the evicted entry is gone whole, never half"
        );
        // The same entry, read once the eviction is done: a clean miss, not an
        // error and not a body whose metadata has gone.
        assert!(
            cache
                .get(&target)
                .await
                .expect("a miss is not an error")
                .is_none(),
            "round {round}: an evicted entry reads as a miss"
        );
    }
    assert_eq!(hits + misses, 10);
    assert!(hits > 0, "a reader still saw whole entries under eviction");
}

/// A stale body from an interrupted `put` (body written, metadata never landed)
/// is not an entry: it is evicted before any whole entry, and it never counts
/// as a cache hit.
#[tokio::test]
async fn an_orphan_body_is_evicted_before_a_whole_entry() {
    let directory = tempdir().expect("temporary directory should be created");
    let root = directory.path();
    let cache = FileRawResourceCache::with_cap_mib(root, 1);
    let kept = DocumentSource("https://example.test/kept.pdf".into());
    cache
        .put(&kept, resource(&kept, vec![b'k'; 300_000]))
        .await
        .expect("the entry should be written");

    let orphan = DocumentSource("https://example.test/orphan.pdf".into());
    let (orphan_body, orphan_metadata) = entry_paths(root, &orphan);
    std::fs::write(&orphan_body, vec![b'o'; 400_000]).expect("orphan body should be written");
    backdate(&orphan_body, Duration::from_secs(60));
    let filler = DocumentSource("https://example.test/filler.pdf".into());
    cache
        .put(&filler, resource(&filler, vec![b'f'; 400_000]))
        .await
        .expect("the entry should be written");

    assert!(
        !orphan_body.exists() && !orphan_metadata.exists(),
        "the orphan body goes first"
    );
    assert!(
        cache.get(&kept).await.expect("entry should read").is_some(),
        "the whole entry survives"
    );
    assert!(
        cache
            .get(&orphan)
            .await
            .expect("read should work")
            .is_none(),
        "an orphan body was never a hit"
    );
}
