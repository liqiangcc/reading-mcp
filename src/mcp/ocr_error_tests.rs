//! Real MCP transport/envelope tests. Deadline and output-cap errors originate
//! in the actual budget/application path; busy is injected at the Parser port
//! here and separately produced by OcrSingleFlight's admission tests.
use super::*;
use crate::application::ports::{
    DocumentRepository, Parser, RetrievalOptions, RetrievedResource, Retriever, SourcePolicy,
};
use crate::domain::{Document, DocumentId, DocumentSource, MediaType};
use crate::infrastructure::{BudgetedParser, InMemorySearchIndex, ResourceBudget};
use async_trait::async_trait;
use rmcp::ServiceExt;
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
    async fn get(&self, _: &DocumentId) -> Result<Option<Document>, ApplicationError> {
        Ok(None)
    }
}

#[tokio::test(start_paused = true)]
async fn ocr_deadline_cap_and_busy_have_actionable_private_mcp_errors_without_publication() {
    for (case, code, retryable) in [
        (0, "OCR_TIMEOUT", true),
        (1, "OCR_RESOURCE_LIMIT", false),
        (2, "OCR_RESOURCE_LIMIT", true),
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
        let application = OpenDocumentUseCase::new(
            fixture.clone(),
            fixture,
            Arc::new(parser),
            repository.clone(),
            Arc::new(InMemorySearchIndex::default()),
        )
        .with_ocr_budget(true);
        let mut server = crate::runtime::build_server(RuntimeConfig {
            state_dir: None,
            ..RuntimeConfig::default()
        })
        .unwrap();
        server.open_document = Arc::new(application);
        let (server_transport, client_transport) = tokio::io::duplex(64 * 1024);
        let serving = tokio::spawn(async move { server.serve(server_transport).await.unwrap() });
        let client = ().serve(client_transport).await.unwrap();
        let server_handle = serving.await.unwrap();
        let error = client
            .call_tool(
                rmcp::model::CallToolRequestParams::new("open_document").with_arguments(
                    json!({"source":"file:///private-path-must-not-escape.pdf"})
                        .as_object()
                        .unwrap()
                        .clone(),
                ),
            )
            .await
            .expect_err("OCR failure must not publish an open result");
        let encoded = error.to_string();
        assert!(encoded.contains(code), "{encoded}");
        assert!(
            encoded.contains(&format!("\"retryable\":{retryable}")),
            "{encoded}"
        );
        assert!(!encoded.contains("private-path-must-not-escape"));
        assert_eq!(repository.0.load(Ordering::SeqCst), 0);
        client.cancel().await.unwrap();
        server_handle.cancel().await.unwrap();
    }
}
