//! `cite.page` is the cited unit's own page (#250): a paragraph or sentence handle of a PDF
//! reports the page of the unit's first character — the page `source_view` renders — read from
//! the owner section's `original_source_bindings_v1` rows, one indexed read per handle.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use lopdf::content::{Content, Operation};
use lopdf::{Bookmark, Document as PdfDocument, Object, Stream, dictionary};

use reading_mcp::application::cite_reading::CiteReadingUseCase;
use reading_mcp::application::open_document::{OpenDocumentCommand, OpenDocumentUseCase};
use reading_mcp::application::ports::{
    ApplicationError, DocumentRepository, RetrievalOptions, TextUnitIndex,
};
use reading_mcp::application::reading_error::ReadingErrorCode;
use reading_mcp::application::short_handle::{
    DocumentHandle, SectionHandle, handle_checksum, paragraph_handle, sentence_handle,
};
use reading_mcp::application::source_view::{
    GetSourceViewCommand, SourceViewLimits, SourceViewRepresentation, SourceViewUseCase,
};
use reading_mcp::application::text_unit_store::store_text_units;
use reading_mcp::domain::{
    ContentHash, Document, DocumentId, DocumentSource, Location, MediaType, NormalizedDocumentHash,
    NormalizedTextRange, ORIGINAL_SOURCE_BINDING_METADATA_KEY, OriginalSourceBinding,
    OriginalSourceBindingMap, OriginalSourceTarget, Section, SectionId,
};
use reading_mcp::infrastructure::{
    InMemorySearchIndex, SqliteDocumentRepository, SqliteTextUnitIndex,
};
use reading_mcp::mcp::v2_contracts::CiteView;
use reading_mcp::parsing::{ParserRouter, PdfSourceViewRenderer};
use reading_mcp::retrieval::{FileRetriever, LocalFileSourcePolicy};

const CHAPTER_A: &str = "section://chapter-a";

/// A three-page PDF whose outline gives `Chapter A` pages 1-2 and `Chapter B` page 3: the same
/// fixture `source_view_page_binding` renders, with a second sentence added to page 1 so the
/// section has two paragraphs on two pages.
fn build_multi_page_toc_pdf() -> Vec<u8> {
    let mut document = PdfDocument::with_version("1.5");
    let pages_id = document.new_object_id();
    let font_id = document.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => "Helvetica",
    });
    let resources_id = document.add_object(dictionary! {
        "Font" => dictionary! { "F1" => font_id },
    });
    let mut page_ids = Vec::new();

    for text in [
        "FIRST PAGE SENTENCE. SECOND PAGE COMES NEXT AND",
        "FINISHES ON PAGE TWO.",
        "THIRD PAGE SENTENCE.",
    ] {
        let content = Content {
            operations: vec![
                Operation::new("BT", vec![]),
                Operation::new("Tf", vec!["F1".into(), 14.into()]),
                Operation::new("Td", vec![72.into(), 720.into()]),
                Operation::new("Tj", vec![Object::string_literal(text)]),
                Operation::new("ET", vec![]),
            ],
        };
        let content_id = document.add_object(Stream::new(
            dictionary! {},
            content.encode().expect("fixture content should encode"),
        ));
        page_ids.push(document.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "Contents" => content_id,
        }));
    }

    document.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => page_ids.iter().copied().map(Object::Reference).collect::<Vec<_>>(),
            "Count" => page_ids.len() as i64,
            "Resources" => resources_id,
            "MediaBox" => vec![0.into(), 0.into(), 595.into(), 842.into()],
        }),
    );
    let catalog_id = document.add_object(dictionary! {
        "Type" => "Catalog",
        "Pages" => pages_id,
    });
    document.trailer.set("Root", catalog_id);

    document.add_bookmark(
        Bookmark::new("Chapter A".into(), [0.0, 0.0, 0.0], 0, page_ids[0]),
        None,
    );
    document.add_bookmark(
        Bookmark::new("Chapter B".into(), [0.0, 0.0, 0.0], 0, page_ids[2]),
        None,
    );
    if let Some(outline_id) = document.build_outline() {
        document
            .get_dictionary_mut(catalog_id)
            .expect("catalog should exist")
            .set("Outlines", outline_id);
    }

    let mut bytes = Vec::new();
    document
        .save_to(&mut bytes)
        .expect("fixture PDF should serialize");
    bytes
}

struct PdfCase {
    repository: Arc<SqliteDocumentRepository>,
    index: Arc<SqliteTextUnitIndex>,
    document_id: DocumentId,
    normalized_document_hash: NormalizedDocumentHash,
    database: std::path::PathBuf,
}

async fn open_pdf() -> (tempfile::TempDir, PdfCase) {
    let directory = tempfile::tempdir().expect("temporary directory should be created");
    let path = directory.path().join("multi-page-section.pdf");
    tokio::fs::write(&path, build_multi_page_toc_pdf())
        .await
        .expect("PDF fixture should be written");
    let database = directory.path().join("store.db");

    let repository = Arc::new(SqliteDocumentRepository::open(&database).expect("repository"));
    let index = Arc::new(SqliteTextUnitIndex::open(&database).expect("text unit index"));
    let opened = OpenDocumentUseCase::with_text_unit_index(
        Arc::new(LocalFileSourcePolicy::allow_roots([directory.path()])),
        Arc::new(FileRetriever),
        Arc::new(ParserRouter::phase4()),
        repository.clone(),
        index.clone(),
        Arc::new(InMemorySearchIndex::default()),
    )
    .execute(OpenDocumentCommand {
        source: DocumentSource(path.to_string_lossy().into_owned()),
        options: RetrievalOptions::default(),
    })
    .await
    .expect("PDF should open");

    (
        directory,
        PdfCase {
            repository,
            index,
            document_id: opened.document_id,
            normalized_document_hash: opened.normalized_document_hash,
            database,
        },
    )
}

impl PdfCase {
    async fn document_handle(&self) -> DocumentHandle {
        DocumentHandle {
            doc_key: self
                .index
                .document_key(&self.document_id)
                .await
                .expect("document key")
                .expect("stored document"),
            checksum: handle_checksum(
                &self.document_id,
                &self.normalized_document_hash,
                &MediaType("application/pdf".into()),
            ),
        }
    }

    async fn section_handle(&self, section_id: &str) -> SectionHandle {
        SectionHandle {
            document: self.document_handle().await,
            section_key: self
                .index
                .section_key(&self.document_id, &SectionId(section_id.into()))
                .await
                .expect("section key")
                .expect("stored section"),
        }
    }

    fn cite(&self) -> CiteReadingUseCase {
        CiteReadingUseCase::new(
            Arc::clone(&self.repository) as Arc<dyn DocumentRepository>,
            Arc::clone(&self.index) as Arc<dyn TextUnitIndex>,
        )
    }

    /// The page `source_view` renders for the handle, or the failure it reports.
    async fn source_view_page(&self, at: &str) -> Result<u32, String> {
        let locator = self
            .cite()
            .source_locator(at)
            .await
            .map_err(|error| error.detail)?;
        SourceViewUseCase::new(
            Arc::clone(&self.repository) as Arc<dyn DocumentRepository>,
            Arc::new(FileRetriever),
            Arc::new(PdfSourceViewRenderer),
            SourceViewLimits::default(),
        )
        .with_text_unit_index(Arc::clone(&self.index) as Arc<dyn TextUnitIndex>)
        .execute(GetSourceViewCommand {
            document_id: self.document_id.clone(),
            target_locator: locator,
            representation: SourceViewRepresentation::Original,
            dpi: Some(72),
        })
        .await
        .map(|result| result.page_number)
        .map_err(|error| error.to_string())
    }
}

#[tokio::test]
async fn paragraph_and_sentence_handles_cite_the_page_the_unit_is_on() {
    let (_directory, case) = open_pdf().await;
    let section = case.section_handle(CHAPTER_A).await;
    let p1 = paragraph_handle(&section, 1);
    let p2 = paragraph_handle(&section, 2);
    let s21 = sentence_handle(&section, 2, 1);

    let cite = case
        .cite()
        .execute(&[p1.clone(), p2.clone(), s21.clone()])
        .await
        .expect("cite should answer");

    // `Chapter A` spans `pdf:pages:1-2`; paragraph 2 starts on the section's second page.
    assert_eq!(cite.items[0].page, Some(1), "p1 sits on page 1");
    assert_eq!(cite.items[1].page, Some(2), "p2 sits on page 2");
    assert_eq!(cite.items[2].page, Some(2), "p2's sentence sits on page 2");

    // The cited page is the page `source_view` renders for the same handle.
    assert_eq!(
        case.source_view_page(&p1).await.expect("p1 renders"),
        cite.items[0].page.expect("p1 page"),
    );
    assert_eq!(
        case.source_view_page(&p2).await.expect("p2 renders"),
        cite.items[1].page.expect("p2 page"),
    );
    assert_eq!(
        case.source_view_page(&s21).await.expect("s2.1 renders"),
        cite.items[2].page.expect("s2.1 page"),
    );

    // `src` stays the owner section's native location on every handle.
    for item in &cite.items {
        assert_eq!(item.source_position.as_deref(), Some("pdf:pages:1-2"));
    }
}

#[tokio::test]
async fn section_and_document_handles_keep_the_section_start_page() {
    let (_directory, case) = open_pdf().await;
    let section = case.section_handle(CHAPTER_A).await.to_string();
    let document = case.document_handle().await.to_string();

    let cite = case
        .cite()
        .execute(&[section, document])
        .await
        .expect("cite should answer");

    assert_eq!(
        cite.items[0].page,
        Some(1),
        "a section handle still cites its start page"
    );
    assert_eq!(
        cite.items[0].source_position.as_deref(),
        Some("pdf:pages:1-2")
    );
    assert_eq!(
        cite.items[1].page, None,
        "a document handle carries no page"
    );
    assert_eq!(cite.items[1].source_position, None);
    assert_eq!(cite.items[1].locator, None);
}

#[tokio::test]
async fn a_unit_crossing_a_page_break_cites_its_first_page_and_source_view_refuses() {
    let directory = tempfile::tempdir().expect("temporary directory should be created");
    let database = directory.path().join("store.db");
    let repository = Arc::new(SqliteDocumentRepository::open(&database).expect("repository"));
    let index = Arc::new(SqliteTextUnitIndex::open(&database).expect("text unit index"));

    // One paragraph whose normalized range spans two page bindings: partial-page bindings, the
    // shape inferred-heading sections already store (#255 binds outline sections the same way).
    let section_id = SectionId("section://chapter".into());
    let content = "Alpha bravo charlie. Delta echo foxtrot.".to_owned();
    // Mid-"charlie": the first sentence and its paragraph cross the page break.
    let boundary = 15;
    let mut document = Document {
        id: DocumentId("doc:cross-page".into()),
        source: DocumentSource("memory:cross-page.pdf".into()),
        title: "Cross page".into(),
        media_type: MediaType("application/pdf".into()),
        content_hash: ContentHash("sha256:cross-page".into()),
        metadata: BTreeMap::new(),
        root_sections: vec![Section {
            id: section_id.clone(),
            parent_id: None,
            title: "Chapter".into(),
            level: 1,
            content: content.clone(),
            location: Location {
                page: Some(1),
                native_location: Some("pdf:pages:1-2".into()),
                ..Location::default()
            },
            children: vec![],
        }],
    };
    document
        .set_original_source_binding_map(OriginalSourceBindingMap::new(vec![
            OriginalSourceBinding {
                owner_section_id: section_id.clone(),
                normalized_range: NormalizedTextRange::new(0, boundary).expect("range"),
                target: OriginalSourceTarget::Page { page_number: 1 },
            },
            OriginalSourceBinding {
                owner_section_id: section_id.clone(),
                normalized_range: NormalizedTextRange::new(boundary, content.chars().count())
                    .expect("range"),
                target: OriginalSourceTarget::Page { page_number: 2 },
            },
        ]))
        .expect("binding map should validate");
    let normalized_document_hash = document.normalized_document_hash();
    repository
        .save(document.clone())
        .await
        .expect("document should save");
    store_text_units(index.as_ref(), &document)
        .await
        .expect("units should store");

    let document_handle = DocumentHandle {
        doc_key: index
            .document_key(&document.id)
            .await
            .expect("document key")
            .expect("stored document"),
        checksum: handle_checksum(
            &document.id,
            &normalized_document_hash,
            &document.media_type,
        ),
    };
    let section = SectionHandle {
        document: document_handle,
        section_key: index
            .section_key(&document.id, &section_id)
            .await
            .expect("section key")
            .expect("stored section"),
    };
    // Paragraph 1 covers the whole section text: it crosses the page break. Its first sentence
    // "Alpha bravo charlie." starts on page 1 and ends on page 2.
    let p1 = paragraph_handle(&section, 1);
    let s11 = sentence_handle(&section, 1, 1);

    let cite = CiteReadingUseCase::new(
        Arc::clone(&repository) as Arc<dyn DocumentRepository>,
        Arc::clone(&index) as Arc<dyn TextUnitIndex>,
    );
    let result = cite
        .execute(&[p1.clone(), s11.clone()])
        .await
        .expect("cite should answer");
    assert_eq!(
        result.items[0].page,
        Some(1),
        "a paragraph crossing a page break cites its first page"
    );
    assert_eq!(
        result.items[1].page,
        Some(1),
        "a sentence crossing a page break cites its first page"
    );

    // `source_view` still refuses the same handles with AmbiguousTarget.
    let source_view = SourceViewUseCase::new(
        Arc::clone(&repository) as Arc<dyn DocumentRepository>,
        Arc::new(FileRetriever),
        Arc::new(PdfSourceViewRenderer),
        SourceViewLimits::default(),
    )
    .with_text_unit_index(Arc::clone(&index) as Arc<dyn TextUnitIndex>);
    for at in [&p1, &s11] {
        let locator = cite.source_locator(at).await.expect("locator resolves");
        let error = source_view
            .execute(GetSourceViewCommand {
                document_id: document.id.clone(),
                target_locator: locator,
                representation: SourceViewRepresentation::Original,
                dpi: Some(72),
            })
            .await
            .expect_err("a unit spanning two pages is ambiguous to render");
        assert!(
            matches!(error, ApplicationError::InvalidRequest(_)),
            "expected the ambiguous-target refusal, got {error:?}"
        );
    }
}

#[tokio::test]
async fn a_pdf_stored_before_the_binding_rows_refuses_instead_of_citing_the_wrong_page() {
    let (_directory, case) = open_pdf().await;
    let section = case.section_handle(CHAPTER_A).await;
    let p2 = paragraph_handle(&section, 2);

    // The rows a store written before this change keeps.
    let deleted = rusqlite::Connection::open(&case.database)
        .expect("database should open")
        .execute(
            "DELETE FROM original_source_bindings_v1 WHERE document_id = ?1",
            rusqlite::params![case.document_id.0],
        )
        .expect("rows should delete");
    assert_eq!(deleted, 3);

    // No open result of the current store version means the bump has not re-ingested this
    // document yet: refuse, because reopening re-ingests it and stores the rows.
    let error = case
        .cite()
        .execute(std::slice::from_ref(&p2))
        .await
        .expect_err("a pre-change store has no covering binding for p2");
    assert_eq!(error.code, ReadingErrorCode::StaleHandle);

    // The same miss under a current-version open result is a freshly ingested document with
    // no binding rows: reopening cannot add them, so a unit on a multi-page section reports
    // no page instead of looping on stale_handle, and a unit on a one-page section answers
    // the section's own page.
    let connection = rusqlite::Connection::open(&case.database).expect("database should open");
    connection
        .execute_batch(
            "CREATE TABLE IF NOT EXISTS open_results_v1 (
                document_id TEXT PRIMARY KEY,
                fingerprint TEXT NOT NULL,
                result_json TEXT NOT NULL
             );",
        )
        .expect("open results table should be created");
    connection
        .execute(
            "INSERT INTO open_results_v1(document_id, fingerprint, result_json)
             VALUES (?1, ?2, '{}')",
            rusqlite::params![
                case.document_id.0,
                format!(
                    "open-result/v1|test|text-unit-stream/v1|{}",
                    reading_mcp::application::text_unit_stream::TEXT_UNIT_STORE_VERSION
                )
            ],
        )
        .expect("current open result should be inserted");
    drop(connection);
    let cite = case
        .cite()
        .execute(std::slice::from_ref(&p2))
        .await
        .expect("cite should answer");
    assert_eq!(
        cite.items[0].page, None,
        "a unit no binding covers on a multi-page section reports no page"
    );
    let chapter_b = case.section_handle("section://chapter-b").await;
    let b1 = paragraph_handle(&chapter_b, 1);
    let cite = case
        .cite()
        .execute(std::slice::from_ref(&b1))
        .await
        .expect("cite should answer");
    assert_eq!(
        cite.items[0].page,
        Some(3),
        "a unit on a one-page section answers the section's own page"
    );

    // Re-ingesting — what the store-version bump forces on the next open — stores the rows and
    // the same handle answers.
    let (document, _) = case
        .repository
        .get_with_hash(&case.document_id)
        .await
        .expect("load")
        .expect("document");
    case.repository
        .save_shared(document)
        .await
        .expect("re-ingest save");
    let cite = case.cite().execute(&[p2]).await.expect("cite answers");
    assert_eq!(cite.items[0].page, Some(2));
}

#[tokio::test]
async fn cite_reads_one_binding_row_per_unit_handle_and_never_the_whole_map() {
    let (_directory, case) = open_pdf().await;
    let section = case.section_handle(CHAPTER_A).await;
    let handles: Vec<String> = (1..=2)
        .map(|paragraph| paragraph_handle(&section, paragraph))
        .chain([
            sentence_handle(&section, 1, 1),
            sentence_handle(&section, 2, 1),
        ])
        .collect();

    let calls = Arc::new(AtomicUsize::new(0));
    let repository: Arc<dyn DocumentRepository> = Arc::new(UnitPageOnlyRepository {
        inner: Arc::clone(&case.repository) as Arc<dyn DocumentRepository>,
        calls: Arc::clone(&calls),
    });
    let cite = CiteReadingUseCase::new(
        repository,
        Arc::clone(&case.index) as Arc<dyn TextUnitIndex>,
    );
    let result = cite.execute(&handles).await.expect("cite should answer");

    assert_eq!(result.items[0].page, Some(1));
    assert_eq!(result.items[3].page, Some(2));
    assert_eq!(
        calls.load(Ordering::Relaxed),
        handles.len(),
        "one indexed binding read per unit handle"
    );
}

/// The repository `cite` may use: every whole-document read and the whole
/// `original_source_binding_map` metadata read panic, so a page lookup that loaded them fails the
/// test instead of silently costing O(document). The binding rows themselves forward and count.
struct UnitPageOnlyRepository {
    inner: Arc<dyn DocumentRepository>,
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl DocumentRepository for UnitPageOnlyRepository {
    async fn save(&self, document: Document) -> Result<(), ApplicationError> {
        self.inner.save(document).await
    }

    async fn get(&self, id: &DocumentId) -> Result<Option<Arc<Document>>, ApplicationError> {
        panic!("cite must not load the document ({})", id.0);
    }

    async fn get_with_hash(
        &self,
        id: &DocumentId,
    ) -> Result<Option<(Arc<Document>, NormalizedDocumentHash)>, ApplicationError> {
        panic!("cite must not load the document ({})", id.0);
    }

    async fn get_light_with_hash(
        &self,
        id: &DocumentId,
    ) -> Result<Option<(Arc<Document>, NormalizedDocumentHash)>, ApplicationError> {
        panic!("cite must not load the light document ({})", id.0);
    }

    async fn load_metadata(
        &self,
        id: &DocumentId,
        key: &str,
    ) -> Result<Option<String>, ApplicationError> {
        assert_ne!(
            key, ORIGINAL_SOURCE_BINDING_METADATA_KEY,
            "cite must not decode the whole binding map of {}",
            id.0
        );
        self.inner.load_metadata(id, key).await
    }

    async fn load_section_text(
        &self,
        id: &DocumentId,
        section_id: &SectionId,
    ) -> Result<Option<String>, ApplicationError> {
        self.inner.load_section_text(id, section_id).await
    }

    async fn load_section_char_len(
        &self,
        id: &DocumentId,
        section_id: &SectionId,
    ) -> Result<Option<usize>, ApplicationError> {
        self.inner.load_section_char_len(id, section_id).await
    }

    async fn original_source_page_at(
        &self,
        id: &DocumentId,
        section_id: &SectionId,
        offset: usize,
    ) -> Result<Option<u32>, ApplicationError> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        self.inner
            .original_source_page_at(id, section_id, offset)
            .await
    }

    async fn body_order(
        &self,
        id: &DocumentId,
    ) -> Result<Option<Result<Vec<SectionId>, ApplicationError>>, ApplicationError> {
        self.inner.body_order(id).await
    }

    async fn body_order_keys(
        &self,
        id: &DocumentId,
    ) -> Result<Option<Result<Vec<i64>, ApplicationError>>, ApplicationError> {
        self.inner.body_order_keys(id).await
    }

    async fn document_header(
        &self,
        id: &DocumentId,
    ) -> Result<Option<reading_mcp::application::ports::DocumentHeader>, ApplicationError> {
        self.inner.document_header(id).await
    }

    async fn section_by_id(
        &self,
        id: &DocumentId,
        section_id: &SectionId,
    ) -> Result<Option<reading_mcp::application::ports::SectionRow>, ApplicationError> {
        self.inner.section_by_id(id, section_id).await
    }

    async fn section_by_key(
        &self,
        id: &DocumentId,
        section_key: i64,
    ) -> Result<Option<reading_mcp::application::ports::SectionRow>, ApplicationError> {
        self.inner.section_by_key(id, section_key).await
    }
}

#[tokio::test]
async fn a_markdown_document_cites_exactly_what_it_cited_before() {
    let directory = tempfile::tempdir().expect("temporary directory should be created");
    let database = directory.path().join("store.db");
    let repository = Arc::new(SqliteDocumentRepository::open(&database).expect("repository"));
    let index = Arc::new(SqliteTextUnitIndex::open(&database).expect("text unit index"));

    let document = Document {
        id: DocumentId("doc:markdown-golden".into()),
        source: DocumentSource("memory:golden.md".into()),
        title: "Golden".into(),
        media_type: MediaType("text/markdown".into()),
        content_hash: ContentHash("sha256:markdown-golden".into()),
        metadata: BTreeMap::new(),
        root_sections: vec![Section {
            id: SectionId("section://alpha".into()),
            parent_id: None,
            title: "Alpha".into(),
            level: 1,
            content: "Alpha one. Alpha two.\n\nAlpha three.".into(),
            location: Location::default(),
            children: vec![],
        }],
    };
    let normalized_document_hash = document.normalized_document_hash();
    repository
        .save(document.clone())
        .await
        .expect("document should save");
    store_text_units(index.as_ref(), &document)
        .await
        .expect("units should store");

    let document_handle = DocumentHandle {
        doc_key: index
            .document_key(&document.id)
            .await
            .expect("document key")
            .expect("stored document"),
        checksum: handle_checksum(
            &document.id,
            &normalized_document_hash,
            &document.media_type,
        ),
    };
    let section = SectionHandle {
        document: document_handle.clone(),
        section_key: index
            .section_key(&document.id, &SectionId("section://alpha".into()))
            .await
            .expect("section key")
            .expect("stored section"),
    };
    let cite = CiteReadingUseCase::new(
        Arc::clone(&repository) as Arc<dyn DocumentRepository>,
        Arc::clone(&index) as Arc<dyn TextUnitIndex>,
    );
    let result = cite
        .execute(&[
            paragraph_handle(&section, 1),
            sentence_handle(&section, 1, 1),
            section.to_string(),
            document_handle.to_string(),
        ])
        .await
        .expect("cite should answer");

    // The golden: a non-PDF citation carries the locator and the identity, and no `src`/`page`.
    let view = serde_json::to_value(CiteView::from(result)).expect("view serializes");
    let locator_of = |index_of: usize| {
        view["items"][index_of]["locator"]
            .as_object()
            .expect("a unit or section citation carries its locator")
    };
    let expected_identity = serde_json::json!({
        "id": "doc:markdown-golden",
        "content_hash": "sha256:markdown-golden",
        "normalized_document_hash": normalized_document_hash.0,
        "norm": reading_mcp::domain::NORMALIZATION_VERSION,
        "ndh": reading_mcp::domain::NORMALIZED_DOCUMENT_HASH_VERSION,
        "seg": reading_mcp::domain::TEXT_SEGMENTATION_VERSION,
    });
    for (index_of, at) in [
        format!("{section}.p1"),
        format!("{section}.p1.1"),
        section.to_string(),
        document_handle.to_string(),
    ]
    .iter()
    .enumerate()
    {
        assert_eq!(view["items"][index_of]["at"], *at);
        assert_eq!(view["items"][index_of]["doc"], expected_identity);
        assert!(
            view["items"][index_of].get("src").is_none(),
            "a markdown citation has no src"
        );
        assert!(
            view["items"][index_of].get("page").is_none(),
            "a markdown citation has no page"
        );
    }
    let paragraph = index
        .section_unit(
            &document.id,
            &SectionId("section://alpha".into()),
            reading_mcp::application::get_text_units::EffectiveTextUnitKind::Paragraph,
            1,
            None,
        )
        .await
        .expect("stored paragraph")
        .expect("paragraph 1");
    let reading_mcp::application::ports::OwnedStreamUnit::Paragraph(paragraph) = paragraph else {
        panic!("paragraph 1 is stored as a paragraph");
    };
    assert_eq!(
        locator_of(0)["normalized_range"],
        serde_json::json!({
            "start": paragraph.normalized_range.start(),
            "end": paragraph.normalized_range.end(),
        }),
    );
    assert_eq!(locator_of(0)["paragraph_index"], 1);
    assert_eq!(locator_of(1)["sentence_index"], 1);
    assert_eq!(locator_of(2)["owner_section_id"], "section://alpha");
    assert!(
        view["items"][3].get("locator").is_none(),
        "a document citation keeps no locator"
    );
}
