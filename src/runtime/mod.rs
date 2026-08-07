mod config;

use std::sync::Arc;

use crate::application::get_context::GetContextUseCase;
use crate::application::get_document_structure::GetDocumentStructureUseCase;
use crate::application::open_document::OpenDocumentUseCase;
use crate::application::ports::{
    DocumentRepository, ParsedDocumentCache, Parser, RawResourceCache, Retriever, SearchIndex,
    SourcePolicy,
};
use crate::application::read_document::ReadDocumentUseCase;
use crate::application::search_document::SearchDocumentUseCase;
use crate::infrastructure::{
    BudgetedParser, BudgetedRetriever, CachingParser, FileParsedDocumentCache,
    FileRawResourceCache, InMemoryDocumentRepository, InMemoryParsedDocumentCache,
    InMemoryRawResourceCache, InMemorySearchIndex, ObservedParser, ObservedRetriever,
    ObservedSearchIndex, SqliteDocumentRepository, SqliteSearchIndex,
};
use crate::mcp::ReadingMcpServer;
use crate::parsing::ParserRouter;
use crate::retrieval::{
    EnvironmentCredentialProvider, HttpRetriever, LimitedFileRetriever, RetrieverRouter,
    RevalidatingHttpRetriever, SourcePolicyRouter,
};
use crate::security::{HttpAccessPolicy, PublicHttpAccessPolicy};

pub use config::RuntimeConfig;

pub fn build_server(
    config: RuntimeConfig,
) -> Result<ReadingMcpServer, crate::application::ports::ApplicationError> {
    let http_policy = Arc::new(if config.allow_http {
        PublicHttpAccessPolicy::allow_http()
    } else {
        PublicHttpAccessPolicy::https_only()
    });
    let source_http_policy: Arc<dyn SourcePolicy> = http_policy.clone();
    let retriever_http_policy: Arc<dyn HttpAccessPolicy> = http_policy;

    let source_policy: Arc<dyn SourcePolicy> = Arc::new(SourcePolicyRouter::new(
        Arc::new(crate::retrieval::LocalFileSourcePolicy::allow_roots(
            config.local_roots.clone(),
        )),
        source_http_policy,
    ));

    let (raw_cache, parsed_cache, repository, search_index): (
        Arc<dyn RawResourceCache>,
        Arc<dyn ParsedDocumentCache>,
        Arc<dyn DocumentRepository>,
        Arc<dyn SearchIndex>,
    ) = match &config.state_dir {
        Some(state_dir) => {
            std::fs::create_dir_all(state_dir).map_err(|error| {
                crate::application::ports::ApplicationError::RepositoryFailed(format!(
                    "{}: {error}",
                    state_dir.display()
                ))
            })?;
            let cache_root = state_dir.join("cache");
            let database = state_dir.join("reading-mcp.sqlite");
            (
                Arc::new(FileRawResourceCache::new(&cache_root)),
                Arc::new(FileParsedDocumentCache::new(&cache_root)),
                Arc::new(SqliteDocumentRepository::open(&database)?),
                Arc::new(SqliteSearchIndex::open(&database)?),
            )
        }
        None => (
            Arc::new(InMemoryRawResourceCache::default()),
            Arc::new(InMemoryParsedDocumentCache::default()),
            Arc::new(InMemoryDocumentRepository::default()),
            Arc::new(InMemorySearchIndex::default()),
        ),
    };

    let http = Arc::new(HttpRetriever::with_credentials(
        retriever_http_policy,
        config.http.clone(),
        Arc::new(EnvironmentCredentialProvider),
    ));
    let http: Arc<dyn Retriever> = Arc::new(RevalidatingHttpRetriever::new(http, raw_cache));
    let file: Arc<dyn Retriever> = Arc::new(LimitedFileRetriever::new(
        config.resource_budget.max_document_bytes,
    ));
    let retriever: Arc<dyn Retriever> = Arc::new(RetrieverRouter::new(file, http));
    let retriever: Arc<dyn Retriever> = Arc::new(BudgetedRetriever::new(
        retriever,
        config.resource_budget.max_document_bytes,
    ));
    let retriever: Arc<dyn Retriever> = if config.telemetry {
        Arc::new(ObservedRetriever::new(retriever))
    } else {
        retriever
    };

    let parser: Arc<dyn Parser> = Arc::new(CachingParser::new(
        Arc::new(ParserRouter::phase4_with_pdf_limit(
            config.resource_budget.max_pdf_pages,
        )),
        parsed_cache,
    ));
    let parser: Arc<dyn Parser> =
        Arc::new(BudgetedParser::new(parser, config.resource_budget.clone()));
    let parser: Arc<dyn Parser> = if config.telemetry {
        Arc::new(ObservedParser::new(parser))
    } else {
        parser
    };

    let search_index: Arc<dyn SearchIndex> = if config.telemetry {
        Arc::new(ObservedSearchIndex::new(search_index))
    } else {
        search_index
    };

    let open_document = Arc::new(OpenDocumentUseCase::new(
        source_policy,
        retriever,
        parser,
        repository.clone(),
        search_index.clone(),
    ));
    let get_structure = Arc::new(GetDocumentStructureUseCase::new(repository.clone()));
    let search_document = Arc::new(SearchDocumentUseCase::new(search_index));
    let read_document = Arc::new(ReadDocumentUseCase::new(repository.clone()));
    let get_context = Arc::new(GetContextUseCase::new(repository));

    Ok(ReadingMcpServer::from_use_cases(
        open_document,
        get_structure,
        search_document,
        read_document,
        get_context,
    ))
}
