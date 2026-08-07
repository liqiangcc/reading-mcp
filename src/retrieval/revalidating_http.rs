use std::sync::Arc;

use async_trait::async_trait;

use crate::application::ports::{
    ApplicationError, RawResourceCache, RetrievalOptions, RetrievedResource, Retriever,
};
use crate::domain::DocumentSource;

use super::{HttpRetrievalOutcome, HttpRetriever, HttpValidators};

pub struct RevalidatingHttpRetriever {
    inner: Arc<HttpRetriever>,
    cache: Arc<dyn RawResourceCache>,
}

impl RevalidatingHttpRetriever {
    pub fn new(inner: Arc<HttpRetriever>, cache: Arc<dyn RawResourceCache>) -> Self {
        Self { inner, cache }
    }
}

#[async_trait]
impl Retriever for RevalidatingHttpRetriever {
    async fn retrieve(
        &self,
        source: &DocumentSource,
        options: &RetrievalOptions,
    ) -> Result<RetrievedResource, ApplicationError> {
        let cache_key = scoped_cache_key(source, options.auth_profile.as_deref());

        if options.force_refresh {
            let resource = self.inner.retrieve(source, options).await?;
            self.cache.put(&cache_key, resource.clone()).await?;
            return Ok(resource);
        }

        let Some(cached) = self.cache.get(&cache_key).await? else {
            let resource = self.inner.retrieve(source, options).await?;
            self.cache.put(&cache_key, resource.clone()).await?;
            return Ok(resource);
        };

        let validators = HttpValidators {
            etag: cached.etag.clone(),
            last_modified: cached.last_modified.clone(),
        };
        if validators.etag.is_none() && validators.last_modified.is_none() {
            return Ok(cached);
        }

        match self
            .inner
            .retrieve_conditional(&cached.final_source, options, &validators)
            .await?
        {
            HttpRetrievalOutcome::NotModified => Ok(cached),
            HttpRetrievalOutcome::Resource(mut resource) => {
                resource.source = source.clone();
                self.cache.put(&cache_key, resource.clone()).await?;
                Ok(resource)
            }
        }
    }
}

fn scoped_cache_key(source: &DocumentSource, auth_profile: Option<&str>) -> DocumentSource {
    match auth_profile {
        Some(profile) => DocumentSource(format!("auth-profile:{profile}\0{}", source.0)),
        None => source.clone(),
    }
}
