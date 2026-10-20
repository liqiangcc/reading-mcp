use std::collections::BTreeMap;
use std::collections::HashMap;
use std::collections::VecDeque;
use std::collections::hash_map::Entry;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use super::sqlite_support::{open_connection, run_sqlite};
use crate::application::get_text_units::{
    EffectiveTextUnitKind, RequestedTextUnitKind, TextUnitCoveragePolicy, TextUnitDirection,
    TextUnitEnumerationCoverage,
};
use crate::application::open_document::OpenDocumentResult;
use crate::application::ports::{
    ApplicationError, DocumentRepository, OpenResultStore, OwnedStreamUnit, SectionTextUnits,
    StoredStreamItem, TextUnitIndex, TextUnitStreamKey, TextUnitStreamSummary,
};
use crate::application::text_unit_stream::{StreamUnit, plan_section_stream};
use crate::domain::{
    ContentHash, Document, DocumentId, DocumentSource, Location, MediaType, NORMALIZATION_VERSION,
    NormalizedDocumentHash, NormalizedTextRange, ParagraphContentClass, ParagraphSectionCoverage,
    ParagraphTextUnitSet, Section, SectionId, SentenceEligibility, SentenceParagraphCoverage,
    SentenceTextUnit, SentenceTextUnitSet, TEXT_SEGMENTATION_VERSION, TextUnit, TextUnitId,
    TextUnitKind, paragraph_text_unit_id, sentence_text_unit_id,
};

/// How many decoded documents `SqliteDocumentRepository` keeps in memory.
const DOCUMENT_CACHE_CAPACITY: usize = 8;

/// Most recently used decoded documents, front first. `data_version` is `PRAGMA data_version` at the time the
/// entries were loaded: SQLite changes it when another connection commits, so a new value means they may be stale.
#[derive(Default)]
struct DocumentCache {
    data_version: Option<i64>,
    entries: VecDeque<(DocumentId, Arc<Document>)>,
}

impl DocumentCache {
    fn sync_version(&mut self, data_version: i64) {
        if self.data_version != Some(data_version) {
            self.entries.clear();
            self.data_version = Some(data_version);
        }
    }

    fn get(&mut self, id: &DocumentId) -> Option<Arc<Document>> {
        let index = self.entries.iter().position(|(cached, _)| cached == id)?;
        let entry = self.entries.remove(index)?;
        let document = Arc::clone(&entry.1);
        self.entries.push_front(entry);
        Some(document)
    }

    fn insert(&mut self, id: DocumentId, document: Arc<Document>) {
        self.remove(&id);
        self.entries.push_front((id, document));
        self.entries.truncate(DOCUMENT_CACHE_CAPACITY);
    }

    fn remove(&mut self, id: &DocumentId) {
        self.entries.retain(|(cached, _)| cached != id);
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
        let json = serde_json::to_string(&StoredDocument::from_document(&document))
            .map_err(|error| ApplicationError::RepositoryFailed(error.to_string()))?;
        let cache = Arc::clone(&self.cache);
        run_sqlite(
            &self.connection,
            ApplicationError::RepositoryFailed,
            move |connection| {
                connection
                    .execute(
                        "INSERT INTO documents(id, source, content_hash, document_json)
                         VALUES (?1, ?2, ?3, ?4)
                         ON CONFLICT(id) DO UPDATE SET
                            source = excluded.source,
                            content_hash = excluded.content_hash,
                            document_json = excluded.document_json",
                        params![
                            document.id.0,
                            document.source.0,
                            document.content_hash.0,
                            json
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
                if let Some(document) = cache.get(&id) {
                    return Ok(Some(document));
                }
                let json = connection
                    .query_row(
                        "SELECT document_json FROM documents WHERE id = ?1",
                        params![&id.0],
                        |row| row.get::<_, String>(0),
                    )
                    .optional()
                    .map_err(repository_error)?;
                json.map(|json| {
                    let stored = serde_json::from_str::<StoredDocument>(&json)
                        .map_err(|error| ApplicationError::RepositoryFailed(error.to_string()))?;
                    let document = stored.into_document()?;
                    document
                        .validate_ocr_publication()
                        .map_err(ApplicationError::RepositoryFailed)?;
                    let document = Arc::new(document);
                    cache.insert(id.clone(), Arc::clone(&document));
                    Ok(document)
                })
                .transpose()
            },
        )
        .await
    }
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
        connection
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS text_unit_documents_v2 (
                    doc_key INTEGER PRIMARY KEY,
                    document_id TEXT NOT NULL UNIQUE,
                    content_hash TEXT NOT NULL,
                    normalized_document_hash TEXT NOT NULL,
                    segmentation_version TEXT NOT NULL
                 );
                 CREATE TABLE IF NOT EXISTS text_unit_sections_v2 (
                    doc_key INTEGER NOT NULL,
                    section_key INTEGER NOT NULL,
                    section_id TEXT NOT NULL,
                    paragraph_coverage_json TEXT,
                    PRIMARY KEY (doc_key, section_key)
                 ) WITHOUT ROWID;
                 CREATE UNIQUE INDEX IF NOT EXISTS idx_text_unit_sections_v2_id
                    ON text_unit_sections_v2(doc_key, section_id);
                 CREATE TABLE IF NOT EXISTS text_unit_items_v2 (
                    doc_key INTEGER NOT NULL,
                    kind INTEGER NOT NULL,
                    source_order INTEGER NOT NULL,
                    section_key INTEGER NOT NULL,
                    paragraph_index INTEGER NOT NULL,
                    sentence_index INTEGER NOT NULL,
                    range_start INTEGER NOT NULL,
                    range_end INTEGER NOT NULL,
                    parent_start INTEGER,
                    parent_end INTEGER,
                    text TEXT NOT NULL,
                    PRIMARY KEY (doc_key, kind, source_order)
                 ) WITHOUT ROWID;
                 CREATE INDEX IF NOT EXISTS idx_text_unit_items_v2_unit
                    ON text_unit_items_v2(doc_key, section_key, kind, paragraph_index, sentence_index);
                 CREATE TABLE IF NOT EXISTS text_unit_sentence_coverage_v2 (
                    doc_key INTEGER NOT NULL,
                    section_key INTEGER NOT NULL,
                    position INTEGER NOT NULL,
                    paragraph_index INTEGER NOT NULL,
                    paragraph_start INTEGER NOT NULL,
                    paragraph_end INTEGER NOT NULL,
                    content_class INTEGER NOT NULL,
                    eligibility INTEGER NOT NULL,
                    paragraph_chars INTEGER NOT NULL,
                    sentence_chars INTEGER NOT NULL,
                    separator_chars INTEGER NOT NULL,
                    coarse_only_chars INTEGER NOT NULL,
                    sentence_count INTEGER NOT NULL,
                    PRIMARY KEY (doc_key, section_key, position)
                 ) WITHOUT ROWID;
                 CREATE TABLE IF NOT EXISTS text_unit_streams_v2 (
                    doc_key INTEGER NOT NULL,
                    section_key INTEGER NOT NULL,
                    requested_kind INTEGER NOT NULL,
                    position INTEGER NOT NULL,
                    eligible_position INTEGER,
                    unit_kind INTEGER NOT NULL,
                    unit_source_order INTEGER NOT NULL,
                    content_class INTEGER NOT NULL,
                    degradation INTEGER,
                    char_count INTEGER NOT NULL,
                    PRIMARY KEY (doc_key, section_key, requested_kind, position)
                 ) WITHOUT ROWID;
                 CREATE UNIQUE INDEX IF NOT EXISTS idx_text_unit_streams_v2_eligible
                    ON text_unit_streams_v2(doc_key, section_key, requested_kind, eligible_position)
                    WHERE eligible_position IS NOT NULL;
                 CREATE INDEX IF NOT EXISTS idx_text_unit_streams_v2_unit
                    ON text_unit_streams_v2(doc_key, section_key, requested_kind, unit_kind, unit_source_order);
                 CREATE TABLE IF NOT EXISTS text_unit_stream_summaries_v2 (
                    doc_key INTEGER NOT NULL,
                    section_key INTEGER NOT NULL,
                    requested_kind INTEGER NOT NULL,
                    coverage_policy INTEGER NOT NULL,
                    total_items INTEGER NOT NULL,
                    coverage_json TEXT NOT NULL,
                    PRIMARY KEY (doc_key, section_key, requested_kind, coverage_policy)
                 ) WITHOUT ROWID;",
            )
            .map_err(text_unit_index_error)?;
        Ok(Self {
            path,
            connection: Arc::new(Mutex::new(connection)),
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
        run_sqlite(
            &self.connection,
            ApplicationError::TextUnitIndexFailed,
            move |connection| {
                if let Some(meta) = load_stored_document(connection, &document_id)? {
                    return paragraph_units_from_v2(connection, &meta);
                }
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
                    let normalized_range = NormalizedTextRange::new(range_start, range_end)
                        .map_err(|error| {
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
            },
        )
        .await
    }

    async fn replace_document_sets(
        &self,
        document_id: &DocumentId,
        paragraphs: Arc<ParagraphTextUnitSet>,
        sentences: Arc<SentenceTextUnitSet>,
    ) -> Result<(), ApplicationError> {
        validate_paragraph_units(document_id, &paragraphs.units)?;

        let document_id = document_id.clone();
        run_sqlite(
            &self.connection,
            ApplicationError::TextUnitIndexFailed,
            move |connection| {
                let transaction = connection.transaction().map_err(text_unit_index_error)?;

                let content_hash = paragraphs
                    .units
                    .first()
                    .map(|unit| unit.content_hash.0.as_str())
                    .or_else(|| {
                        sentences
                            .units
                            .first()
                            .map(|unit| unit.content_hash.0.as_str())
                    })
                    .unwrap_or("");
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
                            &document_id.0,
                            content_hash,
                            paragraphs.normalized_document_hash.as_ref(),
                            TEXT_SEGMENTATION_VERSION,
                        ],
                    )
                    .map_err(text_unit_index_error)?;
                let doc_key: i64 = transaction
                    .query_row(
                        "SELECT doc_key FROM text_unit_documents_v2 WHERE document_id = ?1",
                        params![&document_id.0],
                        |row| row.get(0),
                    )
                    .map_err(text_unit_index_error)?;

                for table in [
                    "text_unit_sections_v2",
                    "text_unit_items_v2",
                    "text_unit_sentence_coverage_v2",
                    "text_unit_streams_v2",
                    "text_unit_stream_summaries_v2",
                ] {
                    transaction
                        .execute(
                            &format!("DELETE FROM {table} WHERE doc_key = ?1"),
                            params![doc_key],
                        )
                        .map_err(text_unit_index_error)?;
                }

                // Section keys are assigned in first-seen order: paragraph coverages, paragraph units,
                // sentence units, then sentence coverages.
                let mut section_keys: HashMap<SectionId, i64> = HashMap::new();
                let mut section_rows: Vec<(SectionId, i64, Option<String>)> = Vec::new();
                let mut next_section_key = 0i64;
                for coverage in &paragraphs.coverage {
                    match section_keys.entry(coverage.owner_section_id.clone()) {
                        Entry::Occupied(_) => continue,
                        Entry::Vacant(entry) => {
                            let coverage_json = serde_json::to_string(
                                &StoredParagraphCoverage::from_coverage(coverage),
                            )
                            .map_err(|error| {
                                ApplicationError::TextUnitIndexFailed(error.to_string())
                            })?;
                            entry.insert(next_section_key);
                            section_rows.push((
                                coverage.owner_section_id.clone(),
                                next_section_key,
                                Some(coverage_json),
                            ));
                            next_section_key += 1;
                        }
                    }
                }
                for section_id in paragraphs
                    .units
                    .iter()
                    .map(|unit| &unit.owner_section_id)
                    .chain(sentences.units.iter().map(|unit| &unit.owner_section_id))
                    .chain(
                        sentences
                            .coverage
                            .iter()
                            .map(|coverage| &coverage.owner_section_id),
                    )
                {
                    match section_keys.entry(section_id.clone()) {
                        Entry::Occupied(_) => continue,
                        Entry::Vacant(entry) => {
                            entry.insert(next_section_key);
                            section_rows.push((section_id.clone(), next_section_key, None));
                            next_section_key += 1;
                        }
                    }
                }
                {
                    let mut statement = transaction
                        .prepare(
                            "INSERT INTO text_unit_sections_v2(
                                doc_key,
                                section_key,
                                section_id,
                                paragraph_coverage_json
                             ) VALUES (?1, ?2, ?3, ?4)",
                        )
                        .map_err(text_unit_index_error)?;
                    for (section_id, section_key, coverage_json) in &section_rows {
                        statement
                            .execute(params![doc_key, section_key, &section_id.0, coverage_json])
                            .map_err(text_unit_index_error)?;
                    }
                }

                // Paragraph ranges, by section key and 1-based paragraph index, for recomputing ids of
                // sentences and of sentence coverage rows.
                let paragraph_ranges: HashMap<(i64, usize), NormalizedTextRange> = paragraphs
                    .units
                    .iter()
                    .map(|unit| {
                        (
                            (section_keys[&unit.owner_section_id], unit.paragraph_index),
                            unit.normalized_range,
                        )
                    })
                    .collect();

                {
                    let mut statement = transaction
                        .prepare(
                            "INSERT INTO text_unit_items_v2(
                                doc_key,
                                kind,
                                source_order,
                                section_key,
                                paragraph_index,
                                sentence_index,
                                range_start,
                                range_end,
                                parent_start,
                                parent_end,
                                text
                             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                        )
                        .map_err(text_unit_index_error)?;

                    for unit in &paragraphs.units {
                        let section_key = section_keys[&unit.owner_section_id];
                        statement
                            .execute(params![
                                doc_key,
                                UNIT_KIND_PARAGRAPH,
                                usize_to_i64(unit.source_order)?,
                                section_key,
                                usize_to_i64(unit.paragraph_index)?,
                                0i64,
                                usize_to_i64(unit.normalized_range.start())?,
                                usize_to_i64(unit.normalized_range.end())?,
                                Option::<i64>::None,
                                Option::<i64>::None,
                                &unit.text,
                            ])
                            .map_err(text_unit_index_error)?;
                    }

                    for unit in &sentences.units {
                        let section_key = section_keys[&unit.owner_section_id];
                        let parent = paragraph_ranges
                            .get(&(section_key, unit.paragraph_index))
                            .ok_or_else(|| {
                                ApplicationError::TextUnitIndexFailed(format!(
                                    "sentence {} has no persisted parent paragraph range",
                                    unit.id.0
                                ))
                            })?;
                        statement
                            .execute(params![
                                doc_key,
                                UNIT_KIND_SENTENCE,
                                usize_to_i64(unit.source_order)?,
                                section_key,
                                usize_to_i64(unit.paragraph_index)?,
                                usize_to_i64(unit.sentence_index)?,
                                usize_to_i64(unit.normalized_range.start())?,
                                usize_to_i64(unit.normalized_range.end())?,
                                usize_to_i64(parent.start())?,
                                usize_to_i64(parent.end())?,
                                &unit.text,
                            ])
                            .map_err(text_unit_index_error)?;
                    }
                }

                {
                    let mut statement = transaction
                        .prepare(
                            "INSERT INTO text_unit_sentence_coverage_v2(
                                doc_key,
                                section_key,
                                position,
                                paragraph_index,
                                paragraph_start,
                                paragraph_end,
                                content_class,
                                eligibility,
                                paragraph_chars,
                                sentence_chars,
                                separator_chars,
                                coarse_only_chars,
                                sentence_count
                             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
                        )
                        .map_err(text_unit_index_error)?;

                    let mut positions: HashMap<SectionId, i64> = HashMap::new();
                    for coverage in &sentences.coverage {
                        let section_key = section_keys[&coverage.owner_section_id];
                        let position = positions
                            .entry(coverage.owner_section_id.clone())
                            .or_insert(0);
                        let paragraph = paragraph_ranges
                            .get(&(section_key, coverage.paragraph_index))
                            .ok_or_else(|| {
                                ApplicationError::TextUnitIndexFailed(format!(
                                    "sentence coverage {} has no persisted paragraph range",
                                    coverage.paragraph_id.0
                                ))
                            })?;
                        statement
                            .execute(params![
                                doc_key,
                                section_key,
                                *position,
                                usize_to_i64(coverage.paragraph_index)?,
                                usize_to_i64(paragraph.start())?,
                                usize_to_i64(paragraph.end())?,
                                content_class_code(coverage.content_class),
                                sentence_eligibility_code(coverage.eligibility),
                                usize_to_i64(coverage.paragraph_chars)?,
                                usize_to_i64(coverage.sentence_chars)?,
                                usize_to_i64(coverage.separator_chars)?,
                                usize_to_i64(coverage.coarse_only_chars)?,
                                usize_to_i64(coverage.sentence_count)?,
                            ])
                            .map_err(text_unit_index_error)?;
                        *position += 1;
                    }
                }

                // Group the section's units and coverages in one pass, then plan and store each section's
                // stream. Never call `SectionTextUnits::from_sets` per section (that would be O(units x
                // sections)) and never clone unit text.
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
                    let mut stream_statement = transaction
                        .prepare(
                            "INSERT INTO text_unit_streams_v2(
                                doc_key,
                                section_key,
                                requested_kind,
                                position,
                                eligible_position,
                                unit_kind,
                                unit_source_order,
                                content_class,
                                degradation,
                                char_count
                             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                        )
                        .map_err(text_unit_index_error)?;
                    let mut summary_statement = transaction
                        .prepare(
                            "INSERT INTO text_unit_stream_summaries_v2(
                                doc_key,
                                section_key,
                                requested_kind,
                                coverage_policy,
                                total_items,
                                coverage_json
                             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                        )
                        .map_err(text_unit_index_error)?;

                    for (section_id, group) in &groups {
                        let Some(_) = group.paragraph_coverage else {
                            continue;
                        };
                        let section_key = section_keys[section_id];
                        for requested_kind in [
                            RequestedTextUnitKind::Paragraph,
                            RequestedTextUnitKind::Sentence,
                        ] {
                            let plan = plan_section_stream(
                                section_id,
                                &group.paragraphs,
                                group.paragraph_coverage,
                                &group.sentences,
                                &group.sentence_coverage,
                                requested_kind,
                            )
                            .map_err(|error| {
                                ApplicationError::TextUnitIndexFailed(format!(
                                    "cannot plan the text unit stream of section {}: {error}",
                                    section_id.0
                                ))
                            })?;

                            let mut eligible_position = 0i64;
                            for (position, entry) in plan.entries.iter().enumerate() {
                                let eligible_position_value = if entry.eligible {
                                    Some(eligible_position)
                                } else {
                                    None
                                };
                                if entry.eligible {
                                    eligible_position += 1;
                                }
                                stream_statement
                                    .execute(params![
                                        doc_key,
                                        section_key,
                                        requested_kind_code(requested_kind),
                                        usize_to_i64(position)?,
                                        eligible_position_value,
                                        unit_kind_code(entry.unit),
                                        usize_to_i64(entry.unit.source_order())?,
                                        content_class_code(entry.content_class),
                                        entry.degradation.map(degradation_code).transpose()?,
                                        usize_to_i64(entry.char_count)?,
                                    ])
                                    .map_err(text_unit_index_error)?;
                            }

                            for (policy, coverage, total_items) in [
                                (
                                    TextUnitCoveragePolicy::PreserveSource,
                                    &plan.preserve_source,
                                    plan.entries.len(),
                                ),
                                (
                                    TextUnitCoveragePolicy::EligibleOnly,
                                    &plan.eligible_only,
                                    eligible_position as usize,
                                ),
                            ] {
                                let coverage_json = serde_json::to_string(
                                    &StoredStreamCoverage::from_coverage(coverage),
                                )
                                .map_err(|error| {
                                    ApplicationError::TextUnitIndexFailed(error.to_string())
                                })?;
                                summary_statement
                                    .execute(params![
                                        doc_key,
                                        section_key,
                                        requested_kind_code(requested_kind),
                                        coverage_policy_code(policy),
                                        usize_to_i64(total_items)?,
                                        coverage_json,
                                    ])
                                    .map_err(text_unit_index_error)?;
                            }
                        }
                    }
                }

                transaction.commit().map_err(text_unit_index_error)?;
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
                let Some(section) = load_section(connection, meta.doc_key, &section_id)? else {
                    return Ok(Some(SectionTextUnits {
                        paragraphs: Vec::new(),
                        paragraph_coverage: None,
                        sentences: Vec::new(),
                        sentence_coverage: Vec::new(),
                    }));
                };

                let paragraph_coverage = section
                    .paragraph_coverage_json
                    .map(
                        |json| -> Result<ParagraphSectionCoverage, ApplicationError> {
                            let stored: StoredParagraphCoverage = serde_json::from_str(&json)
                                .map_err(|error| {
                                    ApplicationError::TextUnitIndexFailed(error.to_string())
                                })?;
                            Ok(stored.into_coverage())
                        },
                    )
                    .transpose()?;

                let mut paragraphs = Vec::new();
                let mut sentences = Vec::new();
                {
                    let mut statement = connection
                        .prepare(
                            "SELECT kind, source_order, paragraph_index, sentence_index,
                                    range_start, range_end, parent_start, parent_end, text
                             FROM text_unit_items_v2
                             WHERE doc_key = ?1 AND section_key = ?2
                             ORDER BY kind, source_order",
                        )
                        .map_err(text_unit_index_error)?;
                    let rows = statement
                        .query_map(params![meta.doc_key, section.section_key], |row| {
                            Ok((
                                row.get::<_, i64>(0)?,
                                row.get::<_, i64>(1)?,
                                row.get::<_, i64>(2)?,
                                row.get::<_, i64>(3)?,
                                row.get::<_, i64>(4)?,
                                row.get::<_, i64>(5)?,
                                row.get::<_, Option<i64>>(6)?,
                                row.get::<_, Option<i64>>(7)?,
                                row.get::<_, String>(8)?,
                            ))
                        })
                        .map_err(text_unit_index_error)?;
                    for row in rows {
                        let (
                            kind,
                            source_order,
                            paragraph_index,
                            sentence_index,
                            range_start,
                            range_end,
                            parent_start,
                            parent_end,
                            text,
                        ) = row.map_err(text_unit_index_error)?;
                        match unit_kind_from_code(kind)? {
                            UNIT_KIND_PARAGRAPH => paragraphs.push(rebuild_paragraph_unit(
                                &meta,
                                &section_id,
                                paragraph_index,
                                source_order,
                                range_start,
                                range_end,
                                text,
                            )?),
                            UNIT_KIND_SENTENCE => sentences.push(rebuild_sentence_unit(
                                &meta,
                                &section_id,
                                paragraph_index,
                                sentence_index,
                                source_order,
                                range_start,
                                range_end,
                                parent_start,
                                parent_end,
                                text,
                            )?),
                            _ => unreachable!("unit kind codes are validated"),
                        }
                    }
                }

                let mut sentence_coverage = Vec::new();
                {
                    let mut statement = connection
                        .prepare(
                            "SELECT paragraph_index, paragraph_start, paragraph_end, content_class,
                                    eligibility, paragraph_chars, sentence_chars, separator_chars,
                                    coarse_only_chars, sentence_count
                             FROM text_unit_sentence_coverage_v2
                             WHERE doc_key = ?1 AND section_key = ?2
                             ORDER BY position",
                        )
                        .map_err(text_unit_index_error)?;
                    let rows = statement
                        .query_map(params![meta.doc_key, section.section_key], |row| {
                            Ok((
                                row.get::<_, i64>(0)?,
                                row.get::<_, i64>(1)?,
                                row.get::<_, i64>(2)?,
                                row.get::<_, i64>(3)?,
                                row.get::<_, i64>(4)?,
                                row.get::<_, i64>(5)?,
                                row.get::<_, i64>(6)?,
                                row.get::<_, i64>(7)?,
                                row.get::<_, i64>(8)?,
                                row.get::<_, i64>(9)?,
                            ))
                        })
                        .map_err(text_unit_index_error)?;
                    for row in rows {
                        let (
                            paragraph_index,
                            paragraph_start,
                            paragraph_end,
                            content_class,
                            eligibility,
                            paragraph_chars,
                            sentence_chars,
                            separator_chars,
                            coarse_only_chars,
                            sentence_count,
                        ) = row.map_err(text_unit_index_error)?;
                        let paragraph_index = i64_to_usize(paragraph_index, "paragraph_index")?;
                        let paragraph_range = stored_normalized_range(
                            i64_to_usize(paragraph_start, "paragraph_start")?,
                            i64_to_usize(paragraph_end, "paragraph_end")?,
                        )?;
                        sentence_coverage.push(SentenceParagraphCoverage {
                            owner_section_id: section_id.clone(),
                            paragraph_id: paragraph_text_unit_id(
                                &meta.document_id,
                                &meta.normalized_document_hash,
                                &section_id,
                                paragraph_index,
                                paragraph_range,
                            ),
                            paragraph_index,
                            content_class: content_class_from_code(content_class)?,
                            eligibility: sentence_eligibility_from_code(eligibility)?,
                            paragraph_chars: i64_to_usize(paragraph_chars, "paragraph_chars")?,
                            sentence_chars: i64_to_usize(sentence_chars, "sentence_chars")?,
                            separator_chars: i64_to_usize(separator_chars, "separator_chars")?,
                            coarse_only_chars: i64_to_usize(
                                coarse_only_chars,
                                "coarse_only_chars",
                            )?,
                            sentence_count: i64_to_usize(sentence_count, "sentence_count")?,
                        });
                    }
                }

                Ok(Some(SectionTextUnits {
                    paragraphs,
                    paragraph_coverage,
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
                let Some(section) = load_section(connection, meta.doc_key, &section_id)? else {
                    return Err(stale_text_units(&document_id));
                };

                let has_paragraph_stream = connection
                    .query_row(
                        "SELECT 1
                         FROM text_unit_stream_summaries_v2
                         WHERE doc_key = ?1
                           AND section_key = ?2
                           AND requested_kind = ?3
                           AND coverage_policy = ?4",
                        params![
                            meta.doc_key,
                            section.section_key,
                            requested_kind_code(RequestedTextUnitKind::Paragraph),
                            coverage_policy_code(TextUnitCoveragePolicy::PreserveSource)
                        ],
                        |_| Ok(()),
                    )
                    .optional()
                    .map_err(text_unit_index_error)?
                    .is_some();
                if !has_paragraph_stream {
                    return Err(stale_text_units(&document_id));
                }

                let paragraph_index = usize_to_i64(paragraph_index)?;
                let sentence_index = sentence_index.map(usize_to_i64).transpose()?.unwrap_or(0);
                let found = connection
                    .query_row(
                        "SELECT source_order, paragraph_index, sentence_index, range_start,
                                range_end, parent_start, parent_end, text
                         FROM text_unit_items_v2
                         WHERE doc_key = ?1
                           AND section_key = ?2
                           AND kind = ?3
                           AND paragraph_index = ?4
                           AND sentence_index = ?5
                         LIMIT 1",
                        params![
                            meta.doc_key,
                            section.section_key,
                            effective_unit_kind_code(unit_kind),
                            paragraph_index,
                            sentence_index,
                        ],
                        |row| {
                            Ok((
                                row.get::<_, i64>(0)?,
                                row.get::<_, i64>(1)?,
                                row.get::<_, i64>(2)?,
                                row.get::<_, i64>(3)?,
                                row.get::<_, i64>(4)?,
                                row.get::<_, Option<i64>>(5)?,
                                row.get::<_, Option<i64>>(6)?,
                                row.get::<_, String>(7)?,
                            ))
                        },
                    )
                    .optional()
                    .map_err(text_unit_index_error)?;
                let Some((
                    source_order,
                    paragraph_index,
                    sentence_index,
                    range_start,
                    range_end,
                    parent_start,
                    parent_end,
                    text,
                )) = found
                else {
                    return Ok(None);
                };
                let unit = match unit_kind {
                    EffectiveTextUnitKind::Paragraph => {
                        OwnedStreamUnit::Paragraph(rebuild_paragraph_unit(
                            &meta,
                            &section_id,
                            paragraph_index,
                            source_order,
                            range_start,
                            range_end,
                            text,
                        )?)
                    }
                    EffectiveTextUnitKind::Sentence => {
                        OwnedStreamUnit::Sentence(rebuild_sentence_unit(
                            &meta,
                            &section_id,
                            paragraph_index,
                            sentence_index,
                            source_order,
                            range_start,
                            range_end,
                            parent_start,
                            parent_end,
                            text,
                        )?)
                    }
                };
                Ok(Some(unit))
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

                let section = load_section(connection, meta.doc_key, &section_id)?;
                let summary = match &section {
                    Some(section) => connection
                        .query_row(
                            "SELECT total_items, coverage_json
                             FROM text_unit_stream_summaries_v2
                             WHERE doc_key = ?1
                               AND section_key = ?2
                               AND requested_kind = ?3
                               AND coverage_policy = ?4",
                            params![
                                meta.doc_key,
                                section.section_key,
                                requested_kind_code(kind),
                                coverage_policy_code(policy)
                            ],
                            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
                        )
                        .optional()
                        .map_err(text_unit_index_error)?,
                    None => None,
                };
                let Some((total_items, coverage_json)) = summary else {
                    // A section without paragraph coverage was never written (it cannot be planned), so the
                    // planner path must report today's "paragraph coverage is unavailable" error. A section
                    // that has coverage but no summary row is stale storage: report it as missing so the
                    // caller asks for a reopen.
                    let has_coverage = section
                        .as_ref()
                        .is_some_and(|section| section.paragraph_coverage_json.is_some());
                    if has_coverage {
                        return Ok(None);
                    }
                    return Err(ApplicationError::InvalidRequest(format!(
                        "paragraph coverage is unavailable for section {}",
                        section_id.0
                    )));
                };

                let stored: StoredStreamCoverage = serde_json::from_str(&coverage_json)
                    .map_err(|error| ApplicationError::TextUnitIndexFailed(error.to_string()))?;
                Ok(Some(TextUnitStreamSummary {
                    total_items: i64_to_usize(total_items, "total_items")?,
                    coverage: stored.into_coverage(),
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
        run_sqlite(
            &self.connection,
            ApplicationError::TextUnitIndexFailed,
            move |connection| {
                let Some(meta) = load_stored_document(connection, &document_id)? else {
                    return Ok(Vec::new());
                };
                let Some(section) = load_section(connection, meta.doc_key, &section_id)? else {
                    return Ok(Vec::new());
                };
                let position_column = match policy {
                    TextUnitCoveragePolicy::PreserveSource => "position",
                    TextUnitCoveragePolicy::EligibleOnly => "eligible_position",
                };
                let order = match direction {
                    TextUnitDirection::Forward => "ASC",
                    TextUnitDirection::Backward => "DESC",
                };
                let comparison = match direction {
                    TextUnitDirection::Forward => ">=",
                    TextUnitDirection::Backward => "<",
                };
                let sql = format!(
                    "SELECT s.{position_column}, s.unit_kind, s.content_class, s.degradation,
                            s.char_count, i.source_order, i.paragraph_index, i.sentence_index,
                            i.range_start, i.range_end, i.parent_start, i.parent_end, i.text
                     FROM text_unit_streams_v2 s
                     JOIN text_unit_items_v2 i
                       ON i.doc_key = s.doc_key
                      AND i.kind = s.unit_kind
                      AND i.source_order = s.unit_source_order
                     WHERE s.doc_key = ?1
                       AND s.section_key = ?2
                       AND s.requested_kind = ?3
                       AND s.{position_column} {comparison} ?4
                     ORDER BY s.{position_column} {order}
                     LIMIT ?5"
                );
                let position = usize_to_i64(position)?;
                let limit = usize_to_i64(limit)?;
                let mut statement = connection.prepare(&sql).map_err(text_unit_index_error)?;
                let rows = statement
                    .query_map(
                        params![
                            meta.doc_key,
                            section.section_key,
                            requested_kind_code(kind),
                            position,
                            limit
                        ],
                        |row| {
                            Ok((
                                row.get::<_, i64>(0)?,
                                row.get::<_, i64>(1)?,
                                row.get::<_, i64>(2)?,
                                row.get::<_, Option<i64>>(3)?,
                                row.get::<_, i64>(4)?,
                                row.get::<_, i64>(5)?,
                                row.get::<_, i64>(6)?,
                                row.get::<_, i64>(7)?,
                                row.get::<_, i64>(8)?,
                                row.get::<_, i64>(9)?,
                                row.get::<_, Option<i64>>(10)?,
                                row.get::<_, Option<i64>>(11)?,
                                row.get::<_, String>(12)?,
                            ))
                        },
                    )
                    .map_err(text_unit_index_error)?;

                let mut items = Vec::new();
                for row in rows {
                    let (
                        position,
                        unit_kind,
                        content_class,
                        degradation,
                        char_count,
                        source_order,
                        paragraph_index,
                        sentence_index,
                        range_start,
                        range_end,
                        parent_start,
                        parent_end,
                        text,
                    ) = row.map_err(text_unit_index_error)?;
                    let unit = match unit_kind_from_code(unit_kind)? {
                        UNIT_KIND_PARAGRAPH => OwnedStreamUnit::Paragraph(rebuild_paragraph_unit(
                            &meta,
                            &section_id,
                            paragraph_index,
                            source_order,
                            range_start,
                            range_end,
                            text,
                        )?),
                        UNIT_KIND_SENTENCE => OwnedStreamUnit::Sentence(rebuild_sentence_unit(
                            &meta,
                            &section_id,
                            paragraph_index,
                            sentence_index,
                            source_order,
                            range_start,
                            range_end,
                            parent_start,
                            parent_end,
                            text,
                        )?),
                        _ => unreachable!("unit kind codes are validated"),
                    };
                    items.push(StoredStreamItem {
                        position: i64_to_usize(position, "stream position")?,
                        unit,
                        content_class: content_class_from_code(content_class)?,
                        degradation: degradation.map(degradation_from_code).transpose()?,
                        char_count: i64_to_usize(char_count, "char_count")?,
                    });
                }
                if direction == TextUnitDirection::Backward {
                    items.reverse();
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
        run_sqlite(
            &self.connection,
            ApplicationError::TextUnitIndexFailed,
            move |connection| {
                let Some(meta) = load_stored_document(connection, &document_id)? else {
                    return Ok(None);
                };
                let Some(section) = load_section(connection, meta.doc_key, &section_id)? else {
                    return Ok(None);
                };
                let position_column = match policy {
                    TextUnitCoveragePolicy::PreserveSource => "position",
                    TextUnitCoveragePolicy::EligibleOnly => "eligible_position",
                };
                let sql = format!(
                    "SELECT s.{position_column}
                     FROM text_unit_streams_v2 s
                     WHERE s.doc_key = ?1
                       AND s.section_key = ?2
                       AND s.requested_kind = ?3
                       AND s.unit_kind = ?4
                       AND s.paragraph_index = ?5
                       AND s.sentence_index = ?6"
                );
                let paragraph_index = usize_to_i64(paragraph_index)?;
                let sentence_index = sentence_index.map(usize_to_i64).transpose()?.unwrap_or(0);
                let found = connection
                    .query_row(
                        &sql,
                        params![
                            meta.doc_key,
                            section.section_key,
                            requested_kind_code(kind),
                            effective_unit_kind_code(unit_kind),
                            paragraph_index,
                            sentence_index
                        ],
                        |row| row.get::<_, Option<i64>>(0),
                    )
                    .optional()
                    .map_err(text_unit_index_error)?;
                match found {
                    Some(Some(position)) => Ok(Some(i64_to_usize(position, "stream position")?)),
                    _ => Ok(None),
                }
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

struct StoredUnitSection {
    section_key: i64,
    paragraph_coverage_json: Option<String>,
}

fn load_stored_document(
    connection: &Connection,
    document_id: &DocumentId,
) -> Result<Option<StoredUnitDocument>, ApplicationError> {
    connection
        .query_row(
            "SELECT doc_key, document_id, content_hash, normalized_document_hash, segmentation_version
             FROM text_unit_documents_v2
             WHERE document_id = ?1",
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

fn load_section(
    connection: &Connection,
    doc_key: i64,
    section_id: &SectionId,
) -> Result<Option<StoredUnitSection>, ApplicationError> {
    connection
        .query_row(
            "SELECT section_key, paragraph_coverage_json
             FROM text_unit_sections_v2
             WHERE doc_key = ?1 AND section_id = ?2",
            params![doc_key, &section_id.0],
            |row| {
                Ok(StoredUnitSection {
                    section_key: row.get(0)?,
                    paragraph_coverage_json: row.get(1)?,
                })
            },
        )
        .optional()
        .map_err(text_unit_index_error)
}

fn paragraph_units_from_v2(
    connection: &Connection,
    meta: &StoredUnitDocument,
) -> Result<Vec<TextUnit>, ApplicationError> {
    let mut statement = connection
        .prepare(
            "SELECT i.paragraph_index, i.source_order, i.range_start, i.range_end, i.text,
                    s.section_id
             FROM text_unit_items_v2 i
             JOIN text_unit_sections_v2 s
               ON s.doc_key = i.doc_key AND s.section_key = i.section_key
             WHERE i.doc_key = ?1 AND i.kind = ?2
             ORDER BY i.source_order ASC",
        )
        .map_err(text_unit_index_error)?;
    let rows = statement
        .query_map(params![meta.doc_key, UNIT_KIND_PARAGRAPH], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
            ))
        })
        .map_err(text_unit_index_error)?;
    let mut units = Vec::new();
    for row in rows {
        let (paragraph_index, source_order, range_start, range_end, text, section_id) =
            row.map_err(text_unit_index_error)?;
        units.push(rebuild_paragraph_unit(
            meta,
            &SectionId(section_id),
            paragraph_index,
            source_order,
            range_start,
            range_end,
            text,
        )?);
    }
    Ok(units)
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
            parent_start.ok_or_else(|| missing_parent_range())?,
            "parent_start",
        )?,
        i64_to_usize(
            parent_end.ok_or_else(|| missing_parent_range())?,
            "parent_end",
        )?,
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

// Persisted integer codes. These mappings are explicit and append-only.
//
// unit kind: 0 = paragraph, 1 = sentence
const UNIT_KIND_PARAGRAPH: i64 = 0;
const UNIT_KIND_SENTENCE: i64 = 1;

fn unit_kind_from_code(code: i64) -> Result<i64, ApplicationError> {
    match code {
        UNIT_KIND_PARAGRAPH | UNIT_KIND_SENTENCE => Ok(code),
        other => Err(ApplicationError::TextUnitIndexFailed(format!(
            "unsupported persisted text unit kind code {other}"
        ))),
    }
}

fn unit_kind_code(unit: StreamUnit<'_>) -> i64 {
    match unit {
        StreamUnit::Paragraph(_) => UNIT_KIND_PARAGRAPH,
        StreamUnit::Sentence(_) => UNIT_KIND_SENTENCE,
    }
}

fn effective_unit_kind_code(kind: EffectiveTextUnitKind) -> i64 {
    match kind {
        EffectiveTextUnitKind::Paragraph => UNIT_KIND_PARAGRAPH,
        EffectiveTextUnitKind::Sentence => UNIT_KIND_SENTENCE,
    }
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

// RequestedTextUnitKind: 0 paragraph, 1 sentence.
fn requested_kind_code(kind: RequestedTextUnitKind) -> i64 {
    match kind {
        RequestedTextUnitKind::Paragraph => 0,
        RequestedTextUnitKind::Sentence => 1,
    }
}

// TextUnitCoveragePolicy: 0 preserve_source, 1 eligible_only.
fn coverage_policy_code(policy: TextUnitCoveragePolicy) -> i64 {
    match policy {
        TextUnitCoveragePolicy::PreserveSource => 0,
        TextUnitCoveragePolicy::EligibleOnly => 1,
    }
}

// Degradation strings: 1 flat_native_container_no_nested_textunit_evidence,
// 2 requested_sentence_but_non_prose_is_paragraph_only,
// 3 requested_sentence_but_paragraph_is_coarse_only. NULL means no degradation.
fn degradation_code(value: &str) -> Result<i64, ApplicationError> {
    match value {
        "flat_native_container_no_nested_textunit_evidence" => Ok(1),
        "requested_sentence_but_non_prose_is_paragraph_only" => Ok(2),
        "requested_sentence_but_paragraph_is_coarse_only" => Ok(3),
        other => Err(ApplicationError::TextUnitIndexFailed(format!(
            "unsupported persisted degradation {other:?}"
        ))),
    }
}

fn degradation_from_code(code: i64) -> Result<String, ApplicationError> {
    match code {
        1 => Ok("flat_native_container_no_nested_textunit_evidence".to_string()),
        2 => Ok("requested_sentence_but_non_prose_is_paragraph_only".to_string()),
        3 => Ok("requested_sentence_but_paragraph_is_coarse_only".to_string()),
        other => Err(ApplicationError::TextUnitIndexFailed(format!(
            "unsupported persisted degradation code {other}"
        ))),
    }
}

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

#[derive(Serialize, Deserialize)]
struct StoredStreamCoverage {
    owner_chars: usize,
    section_separator_chars: usize,
    sentence_separator_chars: usize,
    paragraph_count: usize,
    sentence_eligible_paragraphs: usize,
    non_prose_paragraphs: usize,
    coarse_structural_paragraphs: usize,
    represented_paragraphs: usize,
    represented_sentences: usize,
    coarse_non_prose_items: usize,
    coarse_structural_items: usize,
    intentionally_skipped: usize,
    unsupported_gaps: usize,
    source_complete: bool,
}

impl StoredStreamCoverage {
    fn from_coverage(coverage: &TextUnitEnumerationCoverage) -> Self {
        Self {
            owner_chars: coverage.owner_chars,
            section_separator_chars: coverage.section_separator_chars,
            sentence_separator_chars: coverage.sentence_separator_chars,
            paragraph_count: coverage.paragraph_count,
            sentence_eligible_paragraphs: coverage.sentence_eligible_paragraphs,
            non_prose_paragraphs: coverage.non_prose_paragraphs,
            coarse_structural_paragraphs: coverage.coarse_structural_paragraphs,
            represented_paragraphs: coverage.represented_paragraphs,
            represented_sentences: coverage.represented_sentences,
            coarse_non_prose_items: coverage.coarse_non_prose_items,
            coarse_structural_items: coverage.coarse_structural_items,
            intentionally_skipped: coverage.intentionally_skipped,
            unsupported_gaps: coverage.unsupported_gaps,
            source_complete: coverage.source_complete,
        }
    }

    fn into_coverage(self) -> TextUnitEnumerationCoverage {
        TextUnitEnumerationCoverage {
            owner_chars: self.owner_chars,
            section_separator_chars: self.section_separator_chars,
            sentence_separator_chars: self.sentence_separator_chars,
            paragraph_count: self.paragraph_count,
            sentence_eligible_paragraphs: self.sentence_eligible_paragraphs,
            non_prose_paragraphs: self.non_prose_paragraphs,
            coarse_structural_paragraphs: self.coarse_structural_paragraphs,
            represented_paragraphs: self.represented_paragraphs,
            represented_sentences: self.represented_sentences,
            coarse_non_prose_items: self.coarse_non_prose_items,
            coarse_structural_items: self.coarse_structural_items,
            intentionally_skipped: self.intentionally_skipped,
            unsupported_gaps: self.unsupported_gaps,
            source_complete: self.source_complete,
        }
    }
}
