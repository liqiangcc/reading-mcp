//! Real MCP transport/envelope tests. Deadline and output-cap errors originate
//! in the actual budget/application path; busy is injected at the Parser port
//! here and separately produced by OcrSingleFlight's admission tests.
//!
//! The v1 test called `open_document` on `ToolSet::V1`; the v2 `open` answers the
//! same failures as `fetch_failed` (timeout, busy) and `too_large` (the output
//! cap), keeps the retry hint, and never publishes or leaks the private path.
use super::*;
use crate::application::ports::{
    DocumentRepository, Parser, RetrievalOptions, RetrievedResource, Retriever, SourcePolicy,
};
use crate::domain::{Document, DocumentId, DocumentSource, MediaType};
use crate::infrastructure::{
    BudgetedParser, InMemorySearchIndex, InMemoryTextUnitIndex, ResourceBudget,
};
use async_trait::async_trait;
use rmcp::ServiceExt;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

struct Fixture;
#[async_trait]
impl SourcePolicy for Fixture {
    async fn validate(&self, _: &DocumentSource) -> Result<(), ApplicationError> {
        Ok(())
    }
}
#[async_trait]
impl Retriever for Fixture {
    async fn retrieve(
        &self,
        source: &DocumentSource,
        _: &RetrievalOptions,
    ) -> Result<RetrievedResource, ApplicationError> {
        Ok(RetrievedResource {
            source: source.clone(),
            final_source: source.clone(),
            media_type: MediaType("application/pdf".into()),
            bytes:
                b"# Public synthetic\n\nA complete paragraph longer than the configured output cap."
                    .to_vec(),
            etag: None,
            last_modified: None,
            metadata: Default::default(),
        })
    }
}
struct Operation(u8);
#[async_trait]
impl Parser for Operation {
    async fn parse(&self, resource: RetrievedResource) -> Result<Document, ApplicationError> {
        match self.0 {
            0 => tokio::time::sleep(std::time::Duration::from_secs(61)).await,
            2 => return Err(ApplicationError::OcrBusy),
            _ => {}
        }
        crate::parsing::MarkdownParser.parse(resource).await
    }
}
#[derive(Default)]
struct NoPublication(AtomicUsize);
#[async_trait]
impl DocumentRepository for NoPublication {
    async fn save(&self, _: Document) -> Result<(), ApplicationError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
    async fn get(&self, _: &DocumentId) -> Result<Option<Arc<Document>>, ApplicationError> {
        Ok(None)
    }
}

#[tokio::test(start_paused = true)]
async fn ocr_deadline_cap_and_busy_have_actionable_private_mcp_errors_without_publication() {
    // (case, v2 error code, whether the answer carries a retry)
    for (case, code, retryable) in [
        (0, "fetch_failed", true),
        (1, "too_large", false),
        (2, "fetch_failed", true),
    ] {
        let fixture = Arc::new(Fixture);
        let repository = Arc::new(NoPublication::default());
        let parser = BudgetedParser::new(
            Arc::new(Operation(case)),
            ResourceBudget {
                max_normalized_chars: if case == 1 { 8 } else { 1024 },
                ..ResourceBudget::default()
            },
        )
        .with_ocr_budget(true);
        let application = Arc::new(
            OpenDocumentUseCase::new(
                fixture.clone(),
                fixture,
                Arc::new(parser),
                repository.clone(),
                Arc::new(InMemorySearchIndex::default()),
            )
            .with_ocr_budget(true),
        );
        let mut server = crate::runtime::build_server(RuntimeConfig {
            state_dir: None,
            tools: crate::runtime::ToolSet::V2,
            ..RuntimeConfig::default()
        })
        .unwrap();
        server.open_reading = Arc::new(OpenReadingUseCase::new(
            application,
            repository.clone(),
            Arc::new(InMemoryTextUnitIndex::default()),
        ));
        let (server_transport, client_transport) = tokio::io::duplex(64 * 1024);
        let serving = tokio::spawn(async move { server.serve(server_transport).await.unwrap() });
        let client = ().serve(client_transport).await.unwrap();
        let server_handle = serving.await.unwrap();
        let result = client
            .call_tool(
                rmcp::model::CallToolRequestParams::new("open").with_arguments(
                    json!({"source":"file:///private-path-must-not-escape.pdf"})
                        .as_object()
                        .unwrap()
                        .clone(),
                ),
            )
            .await
            .expect("the v2 open answers a tool error, not a protocol error");
        assert_eq!(result.is_error, Some(true), "OCR failure must be an error");
        let encoded = result
            .content
            .iter()
            .find_map(|block| match block {
                rmcp::model::ContentBlock::Text(text) => Some(text.text.clone()),
                _ => None,
            })
            .expect("the error answers with one text block");
        assert!(encoded.contains(code), "{encoded}");
        let body: serde_json::Value =
            serde_json::from_str(&encoded).expect("the error body is JSON");
        assert_eq!(
            body["retry"].is_object(),
            retryable,
            "the retry hint must match the failure: {encoded}"
        );
        assert!(!encoded.contains("private-path-must-not-escape"));
        assert_eq!(repository.0.load(Ordering::SeqCst), 0);
        client.cancel().await.unwrap();
        server_handle.cancel().await.unwrap();
    }
}
