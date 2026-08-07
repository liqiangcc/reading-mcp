use std::sync::Arc;

use async_trait::async_trait;

use crate::application::ports::{ApplicationError, Parser, RetrievedResource};
use crate::domain::Document;

use super::{MarkdownParser, TextParser};

pub struct ParserRouter {
    markdown: Arc<dyn Parser>,
    text: Arc<dyn Parser>,
}

impl ParserRouter {
    pub fn new(markdown: Arc<dyn Parser>, text: Arc<dyn Parser>) -> Self {
        Self { markdown, text }
    }

    pub fn phase1() -> Self {
        Self::new(Arc::new(MarkdownParser), Arc::new(TextParser))
    }
}

#[async_trait]
impl Parser for ParserRouter {
    async fn parse(&self, resource: RetrievedResource) -> Result<Document, ApplicationError> {
        let media_type = resource
            .media_type
            .0
            .split(';')
            .next()
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase();

        match media_type.as_str() {
            "text/markdown" | "text/x-markdown" => self.markdown.parse(resource).await,
            "text/plain" => self.text.parse(resource).await,
            _ => Err(ApplicationError::ParseFailed(format!(
                "unsupported media type: {}",
                resource.media_type.0
            ))),
        }
    }
}
