use std::sync::Arc;

use async_trait::async_trait;

use crate::application::ports::{ApplicationError, Parser, RetrievedResource};
use crate::domain::Document;

use super::{
    ArchiveLimits, DocxParser, EpubParser, HtmlParser, LimitedPdfParser, MarkdownParser,
    OpenApiParser, PdfParser, TextParser,
};

pub struct ParserRouter {
    markdown: Arc<dyn Parser>,
    text: Arc<dyn Parser>,
    html: Option<Arc<dyn Parser>>,
    pdf: Option<Arc<dyn Parser>>,
    epub: Option<Arc<dyn Parser>>,
    docx: Option<Arc<dyn Parser>>,
    openapi: Option<Arc<dyn Parser>>,
}

impl ParserRouter {
    pub fn new(markdown: Arc<dyn Parser>, text: Arc<dyn Parser>) -> Self {
        Self {
            markdown,
            text,
            html: None,
            pdf: None,
            epub: None,
            docx: None,
            openapi: None,
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
            epub: None,
            docx: None,
            openapi: None,
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
            epub: None,
            docx: None,
            openapi: None,
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

    pub fn phase4_with_pdf_limit(max_pages: usize) -> Self {
        Self::with_html_pdf(
            Arc::new(MarkdownParser),
            Arc::new(TextParser),
            Arc::new(HtmlParser),
            Arc::new(LimitedPdfParser::new(max_pages)),
        )
    }

    pub fn release(max_pdf_pages: usize, archive_limits: ArchiveLimits) -> Self {
        Self {
            markdown: Arc::new(MarkdownParser),
            text: Arc::new(TextParser),
            html: Some(Arc::new(HtmlParser)),
            pdf: Some(Arc::new(LimitedPdfParser::new(max_pdf_pages))),
            epub: Some(Arc::new(EpubParser::new(archive_limits.clone()))),
            docx: Some(Arc::new(DocxParser::new(archive_limits))),
            openapi: Some(Arc::new(OpenApiParser)),
        }
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

        let parser = match media_type.as_str() {
            "text/markdown" | "text/x-markdown" => return self.markdown.parse(resource).await,
            "text/plain" => return self.text.parse(resource).await,
            "text/html" | "application/xhtml+xml" => self.html.as_ref(),
            "application/pdf" => self.pdf.as_ref(),
            "application/epub+zip" => self.epub.as_ref(),
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document" => {
                self.docx.as_ref()
            }
            "application/json"
            | "application/yaml"
            | "application/x-yaml"
            | "text/yaml"
            | "application/vnd.oai.openapi+json"
            | "application/vnd.oai.openapi+yaml" => self.openapi.as_ref(),
            _ => None,
        };

        parser
            .ok_or_else(|| {
                ApplicationError::ParseFailed(format!(
                    "unsupported media type: {}",
                    resource.media_type.0
                ))
            })?
            .parse(resource)
            .await
    }
}
