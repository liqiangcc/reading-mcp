use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tokio::time::timeout;

use crate::application::ports::{
    ApplicationError, Parser, RetrievalOptions, RetrievedResource, Retriever,
};
use crate::domain::{Document, DocumentSource, Section};

#[derive(Clone, Debug)]
pub struct ResourceBudget {
    pub max_document_bytes: usize,
    pub max_pdf_pages: usize,
    pub max_archive_entries: usize,
    pub max_archive_entry_bytes: usize,
    pub max_archive_total_bytes: usize,
    pub max_sections: usize,
    pub max_section_depth: usize,
    pub max_normalized_chars: usize,
    pub parse_timeout: Duration,
}

impl Default for ResourceBudget {
    fn default() -> Self {
        Self {
            max_document_bytes: 32 * 1024 * 1024,
            max_pdf_pages: 2_000,
            max_archive_entries: 10_000,
            max_archive_entry_bytes: 16 * 1024 * 1024,
            max_archive_total_bytes: 64 * 1024 * 1024,
            max_sections: 20_000,
            max_section_depth: 32,
            max_normalized_chars: 16_000_000,
            parse_timeout: Duration::from_secs(30),
        }
    }
}

pub struct BudgetedRetriever {
    inner: Arc<dyn Retriever>,
    max_document_bytes: usize,
}

impl BudgetedRetriever {
    pub fn new(inner: Arc<dyn Retriever>, max_document_bytes: usize) -> Self {
        Self {
            inner,
            max_document_bytes,
        }
    }
}

#[async_trait]
impl Retriever for BudgetedRetriever {
    async fn retrieve(
        &self,
        source: &DocumentSource,
        options: &RetrievalOptions,
    ) -> Result<RetrievedResource, ApplicationError> {
        let resource = self.inner.retrieve(source, options).await?;
        if resource.bytes.len() > self.max_document_bytes {
            return Err(ApplicationError::ResourceLimitExceeded(format!(
                "document is {} bytes; limit is {} bytes",
                resource.bytes.len(),
                self.max_document_bytes
            )));
        }
        Ok(resource)
    }
}

/// Moves parser work onto Tokio's blocking pool so synchronous PDF/ZIP/XML
/// parsing cannot monopolize an async runtime worker. Resource budgets remain
/// the primary protection because an already-running blocking task cannot be
/// forcefully killed by Tokio when an outer cooperative timeout expires.
pub struct BlockingParser {
    inner: Arc<dyn Parser>,
}

impl BlockingParser {
    pub fn new(inner: Arc<dyn Parser>) -> Self {
        Self { inner }
    }
}

#[async_trait]
impl Parser for BlockingParser {
    async fn parse(&self, resource: RetrievedResource) -> Result<Document, ApplicationError> {
        let inner = self.inner.clone();
        let runtime = tokio::runtime::Handle::current();
        tokio::task::spawn_blocking(move || runtime.block_on(inner.parse(resource)))
            .await
            .map_err(|error| ApplicationError::ParseFailed(format!("parser worker failed: {error}")))?
    }
}

pub struct BudgetedParser {
    inner: Arc<dyn Parser>,
    budget: ResourceBudget,
}

impl BudgetedParser {
    pub fn new(inner: Arc<dyn Parser>, budget: ResourceBudget) -> Self {
        Self { inner, budget }
    }
}

#[async_trait]
impl Parser for BudgetedParser {
    async fn parse(&self, resource: RetrievedResource) -> Result<Document, ApplicationError> {
        let document = timeout(self.budget.parse_timeout, self.inner.parse(resource))
            .await
            .map_err(|_| {
                ApplicationError::ResourceLimitExceeded(format!(
                    "parser exceeded {:?} timeout",
                    self.budget.parse_timeout
                ))
            })??;

        validate_document_budget(&document, &self.budget)?;
        Ok(document)
    }
}

fn validate_document_budget(
    document: &Document,
    budget: &ResourceBudget,
) -> Result<(), ApplicationError> {
    let mut section_count = 0usize;
    let mut char_count = 0usize;
    let mut max_depth = 0usize;

    for section in &document.root_sections {
        accumulate(
            section,
            1,
            &mut section_count,
            &mut char_count,
            &mut max_depth,
        );
    }

    if section_count > budget.max_sections {
        return Err(ApplicationError::ResourceLimitExceeded(format!(
            "normalized document has {section_count} sections; limit is {}",
            budget.max_sections
        )));
    }
    if max_depth > budget.max_section_depth {
        return Err(ApplicationError::ResourceLimitExceeded(format!(
            "normalized document depth is {max_depth}; limit is {}",
            budget.max_section_depth
        )));
    }
    if char_count > budget.max_normalized_chars {
        return Err(ApplicationError::ResourceLimitExceeded(format!(
            "normalized document has {char_count} characters; limit is {}",
            budget.max_normalized_chars
        )));
    }

    Ok(())
}

fn accumulate(
    section: &Section,
    depth: usize,
    section_count: &mut usize,
    char_count: &mut usize,
    max_depth: &mut usize,
) {
    *section_count += 1;
    *char_count = char_count.saturating_add(section.content.chars().count());
    *max_depth = (*max_depth).max(depth);
    for child in &section.children {
        accumulate(child, depth + 1, section_count, char_count, max_depth);
    }
}
