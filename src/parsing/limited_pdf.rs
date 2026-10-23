use async_trait::async_trait;

use crate::application::ports::{ApplicationError, Parser, RetrievedResource};
use crate::domain::Document;

use super::PdfParser;
use super::blocking::run_blocking;

#[derive(Clone)]
pub struct LimitedPdfParser {
    max_pages: usize,
}

impl LimitedPdfParser {
    pub fn new(max_pages: usize) -> Self {
        Self { max_pages }
    }

    fn parse_sync(&self, mut resource: RetrievedResource) -> Result<Document, ApplicationError> {
        let (pdf, hash) = super::pdf::load_and_release(&mut resource)?;
        let page_count = pdf.get_pages().len();
        if page_count > self.max_pages {
            return Err(ApplicationError::ResourceLimitExceeded(format!(
                "PDF has {page_count} pages; limit is {} pages",
                self.max_pages
            )));
        }
        // The tree is already loaded: hand it to the parser instead of loading the same bytes a
        // second time, which used to keep two full object trees alive at the parse's peak.
        PdfParser.parse_loaded(pdf, resource, hash)
    }
}

#[async_trait]
impl Parser for LimitedPdfParser {
    async fn parse(&self, resource: RetrievedResource) -> Result<Document, ApplicationError> {
        let parser = self.clone();
        run_blocking(move || parser.parse_sync(resource)).await
    }
}
