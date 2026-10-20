use std::collections::BTreeMap;
use std::collections::HashMap;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use async_trait::async_trait;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use super::section_layout::{
    CHUNK_CHARS, LayoutCoverage, LayoutParagraph, LayoutSentence, SectionLayout,
};
use super::sqlite_support::{open_connection, run_sqlite};
use crate::application::get_text_units::{
    EffectiveTextUnitKind, RequestedTextUnitKind, TextUnitCoveragePolicy, TextUnitDirection,
};
use crate::application::open_document::OpenDocumentResult;
use crate::application::ports::{
    ApplicationError, DocumentRepository, OpenResultStore, OwnedStreamUnit, SectionTextUnits,
    StoredStreamItem, TextUnitIndex, TextUnitStreamKey, TextUnitStreamSummary,
};
use crate::application::text_unit_stream::{
    ParagraphCoverageDesc, ParagraphDesc, SectionCoverageDesc, SectionStreamPlanDesc, SentenceDesc,
    UnitRef, plan_section_stream_desc,
};
use crate::domain::{
    ContentHash, Document, DocumentId, DocumentSource, Location, MediaType, NORMALIZATION_VERSION,
    NormalizedDocumentHash, NormalizedTextIndex, NormalizedTextRange, ParagraphContentClass,
    ParagraphSectionCoverage, ParagraphTextUnitSet, Section, SectionId, SentenceEligibility,
    SentenceParagraphCoverage, SentenceTextUnit, SentenceTextUnitSet, TEXT_SEGMENTATION_VERSION,
    TextUnit, TextUnitId, TextUnitKind, paragraph_text_unit_id, sentence_text_unit_id,
};

/// How many decoded documents `SqliteDocumentRepository` keeps in memory.
const DOCUMENT_CACHE_CAPACITY: usize = 8;

/// Most recently used decoded documents, front first. `data_version` is `PRAGMA data_version` at the time the
/// entries were loaded: SQLite changes it when another connection commits, so a new value means they may be stale.
#[derive(Default)]
struct DocumentCache {
    data_version: Option<i64>,
    entries: VecDeque<(DocumentId, Arc<Document>, NormalizedDocumentHash)>,
}

impl DocumentCache {
    fn sync_version(&mut self, data_version: i64) {
        if self.data_version != Some(data_version) {
            self.entries.clear();
            self.data_version = Some(data_version);
        }
    }

    fn get(&mut self, id: &DocumentId) -> Option<(Arc<Document>, NormalizedDocumentHash)> {
        let index = self
            .entries
            .iter()
            .position(|(cached, _, _)| cached == id)?;
        let entry = self.entries.remove(index)?;
        let result = (Arc::clone(&entry.1), entry.2.clone());
        self.entries.push_front(entry);
        Some(result)
    }

    fn insert(
        &mut self,
        id: DocumentId,
        document: Arc<Document>,
        normalized_document_hash: NormalizedDocumentHash,
    ) {
        self.remove(&id);
        self.entries
            .push_front((id, document, normalized_document_hash));
        self.entries.truncate(DOCUMENT_CACHE_CAPACITY);
    }

    fn remove(&mut self, id: &DocumentId) {
        self.entries.retain(|(cached, _, _)| cached != id);
    }
}

/// How many decoded section layouts `SqliteTextUnitIndex` keeps in memory.
const SECTION_LAYOUT_CACHE_CAPACITY: usize = 32;

/// One decoded section layout, its planner descriptors and the stream plans built from them.
struct CachedSectionLayout {
    doc_key: i64,
    section_key: i64,
    section_id: SectionId,
    char_len: usize,
    paragraph_coverage: Option<ParagraphSectionCoverage>,
    layout: Arc<SectionLayout>,
    paragraphs: Vec<ParagraphDesc>,
    sentences: Vec<SentenceDesc>,
    coverage: Vec<ParagraphCoverageDesc>,
    section_coverage: Option<SectionCoverageDesc>,
    paragraph_plan: OnceLock<Result<Arc<SectionStreamPlanDesc>, String>>,
    sentence_plan: OnceLock<Result<Arc<SectionStreamPlanDesc>, String>>,
}

impl CachedSectionLayout {
    fn new(
        doc_key: i64,
        section_key: i64,
        section_id: SectionId,
        char_len: usize,
        paragraph_coverage: Option<ParagraphSectionCoverage>,
        layout: SectionLayout,
    ) -> Result<Self, ApplicationError> {
        let paragraphs: Vec<ParagraphDesc> = layout
            .paragraphs
            .iter()
            .map(|paragraph| ParagraphDesc {
                paragraph_index: paragraph.paragraph_index,
                char_count: paragraph.end - paragraph.start,
            })
            .collect();
        let sentences: Vec<SentenceDesc> = layout
            .sentences
            .iter()
            .map(|sentence| SentenceDesc {
                paragraph_index: sentence.paragraph_index,
                sentence_index: sentence.sentence_index,
                char_count: sentence.end - sentence.start,
            })
            .collect();
        let coverage = layout
            .coverage
            .iter()
            .map(|coverage| {
                Ok(ParagraphCoverageDesc {
                    paragraph_index: coverage.paragraph_index,
                    content_class: content_class_from_code(i64::from(coverage.content_class))?,
                    eligibility: sentence_eligibility_from_code(i64::from(coverage.eligibility))?,
                    separator_chars: coverage.separator_chars,
                })
            })
            .collect::<Result<Vec<_>, ApplicationError>>()?;
        let section_coverage = paragraph_coverage
            .as_ref()
            .map(|coverage| SectionCoverageDesc {
                owner_chars: coverage.owner_chars,
                separator_chars: coverage.separator_chars,
                paragraph_count: coverage.paragraph_count,
            });
        Ok(Self {
            doc_key,
            section_key,
            section_id,
            char_len,
            paragraph_coverage,
            layout: Arc::new(layout),
            paragraphs,
            sentences,
            coverage,
            section_coverage,
            paragraph_plan: OnceLock::new(),
            sentence_plan: OnceLock::new(),
        })
    }

    fn plan(
        &self,
        kind: RequestedTextUnitKind,
    ) -> Result<Arc<SectionStreamPlanDesc>, ApplicationError> {
        let cell = match kind {
            RequestedTextUnitKind::Paragraph => &self.paragraph_plan,
            RequestedTextUnitKind::Sentence => &self.sentence_plan,
        };
        let plan = cell.get_or_init(|| {
            plan_section_stream_desc(
                &self.section_id,
                &self.paragraphs,
                self.section_coverage.as_ref(),
                &self.sentences,
                &self.coverage,
                kind,
            )
            .map(Arc::new)
            .map_err(|error| error.to_string())
        });
        match plan {
            Ok(plan) => Ok(Arc::clone(plan)),
            Err(error) => Err(ApplicationError::InvalidRequest(error.clone())),
        }
    }
}

/// Most recently used section layouts, front first. `data_version` is `PRAGMA data_version` at the
/// time the entries were loaded, so a commit from another connection invalidates them.
#[derive(Default)]
struct SectionLayoutCache {
    data_version: Option<i64>,
    entries: VecDeque<Arc<CachedSectionLayout>>,
}

impl SectionLayoutCache {
    fn sync_version(&mut self, data_version: i64) {
        if self.data_version != Some(data_version) {
            self.entries.clear();
            self.data_version = Some(data_version);
        }
    }

    fn get(&mut self, doc_key: i64, section_key: i64) -> Option<Arc<CachedSectionLayout>> {
        let index = self
            .entries
            .iter()
            .position(|entry| entry.doc_key == doc_key && entry.section_key == section_key)?;
        let entry = self.entries.remove(index)?;
        let result = Arc::clone(&entry);
        self.entries.push_front(entry);
        Some(result)
    }

    fn insert(&mut self, entry: Arc<CachedSectionLayout>) {
        self.entries.retain(|cached| {
            cached.doc_key != entry.doc_key || cached.section_key != entry.section_key
        });
        self.entries.push_front(entry);
        self.entries.truncate(SECTION_LAYOUT_CACHE_CAPACITY);
    }

    fn remove_document(&mut self, doc_key: i64) {
        self.entries.retain(|cached| cached.doc_key != doc_key);
    }
}

pub struct SqliteDocumentRepository {
    path: PathBuf,
    connection: Arc<Mutex<Connection>>,
    cache: Arc<Mutex<DocumentCache>>,
}

impl SqliteDocumentRepository {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, ApplicationError> {
        let path = path.as_ref().to_path_buf();
        let connection = open_connection(&path, ApplicationError::RepositoryFailed)?;
        connection
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS documents (
                    id TEXT PRIMARY KEY,
                    source TEXT NOT NULL,
                    content_hash TEXT NOT NULL,
                    document_json TEXT NOT NULL
                 );",
            )
            .map_err(repository_error)?;
        ensure_normalized_document_hash_column(&connection)?;
        Ok(Self {
            path,
            connection: Arc::new(Mutex::new(connection)),
            cache: Arc::new(Mutex::new(DocumentCache::default())),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

#[async_trait]
impl DocumentRepository for SqliteDocumentRepository {
    async fn save(&self, document: Document) -> Result<(), ApplicationError> {
        self.save_shared(Arc::new(document)).await
    }

    async fn save_shared(&self, document: Arc<Document>) -> Result<(), ApplicationError> {
        document
            .validate_ocr_publication()
            .map_err(ApplicationError::RepositoryFailed)?;
        let normalized_document_hash = document.normalized_document_hash();
        let json = serde_json::to_string(&StoredDocument::from_document(&document))
            .map_err(|error| ApplicationError::RepositoryFailed(error.to_string()))?;
        let cache = Arc::clone(&self.cache);
        run_sqlite(
            &self.connection,
            ApplicationError::RepositoryFailed,
            move |connection| {
                connection
                    .execute(
                        "INSERT INTO documents(id, source, content_hash, document_json, normalized_document_hash)
                         VALUES (?1, ?2, ?3, ?4, ?5)
                         ON CONFLICT(id) DO UPDATE SET
                            source = excluded.source,
                            content_hash = excluded.content_hash,
                            document_json = excluded.document_json,
                            normalized_document_hash = excluded.normalized_document_hash",
                        params![
                            document.id.0,
                            document.source.0,
                            document.content_hash.0,
                            json,
                            normalized_document_hash.0
                        ],
                    )
                    .map_err(repository_error)?;
                let mut cache = cache.lock().map_err(|_| {
                    ApplicationError::RepositoryFailed("document cache lock poisoned".into())
                })?;
                cache.remove(&document.id);
                Ok(())
            },
        )
        .await
    }

    async fn get(&self, id: &DocumentId) -> Result<Option<Arc<Document>>, ApplicationError> {
        Ok(self.get_with_hash(id).await?.map(|(document, _)| document))
    }

    async fn get_with_hash(
        &self,
        id: &DocumentId,
    ) -> Result<Option<(Arc<Document>, NormalizedDocumentHash)>, ApplicationError> {
        let id = id.clone();
        let cache = Arc::clone(&self.cache);
        run_sqlite(
            &self.connection,
            ApplicationError::RepositoryFailed,
            move |connection| {
                let data_version: i64 = connection
                    .query_row("PRAGMA data_version", [], |row| row.get(0))
                    .map_err(repository_error)?;
                let mut cache = cache.lock().map_err(|_| {
                    ApplicationError::RepositoryFailed("document cache lock poisoned".into())
                })?;
                cache.sync_version(data_version);
                if let Some(cached) = cache.get(&id) {
                    return Ok(Some(cached));
                }
                let row = connection
                    .query_row(
                        "SELECT document_json, normalized_document_hash FROM documents WHERE id = ?1",
                        params![&id.0],
                        |row| {
                            Ok((
                                row.get::<_, String>(0)?,
                                row.get::<_, Option<String>>(1)?,
                            ))
                        },
                    )
                    .optional()
                    .map_err(repository_error)?;
                let Some((json, stored_hash)) = row else {
                    return Ok(None);
                };
                let stored = serde_json::from_str::<StoredDocument>(&json)
                    .map_err(|error| ApplicationError::RepositoryFailed(error.to_string()))?;
                let document = stored.into_document()?;
                document
                    .validate_ocr_publication()
                    .map_err(ApplicationError::RepositoryFailed)?;
                let document = Arc::new(document);
                let normalized_document_hash = match stored_hash {
                    Some(hash) => NormalizedDocumentHash(hash),
                    None => {
                        let hash = document.normalized_document_hash();
                        connection
                            .execute(
                                "UPDATE documents SET normalized_document_hash = ?1 WHERE id = ?2",
                                params![&hash.0, &id.0],
                            )
                            .map_err(repository_error)?;
                        hash
                    }
                };
                cache.insert(
                    id.clone(),
                    Arc::clone(&document),
                    normalized_document_hash.clone(),
                );
                Ok(Some((document, normalized_document_hash)))
            },
        )
        .await
    }
}

fn ensure_normalized_document_hash_column(connection: &Connection) -> Result<(), ApplicationError> {
    let has_column = {
        let mut statement = connection
            .prepare("PRAGMA table_info(documents)")
            .map_err(repository_error)?;
        let columns = statement
            .query_map([], |row| row.get::<_, String>(1))
            .map_err(repository_error)?;
        let mut has_column = false;
        for column in columns {
            if column.map_err(repository_error)? == "normalized_document_hash" {
                has_column = true;
            }
        }
        has_column
    };
    if !has_column {
        connection
            .execute_batch("ALTER TABLE documents ADD COLUMN normalized_document_hash TEXT")
            .map_err(repository_error)?;
    }
    Ok(())
}

/// Stores the open result of each document, keyed by document id and checked against a fingerprint on read.
pub struct SqliteOpenResultStore {
    connection: Arc<Mutex<Connection>>,
}

impl SqliteOpenResultStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, ApplicationError> {
        let connection = open_connection(path.as_ref(), ApplicationError::RepositoryFailed)?;
        connection
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS open_results_v1 (
                    document_id TEXT PRIMARY KEY,
                    fingerprint TEXT NOT NULL,
                    result_json TEXT NOT NULL
                 );",
            )
            .map_err(repository_error)?;
        Ok(Self {
            connection: Arc::new(Mutex::new(connection)),
        })
    }
}

#[async_trait]
impl OpenResultStore for SqliteOpenResultStore {
    async fn get(
        &self,
        id: &DocumentId,
        fingerprint: &str,
    ) -> Result<Option<OpenDocumentResult>, ApplicationError> {
        let id = id.0.clone();
        let fingerprint = fingerprint.to_owned();
        run_sqlite(
            &self.connection,
            ApplicationError::RepositoryFailed,
            move |connection| {
                let row = connection
                    .query_row(
                        "SELECT fingerprint, result_json FROM open_results_v1 WHERE document_id = ?1",
                        params![id],
                        |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
                    )
                    .optional()
                    .map_err(repository_error)?;
                Ok(row.and_then(|(stored_fingerprint, json)| {
                    if stored_fingerprint != fingerprint {
                        return None;
                    }
                    // A row that no longer deserializes is a miss; the document is simply re-opened.
                    serde_json::from_str::<OpenDocumentResult>(&json).ok()
                }))
            },
        )
        .await
    }

    async fn put(
        &self,
        result: &OpenDocumentResult,
        fingerprint: &str,
    ) -> Result<(), ApplicationError> {
        let json = serde_json::to_string(result)
            .map_err(|error| ApplicationError::RepositoryFailed(error.to_string()))?;
        let id = result.document_id.0.clone();
        let fingerprint = fingerprint.to_owned();
        run_sqlite(
            &self.connection,
            ApplicationError::RepositoryFailed,
            move |connection| {
                connection
                    .execute(
                        "INSERT INTO open_results_v1(document_id, fingerprint, result_json)
                         VALUES (?1, ?2, ?3)
                         ON CONFLICT(document_id) DO UPDATE SET
                            fingerprint = excluded.fingerprint,
                            result_json = excluded.result_json",
                        params![id, fingerprint, json],
                    )
                    .map_err(repository_error)?;
                Ok(())
            },
        )
        .await
    }

    async fn remove(&self, id: &DocumentId) -> Result<(), ApplicationError> {
        let id = id.0.clone();
        run_sqlite(
            &self.connection,
            ApplicationError::RepositoryFailed,
            move |connection| {
                connection
                    .execute(
                        "DELETE FROM open_results_v1 WHERE document_id = ?1",
                        params![id],
                    )
                    .map_err(repository_error)?;
                Ok(())
            },
        )
        .await
    }
}

pub struct SqliteTextUnitIndex {
    path: PathBuf,
    connection: Arc<Mutex<Connection>>,
    cache: Arc<Mutex<SectionLayoutCache>>,
}

impl SqliteTextUnitIndex {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, ApplicationError> {
        let path = path.as_ref().to_path_buf();
        let connection = open_connection(&path, ApplicationError::TextUnitIndexFailed)?;
        connection
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS text_units (
                    unit_id TEXT PRIMARY KEY,
                    document_id TEXT NOT NULL,
                    content_hash TEXT NOT NULL,
                    normalized_document_hash TEXT NOT NULL,
                    owner_section_id TEXT NOT NULL,
                    kind TEXT NOT NULL,
                    paragraph_index INTEGER NOT NULL,
                    source_order INTEGER NOT NULL,
                    range_start INTEGER NOT NULL,
                    range_end INTEGER NOT NULL,
                    text TEXT NOT NULL,
                    segmentation_version TEXT NOT NULL,
                    UNIQUE(
                        document_id,
                        normalized_document_hash,
                        owner_section_id,
                        kind,
                        paragraph_index,
                        segmentation_version
                    )
                 );
                 CREATE INDEX IF NOT EXISTS idx_text_units_document_source_order
                    ON text_units(document_id, source_order);",
            )
            .map_err(text_unit_index_error)?;
        // The v1 store is no longer read; dropping it here makes documents stored only in v1 read as
        // "not stored" (the caller reopens the document) and releases its space.
        connection
            .execute_batch(
                "DROP TABLE IF EXISTS text_unit_items_v1;
                 DROP TABLE IF EXISTS text_unit_sentence_coverage_v1;
                 DROP TABLE IF EXISTS text_unit_streams_v1;
                 DROP TABLE IF EXISTS text_unit_stream_summaries_v1;
                 DROP TABLE IF EXISTS text_unit_sections_v1;
                 DROP TABLE IF EXISTS text_unit_sets_v1;",
            )
            .map_err(text_unit_index_error)?;
        // The v2 store kept unit text on every row. The v3 store keeps section text once and only
        // the unit boundaries, so the v2 tables are removed and their documents re-ingest on reopen.
        connection
            .execute_batch(
                "DROP TABLE IF EXISTS text_unit_sections_v2;
                 DROP TABLE IF EXISTS text_unit_items_v2;
                 DROP TABLE IF EXISTS text_unit_sentence_coverage_v2;
                 DROP TABLE IF EXISTS text_unit_streams_v2;
                 DROP TABLE IF EXISTS text_unit_stream_summaries_v2;",
            )
            .map_err(text_unit_index_error)?;
        connection
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS text_unit_documents_v2 (
                    doc_key INTEGER PRIMARY KEY,
                    document_id TEXT NOT NULL UNIQUE,
                    content_hash TEXT NOT NULL,
                    normalized_document_hash TEXT NOT NULL,
                    segmentation_version TEXT NOT NULL
                 );
                 CREATE TABLE IF NOT EXISTS section_text_v1 (
                    doc_key INTEGER NOT NULL,
                    section_key INTEGER NOT NULL,
                    chunk_no INTEGER NOT NULL,
                    text TEXT NOT NULL,
                    PRIMARY KEY (doc_key, section_key, chunk_no)
                 ) WITHOUT ROWID;
                 CREATE TABLE IF NOT EXISTS section_layout_v1 (
                    doc_key INTEGER NOT NULL,
                    section_key INTEGER NOT NULL,
                    section_id TEXT NOT NULL,
                    char_len INTEGER NOT NULL,
                    paragraph_coverage_json TEXT,
                    layout BLOB NOT NULL,
                    PRIMARY KEY (doc_key, section_key)
                 ) WITHOUT ROWID;
                 CREATE UNIQUE INDEX IF NOT EXISTS idx_section_layout_v1_id
                    ON section_layout_v1(doc_key, section_id);",
            )
            .map_err(text_unit_index_error)?;
        Ok(Self {
            path,
            connection: Arc::new(Mutex::new(connection)),
            cache: Arc::new(Mutex::new(SectionLayoutCache::default())),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// One section's borrowed units and coverages, grouped in a single pass over the unit sets.
#[derive(Default)]
struct SectionGroups<'a> {
    paragraphs: Vec<&'a TextUnit>,
    paragraph_coverage: Option<&'a ParagraphSectionCoverage>,
    sentences: Vec<&'a SentenceTextUnit>,
    sentence_coverage: Vec<&'a SentenceParagraphCoverage>,
}

#[async_trait]
impl TextUnitIndex for SqliteTextUnitIndex {
    async fn replace_document(
        &self,
        document_id: &DocumentId,
        units: &[TextUnit],
    ) -> Result<(), ApplicationError> {
        for (expected_source_order, unit) in units.iter().enumerate() {
            if &unit.document_id != document_id {
                return Err(ApplicationError::TextUnitIndexFailed(format!(
                    "unit {} belongs to document {}, expected {}",
                    unit.id.0, unit.document_id.0, document_id.0
                )));
            }
            if unit.source_order != expected_source_order {
                return Err(ApplicationError::TextUnitIndexFailed(format!(
                    "unit {} has source order {}, expected {}",
                    unit.id.0, unit.source_order, expected_source_order
                )));
            }
            if unit.paragraph_index == 0 {
                return Err(ApplicationError::TextUnitIndexFailed(format!(
                    "unit {} has invalid zero paragraph index",
                    unit.id.0
                )));
            }
            if unit.normalized_range.len() != unit.text.chars().count() {
                return Err(ApplicationError::TextUnitIndexFailed(format!(
                    "unit {} range length {} does not match text length {}",
                    unit.id.0,
                    unit.normalized_range.len(),
                    unit.text.chars().count()
                )));
            }
        }

        let document_id = document_id.clone();
        let units = units.to_vec();
        run_sqlite(
            &self.connection,
            ApplicationError::TextUnitIndexFailed,
            move |connection| {
                let transaction = connection.transaction().map_err(text_unit_index_error)?;
                transaction
                    .execute(
                        "DELETE FROM text_units WHERE document_id = ?1",
                        params![&document_id.0],
                    )
                    .map_err(text_unit_index_error)?;
                {
                    let mut statement = transaction
                        .prepare(
                            "INSERT INTO text_units(
                                unit_id,
                                document_id,
                                content_hash,
                                normalized_document_hash,
                                owner_section_id,
                                kind,
                                paragraph_index,
                                source_order,
                                range_start,
                                range_end,
                                text,
                                segmentation_version
                             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                        )
                        .map_err(text_unit_index_error)?;

                    for unit in &units {
                        statement
                            .execute(params![
                                &unit.id.0,
                                &unit.document_id.0,
                                &unit.content_hash.0,
                                &unit.normalized_document_hash.0,
                                &unit.owner_section_id.0,
                                unit.kind.as_str(),
                                usize_to_i64(unit.paragraph_index)?,
                                usize_to_i64(unit.source_order)?,
                                usize_to_i64(unit.normalized_range.start())?,
                                usize_to_i64(unit.normalized_range.end())?,
                                &unit.text,
                                &unit.segmentation_version,
                            ])
                            .map_err(text_unit_index_error)?;
                    }
                }
                transaction.commit().map_err(text_unit_index_error)?;
                Ok(())
            },
        )
        .await
    }

    async fn list_document(
        &self,
        document_id: &DocumentId,
    ) -> Result<Vec<TextUnit>, ApplicationError> {
        let document_id = document_id.clone();
        let cache = Arc::clone(&self.cache);
        run_sqlite(
            &self.connection,
            ApplicationError::TextUnitIndexFailed,
            move |connection| {
                if let Some(meta) = load_stored_document(connection, &document_id)? {
                    return paragraph_units_from_layout(connection, &cache, &meta);
                }
                legacy_list_document(connection, &document_id)
            },
        )
        .await
    }

    async fn replace_document_sets(
        &self,
        document: &Arc<Document>,
        paragraphs: Arc<ParagraphTextUnitSet>,
        sentences: Arc<SentenceTextUnitSet>,
    ) -> Result<(), ApplicationError> {
        validate_paragraph_units(&document.id, &paragraphs.units)?;

        let document = Arc::clone(document);
        let cache = Arc::clone(&self.cache);
        run_sqlite(
            &self.connection,
            ApplicationError::TextUnitIndexFailed,
            move |connection| {
                let transaction = connection.transaction().map_err(text_unit_index_error)?;

                transaction
                    .execute(
                        "INSERT INTO text_unit_documents_v2(
                            document_id,
                            content_hash,
                            normalized_document_hash,
                            segmentation_version
                         ) VALUES (?1, ?2, ?3, ?4)
                         ON CONFLICT(document_id) DO UPDATE SET
                            content_hash = excluded.content_hash,
                            normalized_document_hash = excluded.normalized_document_hash,
                            segmentation_version = excluded.segmentation_version",
                        params![
                            &document.id.0,
                            &document.content_hash.0,
                            paragraphs.normalized_document_hash.as_ref(),
                            TEXT_SEGMENTATION_VERSION,
                        ],
                    )
                    .map_err(text_unit_index_error)?;
                let doc_key: i64 = transaction
                    .query_row(
                        "SELECT doc_key FROM text_unit_documents_v2 WHERE document_id = ?1",
                        params![&document.id.0],
                        |row| row.get(0),
                    )
                    .map_err(text_unit_index_error)?;

                transaction
                    .execute(
                        "DELETE FROM section_text_v1 WHERE doc_key = ?1",
                        params![doc_key],
                    )
                    .map_err(text_unit_index_error)?;
                transaction
                    .execute(
                        "DELETE FROM section_layout_v1 WHERE doc_key = ?1",
                        params![doc_key],
                    )
                    .map_err(text_unit_index_error)?;

                // Section keys follow document order: a depth-first walk of the root sections and their
                // children. Every section is stored, including sections without units or coverage.
                let mut ordered_sections = Vec::new();
                collect_sections(&document.root_sections, &mut ordered_sections);
                let mut section_keys = HashMap::new();
                for (index, section) in ordered_sections.iter().enumerate() {
                    section_keys.insert(
                        section.id.clone(),
                        i64::try_from(index).map_err(|_| {
                            ApplicationError::TextUnitIndexFailed(
                                "document has too many sections".into(),
                            )
                        })?,
                    );
                }

                // Group each section's units and coverages in one pass over the sets.
                let mut groups: HashMap<SectionId, SectionGroups<'_>> = HashMap::new();
                for unit in &paragraphs.units {
                    groups
                        .entry(unit.owner_section_id.clone())
                        .or_default()
                        .paragraphs
                        .push(unit);
                }
                for coverage in &paragraphs.coverage {
                    groups
                        .entry(coverage.owner_section_id.clone())
                        .or_default()
                        .paragraph_coverage = Some(coverage);
                }
                for unit in &sentences.units {
                    groups
                        .entry(unit.owner_section_id.clone())
                        .or_default()
                        .sentences
                        .push(unit);
                }
                for coverage in &sentences.coverage {
                    groups
                        .entry(coverage.owner_section_id.clone())
                        .or_default()
                        .sentence_coverage
                        .push(coverage);
                }

                {
                    let mut chunk_statement = transaction
                        .prepare(
                            "INSERT INTO section_text_v1(doc_key, section_key, chunk_no, text)
                             VALUES (?1, ?2, ?3, ?4)",
                        )
                        .map_err(text_unit_index_error)?;
                    let mut layout_statement = transaction
                        .prepare(
                            "INSERT INTO section_layout_v1(
                                doc_key,
                                section_key,
                                section_id,
                                char_len,
                                paragraph_coverage_json,
                                layout
                             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                        )
                        .map_err(text_unit_index_error)?;

                    for section in &ordered_sections {
                        let section_key = section_keys[&section.id];
                        insert_section_text(
                            &mut chunk_statement,
                            doc_key,
                            section_key,
                            &section.content,
                        )?;

                        let group = groups.get(&section.id);
                        let paragraph_coverage_json =
                            match group.and_then(|group| group.paragraph_coverage) {
                                Some(coverage) => Some(
                                    serde_json::to_string(&StoredParagraphCoverage::from_coverage(
                                        coverage,
                                    ))
                                    .map_err(|error| {
                                        ApplicationError::TextUnitIndexFailed(error.to_string())
                                    })?,
                                ),
                                None => None,
                            };
                        let layout = match group {
                            Some(group) => build_section_layout(group)?,
                            None => SectionLayout {
                                paragraphs: Vec::new(),
                                sentences: Vec::new(),
                                coverage: Vec::new(),
                            },
                        };
                        let layout_blob = layout.encode()?;
                        layout_statement
                            .execute(params![
                                doc_key,
                                section_key,
                                &section.id.0,
                                usize_to_i64(section.normalized_text_len())?,
                                paragraph_coverage_json,
                                layout_blob,
                            ])
                            .map_err(text_unit_index_error)?;
                    }
                }

                transaction.commit().map_err(text_unit_index_error)?;
                let mut cache = cache.lock().map_err(|_| {
                    ApplicationError::TextUnitIndexFailed(
                        "section layout cache lock poisoned".into(),
                    )
                })?;
                cache.remove_document(doc_key);
                Ok(())
            },
        )
        .await
    }

    async fn section_units(
        &self,
        document_id: &DocumentId,
        section_id: &SectionId,
    ) -> Result<Option<SectionTextUnits>, ApplicationError> {
        let document_id = document_id.clone();
        let section_id = section_id.clone();
        let cache = Arc::clone(&self.cache);
        run_sqlite(
            &self.connection,
            ApplicationError::TextUnitIndexFailed,
            move |connection| {
                let Some(meta) = load_stored_document(connection, &document_id)? else {
                    return Ok(None);
                };
                if meta.segmentation_version != TEXT_SEGMENTATION_VERSION {
                    return Ok(None);
                }
                let Some(entry) =
                    load_cached_section_layout(connection, &cache, &meta, &section_id)?
                else {
                    return Ok(Some(SectionTextUnits {
                        paragraphs: Vec::new(),
                        paragraph_coverage: None,
                        sentences: Vec::new(),
                        sentence_coverage: Vec::new(),
                    }));
                };

                let text =
                    load_section_text(connection, meta.doc_key, entry.section_key, entry.char_len)?;
                let index = NormalizedTextIndex::new(&text);

                let mut paragraphs = Vec::with_capacity(entry.layout.paragraphs.len());
                for paragraph in &entry.layout.paragraphs {
                    let unit_text =
                        slice_section_text(&index, paragraph.start, paragraph.end, "paragraph")?;
                    paragraphs.push(rebuild_paragraph_unit(
                        &meta,
                        &section_id,
                        usize_to_i64(paragraph.paragraph_index)?,
                        usize_to_i64(paragraph.source_order)?,
                        usize_to_i64(paragraph.start)?,
                        usize_to_i64(paragraph.end)?,
                        unit_text,
                    )?);
                }

                let mut sentences = Vec::with_capacity(entry.layout.sentences.len());
                for sentence in &entry.layout.sentences {
                    let unit_text =
                        slice_section_text(&index, sentence.start, sentence.end, "sentence")?;
                    sentences.push(rebuild_sentence_unit(
                        &meta,
                        &section_id,
                        usize_to_i64(sentence.paragraph_index)?,
                        usize_to_i64(sentence.sentence_index)?,
                        usize_to_i64(sentence.source_order)?,
                        usize_to_i64(sentence.start)?,
                        usize_to_i64(sentence.end)?,
                        Some(usize_to_i64(sentence.parent_start)?),
                        Some(usize_to_i64(sentence.parent_end)?),
                        unit_text,
                    )?);
                }

                let mut sentence_coverage = Vec::with_capacity(entry.layout.coverage.len());
                for coverage in &entry.layout.coverage {
                    let paragraph_range =
                        stored_normalized_range(coverage.paragraph_start, coverage.paragraph_end)?;
                    sentence_coverage.push(SentenceParagraphCoverage {
                        owner_section_id: section_id.clone(),
                        paragraph_id: paragraph_text_unit_id(
                            &meta.document_id,
                            &meta.normalized_document_hash,
                            &section_id,
                            coverage.paragraph_index,
                            paragraph_range,
                        ),
                        paragraph_index: coverage.paragraph_index,
                        content_class: content_class_from_code(i64::from(coverage.content_class))?,
                        eligibility: sentence_eligibility_from_code(i64::from(
                            coverage.eligibility,
                        ))?,
                        paragraph_chars: coverage.paragraph_chars,
                        sentence_chars: coverage.sentence_chars,
                        separator_chars: coverage.separator_chars,
                        coarse_only_chars: coverage.coarse_only_chars,
                        sentence_count: coverage.sentence_count,
                    });
                }

                Ok(Some(SectionTextUnits {
                    paragraphs,
                    paragraph_coverage: entry.paragraph_coverage.clone(),
                    sentences,
                    sentence_coverage,
                }))
            },
        )
        .await
    }

    async fn section_unit(
        &self,
        document_id: &DocumentId,
        section_id: &SectionId,
        unit_kind: EffectiveTextUnitKind,
        paragraph_index: usize,
        sentence_index: Option<usize>,
    ) -> Result<Option<OwnedStreamUnit>, ApplicationError> {
        let document_id = document_id.clone();
        let section_id = section_id.clone();
        let cache = Arc::clone(&self.cache);
        run_sqlite(
            &self.connection,
            ApplicationError::TextUnitIndexFailed,
            move |connection| {
                let Some(meta) = load_stored_document(connection, &document_id)? else {
                    return Err(stale_text_units(&document_id));
                };
                if meta.segmentation_version != TEXT_SEGMENTATION_VERSION {
                    return Err(stale_text_units(&document_id));
                }
                let Some(entry) =
                    load_cached_section_layout(connection, &cache, &meta, &section_id)?
                else {
                    return Err(stale_text_units(&document_id));
                };

                match unit_kind {
                    EffectiveTextUnitKind::Paragraph => {
                        let Some(paragraph) = entry
                            .layout
                            .paragraphs
                            .iter()
                            .find(|paragraph| paragraph.paragraph_index == paragraph_index)
                        else {
                            return Ok(None);
                        };
                        let text = load_text_slice(
                            connection,
                            meta.doc_key,
                            entry.section_key,
                            paragraph.start,
                            paragraph.end,
                        )?;
                        Ok(Some(OwnedStreamUnit::Paragraph(rebuild_paragraph_unit(
                            &meta,
                            &section_id,
                            usize_to_i64(paragraph.paragraph_index)?,
                            usize_to_i64(paragraph.source_order)?,
                            usize_to_i64(paragraph.start)?,
                            usize_to_i64(paragraph.end)?,
                            text,
                        )?)))
                    }
                    EffectiveTextUnitKind::Sentence => {
                        let Some(sentence) = entry.layout.sentences.iter().find(|sentence| {
                            sentence.paragraph_index == paragraph_index
                                && Some(sentence.sentence_index) == sentence_index
                        }) else {
                            return Ok(None);
                        };
                        let text = load_text_slice(
                            connection,
                            meta.doc_key,
                            entry.section_key,
                            sentence.start,
                            sentence.end,
                        )?;
                        Ok(Some(OwnedStreamUnit::Sentence(rebuild_sentence_unit(
                            &meta,
                            &section_id,
                            usize_to_i64(sentence.paragraph_index)?,
                            usize_to_i64(sentence.sentence_index)?,
                            usize_to_i64(sentence.source_order)?,
                            usize_to_i64(sentence.start)?,
                            usize_to_i64(sentence.end)?,
                            Some(usize_to_i64(sentence.parent_start)?),
                            Some(usize_to_i64(sentence.parent_end)?),
                            text,
                        )?)))
                    }
                }
            },
        )
        .await
    }

    async fn section_stream_summary(
        &self,
        key: TextUnitStreamKey<'_>,
    ) -> Result<Option<TextUnitStreamSummary>, ApplicationError> {
        let document_id = key.document_id.clone();
        let section_id = key.section_id.clone();
        let kind = key.kind;
        let policy = key.policy;
        let cache = Arc::clone(&self.cache);
        run_sqlite(
            &self.connection,
            ApplicationError::TextUnitIndexFailed,
            move |connection| {
                let Some(meta) = load_stored_document(connection, &document_id)? else {
                    return Ok(None);
                };
                if meta.segmentation_version != TEXT_SEGMENTATION_VERSION {
                    return Ok(None);
                }
                let Some(entry) =
                    load_cached_section_layout(connection, &cache, &meta, &section_id)?
                else {
                    // A stored document whose section has no layout row behaves like the v2 store's
                    // section without paragraph coverage: the planner reports it as unavailable.
                    return Err(ApplicationError::InvalidRequest(format!(
                        "paragraph coverage is unavailable for section {}",
                        section_id.0
                    )));
                };
                let plan = entry.plan(kind)?;
                let (total_items, coverage) = match policy {
                    TextUnitCoveragePolicy::PreserveSource => {
                        (plan.entries.len(), plan.preserve_source.clone())
                    }
                    TextUnitCoveragePolicy::EligibleOnly => (
                        plan.entries.iter().filter(|entry| entry.eligible).count(),
                        plan.eligible_only.clone(),
                    ),
                };
                Ok(Some(TextUnitStreamSummary {
                    total_items,
                    coverage,
                }))
            },
        )
        .await
    }

    async fn section_stream_items(
        &self,
        key: TextUnitStreamKey<'_>,
        direction: TextUnitDirection,
        position: usize,
        limit: usize,
    ) -> Result<Vec<StoredStreamItem>, ApplicationError> {
        let document_id = key.document_id.clone();
        let section_id = key.section_id.clone();
        let kind = key.kind;
        let policy = key.policy;
        let cache = Arc::clone(&self.cache);
        run_sqlite(
            &self.connection,
            ApplicationError::TextUnitIndexFailed,
            move |connection| {
                let Some(meta) = load_stored_document(connection, &document_id)? else {
                    return Ok(Vec::new());
                };
                let Some(entry) =
                    load_cached_section_layout(connection, &cache, &meta, &section_id)?
                else {
                    return Ok(Vec::new());
                };
                if entry.section_coverage.is_none() {
                    return Ok(Vec::new());
                }
                let plan = entry.plan(kind)?;

                let mut stream = Vec::with_capacity(plan.entries.len());
                let mut eligible_position = 0usize;
                for (preserve_position, item) in plan.entries.iter().enumerate() {
                    let stream_position = match policy {
                        TextUnitCoveragePolicy::PreserveSource => preserve_position,
                        TextUnitCoveragePolicy::EligibleOnly => {
                            if !item.eligible {
                                continue;
                            }
                            let stream_position = eligible_position;
                            eligible_position += 1;
                            stream_position
                        }
                    };
                    stream.push((stream_position, item));
                }

                let total = stream.len();
                let (start, end) = match direction {
                    TextUnitDirection::Forward => {
                        (position.min(total), (position + limit).min(total))
                    }
                    TextUnitDirection::Backward => {
                        let end = position.min(total);
                        (end.saturating_sub(limit), end)
                    }
                };

                let range = stream[start..end]
                    .iter()
                    .try_fold(None, |range, (_, item)| {
                        let (unit_start, unit_end) = unit_char_range(&entry.layout, item.unit)?;
                        Ok::<_, ApplicationError>(Some(match range {
                            None => (unit_start, unit_end),
                            Some((range_start, range_end)) => {
                                (range_start.min(unit_start), range_end.max(unit_end))
                            }
                        }))
                    })?;
                let (range_start, range_text) = match range {
                    Some((range_start, range_end)) => (
                        range_start,
                        load_text_slice(
                            connection,
                            meta.doc_key,
                            entry.section_key,
                            range_start,
                            range_end,
                        )?,
                    ),
                    None => (0, String::new()),
                };
                let text_index = NormalizedTextIndex::new(&range_text);
                let mut items = Vec::with_capacity(end - start);
                for (stream_position, item) in &stream[start..end] {
                    items.push(StoredStreamItem {
                        position: *stream_position,
                        unit: build_owned_unit(&meta, &entry, item.unit, &text_index, range_start)?,
                        content_class: item.content_class,
                        degradation: item.degradation.map(str::to_owned),
                        char_count: item.char_count,
                    });
                }
                Ok(items)
            },
        )
        .await
    }

    async fn section_stream_position(
        &self,
        key: TextUnitStreamKey<'_>,
        unit_kind: EffectiveTextUnitKind,
        paragraph_index: usize,
        sentence_index: Option<usize>,
    ) -> Result<Option<usize>, ApplicationError> {
        let document_id = key.document_id.clone();
        let section_id = key.section_id.clone();
        let kind = key.kind;
        let policy = key.policy;
        let cache = Arc::clone(&self.cache);
        run_sqlite(
            &self.connection,
            ApplicationError::TextUnitIndexFailed,
            move |connection| {
                let Some(meta) = load_stored_document(connection, &document_id)? else {
                    return Ok(None);
                };
                let Some(entry) =
                    load_cached_section_layout(connection, &cache, &meta, &section_id)?
                else {
                    return Ok(None);
                };
                if entry.section_coverage.is_none() {
                    return Ok(None);
                }
                let target = match unit_kind {
                    EffectiveTextUnitKind::Paragraph => {
                        if sentence_index.is_some() {
                            return Ok(None);
                        }
                        entry
                            .layout
                            .paragraphs
                            .iter()
                            .position(|paragraph| paragraph.paragraph_index == paragraph_index)
                            .map(UnitRef::Paragraph)
                    }
                    EffectiveTextUnitKind::Sentence => entry
                        .layout
                        .sentences
                        .iter()
                        .position(|sentence| {
                            sentence.paragraph_index == paragraph_index
                                && Some(sentence.sentence_index) == sentence_index
                        })
                        .map(UnitRef::Sentence),
                };
                let Some(target) = target else {
                    return Ok(None);
                };
                let plan = entry.plan(kind)?;
                let mut eligible_position = 0usize;
                for (preserve_position, item) in plan.entries.iter().enumerate() {
                    let stream_position = match policy {
                        TextUnitCoveragePolicy::PreserveSource => preserve_position,
                        TextUnitCoveragePolicy::EligibleOnly => {
                            if !item.eligible {
                                continue;
                            }
                            let stream_position = eligible_position;
                            eligible_position += 1;
                            stream_position
                        }
                    };
                    if item.unit == target {
                        return Ok(Some(stream_position));
                    }
                }
                Ok(None)
            },
        )
        .await
    }
}

fn stale_text_units(document_id: &DocumentId) -> ApplicationError {
    ApplicationError::StaleDocument(format!(
        "text units of document {} are not stored for {TEXT_SEGMENTATION_VERSION}; open the document again",
        document_id.0
    ))
}

/// The document-level facts the v2 store keeps once per document. Unit ids, hashes and versions are
/// recomputed from these instead of being repeated on every row.
struct StoredUnitDocument {
    doc_key: i64,
    document_id: DocumentId,
    content_hash: ContentHash,
    normalized_document_hash: NormalizedDocumentHash,
    segmentation_version: String,
}

fn load_stored_document(
    connection: &Connection,
    document_id: &DocumentId,
) -> Result<Option<StoredUnitDocument>, ApplicationError> {
    connection
        .query_row(
            "SELECT doc_key, document_id, content_hash, normalized_document_hash, segmentation_version
             FROM text_unit_documents_v2
             WHERE document_id = ?1
               AND EXISTS (
                    SELECT 1 FROM section_layout_v1
                    WHERE section_layout_v1.doc_key = text_unit_documents_v2.doc_key
               )",
            params![&document_id.0],
            |row| {
                Ok(StoredUnitDocument {
                    doc_key: row.get(0)?,
                    document_id: DocumentId(row.get(1)?),
                    content_hash: ContentHash(row.get(2)?),
                    normalized_document_hash: NormalizedDocumentHash(row.get(3)?),
                    segmentation_version: row.get(4)?,
                })
            },
        )
        .optional()
        .map_err(text_unit_index_error)
}

fn load_cached_section_layout(
    connection: &Connection,
    cache: &Mutex<SectionLayoutCache>,
    meta: &StoredUnitDocument,
    section_id: &SectionId,
) -> Result<Option<Arc<CachedSectionLayout>>, ApplicationError> {
    let data_version: i64 = connection
        .query_row("PRAGMA data_version", [], |row| row.get(0))
        .map_err(text_unit_index_error)?;
    let mut cache = cache.lock().map_err(|_| {
        ApplicationError::TextUnitIndexFailed("section layout cache lock poisoned".into())
    })?;
    cache.sync_version(data_version);

    let row = connection
        .query_row(
            "SELECT section_key, char_len, paragraph_coverage_json
             FROM section_layout_v1
             WHERE doc_key = ?1 AND section_id = ?2",
            params![meta.doc_key, &section_id.0],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            },
        )
        .optional()
        .map_err(text_unit_index_error)?;
    let Some((section_key, char_len, coverage_json)) = row else {
        return Ok(None);
    };
    if let Some(entry) = cache.get(meta.doc_key, section_key) {
        return Ok(Some(entry));
    }

    let layout_blob: Vec<u8> = connection
        .query_row(
            "SELECT layout FROM section_layout_v1 WHERE doc_key = ?1 AND section_key = ?2",
            params![meta.doc_key, section_key],
            |row| row.get(0),
        )
        .map_err(text_unit_index_error)?;
    let layout = SectionLayout::decode(&layout_blob)?;
    let paragraph_coverage = coverage_json
        .map(
            |json| -> Result<ParagraphSectionCoverage, ApplicationError> {
                let stored: StoredParagraphCoverage = serde_json::from_str(&json)
                    .map_err(|error| ApplicationError::TextUnitIndexFailed(error.to_string()))?;
                Ok(stored.into_coverage())
            },
        )
        .transpose()?;
    let entry = Arc::new(CachedSectionLayout::new(
        meta.doc_key,
        section_key,
        section_id.clone(),
        i64_to_usize(char_len, "char_len")?,
        paragraph_coverage,
        layout,
    )?);
    cache.insert(Arc::clone(&entry));
    Ok(Some(entry))
}

fn load_section_text(
    connection: &Connection,
    doc_key: i64,
    section_key: i64,
    char_len: usize,
) -> Result<String, ApplicationError> {
    if char_len == 0 {
        return Ok(String::new());
    }
    let mut statement = connection
        .prepare(
            "SELECT chunk_no, text
             FROM section_text_v1
             WHERE doc_key = ?1 AND section_key = ?2
             ORDER BY chunk_no ASC",
        )
        .map_err(text_unit_index_error)?;
    let rows = statement
        .query_map(params![doc_key, section_key], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(text_unit_index_error)?;
    let mut text = String::new();
    for (expected_chunk, row) in rows.enumerate() {
        let (chunk_no, chunk) = row.map_err(text_unit_index_error)?;
        let chunk_no = i64_to_usize(chunk_no, "chunk_no")?;
        if chunk_no != expected_chunk {
            return Err(ApplicationError::TextUnitIndexFailed(format!(
                "section {section_key} is missing text chunk {expected_chunk}"
            )));
        }
        text.push_str(&chunk);
    }
    if text.chars().count() != char_len {
        return Err(ApplicationError::TextUnitIndexFailed(format!(
            "section {section_key} text has {} chars, expected {char_len}",
            text.chars().count()
        )));
    }
    Ok(text)
}

fn load_text_slice(
    connection: &Connection,
    doc_key: i64,
    section_key: i64,
    start: usize,
    end: usize,
) -> Result<String, ApplicationError> {
    if start >= end {
        return Ok(String::new());
    }
    let first_chunk = start / CHUNK_CHARS;
    let last_chunk = (end - 1) / CHUNK_CHARS;
    let mut statement = connection
        .prepare(
            "SELECT chunk_no, text
             FROM section_text_v1
             WHERE doc_key = ?1 AND section_key = ?2
               AND chunk_no BETWEEN ?3 AND ?4
             ORDER BY chunk_no ASC",
        )
        .map_err(text_unit_index_error)?;
    let rows = statement
        .query_map(
            params![
                doc_key,
                section_key,
                usize_to_i64(first_chunk)?,
                usize_to_i64(last_chunk)?
            ],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
        )
        .map_err(text_unit_index_error)?;

    let mut text = String::new();
    for (offset, row) in rows.enumerate() {
        let (chunk_no, chunk) = row.map_err(text_unit_index_error)?;
        let chunk_no = i64_to_usize(chunk_no, "chunk_no")?;
        let expected_chunk = first_chunk + offset;
        if chunk_no != expected_chunk {
            return Err(ApplicationError::TextUnitIndexFailed(format!(
                "section {section_key} is missing text chunk {expected_chunk}"
            )));
        }
        let chunk_base = chunk_no.checked_mul(CHUNK_CHARS).ok_or_else(|| {
            ApplicationError::TextUnitIndexFailed("text chunk offset overflow".into())
        })?;
        let local_start = start.saturating_sub(chunk_base);
        let local_end = end.min(chunk_base + CHUNK_CHARS) - chunk_base;
        let chunk_len = chunk.chars().count();
        if local_end > chunk_len {
            return Err(ApplicationError::TextUnitIndexFailed(format!(
                "section {section_key} chunk {chunk_no} has {chunk_len} chars, expected at least {local_end}"
            )));
        }
        let byte_start = char_to_byte(&chunk, local_start).ok_or_else(|| {
            ApplicationError::TextUnitIndexFailed(format!(
                "section {section_key} chunk {chunk_no} has no char {local_start}"
            ))
        })?;
        let byte_end = char_to_byte(&chunk, local_end).ok_or_else(|| {
            ApplicationError::TextUnitIndexFailed(format!(
                "section {section_key} chunk {chunk_no} has no char {local_end}"
            ))
        })?;
        text.push_str(&chunk[byte_start..byte_end]);
    }
    if text.chars().count() != end - start {
        return Err(ApplicationError::TextUnitIndexFailed(format!(
            "section {section_key} slice [{start}, {end}) has {} chars, expected {}",
            text.chars().count(),
            end - start
        )));
    }
    Ok(text)
}

fn char_to_byte(text: &str, char_offset: usize) -> Option<usize> {
    if char_offset == 0 {
        return Some(0);
    }
    let mut count = 0usize;
    for (byte, _) in text.char_indices() {
        if count == char_offset {
            return Some(byte);
        }
        count += 1;
    }
    (count == char_offset).then_some(text.len())
}

fn slice_section_text(
    index: &NormalizedTextIndex<'_>,
    start: usize,
    end: usize,
    what: &str,
) -> Result<String, ApplicationError> {
    let range = stored_normalized_range(start, end)?;
    index.slice(range).map(str::to_owned).map_err(|error| {
        ApplicationError::TextUnitIndexFailed(format!(
            "persisted {what} range is not a valid section slice: {error}"
        ))
    })
}

fn paragraph_units_from_layout(
    connection: &Connection,
    cache: &Mutex<SectionLayoutCache>,
    meta: &StoredUnitDocument,
) -> Result<Vec<TextUnit>, ApplicationError> {
    let mut statement = connection
        .prepare(
            "SELECT section_key, section_id
             FROM section_layout_v1
             WHERE doc_key = ?1
             ORDER BY section_key ASC",
        )
        .map_err(text_unit_index_error)?;
    let rows = statement
        .query_map(params![meta.doc_key], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(text_unit_index_error)?;

    let mut units = Vec::new();
    for row in rows {
        let (_section_key, section_id) = row.map_err(text_unit_index_error)?;
        let section_id = SectionId(section_id);
        let Some(entry) = load_cached_section_layout(connection, cache, meta, &section_id)? else {
            continue;
        };
        let text = load_section_text(connection, meta.doc_key, entry.section_key, entry.char_len)?;
        let index = NormalizedTextIndex::new(&text);
        for paragraph in &entry.layout.paragraphs {
            let unit_text =
                slice_section_text(&index, paragraph.start, paragraph.end, "paragraph")?;
            units.push(rebuild_paragraph_unit(
                meta,
                &section_id,
                usize_to_i64(paragraph.paragraph_index)?,
                usize_to_i64(paragraph.source_order)?,
                usize_to_i64(paragraph.start)?,
                usize_to_i64(paragraph.end)?,
                unit_text,
            )?);
        }
    }
    units.sort_by_key(|unit| unit.source_order);
    Ok(units)
}

fn legacy_list_document(
    connection: &Connection,
    document_id: &DocumentId,
) -> Result<Vec<TextUnit>, ApplicationError> {
    let mut statement = connection
        .prepare(
            "SELECT
                unit_id,
                content_hash,
                normalized_document_hash,
                owner_section_id,
                kind,
                paragraph_index,
                source_order,
                range_start,
                range_end,
                text,
                segmentation_version
             FROM text_units
             WHERE document_id = ?1
             ORDER BY source_order ASC",
        )
        .map_err(text_unit_index_error)?;
    let rows = statement
        .query_map(params![&document_id.0], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, i64>(5)?,
                row.get::<_, i64>(6)?,
                row.get::<_, i64>(7)?,
                row.get::<_, i64>(8)?,
                row.get::<_, String>(9)?,
                row.get::<_, String>(10)?,
            ))
        })
        .map_err(text_unit_index_error)?;

    let mut units = Vec::new();
    for row in rows {
        let (
            unit_id,
            content_hash,
            normalized_document_hash,
            owner_section_id,
            kind,
            paragraph_index,
            source_order,
            range_start,
            range_end,
            text,
            segmentation_version,
        ) = row.map_err(text_unit_index_error)?;
        let kind = match kind.as_str() {
            "paragraph" => TextUnitKind::Paragraph,
            other => {
                return Err(ApplicationError::TextUnitIndexFailed(format!(
                    "unsupported persisted text unit kind {other:?}"
                )));
            }
        };
        let range_start = i64_to_usize(range_start, "range_start")?;
        let range_end = i64_to_usize(range_end, "range_end")?;
        let normalized_range =
            NormalizedTextRange::new(range_start, range_end).map_err(|error| {
                ApplicationError::TextUnitIndexFailed(format!(
                    "invalid persisted normalized range: {error}"
                ))
            })?;

        if normalized_range.len() != text.chars().count() {
            return Err(ApplicationError::TextUnitIndexFailed(format!(
                "persisted unit {unit_id} range length {} does not match text length {}",
                normalized_range.len(),
                text.chars().count()
            )));
        }

        units.push(TextUnit {
            id: TextUnitId(unit_id),
            document_id: document_id.clone(),
            content_hash: ContentHash(content_hash),
            normalized_document_hash: NormalizedDocumentHash(normalized_document_hash),
            owner_section_id: SectionId(owner_section_id),
            kind,
            paragraph_index: i64_to_usize(paragraph_index, "paragraph_index")?,
            source_order: i64_to_usize(source_order, "source_order")?,
            normalized_range,
            text,
            segmentation_version,
        });
    }
    Ok(units)
}

fn collect_sections<'a>(sections: &'a [Section], out: &mut Vec<&'a Section>) {
    for section in sections {
        out.push(section);
        collect_sections(&section.children, out);
    }
}

fn insert_section_text(
    statement: &mut rusqlite::Statement<'_>,
    doc_key: i64,
    section_key: i64,
    text: &str,
) -> Result<(), ApplicationError> {
    let mut chunk_no = 0i64;
    let mut start = 0usize;
    let mut chars = 0usize;
    for (index, _) in text.char_indices() {
        if chars == CHUNK_CHARS {
            statement
                .execute(params![doc_key, section_key, chunk_no, &text[start..index]])
                .map_err(text_unit_index_error)?;
            chunk_no += 1;
            start = index;
            chars = 0;
        }
        chars += 1;
    }
    if start < text.len() {
        statement
            .execute(params![doc_key, section_key, chunk_no, &text[start..]])
            .map_err(text_unit_index_error)?;
    }
    Ok(())
}

fn build_section_layout(group: &SectionGroups<'_>) -> Result<SectionLayout, ApplicationError> {
    let paragraph_ranges: HashMap<usize, (usize, usize)> = group
        .paragraphs
        .iter()
        .map(|unit| {
            (
                unit.paragraph_index,
                (unit.normalized_range.start(), unit.normalized_range.end()),
            )
        })
        .collect();

    let paragraphs = group
        .paragraphs
        .iter()
        .map(|unit| LayoutParagraph {
            paragraph_index: unit.paragraph_index,
            source_order: unit.source_order,
            start: unit.normalized_range.start(),
            end: unit.normalized_range.end(),
        })
        .collect();

    let sentences = group
        .sentences
        .iter()
        .map(|unit| {
            let (parent_start, parent_end) = paragraph_ranges
                .get(&unit.paragraph_index)
                .copied()
                .ok_or_else(|| {
                ApplicationError::TextUnitIndexFailed(format!(
                    "sentence {} has no persisted parent paragraph range",
                    unit.id.0
                ))
            })?;
            Ok(LayoutSentence {
                paragraph_index: unit.paragraph_index,
                sentence_index: unit.sentence_index,
                source_order: unit.source_order,
                start: unit.normalized_range.start(),
                end: unit.normalized_range.end(),
                parent_start,
                parent_end,
            })
        })
        .collect::<Result<Vec<_>, ApplicationError>>()?;

    let coverage = group
        .sentence_coverage
        .iter()
        .map(|coverage| {
            let (paragraph_start, paragraph_end) = paragraph_ranges
                .get(&coverage.paragraph_index)
                .copied()
                .ok_or_else(|| {
                    ApplicationError::TextUnitIndexFailed(format!(
                        "sentence coverage {} has no persisted paragraph range",
                        coverage.paragraph_id.0
                    ))
                })?;
            Ok(LayoutCoverage {
                paragraph_index: coverage.paragraph_index,
                paragraph_start,
                paragraph_end,
                content_class: u8::try_from(content_class_code(coverage.content_class)).map_err(
                    |_| {
                        ApplicationError::TextUnitIndexFailed(
                            "paragraph content class code does not fit u8".into(),
                        )
                    },
                )?,
                eligibility: u8::try_from(sentence_eligibility_code(coverage.eligibility))
                    .map_err(|_| {
                        ApplicationError::TextUnitIndexFailed(
                            "sentence eligibility code does not fit u8".into(),
                        )
                    })?,
                paragraph_chars: coverage.paragraph_chars,
                sentence_chars: coverage.sentence_chars,
                separator_chars: coverage.separator_chars,
                coarse_only_chars: coverage.coarse_only_chars,
                sentence_count: coverage.sentence_count,
            })
        })
        .collect::<Result<Vec<_>, ApplicationError>>()?;

    Ok(SectionLayout {
        paragraphs,
        sentences,
        coverage,
    })
}

fn unit_char_range(
    layout: &SectionLayout,
    unit: UnitRef,
) -> Result<(usize, usize), ApplicationError> {
    match unit {
        UnitRef::Paragraph(index) => layout
            .paragraphs
            .get(index)
            .map(|paragraph| (paragraph.start, paragraph.end))
            .ok_or_else(|| {
                ApplicationError::TextUnitIndexFailed(format!(
                    "stream references missing paragraph descriptor {index}"
                ))
            }),
        UnitRef::Sentence(index) => layout
            .sentences
            .get(index)
            .map(|sentence| (sentence.start, sentence.end))
            .ok_or_else(|| {
                ApplicationError::TextUnitIndexFailed(format!(
                    "stream references missing sentence descriptor {index}"
                ))
            }),
    }
}

fn slice_loaded_text(
    index: &NormalizedTextIndex<'_>,
    base: usize,
    start: usize,
    end: usize,
    what: &str,
) -> Result<String, ApplicationError> {
    let local_start = start.checked_sub(base).ok_or_else(|| {
        ApplicationError::TextUnitIndexFailed(format!(
            "persisted {what} range starts before the loaded section text"
        ))
    })?;
    let local_end = end.checked_sub(base).ok_or_else(|| {
        ApplicationError::TextUnitIndexFailed(format!(
            "persisted {what} range ends before the loaded section text"
        ))
    })?;
    slice_section_text(index, local_start, local_end, what)
}

fn build_owned_unit(
    meta: &StoredUnitDocument,
    entry: &CachedSectionLayout,
    unit: UnitRef,
    index: &NormalizedTextIndex<'_>,
    base: usize,
) -> Result<OwnedStreamUnit, ApplicationError> {
    match unit {
        UnitRef::Paragraph(unit_index) => {
            let paragraph = entry.layout.paragraphs.get(unit_index).ok_or_else(|| {
                ApplicationError::TextUnitIndexFailed(format!(
                    "stream references missing paragraph descriptor {unit_index}"
                ))
            })?;
            let text = slice_loaded_text(index, base, paragraph.start, paragraph.end, "paragraph")?;
            Ok(OwnedStreamUnit::Paragraph(rebuild_paragraph_unit(
                meta,
                &entry.section_id,
                usize_to_i64(paragraph.paragraph_index)?,
                usize_to_i64(paragraph.source_order)?,
                usize_to_i64(paragraph.start)?,
                usize_to_i64(paragraph.end)?,
                text,
            )?))
        }
        UnitRef::Sentence(unit_index) => {
            let sentence = entry.layout.sentences.get(unit_index).ok_or_else(|| {
                ApplicationError::TextUnitIndexFailed(format!(
                    "stream references missing sentence descriptor {unit_index}"
                ))
            })?;
            let text = slice_loaded_text(index, base, sentence.start, sentence.end, "sentence")?;
            Ok(OwnedStreamUnit::Sentence(rebuild_sentence_unit(
                meta,
                &entry.section_id,
                usize_to_i64(sentence.paragraph_index)?,
                usize_to_i64(sentence.sentence_index)?,
                usize_to_i64(sentence.source_order)?,
                usize_to_i64(sentence.start)?,
                usize_to_i64(sentence.end)?,
                Some(usize_to_i64(sentence.parent_start)?),
                Some(usize_to_i64(sentence.parent_end)?),
                text,
            )?))
        }
    }
}
fn rebuild_paragraph_unit(
    meta: &StoredUnitDocument,
    section_id: &SectionId,
    paragraph_index: i64,
    source_order: i64,
    range_start: i64,
    range_end: i64,
    text: String,
) -> Result<TextUnit, ApplicationError> {
    let paragraph_index = i64_to_usize(paragraph_index, "paragraph_index")?;
    let source_order = i64_to_usize(source_order, "source_order")?;
    let normalized_range = stored_normalized_range(
        i64_to_usize(range_start, "range_start")?,
        i64_to_usize(range_end, "range_end")?,
    )?;
    if normalized_range.len() != text.chars().count() {
        return Err(ApplicationError::TextUnitIndexFailed(format!(
            "persisted unit range length {} does not match text length {}",
            normalized_range.len(),
            text.chars().count()
        )));
    }
    Ok(TextUnit {
        id: paragraph_text_unit_id(
            &meta.document_id,
            &meta.normalized_document_hash,
            section_id,
            paragraph_index,
            normalized_range,
        ),
        document_id: meta.document_id.clone(),
        content_hash: meta.content_hash.clone(),
        normalized_document_hash: meta.normalized_document_hash.clone(),
        owner_section_id: section_id.clone(),
        kind: TextUnitKind::Paragraph,
        paragraph_index,
        source_order,
        normalized_range,
        text,
        segmentation_version: meta.segmentation_version.clone(),
    })
}

#[allow(clippy::too_many_arguments)]
fn rebuild_sentence_unit(
    meta: &StoredUnitDocument,
    section_id: &SectionId,
    paragraph_index: i64,
    sentence_index: i64,
    source_order: i64,
    range_start: i64,
    range_end: i64,
    parent_start: Option<i64>,
    parent_end: Option<i64>,
    text: String,
) -> Result<SentenceTextUnit, ApplicationError> {
    let paragraph_index = i64_to_usize(paragraph_index, "paragraph_index")?;
    let sentence_index = i64_to_usize(sentence_index, "sentence_index")?;
    let source_order = i64_to_usize(source_order, "source_order")?;
    let normalized_range = stored_normalized_range(
        i64_to_usize(range_start, "range_start")?,
        i64_to_usize(range_end, "range_end")?,
    )?;
    if normalized_range.len() != text.chars().count() {
        return Err(ApplicationError::TextUnitIndexFailed(format!(
            "persisted unit range length {} does not match text length {}",
            normalized_range.len(),
            text.chars().count()
        )));
    }
    let parent_range = stored_normalized_range(
        i64_to_usize(
            parent_start.ok_or_else(missing_parent_range)?,
            "parent_start",
        )?,
        i64_to_usize(parent_end.ok_or_else(missing_parent_range)?, "parent_end")?,
    )?;
    Ok(SentenceTextUnit {
        id: sentence_text_unit_id(
            &meta.document_id,
            &meta.normalized_document_hash,
            section_id,
            paragraph_index,
            sentence_index,
            normalized_range,
        ),
        document_id: meta.document_id.clone(),
        content_hash: meta.content_hash.clone(),
        normalized_document_hash: meta.normalized_document_hash.clone(),
        owner_section_id: section_id.clone(),
        paragraph_index,
        sentence_index,
        parent_paragraph_id: paragraph_text_unit_id(
            &meta.document_id,
            &meta.normalized_document_hash,
            section_id,
            paragraph_index,
            parent_range,
        ),
        source_order,
        normalized_range,
        text,
        segmentation_version: meta.segmentation_version.clone(),
    })
}

fn missing_parent_range() -> ApplicationError {
    ApplicationError::TextUnitIndexFailed(
        "persisted sentence has no parent paragraph range".to_string(),
    )
}

// ParagraphContentClass: 0 prose_or_unknown, 1 native_paragraph, 2 blockquote, 3 list_item,
// 4 code_block, 5 preformatted, 6 table.
fn content_class_code(class: ParagraphContentClass) -> i64 {
    match class {
        ParagraphContentClass::ProseOrUnknown => 0,
        ParagraphContentClass::NativeParagraph => 1,
        ParagraphContentClass::BlockQuote => 2,
        ParagraphContentClass::ListItem => 3,
        ParagraphContentClass::CodeBlock => 4,
        ParagraphContentClass::Preformatted => 5,
        ParagraphContentClass::Table => 6,
    }
}

fn content_class_from_code(code: i64) -> Result<ParagraphContentClass, ApplicationError> {
    match code {
        0 => Ok(ParagraphContentClass::ProseOrUnknown),
        1 => Ok(ParagraphContentClass::NativeParagraph),
        2 => Ok(ParagraphContentClass::BlockQuote),
        3 => Ok(ParagraphContentClass::ListItem),
        4 => Ok(ParagraphContentClass::CodeBlock),
        5 => Ok(ParagraphContentClass::Preformatted),
        6 => Ok(ParagraphContentClass::Table),
        other => Err(ApplicationError::TextUnitIndexFailed(format!(
            "unsupported persisted paragraph content class code {other}"
        ))),
    }
}

// SentenceEligibility: 0 eligible, 1 coarse_paragraph_only.
fn sentence_eligibility_code(eligibility: SentenceEligibility) -> i64 {
    match eligibility {
        SentenceEligibility::Eligible => 0,
        SentenceEligibility::CoarseParagraphOnly => 1,
    }
}

fn sentence_eligibility_from_code(code: i64) -> Result<SentenceEligibility, ApplicationError> {
    match code {
        0 => Ok(SentenceEligibility::Eligible),
        1 => Ok(SentenceEligibility::CoarseParagraphOnly),
        other => Err(ApplicationError::TextUnitIndexFailed(format!(
            "unsupported persisted sentence eligibility code {other}"
        ))),
    }
}

// RequestedTextUnitKind and TextUnitCoveragePolicy codes were part of the removed v2 stream rows.

fn validate_paragraph_units(
    document_id: &DocumentId,
    units: &[TextUnit],
) -> Result<(), ApplicationError> {
    for (expected_source_order, unit) in units.iter().enumerate() {
        if &unit.document_id != document_id {
            return Err(ApplicationError::TextUnitIndexFailed(format!(
                "unit {} belongs to document {}, expected {}",
                unit.id.0, unit.document_id.0, document_id.0
            )));
        }
        if unit.source_order != expected_source_order {
            return Err(ApplicationError::TextUnitIndexFailed(format!(
                "unit {} has source order {}, expected {}",
                unit.id.0, unit.source_order, expected_source_order
            )));
        }
        if unit.paragraph_index == 0 {
            return Err(ApplicationError::TextUnitIndexFailed(format!(
                "unit {} has invalid zero paragraph index",
                unit.id.0
            )));
        }
        if unit.normalized_range.len() != unit.text.chars().count() {
            return Err(ApplicationError::TextUnitIndexFailed(format!(
                "unit {} range length {} does not match text length {}",
                unit.id.0,
                unit.normalized_range.len(),
                unit.text.chars().count()
            )));
        }
    }
    Ok(())
}

fn stored_normalized_range(
    start: usize,
    end: usize,
) -> Result<NormalizedTextRange, ApplicationError> {
    NormalizedTextRange::new(start, end).map_err(|error| {
        ApplicationError::TextUnitIndexFailed(format!(
            "invalid persisted normalized range: {error}"
        ))
    })
}

fn repository_error(error: rusqlite::Error) -> ApplicationError {
    ApplicationError::RepositoryFailed(error.to_string())
}

fn text_unit_index_error(error: rusqlite::Error) -> ApplicationError {
    ApplicationError::TextUnitIndexFailed(error.to_string())
}

fn usize_to_i64(value: usize) -> Result<i64, ApplicationError> {
    i64::try_from(value).map_err(|_| {
        ApplicationError::TextUnitIndexFailed(format!(
            "text unit numeric value {value} exceeds SQLite INTEGER range"
        ))
    })
}

fn i64_to_usize(value: i64, field: &str) -> Result<usize, ApplicationError> {
    usize::try_from(value).map_err(|_| {
        ApplicationError::TextUnitIndexFailed(format!(
            "persisted {field} value {value} is outside usize range"
        ))
    })
}

#[derive(Serialize, Deserialize)]
struct StoredDocument {
    #[serde(default)]
    normalization_version: Option<String>,
    id: String,
    source: String,
    title: String,
    media_type: String,
    content_hash: String,
    metadata: BTreeMap<String, String>,
    root_sections: Vec<StoredSection>,
}

impl StoredDocument {
    fn from_document(document: &Document) -> Self {
        Self {
            normalization_version: Some(NORMALIZATION_VERSION.into()),
            id: document.id.0.clone(),
            source: document.source.0.clone(),
            title: document.title.clone(),
            media_type: document.media_type.0.clone(),
            content_hash: document.content_hash.0.clone(),
            metadata: document.metadata.clone(),
            root_sections: document
                .root_sections
                .iter()
                .map(StoredSection::from_section)
                .collect(),
        }
    }

    fn into_document(self) -> Result<Document, ApplicationError> {
        let persisted_version = self.normalization_version.ok_or_else(|| {
            ApplicationError::StaleDocument(format!(
                "persisted document has no normalization version; current version is {NORMALIZATION_VERSION}; explicit source reopen required"
            ))
        })?;
        if persisted_version != NORMALIZATION_VERSION {
            return Err(ApplicationError::StaleDocument(format!(
                "persisted document normalization version {persisted_version} is stale; current version is {NORMALIZATION_VERSION}; explicit source reopen required"
            )));
        }

        Ok(Document {
            id: DocumentId(self.id),
            source: DocumentSource(self.source),
            title: self.title,
            media_type: MediaType(self.media_type),
            content_hash: ContentHash(self.content_hash),
            metadata: self.metadata,
            root_sections: self
                .root_sections
                .into_iter()
                .map(StoredSection::into_section)
                .collect(),
        })
    }
}

#[derive(Serialize, Deserialize)]
struct StoredSection {
    id: String,
    parent_id: Option<String>,
    title: String,
    level: u8,
    content: String,
    location: StoredLocation,
    children: Vec<StoredSection>,
}

impl StoredSection {
    fn from_section(section: &Section) -> Self {
        Self {
            id: section.id.0.clone(),
            parent_id: section.parent_id.as_ref().map(|value| value.0.clone()),
            title: section.title.clone(),
            level: section.level,
            content: section.content.clone(),
            location: StoredLocation::from_location(&section.location),
            children: section
                .children
                .iter()
                .map(StoredSection::from_section)
                .collect(),
        }
    }

    fn into_section(self) -> Section {
        Section {
            id: SectionId(self.id),
            parent_id: self.parent_id.map(SectionId),
            title: self.title,
            level: self.level,
            content: self.content,
            location: self.location.into_location(),
            children: self
                .children
                .into_iter()
                .map(StoredSection::into_section)
                .collect(),
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
struct StoredLocation {
    page: Option<u32>,
    chapter: Option<String>,
    section_path: Vec<String>,
    anchor: Option<String>,
    paragraph: Option<u32>,
    char_start: Option<usize>,
    char_end: Option<usize>,
    native_location: Option<String>,
}

impl StoredLocation {
    fn from_location(location: &Location) -> Self {
        Self {
            page: location.page,
            chapter: location.chapter.clone(),
            section_path: location.section_path.clone(),
            anchor: location.anchor.clone(),
            paragraph: location.paragraph,
            char_start: location.char_start,
            char_end: location.char_end,
            native_location: location.native_location.clone(),
        }
    }

    fn into_location(self) -> Location {
        Location {
            page: self.page,
            chapter: self.chapter,
            section_path: self.section_path,
            anchor: self.anchor,
            paragraph: self.paragraph,
            char_start: self.char_start,
            char_end: self.char_end,
            native_location: self.native_location,
        }
    }
}

#[derive(Serialize, Deserialize)]
struct StoredParagraphCoverage {
    owner_section_id: String,
    owner_chars: usize,
    paragraph_chars: usize,
    separator_chars: usize,
    paragraph_count: usize,
    native_paragraph_chars: usize,
    native_structural_container_chars: usize,
    native_non_prose_chars: usize,
    fallback_chars: usize,
}

impl StoredParagraphCoverage {
    fn from_coverage(coverage: &ParagraphSectionCoverage) -> Self {
        Self {
            owner_section_id: coverage.owner_section_id.0.clone(),
            owner_chars: coverage.owner_chars,
            paragraph_chars: coverage.paragraph_chars,
            separator_chars: coverage.separator_chars,
            paragraph_count: coverage.paragraph_count,
            native_paragraph_chars: coverage.native_paragraph_chars,
            native_structural_container_chars: coverage.native_structural_container_chars,
            native_non_prose_chars: coverage.native_non_prose_chars,
            fallback_chars: coverage.fallback_chars,
        }
    }

    fn into_coverage(self) -> ParagraphSectionCoverage {
        ParagraphSectionCoverage {
            owner_section_id: SectionId(self.owner_section_id),
            owner_chars: self.owner_chars,
            paragraph_chars: self.paragraph_chars,
            separator_chars: self.separator_chars,
            paragraph_count: self.paragraph_count,
            native_paragraph_chars: self.native_paragraph_chars,
            native_structural_container_chars: self.native_structural_container_chars,
            native_non_prose_chars: self.native_non_prose_chars,
            fallback_chars: self.fallback_chars,
        }
    }
}
