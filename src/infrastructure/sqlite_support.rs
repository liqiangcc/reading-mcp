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
