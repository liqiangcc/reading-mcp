use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use rusqlite::Connection;

use crate::application::ports::ApplicationError;

/// The environment variable that sets how many read connections a store keeps.
const READ_POOL_ENV: &str = "READING_MCP_READ_POOL";
/// The read pool size when the variable is unset, empty or not a number.
const READ_POOL_DEFAULT: usize = 4;
/// The largest pool one store keeps, so a typo cannot open an unbounded number of connections.
const READ_POOL_MAX: usize = 16;
/// How long a pooled read waits for a lock instead of reporting `SQLITE_BUSY`: a checkpoint can
/// hold the database for a moment, and waiting is what keeps that from turning into an error.
const READ_BUSY_TIMEOUT_MS: u32 = 5_000;
/// A pooled connection serves point reads, so its page cache only has to hold the pages of the
/// section a call reads. Four readers at 8 MiB are the same memory as the writer's 32 MiB cache.
const READ_CACHE_KIB: i32 = -8 * 1024;

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

/// The read pool size of this process: `READING_MCP_READ_POOL`, default 4.
pub(super) fn read_pool_capacity() -> usize {
    read_pool_capacity_from(std::env::var(READ_POOL_ENV).ok().as_deref())
}

/// Parses `READING_MCP_READ_POOL`: 4 when unset or not a number, at most 16. `0` means "no read
/// connections": every operation runs on the writer connection, which is the behaviour before the
/// pool existed.
fn read_pool_capacity_from(value: Option<&str>) -> usize {
    value
        .and_then(|value| value.trim().parse::<usize>().ok())
        .unwrap_or(READ_POOL_DEFAULT)
        .min(READ_POOL_MAX)
}

/// The read-only connections of one store, separate from its one writer connection.
///
/// A store keeps one `Connection` behind one `Mutex`, and `run_sqlite` holds that mutex for the
/// whole closure, so the whole-book ingest write (1.15 s on 宋史) and the whole-book lexical scan
/// (0.36 s) stall every read on every other document. WAL already lets a reader work on the last
/// committed snapshot while a writer runs, so a small pool of `query_only` connections removes the
/// wait without changing what a reader sees: same file, same snapshot rules, and `query_only`
/// refuses any write on these connections.
pub(super) struct ReadPool {
    /// The writer connection. A read uses it whenever it is idle, and always when the pool is off.
    writer: Arc<Mutex<Connection>>,
    path: PathBuf,
    capacity: usize,
    /// The pooled connections that are not in use. Opened on demand, kept up to `capacity`.
    idle: Mutex<Vec<Connection>>,
}

impl ReadPool {
    pub(super) fn new(path: &Path, writer: Arc<Mutex<Connection>>, capacity: usize) -> Self {
        // A second connection to an in-memory database is a different, empty database, so such a
        // store keeps reading on its writer whatever the variable says.
        let in_memory =
            path.as_os_str() == ":memory:" || path.to_string_lossy().contains("mode=memory");
        Self {
            writer,
            path: path.to_path_buf(),
            capacity: if in_memory { 0 } else { capacity },
            idle: Mutex::new(Vec::new()),
        }
    }

    /// Runs one logical read on the connection that can serve it without waiting.
    ///
    /// The writer serves the read whenever it is idle: that connection is already open and warm,
    /// so a store that never sees a concurrent write never opens a second connection and pays
    /// nothing for the pool. Only a read that would have to wait for the writer takes a pooled
    /// connection; with `READING_MCP_READ_POOL=0` there is none, and the read waits, exactly as it
    /// did before the pool existed.
    fn read<T, F>(
        &self,
        error: fn(String) -> ApplicationError,
        work: F,
    ) -> Result<T, ApplicationError>
    where
        F: FnOnce(&Connection) -> Result<T, ApplicationError>,
    {
        if let Ok(writer) = self.writer.try_lock() {
            return self.snapshot(&writer, error, work);
        }
        match self.take(error)? {
            Some(connection) => {
                let result = self.snapshot(&connection, error, work);
                self.put(connection);
                result
            }
            None => {
                let writer = self
                    .writer
                    .lock()
                    .map_err(|_| error("SQLite lock poisoned".into()))?;
                self.snapshot(&writer, error, work)
            }
        }
    }

    /// A connection to read on: an idle pooled one, a new one, or `None` when the pool is off and
    /// the caller has to use the writer.
    fn take(
        &self,
        error: fn(String) -> ApplicationError,
    ) -> Result<Option<Connection>, ApplicationError> {
        if self.capacity == 0 {
            return Ok(None);
        }
        if let Some(connection) = self
            .idle
            .lock()
            .map_err(|_| error("SQLite read pool poisoned".into()))?
            .pop()
        {
            return Ok(Some(connection));
        }
        let connection =
            Connection::open(&self.path).map_err(|source| error(source.to_string()))?;
        // The writer already put the file in WAL, so a reader never sets the journal mode: that
        // changes the header and `query_only` refuses it. A reader only states that it will not
        // write, waits for a lock instead of failing, and sizes its cache for point reads.
        connection
            .execute_batch(&format!(
                "PRAGMA query_only=1; PRAGMA busy_timeout={READ_BUSY_TIMEOUT_MS}; \
                 PRAGMA cache_size={READ_CACHE_KIB};"
            ))
            .map_err(|source| error(source.to_string()))?;
        Ok(Some(connection))
    }

    fn put(&self, connection: Connection) {
        if let Ok(mut idle) = self.idle.lock()
            && idle.len() < self.capacity
        {
            idle.push(connection);
        }
    }

    /// One logical read on one connection. With the pool on it runs in one deferred transaction, so
    /// every statement of the read sees the same snapshot; with the pool off it runs exactly as
    /// `run_sqlite` runs it, in autocommit, the behaviour before the pool existed.
    fn snapshot<T, F>(
        &self,
        connection: &Connection,
        error: fn(String) -> ApplicationError,
        work: F,
    ) -> Result<T, ApplicationError>
    where
        F: FnOnce(&Connection) -> Result<T, ApplicationError>,
    {
        if self.capacity == 0 {
            return work(connection);
        }
        let transaction = connection
            .unchecked_transaction()
            .map_err(|source| error(source.to_string()))?;
        match work(&transaction) {
            Ok(value) => {
                transaction
                    .commit()
                    .map_err(|source| error(source.to_string()))?;
                Ok(value)
            }
            Err(failure) => {
                let _ = transaction.rollback();
                Err(failure)
            }
        }
    }
}

/// Runs read-only SQLite work on a read connection, so a read never waits behind a write.
///
/// With `READING_MCP_READ_POOL=0` every read runs on the writer connection, exactly as
/// `run_sqlite` runs it today.
pub(super) async fn run_sqlite_read<T, F>(
    pool: &Arc<ReadPool>,
    error: fn(String) -> ApplicationError,
    work: F,
) -> Result<T, ApplicationError>
where
    F: FnOnce(&Connection) -> Result<T, ApplicationError> + Send + 'static,
    T: Send + 'static,
{
    let pool = Arc::clone(pool);
    tokio::task::spawn_blocking(move || pool.read(error, work))
        .await
        .map_err(|_| error("SQLite worker terminated unexpectedly".into()))?
}

/// Runs SQLite work on Tokio's blocking pool so disk I/O and lock waits never stall async workers.
pub(super) async fn run_sqlite<T, F>(
    connection: &Arc<Mutex<Connection>>,
    error: fn(String) -> ApplicationError,
    work: F,
) -> Result<T, ApplicationError>
where
    F: FnOnce(&mut Connection) -> Result<T, ApplicationError> + Send + 'static,
    T: Send + 'static,
{
    let connection = Arc::clone(connection);
    tokio::task::spawn_blocking(move || {
        let mut connection = connection
            .lock()
            .map_err(|_| error("SQLite lock poisoned".into()))?;
        work(&mut connection)
    })
    .await
    .map_err(|_| error("SQLite worker terminated unexpectedly".into()))?
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::{Arc, Mutex};

    use super::{READ_POOL_DEFAULT, READ_POOL_MAX, ReadPool, read_pool_capacity_from};

    #[test]
    fn the_read_pool_size_is_four_unless_the_variable_says_otherwise() {
        assert_eq!(read_pool_capacity_from(None), READ_POOL_DEFAULT);
        assert_eq!(read_pool_capacity_from(None), 4);
        assert_eq!(read_pool_capacity_from(Some("")), 4);
        assert_eq!(read_pool_capacity_from(Some("four")), 4);
        assert_eq!(read_pool_capacity_from(Some("-1")), 4);
    }

    #[test]
    fn zero_read_connections_is_a_size_and_is_kept() {
        // 0 must stay 0: it is how a deployment gets the writer-connection-only behaviour back.
        assert_eq!(read_pool_capacity_from(Some("0")), 0);
        assert_eq!(read_pool_capacity_from(Some(" 0 ")), 0);
    }

    #[test]
    fn an_in_memory_store_reads_on_its_writer() {
        // `Connection::open(":memory:")` twice is two empty databases, so a pool would answer from
        // nothing. The store keeps reading on its writer instead.
        let writer = Arc::new(Mutex::new(
            rusqlite::Connection::open_in_memory().expect("memory"),
        ));
        let pool = ReadPool::new(Path::new(":memory:"), Arc::clone(&writer), 4);
        assert!(
            pool.take(crate::application::ports::ApplicationError::RepositoryFailed)
                .expect("take")
                .is_none()
        );
    }

    #[test]
    fn a_pool_size_is_capped() {
        assert_eq!(read_pool_capacity_from(Some("1")), 1);
        assert_eq!(read_pool_capacity_from(Some("8")), 8);
        assert_eq!(read_pool_capacity_from(Some("1000")), READ_POOL_MAX);
    }
}
