mod config;

use std::sync::Arc;

use crate::application::get_context::GetContextUseCase;
use crate::application::get_document_structure::GetDocumentStructureUseCase;
use crate::application::get_text_units::GetTextUnitsUseCase;
use crate::application::list_directories::ListDirectoryUseCase;
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
    ArchiveLimits, FileProcessIsolatedPdfSourceViewRenderer, ParserRouter,
    PersistedDocumentReliabilityInspector,
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
    // Validate/discover once, before opening persistent state or querying caches.
    let identity = build_ocr_identity(&config)?;
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
    let mut router = ParserRouter::release(config.resource_budget.max_pdf_pages, archive_limits);
    if let Some(python) = &config.pdf_layout_python {
        let parser_python = identity
            .as_ref()
            .and_then(|i| i.runtime_package.as_ref())
            .map(|p| std::path::PathBuf::from(&p.python_path))
            .unwrap_or_else(|| python.clone());
        let parser =
            crate::parsing::LayoutPdfParser::new(parser_python, config.resource_budget.clone());
        let parser = if let Some(identity) = &identity {
            parser
                .with_ocr_config(config.ocr_config())
                .with_ocr_identity(identity.clone())
                .with_systemd_ocr_sandbox()
        } else {
            parser
        };
        let parser = if config.ocr_enabled {
            if let (Some(root), Some(manifest)) =
                (&config.ocr_runtime_root, &config.ocr_runtime_manifest)
            {
                parser.with_ocr_runtime_package(root.clone(), manifest.clone())
            } else {
                parser
            }
        } else {
            parser
        };
        let parser = if let Some(state) = &config.state_dir {
            let store = Arc::new(crate::infrastructure::FileOcrEvidenceStore::new(
                state.join("ocr-evidence"),
            ));
            let parser = parser.with_evidence_store(store);
            // Durable per-page progress survives the hard 60s open deadline;
            // the soft bound lets the worker stop at a bounded unit and report
            // a typed retryable OCR_TIMEOUT instead of being killed mid-page.
            let checkpoints = Arc::new(crate::infrastructure::FileOcrCheckpointStore::new(
                state.join("ocr-checkpoints"),
            ));
            parser
                .with_checkpoint_store(checkpoints)
                .with_ocr_invocation_budget(
                    crate::infrastructure::OCR_PARSE_TIMEOUT - std::time::Duration::from_secs(10),
                )
        } else {
            parser
        };
        router = router.with_pdf_parser(Arc::new(parser));
    }
    let fingerprint = identity
        .as_ref()
        .map(|i| i.sha256.clone())
        .unwrap_or_else(|| "ocr-disabled/v1".into());
    let mut cached = CachingParser::new(Arc::new(router), components.parsed_cache)
        .with_ocr_fingerprint(fingerprint)
        .with_ocr_admission(config.ocr_enabled);
    if config.pdf_layout_python.is_some() {
        cached = cached.with_pdf_namespace(crate::parsing::PDF_LAYOUT_CACHE_NAMESPACE);
    }
    let parser: Arc<dyn Parser> = Arc::new(cached);
    let parser: Arc<dyn Parser> = Arc::new(
        BudgetedParser::new(parser, config.resource_budget.clone())
            .with_ocr_budget(config.ocr_enabled),
    );
    let parser: Arc<dyn Parser> = if config.telemetry {
        Arc::new(ObservedParser::new(parser))
    } else {
        parser
    };

    let repository = components.repository;
    let text_unit_index = components.text_unit_index;
    let search_index = components.search_index;
    let mut source_view_renderer =
        FileProcessIsolatedPdfSourceViewRenderer::current_executable(config.source_view.timeout)?;
    if let Some(python) = &config.pdf_layout_python {
        source_view_renderer = source_view_renderer.with_pymupdf(python.clone());
    }
    let source_view_renderer = Arc::new(source_view_renderer);
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
        .with_reliability_inspector(Arc::new(PersistedDocumentReliabilityInspector))
        .with_ocr_budget(config.ocr_enabled),
    );
    let list_documents = Arc::new(ListDocumentsUseCase::new(config.local_roots.clone()));
    let list_directory = Arc::new(ListDirectoryUseCase::new(config.local_roots.clone()));
    let get_structure = Arc::new(GetDocumentStructureUseCase::new(repository.clone()));
    let get_text_units = Arc::new(GetTextUnitsUseCase::new(repository.clone()));
    let search_document = Arc::new(SearchDocumentUseCase::new(search_index, repository.clone()));
    let read_document = Arc::new(ReadDocumentUseCase::new(repository.clone()));
    let get_context = Arc::new(GetContextUseCase::new(repository));

    Ok(ReadingMcpServer::from_use_cases(
        open_document,
        list_documents,
        list_directory,
        get_structure,
        get_text_units,
        search_document,
        read_document,
        get_context,
        source_view,
    ))
}

fn build_ocr_identity(
    config: &RuntimeConfig,
) -> Result<Option<crate::domain::OcrRuntimeIdentity>, crate::application::ports::ApplicationError>
{
    use crate::application::ports::ApplicationError;
    if !config.ocr_enabled {
        return Ok(None);
    }
    let python = config
        .pdf_layout_python
        .as_ref()
        .ok_or_else(|| ApplicationError::ParseFailed("OCR requires PDF layout backend".into()))?;
    let ocr_config = config.ocr_config();
    ocr_config
        .validate()
        .map_err(ApplicationError::ParseFailed)?;
    match (&config.ocr_runtime_root, &config.ocr_runtime_manifest) {
        (Some(root), Some(manifest)) => {
            crate::parsing::inspect_private_ocr_runtime(ocr_config, python, root, manifest)
                .map(Some)
        }
        (None, None) => {
            crate::parsing::require_systemd_ocr_support()?;
            crate::infrastructure::build_ocr_runtime_identity(ocr_config)
                .map(Some)
                .map_err(ApplicationError::ParseFailed)
        }
        _ => Err(ApplicationError::ParseFailed(
            "OCR runtime root and manifest must be configured together".into(),
        )),
    }
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

#[cfg(test)]
mod ocr_startup_tests {
    use super::*;

    #[test]
    fn disabled_ocr_does_not_inspect_missing_private_package() {
        let config = RuntimeConfig {
            ocr_runtime_root: Some("/missing-test-ocr-root".into()),
            ocr_runtime_manifest: Some("/missing-test-ocr-manifest".into()),
            pdf_layout_python: Some("/missing-test-layout-python".into()),
            ..RuntimeConfig::default()
        };
        assert!(build_ocr_identity(&config).unwrap().is_none());
    }

    #[test]
    fn invalid_ocr_configuration_cannot_create_persistent_state() {
        let directory = tempfile::tempdir().unwrap();
        let state = directory.path().join("must-not-be-created");
        let config = RuntimeConfig {
            ocr_enabled: true,
            ocr_language: "../invalid".into(),
            pdf_layout_python: Some("/missing-test-layout-python".into()),
            state_dir: Some(state.clone()),
            ..RuntimeConfig::default()
        };
        assert!(
            matches!(build_server(config), Err(crate::application::ports::ApplicationError::ParseFailed(message)) if message == "invalid OCR languages")
        );
        assert!(!state.exists());
    }
}
