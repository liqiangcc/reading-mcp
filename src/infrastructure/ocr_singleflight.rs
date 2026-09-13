//! Local, OCR-only admission and same-key cancellation ownership. No durable jobs.
use crate::{
    application::ports::{
        ApplicationError, ParsedCacheKey, ParsedDocumentCache, Parser, RetrievedResource,
    },
    domain::Document,
};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    sync::{Semaphore, watch},
    task::AbortHandle,
};

type Outcome = Result<Document, ApplicationError>;
struct Flight {
    result: watch::Receiver<Option<Outcome>>,
    waiters: usize,
    abort: AbortHandle,
    token: Arc<()>,
}

pub(super) struct OcrSingleFlight {
    flights: Mutex<HashMap<ParsedCacheKey, Flight>>,
    active: Arc<Semaphore>,
    capacity: Arc<Semaphore>,
}

impl Default for OcrSingleFlight {
    fn default() -> Self {
        Self {
            flights: Mutex::new(HashMap::new()),
            active: Arc::new(Semaphore::new(1)),
            capacity: Arc::new(Semaphore::new(3)),
        }
    }
}

impl OcrSingleFlight {
    pub(super) async fn parse(
        self: &Arc<Self>,
        key: ParsedCacheKey,
        resource: RetrievedResource,
        parser: Arc<dyn Parser>,
        cache: Arc<dyn ParsedDocumentCache>,
    ) -> Outcome {
        let (mut result, _waiter) = {
            let mut flights = self
                .flights
                .lock()
                .map_err(|_| ApplicationError::CacheFailed("OCR admission lock poisoned".into()))?;
            if let Some(flight) = flights.get_mut(&key) {
                flight.waiters += 1;
                (
                    flight.result.clone(),
                    Waiter {
                        owner: self.clone(),
                        key,
                        token: flight.token.clone(),
                    },
                )
            } else {
                let capacity = self
                    .capacity
                    .clone()
                    .try_acquire_owned()
                    .map_err(|_| ApplicationError::OcrBusy)?;
                let (sender, receiver) = watch::channel(None);
                let active = self.active.clone();
                let task_key = key.clone();
                let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
                let task = tokio::spawn(async move {
                    let _capacity = capacity;
                    let operation = async {
                        let _active =
                            tokio::time::timeout(Duration::from_secs(2), active.acquire_owned())
                                .await
                                .map_err(|_| ApplicationError::OcrBusy)?
                                .map_err(|_| {
                                    ApplicationError::CacheFailed("OCR admission closed".into())
                                })?;
                        // The cache may have been published between the caller's
                        // initial lookup and admission. Do not execute it twice.
                        if let Some(document) = cache.get(&task_key).await? {
                            document
                                .validate_ocr_publication()
                                .map_err(ApplicationError::CacheFailed)?;
                            return Ok(document);
                        }
                        let document = parser.parse(resource).await?;
                        document
                            .validate_ocr_publication()
                            .map_err(ApplicationError::CacheFailed)?;
                        cache.put(task_key, document.clone()).await?;
                        Ok(document)
                    };
                    let outcome = tokio::time::timeout_at(deadline, operation)
                        .await
                        .unwrap_or(Err(ApplicationError::OcrTimeout));
                    let _ = sender.send(Some(outcome));
                });
                let token = Arc::new(());
                flights.insert(
                    key.clone(),
                    Flight {
                        result: receiver.clone(),
                        waiters: 1,
                        abort: task.abort_handle(),
                        token: token.clone(),
                    },
                );
                (
                    receiver,
                    Waiter {
                        owner: self.clone(),
                        key,
                        token,
                    },
                )
            }
        };
        loop {
            if let Some(outcome) = result.borrow_and_update().clone() {
                return outcome;
            }
            result.changed().await.map_err(|_| {
                ApplicationError::ParseFailed(
                    "OCR shared worker terminated without a result".into(),
                )
            })?;
        }
    }
}

struct Waiter {
    owner: Arc<OcrSingleFlight>,
    key: ParsedCacheKey,
    token: Arc<()>,
}

impl Drop for Waiter {
    fn drop(&mut self) {
        // No await between removing the last waiter and aborting the operation.
        let mut flights = self
            .owner
            .flights
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if let Some(flight) = flights.get_mut(&self.key)
            && Arc::ptr_eq(&flight.token, &self.token)
        {
            flight.waiters -= 1;
            if flight.waiters == 0 {
                flight.abort.abort();
                flights.remove(&self.key);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        domain::{ContentHash, DocumentId, DocumentSource, MediaType},
        infrastructure::InMemoryParsedDocumentCache,
    };
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct GateParser {
        calls: AtomicUsize,
        cancelled: Arc<AtomicUsize>,
        gate: Semaphore,
    }

    impl Default for GateParser {
        fn default() -> Self {
            Self {
                calls: AtomicUsize::new(0),
                cancelled: Arc::new(AtomicUsize::new(0)),
                gate: Semaphore::new(0),
            }
        }
    }

    struct Cancellation(Arc<AtomicUsize>, bool);
    impl Drop for Cancellation {
        fn drop(&mut self) {
            if !self.1 {
                self.0.fetch_add(1, Ordering::SeqCst);
            }
        }
    }

    #[async_trait]
    impl Parser for GateParser {
        async fn parse(&self, resource: RetrievedResource) -> Outcome {
            let mut cancellation = Cancellation(self.cancelled.clone(), false);
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.gate.acquire().await.unwrap().forget();
            cancellation.1 = true;
            Ok(Document {
                id: DocumentId(resource.final_source.0.clone()),
                source: resource.final_source,
                title: "synthetic".into(),
                media_type: resource.media_type,
                content_hash: ContentHash("raw".into()),
                metadata: Default::default(),
                root_sections: vec![],
            })
        }
    }

    fn input(name: &str) -> (ParsedCacheKey, RetrievedResource) {
        let source = DocumentSource(format!("file:///{name}.pdf"));
        (
            ParsedCacheKey {
                final_source: source.clone(),
                raw_sha256: name.into(),
                normalization_version: "v11".into(),
                ocr_fingerprint: "verified-test-identity".into(),
            },
            RetrievedResource {
                source: source.clone(),
                final_source: source,
                media_type: MediaType("application/pdf".into()),
                bytes: name.as_bytes().to_vec(),
                etag: None,
                last_modified: None,
                metadata: Default::default(),
            },
        )
    }

    fn request(
        owner: Arc<OcrSingleFlight>,
        name: &str,
        parser: Arc<GateParser>,
        cache: Arc<InMemoryParsedDocumentCache>,
    ) -> tokio::task::JoinHandle<Outcome> {
        let (key, resource) = input(name);
        tokio::spawn(async move { owner.parse(key, resource, parser, cache).await })
    }

    async fn until(mut ready: impl FnMut() -> bool) {
        tokio::time::timeout(Duration::from_secs(5), async {
            while !ready() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn same_key_shares_work_and_one_cancelled_waiter_does_not_cancel_it() {
        let owner = Arc::new(OcrSingleFlight::default());
        let parser = Arc::new(GateParser::default());
        let cache = Arc::new(InMemoryParsedDocumentCache::default());
        let first = request(owner.clone(), "same", parser.clone(), cache.clone());
        until(|| parser.calls.load(Ordering::SeqCst) == 1).await;
        let second = request(owner.clone(), "same", parser.clone(), cache.clone());
        until(|| {
            owner
                .flights
                .lock()
                .unwrap()
                .get(&input("same").0)
                .is_some_and(|f| f.waiters == 2)
        })
        .await;
        first.abort();
        assert!(first.await.unwrap_err().is_cancelled());
        assert_eq!(parser.cancelled.load(Ordering::SeqCst), 0);
        parser.gate.add_permits(1);
        let result = second.await.unwrap().unwrap();
        assert_eq!(parser.calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            cache.get(&input("same").0).await.unwrap(),
            Some(result.clone())
        );
        assert_eq!(
            request(owner.clone(), "same", parser.clone(), cache)
                .await
                .unwrap()
                .unwrap(),
            result
        );
        assert_eq!(parser.calls.load(Ordering::SeqCst), 1);
        assert!(owner.flights.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn last_waiter_cancellation_drops_parse_and_does_not_publish_cache() {
        let owner = Arc::new(OcrSingleFlight::default());
        let parser = Arc::new(GateParser::default());
        let cache = Arc::new(InMemoryParsedDocumentCache::default());
        let task = request(owner.clone(), "cancel", parser.clone(), cache.clone());
        until(|| parser.calls.load(Ordering::SeqCst) == 1).await;
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        until(|| parser.cancelled.load(Ordering::SeqCst) == 1).await;
        assert!(cache.get(&input("cancel").0).await.unwrap().is_none());
        assert!(owner.flights.lock().unwrap().is_empty());
        until(|| owner.capacity.available_permits() == 3).await;
    }

    #[tokio::test]
    async fn only_two_distinct_keys_can_wait_and_they_expire_without_running() {
        let owner = Arc::new(OcrSingleFlight::default());
        let parser = Arc::new(GateParser::default());
        let cache = Arc::new(InMemoryParsedDocumentCache::default());
        let first = request(owner.clone(), "active", parser.clone(), cache.clone());
        until(|| parser.calls.load(Ordering::SeqCst) == 1).await;
        let second = request(owner.clone(), "queued-a", parser.clone(), cache.clone());
        let third = request(owner.clone(), "queued-b", parser.clone(), cache.clone());
        until(|| owner.flights.lock().unwrap().len() == 3).await;
        let error = request(owner.clone(), "overflow", parser.clone(), cache)
            .await
            .unwrap()
            .unwrap_err();
        assert_eq!(error, ApplicationError::OcrBusy);
        for task in [second, third] {
            let error = task.await.unwrap().unwrap_err();
            assert_eq!(error, ApplicationError::OcrBusy);
        }
        assert_eq!(parser.calls.load(Ordering::SeqCst), 1);
        first.abort();
        let _ = first.await;
        until(|| parser.cancelled.load(Ordering::SeqCst) == 1).await;
    }

    #[tokio::test(start_paused = true)]
    async fn shared_deadline_returns_typed_timeout_and_cancels_without_cache_publication() {
        let owner = Arc::new(OcrSingleFlight::default());
        let parser = Arc::new(GateParser::default());
        let cache = Arc::new(InMemoryParsedDocumentCache::default());
        let started = tokio::time::Instant::now();
        let error = request(owner.clone(), "timeout", parser.clone(), cache.clone())
            .await
            .unwrap()
            .unwrap_err();
        assert_eq!(error, ApplicationError::OcrTimeout);
        assert_eq!(started.elapsed(), Duration::from_secs(60));
        assert_eq!(parser.cancelled.load(Ordering::SeqCst), 1);
        assert!(cache.get(&input("timeout").0).await.unwrap().is_none());
        assert!(owner.flights.lock().unwrap().is_empty());
        assert_eq!(owner.capacity.available_permits(), 3);
    }
}
