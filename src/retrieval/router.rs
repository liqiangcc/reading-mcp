use std::sync::Arc;

use async_trait::async_trait;

use crate::application::ports::{
    ApplicationError, RetrievalOptions, RetrievedResource, Retriever, SourcePolicy,
};
use crate::domain::DocumentSource;

pub struct SourcePolicyRouter {
    file: Arc<dyn SourcePolicy>,
    http: Arc<dyn SourcePolicy>,
}

impl SourcePolicyRouter {
    pub fn new(file: Arc<dyn SourcePolicy>, http: Arc<dyn SourcePolicy>) -> Self {
        Self { file, http }
    }
}

#[async_trait]
impl SourcePolicy for SourcePolicyRouter {
    async fn validate(&self, source: &DocumentSource) -> Result<(), ApplicationError> {
        if is_http_source(source) {
            self.http.validate(source).await
        } else {
            self.file.validate(source).await
        }
    }
}

pub struct RetrieverRouter {
    file: Arc<dyn Retriever>,
    http: Arc<dyn Retriever>,
}

impl RetrieverRouter {
    pub fn new(file: Arc<dyn Retriever>, http: Arc<dyn Retriever>) -> Self {
        Self { file, http }
    }
}

#[async_trait]
impl Retriever for RetrieverRouter {
    async fn retrieve(
        &self,
        source: &DocumentSource,
        options: &RetrievalOptions,
    ) -> Result<RetrievedResource, ApplicationError> {
        if is_http_source(source) {
            self.http.retrieve(source, options).await
        } else {
            self.file.retrieve(source, options).await
        }
    }
}

fn is_http_source(source: &DocumentSource) -> bool {
    let value = source.0.trim().to_ascii_lowercase();
    value.starts_with("https://") || value.starts_with("http://")
}
