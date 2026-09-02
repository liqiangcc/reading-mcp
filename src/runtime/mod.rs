mod config;

use std::sync::Arc;

use crate::application::get_context::GetContextUseCase;
use crate::application::get_document_structure::GetDocumentStructureUseCase;
use crate::application::get_text_units::GetTextUnitsUseCase;
use crate::application::list_documents::ListDocumentsUseCase;
use crate::application::open_document::OpenDocumentUseCase;
use crate::application::ports::{
    DocumentRepository, ParsedDocumentCache, Parser, RawResourceCache, Retriever, SearchIndex,
    SourcePolicy, TextUnitIndex,
};
use crate::application::read_document::ReadDocumentUseCase;
use crate::application::search_document::SearchDocumentUseCase;
use crate::application::source_view::SourceViewUseCase;
use crate::infrastructure::{
    BudgetedParser, BudgetedRetriever, CachingParser, FileParsedDocumentCache,
    FileRawResourceCache, InMemoryDocumentRepository, InMemoryParsedDocumentCache,
    InMemoryRawResourceCache, InMemorySearchIndex, InMemoryTextUnitIndex,
    ObservedParsedDocumentCache, ObservedParser, ObservedRawResourceCache, ObservedRetriever,
    ObservedSearchIndex, SqliteDocumentRepository, SqliteSearchIndex, SqliteTextUnitIndex,
};
use crate::mcp::ReadingMcpServer;
use crate::parsing::{
    ArchiveLimits, ParserRouter, PersistedDocumentReliabilityInspector,
    ProcessIsolatedPdfSourceViewRenderer,
};
use crate::retrieval::{
    EnvironmentCredentialProvider, HttpRetriever, LimitedFileRetriever, RetrieverRouter,
    RevalidatingHttpRetriever, SourcePolicyRouter,
};
use crate::security::{HttpAccessPolicy, PublicHttpAccessPolicy};

pub use config::RuntimeConfig;

struct RuntimeComponents {
    raw_cache: Arc<dyn RawResourceCache>,
    parsed_cache: Arc<dyn ParsedDocumentCache>,
    repository: Arc<dyn DocumentRepository>,
    text_unit_index: Arc<dyn TextUnitIndex>,
    search_index: Arc<dyn SearchIndex>,
}

impl RuntimeComponents {
    fn observed(self) -> Self {
        Self {
            raw_cache: Arc::new(ObservedRawResourceCache::new(self.raw_cache)),
            parsed_cache: Arc::new(ObservedParsedDocumentCache::new(self.parsed_cache)),
            repository: self.repository,
            text_unit_index: self.text_unit_index,
            search_index: Arc::new(ObservedSearchIndex::new(self.search_index)),
        }
    }
}

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

    let components = build_state_components(&config)?;
    let components = if config.telemetry {
        components.observed()
    } else {
        components
    };

    let http = Arc::new(HttpRetriever::with_credentials(
        retriever_http_policy,
        config.http.clone(),
        Arc::new(EnvironmentCredentialProvider),
    ));
    let http: Arc<dyn Retriever> =
        Arc::new(RevalidatingHttpRetriever::new(http, components.raw_cache));
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

    let archive_limits = ArchiveLimits {
        max_entries: config.resource_budget.max_archive_entries,
        max_entry_bytes: config.resource_budget.max_archive_entry_bytes,
        max_total_bytes: config.resource_budget.max_archive_total_bytes,
    };
    let parser: Arc<dyn Parser> = Arc::new(CachingParser::new(
        Arc::new(ParserRouter::release(
            config.resource_budget.max_pdf_pages,
            archive_limits,
        )),
        components.parsed_cache,
    ));
    let parser: Arc<dyn Parser> =
        Arc::new(BudgetedParser::new(parser, config.resource_budget.clone()));
    let parser: Arc<dyn Parser> = if config.telemetry {
        Arc::new(ObservedParser::new(parser))
    } else {
        parser
    };

    let repository = components.repository;
    let text_unit_index = components.text_unit_index;
    let search_index = components.search_index;
    let source_view_renderer = Arc::new(
        ProcessIsolatedPdfSourceViewRenderer::current_executable(config.source_view.timeout)?,
    );
    let source_view = Arc::new(SourceViewUseCase::new(
        repository.clone(),
        retriever.clone(),
        source_view_renderer,
        config.source_view,
    ));
    let open_document = Arc::new(
        OpenDocumentUseCase::with_text_unit_index(
            source_policy,
            retriever,
            parser,
            repository.clone(),
            text_unit_index,
            search_index.clone(),
        )
        .with_reliability_inspector(Arc::new(PersistedDocumentReliabilityInspector)),
    );
    let list_documents = Arc::new(ListDocumentsUseCase::new(config.local_roots.clone()));
    let get_structure = Arc::new(GetDocumentStructureUseCase::new(repository.clone()));
    let get_text_units = Arc::new(GetTextUnitsUseCase::new(repository.clone()));
    let search_document = Arc::new(SearchDocumentUseCase::new(search_index, repository.clone()));
    let read_document = Arc::new(ReadDocumentUseCase::new(repository.clone()));
    let get_context = Arc::new(GetContextUseCase::new(repository));

    Ok(ReadingMcpServer::from_use_cases(
        open_document,
        list_documents,
        get_structure,
        get_text_units,
        search_document,
        read_document,
        get_context,
        source_view,
    ))
}

fn build_state_components(
    config: &RuntimeConfig,
) -> Result<RuntimeComponents, crate::application::ports::ApplicationError> {
    match &config.state_dir {
        Some(state_dir) => {
            std::fs::create_dir_all(state_dir).map_err(|error| {
                crate::application::ports::ApplicationError::RepositoryFailed(format!(
                    "{}: {error}",
                    state_dir.display()
                ))
            })?;
            let cache_root = state_dir.join("cache");
            let database = state_dir.join("reading-mcp.sqlite");
            Ok(RuntimeComponents {
                raw_cache: Arc::new(FileRawResourceCache::new(&cache_root)),
                parsed_cache: Arc::new(FileParsedDocumentCache::new(&cache_root)),
                repository: Arc::new(SqliteDocumentRepository::open(&database)?),
                text_unit_index: Arc::new(SqliteTextUnitIndex::open(&database)?),
                search_index: Arc::new(SqliteSearchIndex::open(&database)?),
            })
        }
        None => Ok(RuntimeComponents {
            raw_cache: Arc::new(InMemoryRawResourceCache::default()),
            parsed_cache: Arc::new(InMemoryParsedDocumentCache::default()),
            repository: Arc::new(InMemoryDocumentRepository::default()),
            text_unit_index: Arc::new(InMemoryTextUnitIndex::default()),
            search_index: Arc::new(InMemorySearchIndex::default()),
        }),
    }
}
