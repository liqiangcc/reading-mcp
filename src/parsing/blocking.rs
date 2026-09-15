use crate::application::ports::ApplicationError;

/// Runs synchronous, CPU-bound parser work on Tokio's blocking thread pool so
/// in-process parsers cannot stall async runtime workers.
///
/// This is worker isolation, not preemption: once started, blocking work
/// cannot be aborted, so an outer `tokio::time::timeout` (for example
/// `BudgetedParser`) only stops waiting while the thread runs to its bounded
/// completion. Actual work volume stays bounded by `ResourceBudget` and
/// `ArchiveLimits`. Killable isolation lives at separate process boundaries
/// (PDF layout worker, OCR sandbox); this helper does not provide those
/// guarantees.
///
/// When no Tokio runtime is present (synchronous callers or tests), the work
/// runs inline.
pub(crate) async fn run_blocking<T, F>(work: F) -> Result<T, ApplicationError>
where
    F: FnOnce() -> Result<T, ApplicationError> + Send + 'static,
    T: Send + 'static,
{
    if tokio::runtime::Handle::try_current().is_err() {
        return work();
    }
    tokio::task::spawn_blocking(work).await.map_err(|_| {
        ApplicationError::ParseFailed("parser worker terminated unexpectedly".into())
    })?
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc;
    use std::time::Duration;

    use super::run_blocking;
    use crate::application::ports::ApplicationError;

    #[tokio::test(flavor = "current_thread")]
    async fn blocking_work_does_not_starve_current_thread_executor() {
        let started = Arc::new(AtomicBool::new(false));
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let parse_started = started.clone();
        let parse = tokio::spawn(async move {
            run_blocking(move || {
                parse_started.store(true, Ordering::SeqCst);
                release_rx.recv().expect("release channel");
                Ok::<u8, ApplicationError>(7)
            })
            .await
        });

        // While the blocking closure is parked, the single async worker must
        // still schedule unrelated tasks. If the work ran inline this loop
        // would never observe `started` and the heartbeat would deadlock.
        let heartbeat = tokio::spawn(async move {
            while !started.load(Ordering::SeqCst) {
                tokio::task::yield_now().await;
            }
            42u8
        });
        let observed = tokio::time::timeout(Duration::from_secs(10), heartbeat)
            .await
            .expect("executor stayed responsive while blocking work was parked")
            .expect("heartbeat task");
        assert_eq!(observed, 42);

        release_tx.send(()).expect("release blocking work");
        let result = tokio::time::timeout(Duration::from_secs(10), parse)
            .await
            .expect("blocking work completed after release")
            .expect("parse task")
            .expect("parse result");
        assert_eq!(result, 7);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn timeout_stops_waiting_but_bounded_work_still_completes() {
        let (done_tx, done_rx) = mpsc::channel::<()>();
        let parse = run_blocking(move || {
            std::thread::sleep(Duration::from_millis(50));
            done_tx.send(()).expect("completion signal");
            Ok::<(), ApplicationError>(())
        });
        // The outer timeout only stops waiting; it cannot preempt started
        // blocking work, which runs to its bounded completion.
        assert!(
            tokio::time::timeout(Duration::from_millis(5), parse)
                .await
                .is_err()
        );
        assert!(
            done_rx.recv_timeout(Duration::from_secs(10)).is_ok(),
            "parked blocking work must still complete after the caller stops waiting"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn errors_and_panics_map_to_typed_errors() {
        let err =
            run_blocking(|| Err::<(), _>(ApplicationError::ParseFailed("typed failure".into())))
                .await
                .expect_err("typed error propagates");
        assert!(matches!(err, ApplicationError::ParseFailed(_)));

        let err = run_blocking(|| -> Result<(), ApplicationError> {
            panic!("internal panic detail");
        })
        .await
        .expect_err("panic maps to typed error");
        match err {
            ApplicationError::ParseFailed(message) => {
                assert!(
                    !message.contains("internal panic detail"),
                    "panic payload must not leak into the typed error: {message}"
                );
            }
            other => panic!("expected ParseFailed, got {other:?}"),
        }
    }
}
