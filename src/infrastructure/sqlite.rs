use std::collections::BTreeMap;
use std::collections::HashMap;
use std::collections::VecDeque;
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
use crate::application::text_unit_stream::plan_section_stream;
use crate::domain::{
    ContentHash, Document, DocumentId, DocumentSource, Location, MediaType, NORMALIZATION_VERSION,
    NormalizedDocumentHash, NormalizedTextRange, ParagraphContentClass, ParagraphSectionCoverage,
    ParagraphTextUnitSet, Section, SectionId, SentenceEligibility, SentenceParagraphCoverage,
    SentenceTextUnit, SentenceTextUnitSet, TEXT_SEGMENTATION_VERSION, TextUnit, TextUnitId,
    TextUnitKind,
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
        connection
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS text_unit_sets_v1 (
                    document_id TEXT PRIMARY KEY,
                    segmentation_version TEXT NOT NULL
                 );
                 CREATE TABLE IF NOT EXISTS text_unit_sections_v1 (
                    document_id TEXT NOT NULL,
                    owner_section_id TEXT NOT NULL,
                    paragraph_coverage_json TEXT NOT NULL,
                    PRIMARY KEY (document_id, owner_section_id)
                 );
                 CREATE TABLE IF NOT EXISTS text_unit_items_v1 (
                    document_id TEXT NOT NULL,
                    owner_section_id TEXT NOT NULL,
                    kind TEXT NOT NULL,
                    source_order INTEGER NOT NULL,
                    unit_json TEXT NOT NULL,
                    PRIMARY KEY (document_id, kind, source_order)
                 );
                 CREATE INDEX IF NOT EXISTS idx_text_unit_items_v1_section
                    ON text_unit_items_v1(document_id, owner_section_id, kind, source_order);
                 CREATE TABLE IF NOT EXISTS text_unit_sentence_coverage_v1 (
                    document_id TEXT NOT NULL,
                    owner_section_id TEXT NOT NULL,
                    position INTEGER NOT NULL,
                    coverage_json TEXT NOT NULL,
                    PRIMARY KEY (document_id, owner_section_id, position)
                 );
                 CREATE TABLE IF NOT EXISTS text_unit_streams_v1 (
                    document_id TEXT NOT NULL,
                    owner_section_id TEXT NOT NULL,
                    requested_kind TEXT NOT NULL,
                    position INTEGER NOT NULL,
                    eligible_position INTEGER,
                    unit_kind TEXT NOT NULL,
                    unit_source_order INTEGER NOT NULL,
                    paragraph_index INTEGER NOT NULL,
                    sentence_index INTEGER,
                    content_class TEXT NOT NULL,
                    degradation TEXT,
                    char_count INTEGER NOT NULL,
                    PRIMARY KEY (document_id, owner_section_id, requested_kind, position)
                 );
                 CREATE UNIQUE INDEX IF NOT EXISTS idx_text_unit_streams_v1_eligible
                    ON text_unit_streams_v1(document_id, owner_section_id, requested_kind, eligible_position)
                    WHERE eligible_position IS NOT NULL;
                 CREATE INDEX IF NOT EXISTS idx_text_unit_streams_v1_unit
                    ON text_unit_streams_v1(document_id, owner_section_id, requested_kind, unit_kind, paragraph_index, sentence_index);
                 CREATE TABLE IF NOT EXISTS text_unit_stream_summaries_v1 (
                    document_id TEXT NOT NULL,
                    owner_section_id TEXT NOT NULL,
                    requested_kind TEXT NOT NULL,
                    coverage_policy TEXT NOT NULL,
                    total_items INTEGER NOT NULL,
                    coverage_json TEXT NOT NULL,
                    PRIMARY KEY (document_id, owner_section_id, requested_kind, coverage_policy)
                 );",
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

                    for unit in &paragraphs.units {
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

                for table in [
                    "text_unit_sets_v1",
                    "text_unit_sections_v1",
                    "text_unit_items_v1",
                    "text_unit_sentence_coverage_v1",
                    "text_unit_streams_v1",
                    "text_unit_stream_summaries_v1",
                ] {
                    transaction
                        .execute(
                            &format!("DELETE FROM {table} WHERE document_id = ?1"),
                            params![&document_id.0],
                        )
                        .map_err(text_unit_index_error)?;
                }

                transaction
                    .execute(
                        "INSERT INTO text_unit_sets_v1(
                            document_id,
                            segmentation_version
                         ) VALUES (?1, ?2)",
                        params![&document_id.0, TEXT_SEGMENTATION_VERSION],
                    )
                    .map_err(text_unit_index_error)?;

                for coverage in &paragraphs.coverage {
                    let coverage_json =
                        serde_json::to_string(&StoredParagraphCoverage::from_coverage(coverage))
                            .map_err(|error| {
                                ApplicationError::TextUnitIndexFailed(error.to_string())
                            })?;
                    transaction
                        .execute(
                            "INSERT INTO text_unit_sections_v1(
                                document_id,
                                owner_section_id,
                                paragraph_coverage_json
                             ) VALUES (?1, ?2, ?3)",
                            params![&document_id.0, &coverage.owner_section_id.0, coverage_json,],
                        )
                        .map_err(text_unit_index_error)?;
                }

                {
                    let mut statement = transaction
                        .prepare(
                            "INSERT INTO text_unit_items_v1(
                                document_id,
                                owner_section_id,
                                kind,
                                source_order,
                                unit_json
                             ) VALUES (?1, ?2, ?3, ?4, ?5)",
                        )
                        .map_err(text_unit_index_error)?;

                    for unit in &paragraphs.units {
                        let unit_json =
                            serde_json::to_string(&StoredParagraphUnit::from_unit(unit)).map_err(
                                |error| ApplicationError::TextUnitIndexFailed(error.to_string()),
                            )?;
                        statement
                            .execute(params![
                                &document_id.0,
                                &unit.owner_section_id.0,
                                TextUnitKind::Paragraph.as_str(),
                                usize_to_i64(unit.source_order)?,
                                unit_json,
                            ])
                            .map_err(text_unit_index_error)?;
                    }

                    for unit in &sentences.units {
                        let unit_json = serde_json::to_string(&StoredSentenceUnit::from_unit(unit))
                            .map_err(|error| {
                                ApplicationError::TextUnitIndexFailed(error.to_string())
                            })?;
                        statement
                            .execute(params![
                                &document_id.0,
                                &unit.owner_section_id.0,
                                "sentence",
                                usize_to_i64(unit.source_order)?,
                                unit_json,
                            ])
                            .map_err(text_unit_index_error)?;
                    }
                }

                {
                    let mut statement = transaction
                        .prepare(
                            "INSERT INTO text_unit_sentence_coverage_v1(
                                document_id,
                                owner_section_id,
                                position,
                                coverage_json
                             ) VALUES (?1, ?2, ?3, ?4)",
                        )
                        .map_err(text_unit_index_error)?;

                    let mut positions: HashMap<SectionId, i64> = HashMap::new();
                    for coverage in &sentences.coverage {
                        let position = positions
                            .entry(coverage.owner_section_id.clone())
                            .or_insert(0);
                        let coverage_json =
                            serde_json::to_string(&StoredSentenceCoverage::from_coverage(coverage))
                                .map_err(|error| {
                                    ApplicationError::TextUnitIndexFailed(error.to_string())
                                })?;
                        statement
                            .execute(params![
                                &document_id.0,
                                &coverage.owner_section_id.0,
                                *position,
                                coverage_json,
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
                            "INSERT INTO text_unit_streams_v1(
                                document_id,
                                owner_section_id,
                                requested_kind,
                                position,
                                eligible_position,
                                unit_kind,
                                unit_source_order,
                                paragraph_index,
                                sentence_index,
                                content_class,
                                degradation,
                                char_count
                             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                        )
                        .map_err(text_unit_index_error)?;
                    let mut summary_statement = transaction
                        .prepare(
                            "INSERT INTO text_unit_stream_summaries_v1(
                                document_id,
                                owner_section_id,
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
                                let sentence_index =
                                    entry.unit.sentence_index().map(usize_to_i64).transpose()?;
                                stream_statement
                                    .execute(params![
                                        &document_id.0,
                                        &section_id.0,
                                        requested_kind.as_str(),
                                        usize_to_i64(position)?,
                                        eligible_position_value,
                                        entry.unit.kind_str(),
                                        usize_to_i64(entry.unit.source_order())?,
                                        usize_to_i64(entry.unit.paragraph_index())?,
                                        sentence_index,
                                        entry.content_class.as_str(),
                                        entry.degradation,
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
                                        &document_id.0,
                                        &section_id.0,
                                        requested_kind.as_str(),
                                        policy.as_str(),
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
                let set = connection
                    .query_row(
                        "SELECT segmentation_version
                         FROM text_unit_sets_v1
                         WHERE document_id = ?1",
                        params![&document_id.0],
                        |row| row.get::<_, String>(0),
                    )
                    .optional()
                    .map_err(text_unit_index_error)?;
                let Some(stored_version) = set else {
                    return Ok(None);
                };
                if stored_version != TEXT_SEGMENTATION_VERSION {
                    return Ok(None);
                }

                let paragraph_coverage = connection
                    .query_row(
                        "SELECT paragraph_coverage_json
                         FROM text_unit_sections_v1
                         WHERE document_id = ?1 AND owner_section_id = ?2",
                        params![&document_id.0, &section_id.0],
                        |row| row.get::<_, String>(0),
                    )
                    .optional()
                    .map_err(text_unit_index_error)?
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
                            "SELECT kind, unit_json
                             FROM text_unit_items_v1
                             WHERE document_id = ?1 AND owner_section_id = ?2
                             ORDER BY kind, source_order",
                        )
                        .map_err(text_unit_index_error)?;
                    let rows = statement
                        .query_map(params![&document_id.0, &section_id.0], |row| {
                            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                        })
                        .map_err(text_unit_index_error)?;
                    for row in rows {
                        let (kind, json) = row.map_err(text_unit_index_error)?;
                        match kind.as_str() {
                            "paragraph" => {
                                let stored: StoredParagraphUnit = serde_json::from_str(&json)
                                    .map_err(|error| {
                                        ApplicationError::TextUnitIndexFailed(error.to_string())
                                    })?;
                                paragraphs.push(stored.into_unit()?);
                            }
                            "sentence" => {
                                let stored: StoredSentenceUnit = serde_json::from_str(&json)
                                    .map_err(|error| {
                                        ApplicationError::TextUnitIndexFailed(error.to_string())
                                    })?;
                                sentences.push(stored.into_unit()?);
                            }
                            other => {
                                return Err(ApplicationError::TextUnitIndexFailed(format!(
                                    "unsupported persisted text unit kind {other:?}"
                                )));
                            }
                        }
                    }
                }

                let mut sentence_coverage = Vec::new();
                {
                    let mut statement = connection
                        .prepare(
                            "SELECT coverage_json
                             FROM text_unit_sentence_coverage_v1
                             WHERE document_id = ?1 AND owner_section_id = ?2
                             ORDER BY position",
                        )
                        .map_err(text_unit_index_error)?;
                    let rows = statement
                        .query_map(params![&document_id.0, &section_id.0], |row| {
                            row.get::<_, String>(0)
                        })
                        .map_err(text_unit_index_error)?;
                    for row in rows {
                        let json = row.map_err(text_unit_index_error)?;
                        let stored: StoredSentenceCoverage =
                            serde_json::from_str(&json).map_err(|error| {
                                ApplicationError::TextUnitIndexFailed(error.to_string())
                            })?;
                        sentence_coverage.push(stored.into_coverage()?);
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
                let set = connection
                    .query_row(
                        "SELECT segmentation_version
                         FROM text_unit_sets_v1
                         WHERE document_id = ?1",
                        params![&document_id.0],
                        |row| row.get::<_, String>(0),
                    )
                    .optional()
                    .map_err(text_unit_index_error)?;
                let Some(stored_version) = set else {
                    return Ok(None);
                };
                if stored_version != TEXT_SEGMENTATION_VERSION {
                    return Ok(None);
                }

                let summary = connection
                    .query_row(
                        "SELECT total_items, coverage_json
                         FROM text_unit_stream_summaries_v1
                         WHERE document_id = ?1
                           AND owner_section_id = ?2
                           AND requested_kind = ?3
                           AND coverage_policy = ?4",
                        params![
                            &document_id.0,
                            &section_id.0,
                            kind.as_str(),
                            policy.as_str()
                        ],
                        |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
                    )
                    .optional()
                    .map_err(text_unit_index_error)?;
                let Some((total_items, coverage_json)) = summary else {
                    // A section without paragraph coverage was never written (it cannot be planned), so the
                    // planner path must report today's "paragraph coverage is unavailable" error. A section
                    // that has coverage but no summary row is stale storage: report it as missing so the
                    // caller asks for a reopen.
                    let has_coverage = connection
                        .query_row(
                            "SELECT 1
                             FROM text_unit_sections_v1
                             WHERE document_id = ?1 AND owner_section_id = ?2",
                            params![&document_id.0, &section_id.0],
                            |_| Ok(()),
                        )
                        .optional()
                        .map_err(text_unit_index_error)?
                        .is_some();
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
                    "SELECT s.{position_column}, s.unit_kind, s.content_class, s.degradation, s.char_count, i.unit_json
                     FROM text_unit_streams_v1 s
                     JOIN text_unit_items_v1 i
                       ON i.document_id = s.document_id
                      AND i.kind = s.unit_kind
                      AND i.source_order = s.unit_source_order
                     WHERE s.document_id = ?1
                       AND s.owner_section_id = ?2
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
                        params![&document_id.0, &section_id.0, kind.as_str(), position, limit],
                        |row| {
                            Ok((
                                row.get::<_, i64>(0)?,
                                row.get::<_, String>(1)?,
                                row.get::<_, String>(2)?,
                                row.get::<_, Option<String>>(3)?,
                                row.get::<_, i64>(4)?,
                                row.get::<_, String>(5)?,
                            ))
                        },
                    )
                    .map_err(text_unit_index_error)?;

                let mut items = Vec::new();
                for row in rows {
                    let (position, unit_kind, content_class, degradation, char_count, unit_json) =
                        row.map_err(text_unit_index_error)?;
                    let unit = decode_stream_unit(&unit_kind, &unit_json)?;
                    items.push(StoredStreamItem {
                        position: i64_to_usize(position, "stream position")?,
                        unit,
                        content_class: parse_paragraph_content_class(&content_class)?,
                        degradation,
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
                let position_column = match policy {
                    TextUnitCoveragePolicy::PreserveSource => "position",
                    TextUnitCoveragePolicy::EligibleOnly => "eligible_position",
                };
                let sql = format!(
                    "SELECT s.{position_column}
                     FROM text_unit_streams_v1 s
                     WHERE s.document_id = ?1
                       AND s.owner_section_id = ?2
                       AND s.requested_kind = ?3
                       AND s.unit_kind = ?4
                       AND s.paragraph_index = ?5
                       AND s.sentence_index IS ?6"
                );
                let paragraph_index = usize_to_i64(paragraph_index)?;
                let sentence_index = sentence_index.map(usize_to_i64).transpose()?;
                let found = connection
                    .query_row(
                        &sql,
                        params![
                            &document_id.0,
                            &section_id.0,
                            kind.as_str(),
                            unit_kind.as_str(),
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

/// Decodes one stream row's unit with the same decoding the section read path uses.
fn decode_stream_unit(
    unit_kind: &str,
    unit_json: &str,
) -> Result<OwnedStreamUnit, ApplicationError> {
    match unit_kind {
        "paragraph" => {
            let stored: StoredParagraphUnit = serde_json::from_str(unit_json)
                .map_err(|error| ApplicationError::TextUnitIndexFailed(error.to_string()))?;
            Ok(OwnedStreamUnit::Paragraph(stored.into_unit()?))
        }
        "sentence" => {
            let stored: StoredSentenceUnit = serde_json::from_str(unit_json)
                .map_err(|error| ApplicationError::TextUnitIndexFailed(error.to_string()))?;
            Ok(OwnedStreamUnit::Sentence(stored.into_unit()?))
        }
        other => Err(ApplicationError::TextUnitIndexFailed(format!(
            "unsupported persisted text unit kind {other:?}"
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

fn parse_text_unit_kind(value: &str) -> Result<TextUnitKind, ApplicationError> {
    match value {
        "paragraph" => Ok(TextUnitKind::Paragraph),
        other => Err(ApplicationError::TextUnitIndexFailed(format!(
            "unsupported persisted text unit kind {other:?}"
        ))),
    }
}

fn parse_paragraph_content_class(value: &str) -> Result<ParagraphContentClass, ApplicationError> {
    match value {
        "prose_or_unknown" => Ok(ParagraphContentClass::ProseOrUnknown),
        "native_paragraph" => Ok(ParagraphContentClass::NativeParagraph),
        "blockquote" => Ok(ParagraphContentClass::BlockQuote),
        "list_item" => Ok(ParagraphContentClass::ListItem),
        "code_block" => Ok(ParagraphContentClass::CodeBlock),
        "preformatted" => Ok(ParagraphContentClass::Preformatted),
        "table" => Ok(ParagraphContentClass::Table),
        other => Err(ApplicationError::TextUnitIndexFailed(format!(
            "unsupported persisted paragraph content class {other:?}"
        ))),
    }
}

fn parse_sentence_eligibility(value: &str) -> Result<SentenceEligibility, ApplicationError> {
    match value {
        "eligible" => Ok(SentenceEligibility::Eligible),
        "coarse_paragraph_only" => Ok(SentenceEligibility::CoarseParagraphOnly),
        other => Err(ApplicationError::TextUnitIndexFailed(format!(
            "unsupported persisted sentence eligibility {other:?}"
        ))),
    }
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
struct StoredParagraphUnit {
    id: String,
    document_id: String,
    content_hash: String,
    normalized_document_hash: String,
    owner_section_id: String,
    kind: String,
    paragraph_index: usize,
    source_order: usize,
    range_start: usize,
    range_end: usize,
    text: String,
    segmentation_version: String,
}

impl StoredParagraphUnit {
    fn from_unit(unit: &TextUnit) -> Self {
        Self {
            id: unit.id.0.clone(),
            document_id: unit.document_id.0.clone(),
            content_hash: unit.content_hash.0.clone(),
            normalized_document_hash: unit.normalized_document_hash.0.clone(),
            owner_section_id: unit.owner_section_id.0.clone(),
            kind: unit.kind.as_str().to_string(),
            paragraph_index: unit.paragraph_index,
            source_order: unit.source_order,
            range_start: unit.normalized_range.start(),
            range_end: unit.normalized_range.end(),
            text: unit.text.clone(),
            segmentation_version: unit.segmentation_version.clone(),
        }
    }

    fn into_unit(self) -> Result<TextUnit, ApplicationError> {
        let kind = parse_text_unit_kind(&self.kind)?;
        let normalized_range = stored_normalized_range(self.range_start, self.range_end)?;
        Ok(TextUnit {
            id: TextUnitId(self.id),
            document_id: DocumentId(self.document_id),
            content_hash: ContentHash(self.content_hash),
            normalized_document_hash: NormalizedDocumentHash(self.normalized_document_hash),
            owner_section_id: SectionId(self.owner_section_id),
            kind,
            paragraph_index: self.paragraph_index,
            source_order: self.source_order,
            normalized_range,
            text: self.text,
            segmentation_version: self.segmentation_version,
        })
    }
}

#[derive(Serialize, Deserialize)]
struct StoredSentenceUnit {
    id: String,
    document_id: String,
    content_hash: String,
    normalized_document_hash: String,
    owner_section_id: String,
    paragraph_index: usize,
    sentence_index: usize,
    parent_paragraph_id: String,
    source_order: usize,
    range_start: usize,
    range_end: usize,
    text: String,
    segmentation_version: String,
}

impl StoredSentenceUnit {
    fn from_unit(unit: &SentenceTextUnit) -> Self {
        Self {
            id: unit.id.0.clone(),
            document_id: unit.document_id.0.clone(),
            content_hash: unit.content_hash.0.clone(),
            normalized_document_hash: unit.normalized_document_hash.0.clone(),
            owner_section_id: unit.owner_section_id.0.clone(),
            paragraph_index: unit.paragraph_index,
            sentence_index: unit.sentence_index,
            parent_paragraph_id: unit.parent_paragraph_id.0.clone(),
            source_order: unit.source_order,
            range_start: unit.normalized_range.start(),
            range_end: unit.normalized_range.end(),
            text: unit.text.clone(),
            segmentation_version: unit.segmentation_version.clone(),
        }
    }

    fn into_unit(self) -> Result<SentenceTextUnit, ApplicationError> {
        let normalized_range = stored_normalized_range(self.range_start, self.range_end)?;
        Ok(SentenceTextUnit {
            id: TextUnitId(self.id),
            document_id: DocumentId(self.document_id),
            content_hash: ContentHash(self.content_hash),
            normalized_document_hash: NormalizedDocumentHash(self.normalized_document_hash),
            owner_section_id: SectionId(self.owner_section_id),
            paragraph_index: self.paragraph_index,
            sentence_index: self.sentence_index,
            parent_paragraph_id: TextUnitId(self.parent_paragraph_id),
            source_order: self.source_order,
            normalized_range,
            text: self.text,
            segmentation_version: self.segmentation_version,
        })
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

#[derive(Serialize, Deserialize)]
struct StoredSentenceCoverage {
    owner_section_id: String,
    paragraph_id: String,
    paragraph_index: usize,
    content_class: String,
    eligibility: String,
    paragraph_chars: usize,
    sentence_chars: usize,
    separator_chars: usize,
    coarse_only_chars: usize,
    sentence_count: usize,
}

impl StoredSentenceCoverage {
    fn from_coverage(coverage: &SentenceParagraphCoverage) -> Self {
        Self {
            owner_section_id: coverage.owner_section_id.0.clone(),
            paragraph_id: coverage.paragraph_id.0.clone(),
            paragraph_index: coverage.paragraph_index,
            content_class: coverage.content_class.as_str().to_string(),
            eligibility: coverage.eligibility.as_str().to_string(),
            paragraph_chars: coverage.paragraph_chars,
            sentence_chars: coverage.sentence_chars,
            separator_chars: coverage.separator_chars,
            coarse_only_chars: coverage.coarse_only_chars,
            sentence_count: coverage.sentence_count,
        }
    }

    fn into_coverage(self) -> Result<SentenceParagraphCoverage, ApplicationError> {
        let content_class = parse_paragraph_content_class(&self.content_class)?;
        let eligibility = parse_sentence_eligibility(&self.eligibility)?;
        Ok(SentenceParagraphCoverage {
            owner_section_id: SectionId(self.owner_section_id),
            paragraph_id: TextUnitId(self.paragraph_id),
            paragraph_index: self.paragraph_index,
            content_class,
            eligibility,
            paragraph_chars: self.paragraph_chars,
            sentence_chars: self.sentence_chars,
            separator_chars: self.separator_chars,
            coarse_only_chars: self.coarse_only_chars,
            sentence_count: self.sentence_count,
        })
    }
}
