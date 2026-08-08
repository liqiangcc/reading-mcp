use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use sha2::{Digest, Sha256};
use tokio::sync::RwLock;

use crate::application::ports::{
    ApplicationError, ParsedCacheKey, ParsedDocumentCache, Parser, RawResourceCache,
    RetrievalOptions, RetrievedResource, Retriever,
};
use crate::domain::{Document, DocumentSource};

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
}

impl CachingParser {
    pub fn new(inner: Arc<dyn Parser>, cache: Arc<dyn ParsedDocumentCache>) -> Self {
        Self { inner, cache }
    }
}

#[async_trait]
impl Parser for CachingParser {
    async fn parse(&self, resource: RetrievedResource) -> Result<Document, ApplicationError> {
        let key = ParsedCacheKey {
            final_source: resource.final_source.clone(),
            raw_sha256: format!("sha256:{:x}", Sha256::digest(&resource.bytes)),
        };

        if let Some(document) = self.cache.get(&key).await? {
            return Ok(document);
        }

        let document = self.inner.parse(resource).await?;
        self.cache.put(key, document.clone()).await?;
        Ok(document)
    }
}
