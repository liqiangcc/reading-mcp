use std::path::Path;
use std::sync::{Arc, Mutex};

use rusqlite::Connection;

use crate::application::ports::ApplicationError;

/// Creates the parent directory, opens the database and enables WAL with `synchronous=NORMAL`.
/// `error` wraps a message in the caller's error variant.
pub(super) fn open_connection(
    path: &Path,
    error: fn(String) -> ApplicationError,
) -> Result<Connection, ApplicationError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|source| error(format!("{}: {source}", parent.display())))?;
    }
    let connection = Connection::open(path).map_err(|source| error(source.to_string()))?;
    // The page cache keeps a reader's point-query pages resident while a whole-book scan (the
    // lexical search) runs on another connection. 16 MiB held the SEC-2 working set, but at
    // 20,000 sections the repository's full section load is ~10 MiB and the scan's layout/text
    // pages are larger still, so 16 MiB evicted the reader's pages after a few searches and every
    // read re-read them. 32 MiB holds the 20,000-section working set; it is the same cache, sized
    // for the book the gate measures, not a new one.
    connection
        .execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL; PRAGMA cache_size=-32768;",
        )
        .map_err(|source| error(source.to_string()))?;
    Ok(connection)
}

/// The file name without the long directory prefix, for a trace line that stays readable.
pub(crate) fn short_file(file: &str) -> &str {
    file.rsplit('/').next().unwrap_or(file)
}

/// OBR prototype: the connections a read may use, separate from the one writer connection.
///
/// The store keeps one `Connection` behind one `Mutex` for writes (and, on main, for reads too), so
/// a read waits for the whole of an ingest write or a whole-book scan. WAL already lets a reader
/// work on the last committed snapshot while a writer runs, so a small pool of read-only
/// connections removes the wait without changing what a reader sees: it is the same file, the same
/// snapshot rules, and `query_only` refuses any write on these connections.
pub(super) struct ReadPool {
    path: std::path::PathBuf,
    slots: Mutex<Vec<Connection>>,
    capacity: usize,
}

impl ReadPool {
    pub(super) fn new(path: &Path) -> Self {
        let capacity = std::env::var("READING_MCP_READ_POOL")
            .ok()
            .and_then(|value| value.trim().parse().ok())
            .unwrap_or(4usize)
            .clamp(1, 16);
        Self {
            path: path.to_path_buf(),
            slots: Mutex::new(Vec::new()),
            capacity,
        }
    }

    fn take(&self, error: fn(String) -> ApplicationError) -> Result<Connection, ApplicationError> {
        let mut slots = self
            .slots
            .lock()
            .map_err(|_| error("SQLite read pool poisoned".into()))?;
        if let Some(connection) = slots.pop() {
            return Ok(connection);
        }
        drop(slots);
        let connection =
            Connection::open(&self.path).map_err(|source| error(source.to_string()))?;
        connection
            .execute_batch(
                "PRAGMA journal_mode=WAL; PRAGMA query_only=1; PRAGMA busy_timeout=5000; \
                 PRAGMA cache_size=-32768;",
            )
            .map_err(|source| error(source.to_string()))?;
        Ok(connection)
    }

    fn put(&self, connection: Connection) {
        if let Ok(mut slots) = self.slots.lock()
            && slots.len() < self.capacity
        {
            slots.push(connection);
        }
    }
}

/// Runs read-only SQLite work on a pooled read connection, so a read never waits behind a write.
pub(super) async fn run_sqlite_read<T, F>(
    label: &'static str,
    pool: &Arc<ReadPool>,
    error: fn(String) -> ApplicationError,
    work: F,
) -> Result<T, ApplicationError>
where
    F: FnOnce(&Connection) -> Result<T, ApplicationError> + Send + 'static,
    T: Send + 'static,
{
    let id = trace::next_id();
    let entered = std::time::Instant::now();
    let traced = trace::enabled();
    let pool = Arc::clone(pool);
    let result = tokio::task::spawn_blocking(move || {
        let queued = std::time::Instant::now();
        let connection = pool.take(error)?;
        let locked = std::time::Instant::now();
        let result = work(&connection);
        let done = std::time::Instant::now();
        pool.put(connection);
        if traced {
            trace::emit(
                "run_sqlite_read",
                label,
                &format!(
                    "id={id} wait_ms={} hold_ms={}",
                    locked.duration_since(queued).as_millis(),
                    done.duration_since(locked).as_millis()
                ),
            );
        }
        result
    })
    .await
    .map_err(|_| error("SQLite worker terminated unexpectedly".into()))?;
    if traced {
        trace::emit(
            "run_sqlite_read_done",
            "run_sqlite_read",
            &format!("id={id} total_ms={}", entered.elapsed().as_millis()),
        );
    }
    result
}

/// Runs SQLite work on Tokio's blocking pool so disk I/O and lock waits never stall async workers.
pub(super) async fn run_sqlite<T, F>(
    label: &'static str,
    connection: &Arc<Mutex<Connection>>,
    error: fn(String) -> ApplicationError,
    work: F,
) -> Result<T, ApplicationError>
where
    F: FnOnce(&mut Connection) -> Result<T, ApplicationError> + Send + 'static,
    T: Send + 'static,
{
    let id = trace::next_id();
    let entered = std::time::Instant::now();
    let traced = trace::enabled();
    let connection = Arc::clone(connection);
    let result = tokio::task::spawn_blocking(move || {
        let queued = std::time::Instant::now();
        let mut connection = connection
            .lock()
            .map_err(|_| error("SQLite lock poisoned".into()))?;
        let locked = std::time::Instant::now();
        let result = work(&mut connection);
        let done = std::time::Instant::now();
        if traced {
            trace::emit(
                "run_sqlite",
                label,
                &format!(
                    "id={id} wait_ms={} hold_ms={}",
                    locked.duration_since(queued).as_millis(),
                    done.duration_since(locked).as_millis()
                ),
            );
        }
        result
    })
    .await
    .map_err(|_| error("SQLite worker terminated unexpectedly".into()))?;
    if traced {
        trace::emit(
            "run_sqlite_done",
            "run_sqlite",
            &format!(
                "id={id} total_ms={}",
                entered.elapsed().as_millis()
            ),
        );
    }
    result
}

/// OBR trace: `READING_MCP_SQLITE_TRACE=1` makes every SQLite wait and hold time visible on
/// stderr, tagged with the call site that asked for the connection. Scratch instrumentation only:
/// it is how the investigation names the mechanism instead of guessing at it.
pub(crate) mod trace {
    use std::sync::OnceLock;
    use std::time::Instant;

    fn origin() -> &'static Instant {
        static ORIGIN: OnceLock<Instant> = OnceLock::new();
        ORIGIN.get_or_init(Instant::now)
    }

    pub(crate) fn enabled() -> bool {
        static ENABLED: OnceLock<bool> = OnceLock::new();
        *ENABLED.get_or_init(|| std::env::var_os("READING_MCP_SQLITE_TRACE").is_some())
    }

    /// Milliseconds since the first traced event of this process.
    pub(crate) fn now_ms() -> u128 {
        origin().elapsed().as_millis()
    }

    pub(crate) fn next_id() -> u64 {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    }

    /// The `reading_mcp` frame that asked for the connection, from a captured backtrace: the first
    /// frame below this module that carries a file and line. Scratch instrumentation only.
    pub(crate) fn caller_site() -> String {
        let backtrace = std::backtrace::Backtrace::force_capture().to_string();
        let mut seen_self = false;
        for line in backtrace.lines() {
            let line = line.trim();
            if !line.contains("reading_mcp") {
                continue;
            }
            if line.contains("sqlite_support") {
                seen_self = true;
                continue;
            }
            if seen_self {
                return line
                    .rsplit_once("reading_mcp::")
                    .map(|(_, rest)| rest)
                    .unwrap_or(line)
                    .to_owned();
            }
        }
        "unknown".into()
    }

    pub(crate) fn emit(kind: &str, caller: &str, fields: &str) {
        if !enabled() {
            return;
        }
        eprintln!(
            "SQLITE_TRACE t_ms={} kind={} caller={} {}",
            now_ms(),
            kind,
            caller,
            fields
        );
    }
}
