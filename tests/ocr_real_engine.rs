use reading_mcp::application::ports::OcrEvidenceStore;
use reading_mcp::application::ports::{Parser, RetrievedResource};
use reading_mcp::domain::{DocumentSource, MediaType, OcrConfig};
use reading_mcp::infrastructure::{FileOcrEvidenceStore, build_ocr_runtime_identity};
use reading_mcp::parsing::LayoutPdfParser;
use sha2::Digest;
use std::sync::Arc;
use tempfile::tempdir;

fn chinese_config() -> OcrConfig {
    OcrConfig {
        enabled: true,
        engine_path: "/usr/bin/tesseract".into(),
        tessdata_path: "/usr/share/tesseract-ocr/5/tessdata".into(),
        languages: vec!["chi_sim".into()],
        operator_revision: "1".into(),
        dpi: 300,
        oem: 1,
        psm: 3,
        detector_version: "pdf-layout/v1".into(),
        protocol_version: "pdf-layout/v1".into(),
    }
}

#[tokio::test]
#[ignore = "requires pinned hosted OCR dependencies"]
async fn supplementary_mixed_native_anchors_preserve_actual_canonical_order() {
    use reading_mcp::application::ports::DocumentRepository;
    use reading_mcp::infrastructure::SqliteDocumentRepository;
    let inputs = std::path::PathBuf::from(std::env::var("READING_MCP_MIXED_ORDER_DIR").unwrap());
    let directory = tempdir().unwrap();
    let store = Arc::new(FileOcrEvidenceStore::new(directory.path().join("evidence")));
    let mut config = chinese_config();
    config.languages = vec!["eng".into()];
    let identity = build_ocr_runtime_identity(config.clone()).unwrap();
    let parser = LayoutPdfParser::new(
        std::env::var("READING_MCP_PDF_LAYOUT_PYTHON")
            .unwrap()
            .into(),
        reading_mcp::infrastructure::ResourceBudget::default(),
    )
    .with_ocr_config(config)
    .with_ocr_identity(identity)
    .with_evidence_store(store.clone());
    for case in [
        "scan_then_native",
        "native_then_scan",
        "alternating_vertical",
    ] {
        let source = DocumentSource(format!("file:///supplementary/{case}.pdf"));
        let raw = std::fs::read(inputs.join(format!("{case}.pdf"))).unwrap();
        let raw_hash = format!("sha256:{:x}", sha2::Sha256::digest(&raw));
        let document = parser
            .parse(RetrievedResource {
                source: source.clone(),
                final_source: source,
                media_type: MediaType("application/pdf".into()),
                bytes: raw,
                etag: None,
                last_modified: None,
                metadata: Default::default(),
            })
            .await
            .unwrap();
        // Authored truth is read only after actual parsing, never fed into OCR.
        let expected: serde_json::Value = serde_json::from_slice(
            &std::fs::read(inputs.join(format!("{case}.expected.json"))).unwrap(),
        )
        .unwrap();
        let actual: Vec<_> = document
            .try_paragraph_text_units()
            .unwrap()
            .units
            .into_iter()
            .map(|u| u.text)
            .collect();
        assert_eq!(serde_json::json!(actual), expected["paragraphs"], "{case}");
        assert_eq!(document.content_hash.0, raw_hash);
        let map = document.original_source_binding_map().unwrap().unwrap();
        assert!(!map.bindings.is_empty());
        assert!(map.bindings.iter().all(|binding| matches!(
            binding.target,
            reading_mcp::domain::OriginalSourceTarget::Page { page_number: 1 }
        )));
        let bytes = store
            .get(&document.metadata["ocr_evidence_blob"])
            .await
            .unwrap()
            .unwrap();
        let blob: reading_mcp::domain::OcrEvidenceBlob = serde_json::from_slice(&bytes).unwrap();
        blob.validate(1).unwrap();
        assert_eq!(blob.pages[0].mixed_order.as_ref().unwrap().entries.len(), 4);
        let mut wrong = blob.clone();
        wrong.pages[0]
            .mixed_order
            .as_mut()
            .unwrap()
            .entries
            .swap(0, 1);
        assert!(wrong.validate(1).is_err(), "reordered evidence must fail");
        wrong.pages[0].mixed_order = None;
        assert!(wrong.validate(1).is_err(), "missing evidence must fail");
        let database = directory.path().join(format!("{case}.sqlite"));
        {
            let repository = SqliteDocumentRepository::open(&database).unwrap();
            repository.save(document.clone()).await.unwrap();
        }
        let restored = SqliteDocumentRepository::open(&database)
            .unwrap()
            .get(&document.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            restored.try_paragraph_text_units().unwrap(),
            document.try_paragraph_text_units().unwrap()
        );
        assert_eq!(
            restored.normalized_document_hash(),
            document.normalized_document_hash()
        );
    }
}

#[tokio::test]
#[ignore = "requires pinned hosted OCR dependencies"]
async fn existing_layers_have_persistent_native_coverage_without_engine_attempts() {
    let directory = tempdir().unwrap();
    let store = Arc::new(FileOcrEvidenceStore::new(directory.path().join("evidence")));
    let mut config = chinese_config();
    config.languages = vec!["eng".into()];
    let identity = build_ocr_runtime_identity(config.clone()).unwrap();
    let parser = LayoutPdfParser::new(
        std::env::var("READING_MCP_PDF_LAYOUT_PYTHON")
            .unwrap()
            .into(),
        reading_mcp::infrastructure::ResourceBudget::default(),
    )
    .with_ocr_config(config)
    .with_ocr_identity(identity.clone())
    .with_evidence_store(store.clone());
    for case in ["F03", "F04-form", "F04-flat"] {
        let source = DocumentSource(format!("file:///frozen/{case}.pdf"));
        let bytes = std::fs::read(format!("tests/fixtures/scanned_pdf/pdf/{case}.pdf")).unwrap();
        let raw_hash = format!("sha256:{:x}", sha2::Sha256::digest(&bytes));
        let document = parser
            .parse(RetrievedResource {
                source: source.clone(),
                final_source: source,
                media_type: MediaType("application/pdf".into()),
                bytes,
                etag: None,
                last_modified: None,
                metadata: Default::default(),
            })
            .await
            .unwrap();
        assert_eq!(document.content_hash.0, raw_hash);
        assert_eq!(document.try_paragraph_text_units().unwrap().units.len(), 6);
        document.validate_ocr_publication().unwrap();
        let derivation: reading_mcp::domain::OcrDerivation =
            serde_json::from_str(&document.metadata["ocr_derivation"]).unwrap();
        assert_eq!(derivation.selected_word_count, Some(0));
        let mut missing_count = document.clone();
        let mut incomplete = serde_json::to_value(&derivation).unwrap();
        incomplete
            .as_object_mut()
            .unwrap()
            .remove("selected_word_count");
        missing_count
            .metadata
            .insert("ocr_derivation".into(), incomplete.to_string());
        assert!(missing_count.validate_ocr_publication().is_err());
        let bytes = store
            .get(&document.metadata["ocr_evidence_blob"])
            .await
            .unwrap()
            .unwrap();
        let blob: reading_mcp::domain::OcrEvidenceBlob = serde_json::from_slice(&bytes).unwrap();
        blob.validate(1).unwrap();
        assert_eq!(blob.runtime_identity, identity);
        assert_eq!(blob.pages.len(), 1);
        assert!(
            blob.pages[0].attempts.is_empty(),
            "{case} must reuse its existing text layer"
        );
        let coverage = blob.pages[0].native_coverage.as_ref().unwrap();
        assert_eq!(coverage.uncovered_samples, 0);
        assert!(!coverage.masks.is_empty());
        assert_ne!(
            coverage.source_samples_sha256,
            coverage.masked_samples_sha256
        );
    }
}

#[tokio::test]
#[ignore = "requires pinned hosted OCR dependencies"]
async fn real_f14_long_sentence_survives_sqlite_reopen_and_exact_read() {
    use reading_mcp::application::ports::{ApplicationError, DocumentRepository};
    use reading_mcp::application::read_document::{ReadDocumentUseCase, ReadExactTargetCommand};
    use reading_mcp::domain::{OriginalSourceTarget, TextLocator};
    use reading_mcp::infrastructure::SqliteDocumentRepository;

    let directory = tempdir().unwrap();
    let store = Arc::new(FileOcrEvidenceStore::new(directory.path().join("evidence")));
    let bytes = std::fs::read("tests/fixtures/scanned_pdf/pdf/F14.pdf").unwrap();
    let source = DocumentSource("file:///frozen/F14.pdf".into());
    let mut documents = Vec::new();
    for revision in ["1", "2"] {
        let mut config = chinese_config();
        config.languages = vec!["eng".into()];
        config.operator_revision = revision.into();
        let identity = build_ocr_runtime_identity(config.clone()).unwrap();
        let parser = LayoutPdfParser::new(
            std::env::var("READING_MCP_PDF_LAYOUT_PYTHON")
                .unwrap()
                .into(),
            reading_mcp::infrastructure::ResourceBudget::default(),
        )
        .with_ocr_config(config)
        .with_ocr_identity(identity)
        .with_evidence_store(store.clone());
        let document = parser
            .parse(RetrievedResource {
                source: source.clone(),
                final_source: source.clone(),
                media_type: MediaType("application/pdf".into()),
                bytes: bytes.clone(),
                etag: None,
                last_modified: None,
                metadata: Default::default(),
            })
            .await
            .unwrap();
        document.validate_ocr_publication().unwrap();
        documents.push(document);
    }
    let document = &documents[0];
    let paragraphs = document.try_paragraph_text_units().unwrap();
    let sentences = document.try_sentence_text_units().unwrap();
    assert_eq!(
        paragraphs.units.len(),
        1,
        "48 clauses must not be line-count chunks"
    );
    assert_eq!(sentences.units.len(), 1, "F14 has one natural sentence");
    let sentence = &sentences.units[0];
    assert!(sentence.text.chars().count() > 2000);
    assert_eq!(sentence.text, paragraphs.units[0].text);
    assert_eq!(
        sentence.normalized_range,
        paragraphs.units[0].normalized_range
    );
    let section = document.find_section(&sentence.owner_section_id).unwrap();
    assert_eq!(
        section
            .normalized_text_slice(sentence.normalized_range)
            .unwrap(),
        sentence.text
    );
    let locator = TextLocator::for_sentence(document, section, sentence);
    assert_eq!(
        document
            .original_source_target_for_range(&sentence.owner_section_id, sentence.normalized_range)
            .unwrap(),
        Some(OriginalSourceTarget::Page { page_number: 1 })
    );
    let db = directory.path().join("state.sqlite");
    {
        let repository = SqliteDocumentRepository::open(&db).unwrap();
        repository.save(document.clone()).await.unwrap();
    }
    let repository = Arc::new(SqliteDocumentRepository::open(&db).unwrap());
    let restored = repository.get(&document.id).await.unwrap().unwrap();
    assert_eq!(restored.try_sentence_text_units().unwrap(), sentences);
    assert_eq!(
        restored.original_source_binding_map().unwrap(),
        document.original_source_binding_map().unwrap()
    );
    let reader = ReadDocumentUseCase::new(repository.clone());
    let command = ReadExactTargetCommand {
        document_id: document.id.clone(),
        target_locator: locator.clone(),
        max_chars: Some(8192),
    };
    let read = reader.read_exact(command).await.unwrap();
    assert!(read.complete);
    assert_eq!(read.content, sentence.text);
    assert_eq!(read.resolved_target_locator, locator);
    let changed = &documents[1];
    assert_eq!(changed.id, document.id);
    assert_eq!(changed.content_hash, document.content_hash);
    assert_ne!(
        changed.normalized_document_hash(),
        document.normalized_document_hash()
    );
    repository.save(changed.clone()).await.unwrap();
    let error = reader
        .read_exact(ReadExactTargetCommand {
            document_id: document.id.clone(),
            target_locator: locator,
            max_chars: Some(8192),
        })
        .await
        .unwrap_err();
    assert!(matches!(error, ApplicationError::StaleLocator(_)));
}

#[tokio::test]
#[ignore = "requires pinned hosted OCR dependencies"]
async fn real_f12_keeps_native_footer_and_excluded_ocr_observation() {
    let directory = tempdir().unwrap();
    let store = Arc::new(FileOcrEvidenceStore::new(directory.path().join("evidence")));
    let mut config = chinese_config();
    config.languages = vec!["eng".into()];
    let identity = build_ocr_runtime_identity(config.clone()).unwrap();
    let parser = LayoutPdfParser::new(
        std::env::var("READING_MCP_PDF_LAYOUT_PYTHON")
            .unwrap()
            .into(),
        reading_mcp::infrastructure::ResourceBudget::default(),
    )
    .with_ocr_config(config)
    .with_ocr_identity(identity)
    .with_evidence_store(store.clone());
    let source = DocumentSource("file:///frozen/F12.pdf".into());
    let document = parser
        .parse(RetrievedResource {
            source: source.clone(),
            final_source: source,
            media_type: MediaType("application/pdf".into()),
            bytes: std::fs::read("tests/fixtures/scanned_pdf/pdf/F12.pdf").unwrap(),
            etag: None,
            last_modified: None,
            metadata: Default::default(),
        })
        .await
        .unwrap();
    document.validate_ocr_publication().unwrap();
    let blob = store
        .get(document.metadata.get("ocr_evidence_blob").unwrap())
        .await
        .unwrap()
        .unwrap();
    let mut persisted: reading_mcp::domain::OcrEvidenceBlob =
        serde_json::from_slice(&blob).unwrap();
    persisted.validate(1).unwrap();
    let page = &persisted.pages[0];
    assert_eq!(page.native_regions.len(), 1);
    assert_eq!(page.native_regions[0].source_class, "page-footer");
    assert_eq!(page.native_regions[0].text, "Page 1");
    assert_eq!(page.excluded_sources.len(), 1);
    assert_eq!(page.selection.len(), 6);
    let reference = &page.excluded_sources[0];
    let excluded = &page
        .attempts
        .iter()
        .find(|a| a.id == reference.attempt)
        .unwrap()
        .boxes[reference.r#box];
    assert!(
        !excluded.textlines.is_empty(),
        "excluded observation must remain immutable"
    );
    assert!(
        document
            .root_sections
            .iter()
            .any(|s| s.content.contains("Page 1"))
    );
    persisted.pages[0].excluded_sources.clear();
    assert!(
        persisted
            .validate(1)
            .unwrap_err()
            .contains("exclusion references")
    );
}

#[tokio::test]
#[ignore = "requires pinned hosted OCR dependencies"]
async fn real_f05_blank_page_is_persisted_without_an_engine_attempt() {
    let directory = tempdir().unwrap();
    let store = Arc::new(FileOcrEvidenceStore::new(directory.path().join("evidence")));
    let mut config = chinese_config();
    config.languages = vec!["eng".into()];
    let identity = build_ocr_runtime_identity(config.clone()).unwrap();
    let parser = LayoutPdfParser::new(
        std::env::var("READING_MCP_PDF_LAYOUT_PYTHON")
            .unwrap()
            .into(),
        reading_mcp::infrastructure::ResourceBudget::default(),
    )
    .with_ocr_config(config)
    .with_ocr_identity(identity)
    .with_evidence_store(store.clone());
    let source = DocumentSource("file:///frozen/F05.pdf".into());
    let bytes = std::fs::read("tests/fixtures/scanned_pdf/pdf/F05.pdf").unwrap();
    let document = parser
        .parse(RetrievedResource {
            source: source.clone(),
            final_source: source,
            media_type: MediaType("application/pdf".into()),
            bytes: bytes.clone(),
            etag: None,
            last_modified: None,
            metadata: Default::default(),
        })
        .await
        .unwrap();
    assert_eq!(
        document.content_hash.0,
        format!("sha256:{:x}", sha2::Sha256::digest(&bytes))
    );
    document.validate_ocr_publication().unwrap();
    let blob = store
        .get(document.metadata.get("ocr_evidence_blob").unwrap())
        .await
        .unwrap()
        .unwrap();
    let mut persisted: reading_mcp::domain::OcrEvidenceBlob =
        serde_json::from_slice(&blob).unwrap();
    persisted.validate(4).unwrap();
    assert_eq!(
        persisted
            .pages
            .iter()
            .filter(|p| !p.attempts.is_empty())
            .map(|p| p.page)
            .collect::<Vec<_>>(),
        vec![2]
    );
    let blank = persisted.pages.iter_mut().find(|p| p.page == 4).unwrap();
    assert!(blank.attempts.is_empty());
    assert!(blank.blank_raster.is_some());
    blank.blank_raster.as_mut().unwrap().samples_sha256 = "0".repeat(64);
    assert!(persisted.validate(4).unwrap_err().contains("digest"));
}

#[tokio::test]
#[ignore = "requires pinned hosted OCR dependencies"]
async fn real_f07_ocr_publishes_typed_evidence_and_page_bindings() {
    let bytes = std::fs::read("tests/fixtures/scanned_pdf/pdf/F07.pdf").unwrap();
    let source = DocumentSource("file:///frozen/F07.pdf".into());
    let resource = RetrievedResource {
        source: source.clone(),
        final_source: source,
        media_type: MediaType("application/pdf".into()),
        bytes: bytes.clone(),
        etag: None,
        last_modified: None,
        metadata: Default::default(),
    };
    let config = chinese_config();
    let identity = build_ocr_runtime_identity(config.clone()).unwrap();
    let python = std::env::var("READING_MCP_PDF_LAYOUT_PYTHON").unwrap();
    let directory = tempdir().unwrap();
    let store = Arc::new(FileOcrEvidenceStore::new(directory.path().join("evidence")));
    let parser = LayoutPdfParser::new(
        python.into(),
        reading_mcp::infrastructure::ResourceBudget::default(),
    )
    .with_ocr_config(config)
    .with_ocr_identity(identity.clone())
    .with_evidence_store(store.clone());
    let document = parser.parse(resource).await.unwrap();
    let units = document.try_paragraph_text_units().unwrap();
    assert!(units.units.len() > 1);
    assert_eq!(
        document.content_hash.0,
        format!("sha256:{:x}", sha2::Sha256::digest(&bytes))
    );
    let derivation: reading_mcp::domain::OcrDerivation =
        serde_json::from_str(document.metadata.get("ocr_derivation").unwrap()).unwrap();
    derivation
        .validate_against(
            &identity,
            document.content_hash.0.trim_start_matches("sha256:"),
        )
        .unwrap();
    let blob = store
        .get(document.metadata.get("ocr_evidence_blob").unwrap())
        .await
        .unwrap()
        .unwrap();
    assert!(!blob.is_empty());
    let persisted: reading_mcp::domain::OcrEvidenceBlob = serde_json::from_slice(&blob).unwrap();
    persisted.validate(1).unwrap();
    assert_eq!(persisted.runtime_identity, identity);
    assert_eq!(persisted.pages.len(), 1);
    assert_eq!(persisted.pages[0].attempts.len(), 2);
    assert_eq!(persisted.pages[0].selection.len(), 4);
    assert!(
        persisted.pages[0]
            .components
            .iter()
            .all(|c| !c.replaced_refs.is_empty())
    );
    assert_eq!(
        derivation.evidence_blob.as_ref(),
        document.metadata.get("ocr_evidence_blob")
    );
    document.validate_ocr_publication().unwrap();
    // A changed discarded observation cannot be passed off as the same blob.
    let mut tampered = persisted.clone();
    tampered.pages[0].selection[0].source.r#box = usize::MAX;
    assert!(tampered.validate(1).is_err());
    let mut tampered = persisted.clone();
    tampered.pages[0].complete = false;
    assert!(tampered.validate(1).is_err());
    let mut tampered = persisted.clone();
    tampered.pages[0].attempts[0].boxes[0].textlines[0].spans[0].confidence = Some(101.0);
    assert!(tampered.validate(1).is_err());
    let mut tampered = persisted.clone();
    tampered.selected_words[0].text.push('x');
    assert!(tampered.validate(1).is_err());
    let map = document.original_source_binding_map().unwrap().unwrap();
    assert!(!map.bindings.is_empty());
    assert!(map.bindings.iter().all(|b| matches!(
        b.target,
        reading_mcp::domain::OriginalSourceTarget::Page { page_number: 1 }
    )));
    let mut changed = document.clone();
    let mut changed_map = map;
    changed_map.bindings[0].target =
        reading_mcp::domain::OriginalSourceTarget::Page { page_number: 2 };
    changed
        .set_original_source_binding_map(changed_map)
        .unwrap();
    assert_ne!(
        changed.normalized_document_hash(),
        document.normalized_document_hash()
    );
    assert!(changed.validate_ocr_publication().is_err());
    let mut changed = document.clone();
    changed.metadata.insert("ocr_derivation".into(), "{".into());
    assert!(changed.validate_ocr_publication().is_err());
}

mod publication_failure {
    use super::*;
    use async_trait::async_trait;
    use reading_mcp::application::open_document::{OpenDocumentCommand, OpenDocumentUseCase};
    use reading_mcp::application::ports::{
        ApplicationError, DocumentRepository, RetrievalOptions, Retriever, SearchHit, SearchIndex,
        SourcePolicy, TextUnitIndex,
    };
    use reading_mcp::domain::{Document, DocumentId, OcrEvidenceBlob, TextUnit};
    use reading_mcp::infrastructure::{ResourceBudget, SqliteDocumentRepository};
    use reading_mcp::parsing::ParserRouter;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Fixture(RetrievedResource);
    #[async_trait]
    impl SourcePolicy for Fixture {
        async fn validate(&self, source: &DocumentSource) -> Result<(), ApplicationError> {
            assert_eq!(source, &self.0.source);
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
            assert_eq!(source, &self.0.source);
            Ok(self.0.clone())
        }
    }
    #[derive(Default)]
    struct FailingEvidence(AtomicUsize);
    #[async_trait]
    impl OcrEvidenceStore for FailingEvidence {
        async fn put_immutable(&self, _: &str, bytes: &[u8]) -> Result<String, ApplicationError> {
            let blob: OcrEvidenceBlob = serde_json::from_slice(bytes).unwrap();
            blob.validate(1).unwrap();
            assert_eq!(blob.pages[0].attempts.len(), 2);
            self.0.fetch_add(1, Ordering::SeqCst);
            Err(ApplicationError::CacheFailed(
                "injected evidence write failure".into(),
            ))
        }
        async fn get(&self, _: &str) -> Result<Option<Vec<u8>>, ApplicationError> {
            panic!("unexpected evidence read");
        }
    }
    struct RecordingRepository {
        inner: Arc<SqliteDocumentRepository>,
        saves: AtomicUsize,
    }
    #[async_trait]
    impl DocumentRepository for RecordingRepository {
        async fn save(&self, document: Document) -> Result<(), ApplicationError> {
            self.saves.fetch_add(1, Ordering::SeqCst);
            self.inner.save(document).await
        }
        async fn get(&self, id: &DocumentId) -> Result<Option<Document>, ApplicationError> {
            self.inner.get(id).await
        }
    }
    #[derive(Default)]
    struct RecordingIndexes {
        search: AtomicUsize,
        units: AtomicUsize,
    }
    #[async_trait]
    impl SearchIndex for RecordingIndexes {
        async fn index(&self, _: &Document) -> Result<(), ApplicationError> {
            self.search.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
        async fn search(
            &self,
            _: &DocumentId,
            _: &str,
            _: usize,
        ) -> Result<Vec<SearchHit>, ApplicationError> {
            Ok(vec![])
        }
    }
    #[async_trait]
    impl TextUnitIndex for RecordingIndexes {
        async fn replace_document(
            &self,
            _: &DocumentId,
            _: &[TextUnit],
        ) -> Result<(), ApplicationError> {
            self.units.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
        async fn list_document(&self, _: &DocumentId) -> Result<Vec<TextUnit>, ApplicationError> {
            Ok(vec![])
        }
    }

    #[tokio::test]
    #[ignore = "requires pinned hosted OCR dependencies"]
    async fn real_ocr_evidence_failure_retains_sqlite_document_without_save_or_index() {
        let directory = tempdir().unwrap();
        let source = DocumentSource("file:///frozen/F07.pdf".into());
        let resource = RetrievedResource {
            source: source.clone(),
            final_source: source.clone(),
            media_type: MediaType("application/pdf".into()),
            bytes: std::fs::read("tests/fixtures/scanned_pdf/pdf/F07.pdf").unwrap(),
            etag: None,
            last_modified: None,
            metadata: Default::default(),
        };
        let old = ParserRouter::phase4()
            .parse(RetrievedResource {
                bytes: b"# Previous source\n\nPrevious canonical document remains available."
                    .to_vec(),
                media_type: MediaType("text/markdown".into()),
                ..resource.clone()
            })
            .await
            .unwrap();
        let sqlite = Arc::new(
            SqliteDocumentRepository::open(directory.path().join("documents.sqlite")).unwrap(),
        );
        sqlite.save(old.clone()).await.unwrap();
        let repository = Arc::new(RecordingRepository {
            inner: sqlite.clone(),
            saves: AtomicUsize::new(0),
        });
        let config = chinese_config();
        let identity = build_ocr_runtime_identity(config.clone()).unwrap();
        let evidence = Arc::new(FailingEvidence::default());
        let parser = Arc::new(
            LayoutPdfParser::new(
                std::env::var("READING_MCP_PDF_LAYOUT_PYTHON")
                    .unwrap()
                    .into(),
                ResourceBudget::default(),
            )
            .with_ocr_config(config)
            .with_ocr_identity(identity)
            .with_evidence_store(evidence.clone()),
        );
        let fixture = Arc::new(Fixture(resource));
        let indexes = Arc::new(RecordingIndexes::default());
        let usecase = OpenDocumentUseCase::with_text_unit_index(
            fixture.clone(),
            fixture,
            parser,
            repository.clone(),
            indexes.clone(),
            indexes.clone(),
        );
        let error = usecase
            .execute(OpenDocumentCommand {
                source,
                options: RetrievalOptions::default(),
            })
            .await
            .unwrap_err();
        assert_eq!(
            error,
            ApplicationError::CacheFailed("injected evidence write failure".into())
        );
        assert_eq!(evidence.0.load(Ordering::SeqCst), 1);
        assert_eq!(repository.saves.load(Ordering::SeqCst), 0);
        assert_eq!(indexes.search.load(Ordering::SeqCst), 0);
        assert_eq!(indexes.units.load(Ordering::SeqCst), 0);
        let retained = sqlite.get(&old.id).await.unwrap().unwrap();
        assert_eq!(retained.content_hash, old.content_hash);
        assert_eq!(
            retained.normalized_document_hash(),
            old.normalized_document_hash()
        );
        assert_eq!(retained.root_sections, old.root_sections);
    }
}
