use async_trait::async_trait;
use lopdf::Document as LopdfDocument;

use crate::application::ports::{ApplicationError, Parser, RetrievedResource};
use crate::domain::Document;

use super::PdfParser;

pub struct LimitedPdfParser {
    max_pages: usize,
}

impl LimitedPdfParser {
    pub fn new(max_pages: usize) -> Self {
        Self { max_pages }
    }
}

#[async_trait]
impl Parser for LimitedPdfParser {
    async fn parse(&self, resource: RetrievedResource) -> Result<Document, ApplicationError> {
        let pdf = LopdfDocument::load_mem(&resource.bytes).map_err(|error| {
            ApplicationError::ParseFailed(format!("invalid PDF document: {error}"))
        })?;
        let page_count = pdf.get_pages().len();
        if page_count > self.max_pages {
            return Err(ApplicationError::ResourceLimitExceeded(format!(
                "PDF has {page_count} pages; limit is {} pages",
                self.max_pages
            )));
        }
        PdfParser.parse(resource).await
    }
}
