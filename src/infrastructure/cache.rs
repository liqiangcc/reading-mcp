use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use sha2::{Digest, Sha256};
use tokio::sync::RwLock;

use crate::application::ports::{
    ApplicationError, ParsedCacheKey, ParsedDocumentCache, Parser, RawResourceCache,
    RetrievalOptions, RetrievedResource, Retriever,
};
use crate::domain::{Document, DocumentSource, NORMALIZATION_VERSION};

#[derive(Default)]
pub struct InMemoryRawResourceCache {
    entries: RwLock<HashMap<DocumentSource, RetrievedResource>>,
}

#[async_trait]
impl RawResourceCache for InMemoryRawResourceCache {
    async fn get(
        &self,
        source: &DocumentSource,
    ) -> Result<Option<RetrievedResource>, ApplicationError> {
        Ok(self.entries.read().await.get(source).cloned())
    }

    async fn put(
        &self,
        source: &DocumentSource,
        resource: RetrievedResource,
    ) -> Result<(), ApplicationError> {
        self.entries.write().await.insert(source.clone(), resource);
        Ok(())
    }
}

#[derive(Default)]
pub struct InMemoryParsedDocumentCache {
    entries: RwLock<HashMap<ParsedCacheKey, Document>>,
}

#[async_trait]
impl ParsedDocumentCache for InMemoryParsedDocumentCache {
    async fn get(&self, key: &ParsedCacheKey) -> Result<Option<Document>, ApplicationError> {
        Ok(self.entries.read().await.get(key).cloned())
    }

    async fn put(&self, key: ParsedCacheKey, document: Document) -> Result<(), ApplicationError> {
        self.entries.write().await.insert(key, document);
        Ok(())
    }
}

pub struct CachingRetriever {
    inner: Arc<dyn Retriever>,
    cache: Arc<dyn RawResourceCache>,
}

impl CachingRetriever {
    pub fn new(inner: Arc<dyn Retriever>, cache: Arc<dyn RawResourceCache>) -> Self {
        Self { inner, cache }
    }
}

#[async_trait]
impl Retriever for CachingRetriever {
    async fn retrieve(
        &self,
        source: &DocumentSource,
        options: &RetrievalOptions,
    ) -> Result<RetrievedResource, ApplicationError> {
        if !options.force_refresh
            && let Some(resource) = self.cache.get(source).await?
        {
            return Ok(resource);
        }

        let resource = self.inner.retrieve(source, options).await?;
        self.cache.put(source, resource.clone()).await?;
        Ok(resource)
    }
}

pub struct CachingParser {
    inner: Arc<dyn Parser>,
    cache: Arc<dyn ParsedDocumentCache>,
    pdf_namespace: Option<String>,
    ocr_fingerprint: String,
    ocr_admission: Option<Arc<super::ocr_singleflight::OcrSingleFlight>>,
}

impl CachingParser {
    pub fn new(inner: Arc<dyn Parser>, cache: Arc<dyn ParsedDocumentCache>) -> Self {
        Self {
            inner,
            cache,
            pdf_namespace: None,
            ocr_fingerprint: "ocr-disabled/v1".into(),
            ocr_admission: None,
        }
    }
    pub fn with_ocr_fingerprint(mut self, fingerprint: impl Into<String>) -> Self {
        self.ocr_fingerprint = fingerprint.into();
        self
    }
    pub fn with_ocr_admission(mut self, enabled: bool) -> Self {
        self.ocr_admission =
            enabled.then(|| Arc::new(super::ocr_singleflight::OcrSingleFlight::default()));
        self
    }
    pub fn with_pdf_namespace(mut self, namespace: &str) -> Self {
        self.pdf_namespace = Some(namespace.into());
        self
    }
}

#[async_trait]
impl Parser for CachingParser {
    async fn parse(&self, resource: RetrievedResource) -> Result<Document, ApplicationError> {
        let key = ParsedCacheKey {
            final_source: resource.final_source.clone(),
            raw_sha256: format!("sha256:{:x}", Sha256::digest(&resource.bytes)),
            normalization_version: if resource
                .media_type
                .0
                .split(';')
                .next()
                .is_some_and(|m| m.trim().eq_ignore_ascii_case("application/pdf"))
            {
                self.pdf_namespace
                    .as_ref()
                    .map(|ns| format!("{NORMALIZATION_VERSION}:{ns}"))
                    .unwrap_or_else(|| NORMALIZATION_VERSION.into())
            } else {
                NORMALIZATION_VERSION.into()
            },
            ocr_fingerprint: self.ocr_fingerprint.clone(),
        };

        if let Some(document) = self.cache.get(&key).await? {
            document
                .validate_ocr_publication()
                .map_err(ApplicationError::CacheFailed)?;
            return Ok(document);
        }

        if let Some(admission) = &self.ocr_admission
            && resource
                .media_type
                .0
                .split(';')
                .next()
                .is_some_and(|m| m.trim().eq_ignore_ascii_case("application/pdf"))
        {
            return admission
                .parse(key, resource, self.inner.clone(), self.cache.clone())
                .await;
        }
        let document = self.inner.parse(resource).await?;
        document
            .validate_ocr_publication()
            .map_err(ApplicationError::CacheFailed)?;
        self.cache.put(key, document.clone()).await?;
        Ok(document)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{ContentHash, DocumentId, DocumentSource, MediaType};
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct FakeParser {
        calls: Arc<AtomicUsize>,
    }
    #[async_trait]
    impl Parser for FakeParser {
        async fn parse(&self, resource: RetrievedResource) -> Result<Document, ApplicationError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(Document {
                id: DocumentId("fake".into()),
                source: resource.final_source,
                title: "fake".into(),
                media_type: resource.media_type,
                content_hash: ContentHash("raw".into()),
                metadata: Default::default(),
                root_sections: vec![],
            })
        }
    }

    #[tokio::test]
    async fn ocr_fingerprint_changes_parsed_cache_identity() {
        let calls = Arc::new(AtomicUsize::new(0));
        let cache: Arc<dyn ParsedDocumentCache> = Arc::new(InMemoryParsedDocumentCache::default());
        let source = DocumentSource("file:///fixture.pdf".into());
        let resource = RetrievedResource {
            source: source.clone(),
            final_source: source,
            media_type: MediaType("application/pdf".into()),
            bytes: b"same".to_vec(),
            etag: None,
            last_modified: None,
            metadata: Default::default(),
        };
        let parser_a = CachingParser::new(
            Arc::new(FakeParser {
                calls: calls.clone(),
            }),
            cache.clone(),
        )
        .with_ocr_fingerprint("A");
        parser_a.parse(resource.clone()).await.unwrap();
        parser_a.parse(resource.clone()).await.unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        let parser_b = CachingParser::new(
            Arc::new(FakeParser {
                calls: calls.clone(),
            }),
            cache,
        )
        .with_ocr_fingerprint("B");
        parser_b.parse(resource).await.unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }
}
