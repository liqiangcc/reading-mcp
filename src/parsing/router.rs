use std::sync::Arc;

use async_trait::async_trait;

use crate::application::ports::{ApplicationError, Parser, RetrievedResource};
use crate::domain::Document;

use super::{HtmlParser, MarkdownParser, PdfParser, TextParser};

pub struct ParserRouter {
    markdown: Arc<dyn Parser>,
    text: Arc<dyn Parser>,
    html: Option<Arc<dyn Parser>>,
    pdf: Option<Arc<dyn Parser>>,
}

impl ParserRouter {
    pub fn new(markdown: Arc<dyn Parser>, text: Arc<dyn Parser>) -> Self {
        Self {
            markdown,
            text,
            html: None,
            pdf: None,
        }
    }

    pub fn with_html(
        markdown: Arc<dyn Parser>,
        text: Arc<dyn Parser>,
        html: Arc<dyn Parser>,
    ) -> Self {
        Self {
            markdown,
            text,
            html: Some(html),
            pdf: None,
        }
    }

    pub fn with_html_pdf(
        markdown: Arc<dyn Parser>,
        text: Arc<dyn Parser>,
        html: Arc<dyn Parser>,
        pdf: Arc<dyn Parser>,
    ) -> Self {
        Self {
            markdown,
            text,
            html: Some(html),
            pdf: Some(pdf),
        }
    }

    pub fn phase1() -> Self {
        Self::new(Arc::new(MarkdownParser), Arc::new(TextParser))
    }

    pub fn phase3() -> Self {
        Self::with_html(
            Arc::new(MarkdownParser),
            Arc::new(TextParser),
            Arc::new(HtmlParser),
        )
    }

    pub fn phase4() -> Self {
        Self::with_html_pdf(
            Arc::new(MarkdownParser),
            Arc::new(TextParser),
            Arc::new(HtmlParser),
            Arc::new(PdfParser),
        )
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
            "text/html" | "application/xhtml+xml" => {
                self.html
                    .as_ref()
                    .ok_or_else(|| {
                        ApplicationError::ParseFailed(format!(
                            "unsupported media type: {}",
                            resource.media_type.0
                        ))
                    })?
                    .parse(resource)
                    .await
            }
            "application/pdf" => {
                self.pdf
                    .as_ref()
                    .ok_or_else(|| {
                        ApplicationError::ParseFailed(format!(
                            "unsupported media type: {}",
                            resource.media_type.0
                        ))
                    })?
                    .parse(resource)
                    .await
            }
            _ => Err(ApplicationError::ParseFailed(format!(
                "unsupported media type: {}",
                resource.media_type.0
            ))),
        }
    }
}
