mod config;
mod ephemeral_state;

use std::sync::Arc;

pub(crate) use ephemeral_state::EphemeralStateDir;

use crate::application::get_context::GetContextUseCase;
use crate::application::get_document_structure::GetDocumentStructureUseCase;
use crate::application::get_text_units::GetTextUnitsUseCase;
use crate::application::list_directories::ListDirectoryUseCase;
use crate::application::list_documents::ListDocumentsUseCase;
use crate::application::open_document::OpenDocumentUseCase;
use crate::application::open_reading::OpenReadingUseCase;
use crate::application::ports::{
    DocumentRepository, OpenResultStore, ParsedDocumentCache, Parser, RawResourceCache, Retriever,
    SearchIndex, SourcePolicy, TextUnitIndex,
};
use crate::application::read_document::ReadDocumentUseCase;
use crate::application::read_reading::ReadReadingUseCase;
use crate::application::search_document::SearchDocumentUseCase;
use crate::application::source_view::SourceViewUseCase;
use crate::infrastructure::{
    BudgetedParser, BudgetedRetriever, CachingParser, FileParsedDocumentCache,
    FileRawResourceCache, ObservedDocumentRepository, ObservedParsedDocumentCache, ObservedParser,
    ObservedRawResourceCache, ObservedRetriever, ObservedSearchIndex, ObservedTextUnitIndex,
    SqliteDocumentRepository, SqliteOpenResultStore, SqliteSearchIndex, SqliteTextUnitIndex,
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

pub use config::{RuntimeConfig, ToolSet};

struct RuntimeComponents {
    raw_cache: Arc<dyn RawResourceCache>,
    parsed_cache: Arc<dyn ParsedDocumentCache>,
    repository: Arc<dyn DocumentRepository>,
    text_unit_index: Arc<dyn TextUnitIndex>,
    search_index: Arc<dyn SearchIndex>,
    open_results: Arc<dyn OpenResultStore>,
}

impl RuntimeComponents {
    fn observed(self) -> Self {
        Self {
            raw_cache: Arc::new(ObservedRawResourceCache::new(self.raw_cache)),
            parsed_cache: Arc::new(ObservedParsedDocumentCache::new(self.parsed_cache)),
            repository: Arc::new(ObservedDocumentRepository::new(self.repository)),
            text_unit_index: Arc::new(ObservedTextUnitIndex::new(self.text_unit_index)),
            search_index: Arc::new(ObservedSearchIndex::new(self.search_index)),
            open_results: self.open_results,
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

    let (components, ephemeral_state) = build_state_components(&config)?;
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
    // Everything that changes what the parser publishes for the same bytes; keys the stored open results.
    let ingest_fingerprint = format!(
        "{fingerprint}|pdf-layout={}",
        if config.pdf_layout_python.is_some() {
            crate::parsing::PDF_LAYOUT_CACHE_NAMESPACE
        } else {
            "off"
        }
    );
    let mut cached = CachingParser::new(Arc::new(router), components.parsed_cache)
        .with_ocr_fingerprint(fingerprint)
        .with_ocr_admission(config.ocr_enabled)
        .with_pdf_only(true);
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
    let open_results = components.open_results;
    let mut source_view_renderer =
        FileProcessIsolatedPdfSourceViewRenderer::current_executable(config.source_view.timeout)?;
    if let Some(python) = &config.pdf_layout_python {
        source_view_renderer = source_view_renderer.with_pymupdf(python.clone());
    }
    let source_view_renderer = Arc::new(source_view_renderer);
    let source_view = Arc::new(
        SourceViewUseCase::new(
            repository.clone(),
            retriever.clone(),
            source_view_renderer,
            config.source_view,
        )
        .with_text_unit_index(text_unit_index.clone()),
    );
    let open_document = Arc::new(
        OpenDocumentUseCase::with_text_unit_index(
            source_policy,
            retriever,
            parser,
            repository.clone(),
            text_unit_index.clone(),
            search_index.clone(),
        )
        .with_reliability_inspector(Arc::new(PersistedDocumentReliabilityInspector))
        .with_ocr_budget(config.ocr_enabled)
        .with_open_result_store(open_results, ingest_fingerprint),
    );
    let list_documents = Arc::new(ListDocumentsUseCase::new(config.local_roots.clone()));
    let list_directory = Arc::new(ListDirectoryUseCase::new(config.local_roots.clone()));
    let get_structure = Arc::new(GetDocumentStructureUseCase::new(repository.clone()));
    let get_text_units = Arc::new(GetTextUnitsUseCase::new(
        repository.clone(),
        text_unit_index.clone(),
    ));
    let search_document = Arc::new(
        SearchDocumentUseCase::new(search_index, repository.clone())
            .with_text_unit_index(text_unit_index.clone()),
    );
    let read_document = Arc::new(
        ReadDocumentUseCase::new(repository.clone()).with_text_unit_index(text_unit_index.clone()),
    );
    let open_reading = Arc::new(OpenReadingUseCase::new(
        open_document.clone(),
        repository.clone(),
        text_unit_index.clone(),
    ));
    let read_reading = Arc::new(ReadReadingUseCase::new(
        repository.clone(),
        text_unit_index.clone(),
    ));
    let get_context =
        Arc::new(GetContextUseCase::new(repository).with_text_unit_index(text_unit_index.clone()));

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
        open_reading,
        read_reading,
        config.tools,
        ephemeral_state,
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
) -> Result<
    (RuntimeComponents, Option<Arc<EphemeralStateDir>>),
    crate::application::ports::ApplicationError,
> {
    let (state_dir, ephemeral_state) = match &config.state_dir {
        Some(state_dir) => (state_dir.clone(), None),
        None => {
            let ephemeral = EphemeralStateDir::new().map_err(|error| {
                crate::application::ports::ApplicationError::RepositoryFailed(format!(
                    "failed to create temporary state directory: {error}"
                ))
            })?;
            (ephemeral.path().to_path_buf(), Some(Arc::new(ephemeral)))
        }
    };
    std::fs::create_dir_all(&state_dir).map_err(|error| {
        crate::application::ports::ApplicationError::RepositoryFailed(format!(
            "{}: {error}",
            state_dir.display()
        ))
    })?;
    let cache_root = state_dir.join("cache");
    // Non-PDF documents are no longer cached as parsed output, and the parsed
    // cache moved to a new directory. Drop the legacy directory once.
    remove_legacy_parsed_cache(&cache_root.join("parsed"));
    let database = state_dir.join("reading-mcp.sqlite");
    let repository = Arc::new(SqliteDocumentRepository::open(&database)?);
    let text_unit_index = Arc::new(SqliteTextUnitIndex::open(&database)?);
    Ok((
        RuntimeComponents {
            raw_cache: Arc::new(FileRawResourceCache::new(&cache_root)),
            parsed_cache: Arc::new(FileParsedDocumentCache::new(&cache_root)),
            search_index: Arc::new(SqliteSearchIndex::new(
                repository.clone(),
                text_unit_index.clone(),
            )),
            repository,
            text_unit_index,
            open_results: Arc::new(SqliteOpenResultStore::open(&database)?),
        },
        ephemeral_state,
    ))
}

fn remove_legacy_parsed_cache(directory: &std::path::Path) {
    match std::fs::remove_dir_all(directory) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => eprintln!(
            "warning: failed to remove legacy parsed cache {}: {error}",
            directory.display()
        ),
    }
}

#[cfg(test)]
mod startup_cache_tests {
    use super::*;

    fn state_config(state: &std::path::Path) -> RuntimeConfig {
        RuntimeConfig {
            state_dir: Some(state.to_path_buf()),
            ..RuntimeConfig::default()
        }
    }

    #[test]
    fn startup_removes_the_legacy_parsed_cache_and_keeps_the_other_caches() {
        let directory = tempfile::tempdir().unwrap();
        let state = directory.path().join("state");
        let cache = state.join("cache");
        std::fs::create_dir_all(cache.join("parsed")).unwrap();
        std::fs::write(cache.join("parsed").join("stale.json"), b"{}").unwrap();
        std::fs::create_dir_all(cache.join("raw")).unwrap();
        std::fs::write(cache.join("raw").join("kept.bin"), b"raw").unwrap();
        std::fs::create_dir_all(cache.join("parsed-v2")).unwrap();
        std::fs::write(cache.join("parsed-v2").join("kept.json"), b"{}").unwrap();

        build_state_components(&state_config(&state)).unwrap();

        assert!(!cache.join("parsed").exists());
        assert!(cache.join("raw").join("kept.bin").is_file());
        assert!(cache.join("parsed-v2").join("kept.json").is_file());
        assert!(state.join("reading-mcp.sqlite").is_file());
    }

    #[test]
    fn startup_tolerates_a_missing_legacy_parsed_cache() {
        let directory = tempfile::tempdir().unwrap();
        let state = directory.path().join("state");

        build_state_components(&state_config(&state)).unwrap();

        assert!(!state.join("cache").join("parsed").exists());
        assert!(state.join("reading-mcp.sqlite").is_file());
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
    fn build_server_without_state_dir_uses_ephemeral_sqlite() {
        let server = build_server(RuntimeConfig {
            state_dir: None,
            telemetry: false,
            ..RuntimeConfig::default()
        })
        .unwrap();
        let prefix = format!("reading-mcp-ephemeral-{}-", std::process::id());
        let database = std::fs::read_dir(std::env::temp_dir())
            .unwrap()
            .flatten()
            .filter(|entry| {
                entry
                    .file_name()
                    .to_str()
                    .is_some_and(|name| name.starts_with(&prefix))
            })
            .map(|entry| entry.path().join("reading-mcp.sqlite"))
            .find(|path| path.is_file());
        assert!(database.is_some());
        drop(server);
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
