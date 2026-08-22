use std::path::{Path, PathBuf};
use std::sync::Mutex;

use async_trait::async_trait;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::application::ports::{
    ApplicationError, LEXICAL_TOKENIZER_VERSION, LexicalSearchHit, SearchHit, SearchHitKind,
    SearchIndex,
};
use crate::domain::{
    ContentHash, Document, DocumentId, DocumentSource, Location, NormalizedDocumentHash,
    NormalizedTextRange, SectionId, TextLocator,
};

use super::lexical::{
    LEXICAL_SEARCH_INDEX_VERSION, build_lexical_candidates, encoded_lexemes, encoded_query,
};

const META_INDEX_VERSION: &str = "lexical_search_index_version";
const META_TOKENIZER_VERSION: &str = "lexical_tokenizer_version";

pub struct SqliteSearchIndex {
    path: PathBuf,
    connection: Mutex<Connection>,
}

impl SqliteSearchIndex {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, ApplicationError> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|error| {
                ApplicationError::IndexFailed(format!("{}: {error}", parent.display()))
            })?;
        }
        let connection = Connection::open(&path).map_err(index_error)?;
        connection
            .execute_batch(
                "PRAGMA journal_mode=WAL;
                 PRAGMA synchronous=NORMAL;
                 CREATE TABLE IF NOT EXISTS lexical_search_meta (
                    key TEXT PRIMARY KEY,
                    value TEXT NOT NULL
                 );",
            )
            .map_err(index_error)?;
        ensure_schema(&connection)?;
        Ok(Self {
            path,
            connection: Mutex::new(connection),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

#[async_trait]
impl SearchIndex for SqliteSearchIndex {
    fn tokenizer_version(&self) -> &'static str {
        LEXICAL_TOKENIZER_VERSION
    }

    async fn index(&self, document: &Document) -> Result<(), ApplicationError> {
        let candidates = build_lexical_candidates(document)?;
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| ApplicationError::IndexFailed("SQLite search lock poisoned".into()))?;
        let transaction = connection.transaction().map_err(index_error)?;
        transaction
            .execute(
                "DELETE FROM lexical_search_units_v2 WHERE document_id = ?1",
                params![&document.id.0],
            )
            .map_err(index_error)?;

        {
            let mut statement = transaction
                .prepare(
                    "INSERT INTO lexical_search_units_v2(
                        document_id,
                        candidate_kind,
                        section_id,
                        title,
                        source,
                        snippet,
                        location_json,
                        locator_json,
                        tokenizer_version,
                        source_order,
                        lexemes
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                )
                .map_err(index_error)?;

            for candidate in candidates {
                let location_json =
                    serde_json::to_string(&StoredLocation::from(&candidate.location))
                        .map_err(|error| ApplicationError::IndexFailed(error.to_string()))?;
                let locator_json =
                    serde_json::to_string(&StoredTextLocator::from(&candidate.text_locator))
                        .map_err(|error| ApplicationError::IndexFailed(error.to_string()))?;
                statement
                    .execute(params![
                        &document.id.0,
                        candidate.candidate_kind.as_str(),
                        &candidate.section_id.0,
                        &candidate.title,
                        &candidate.source.0,
                        &candidate.snippet,
                        location_json,
                        locator_json,
                        LEXICAL_TOKENIZER_VERSION,
                        usize_to_i64(candidate.source_order)?,
                        encoded_lexemes(&candidate.tokens),
                    ])
                    .map_err(index_error)?;
            }
        }

        transaction
            .execute(
                "INSERT INTO lexical_search_documents_v2(
                    document_id,
                    normalized_document_hash,
                    index_version,
                    tokenizer_version
                 ) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(document_id) DO UPDATE SET
                    normalized_document_hash = excluded.normalized_document_hash,
                    index_version = excluded.index_version,
                    tokenizer_version = excluded.tokenizer_version",
                params![
                    &document.id.0,
                    document.normalized_document_hash().0,
                    LEXICAL_SEARCH_INDEX_VERSION,
                    LEXICAL_TOKENIZER_VERSION,
                ],
            )
            .map_err(index_error)?;
        transaction.commit().map_err(index_error)?;
        Ok(())
    }

    async fn search(
        &self,
        document_id: &DocumentId,
        query: &str,
        limit: usize,
    ) -> Result<Vec<SearchHit>, ApplicationError> {
        Ok(self
            .search_lexical(document_id, query, limit)
            .await?
            .into_iter()
            .map(|hit| SearchHit {
                section_id: hit.section_id,
                title: hit.title,
                source: hit.source,
                snippet: hit.snippet,
                score: hit.score,
                location: hit.location,
            })
            .collect())
    }

    async fn search_lexical(
        &self,
        document_id: &DocumentId,
        query: &str,
        limit: usize,
    ) -> Result<Vec<LexicalSearchHit>, ApplicationError> {
        if query.trim().is_empty() {
            return Err(ApplicationError::InvalidRequest(
                "search query must not be empty".into(),
            ));
        }
        if limit == 0 {
            return Ok(vec![]);
        }
        let fts_query = encoded_query(query).ok_or_else(|| {
            ApplicationError::InvalidRequest("search query must contain searchable terms".into())
        })?;
        let connection = self
            .connection
            .lock()
            .map_err(|_| ApplicationError::IndexFailed("SQLite search lock poisoned".into()))?;

        let indexed = connection
            .query_row(
                "SELECT 1 FROM lexical_search_documents_v2 WHERE document_id = ?1",
                params![&document_id.0],
                |_| Ok(()),
            )
            .optional()
            .map_err(index_error)?
            .is_some();
        if !indexed {
            return Err(ApplicationError::DocumentNotFound);
        }

        let mut statement = connection
            .prepare(
                "SELECT
                    candidate_kind,
                    section_id,
                    title,
                    source,
                    snippet,
                    location_json,
                    locator_json,
                    tokenizer_version,
                    bm25(lexical_search_units_v2)
                 FROM lexical_search_units_v2
                 WHERE lexical_search_units_v2 MATCH ?1 AND document_id = ?2
                 ORDER BY bm25(lexical_search_units_v2) ASC, CAST(source_order AS INTEGER) ASC
                 LIMIT ?3",
            )
            .map_err(index_error)?;
        let rows = statement
            .query_map(
                params![fts_query, &document_id.0, usize_to_i64(limit)?],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, String>(5)?,
                        row.get::<_, String>(6)?,
                        row.get::<_, String>(7)?,
                        row.get::<_, f64>(8)?,
                    ))
                },
            )
            .map_err(index_error)?;

        let mut hits = Vec::new();
        for row in rows {
            let (
                candidate_kind,
                section_id,
                title,
                source,
                snippet,
                location_json,
                locator_json,
                tokenizer_version,
                rank,
            ) = row.map_err(index_error)?;
            if tokenizer_version != LEXICAL_TOKENIZER_VERSION {
                return Err(ApplicationError::IndexFailed(format!(
                    "persisted tokenizer version {tokenizer_version} does not match {LEXICAL_TOKENIZER_VERSION}"
                )));
            }
            hits.push(LexicalSearchHit {
                section_id: SectionId(section_id),
                title,
                source: DocumentSource(source),
                snippet,
                score: (1.0 / (1.0 + rank.abs())) as f32,
                location: serde_json::from_str::<StoredLocation>(&location_json)
                    .map_err(|error| ApplicationError::IndexFailed(error.to_string()))?
                    .into(),
                candidate_kind: parse_kind(&candidate_kind)?,
                text_locator: serde_json::from_str::<StoredTextLocator>(&locator_json)
                    .map_err(|error| ApplicationError::IndexFailed(error.to_string()))?
                    .try_into()?,
                tokenizer_version,
            });
        }
        Ok(hits)
    }
}

fn ensure_schema(connection: &Connection) -> Result<(), ApplicationError> {
    let stored_index = meta_value(connection, META_INDEX_VERSION)?;
    let stored_tokenizer = meta_value(connection, META_TOKENIZER_VERSION)?;
    let compatible = stored_index.as_deref() == Some(LEXICAL_SEARCH_INDEX_VERSION)
        && stored_tokenizer.as_deref() == Some(LEXICAL_TOKENIZER_VERSION);

    if !compatible {
        connection
            .execute_batch(
                "DROP TABLE IF EXISTS lexical_search_units_v2;
                 DROP TABLE IF EXISTS lexical_search_documents_v2;",
            )
            .map_err(index_error)?;
    }

    connection
        .execute_batch(
            "CREATE VIRTUAL TABLE IF NOT EXISTS lexical_search_units_v2 USING fts5(
                document_id UNINDEXED,
                candidate_kind UNINDEXED,
                section_id UNINDEXED,
                title UNINDEXED,
                source UNINDEXED,
                snippet UNINDEXED,
                location_json UNINDEXED,
                locator_json UNINDEXED,
                tokenizer_version UNINDEXED,
                source_order UNINDEXED,
                lexemes
             );
             CREATE TABLE IF NOT EXISTS lexical_search_documents_v2 (
                document_id TEXT PRIMARY KEY,
                normalized_document_hash TEXT NOT NULL,
                index_version TEXT NOT NULL,
                tokenizer_version TEXT NOT NULL
             );",
        )
        .map_err(index_error)?;

    set_meta(connection, META_INDEX_VERSION, LEXICAL_SEARCH_INDEX_VERSION)?;
    set_meta(
        connection,
        META_TOKENIZER_VERSION,
        LEXICAL_TOKENIZER_VERSION,
    )?;
    Ok(())
}

fn meta_value(connection: &Connection, key: &str) -> Result<Option<String>, ApplicationError> {
    connection
        .query_row(
            "SELECT value FROM lexical_search_meta WHERE key = ?1",
            params![key],
            |row| row.get(0),
        )
        .optional()
        .map_err(index_error)
}

fn set_meta(connection: &Connection, key: &str, value: &str) -> Result<(), ApplicationError> {
    connection
        .execute(
            "INSERT INTO lexical_search_meta(key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )
        .map_err(index_error)?;
    Ok(())
}

fn parse_kind(value: &str) -> Result<SearchHitKind, ApplicationError> {
    match value {
        "section" => Ok(SearchHitKind::Section),
        "paragraph" => Ok(SearchHitKind::Paragraph),
        "sentence" => Ok(SearchHitKind::Sentence),
        other => Err(ApplicationError::IndexFailed(format!(
            "unsupported lexical candidate kind {other:?}"
        ))),
    }
}

fn index_error(error: rusqlite::Error) -> ApplicationError {
    ApplicationError::IndexFailed(error.to_string())
}

fn usize_to_i64(value: usize) -> Result<i64, ApplicationError> {
    i64::try_from(value).map_err(|_| {
        ApplicationError::IndexFailed(format!(
            "search numeric value {value} exceeds SQLite INTEGER range"
        ))
    })
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

impl From<&Location> for StoredLocation {
    fn from(value: &Location) -> Self {
        Self {
            page: value.page,
            chapter: value.chapter.clone(),
            section_path: value.section_path.clone(),
            anchor: value.anchor.clone(),
            paragraph: value.paragraph,
            char_start: value.char_start,
            char_end: value.char_end,
            native_location: value.native_location.clone(),
        }
    }
}

impl From<StoredLocation> for Location {
    fn from(value: StoredLocation) -> Self {
        Self {
            page: value.page,
            chapter: value.chapter,
            section_path: value.section_path,
            anchor: value.anchor,
            paragraph: value.paragraph,
            char_start: value.char_start,
            char_end: value.char_end,
            native_location: value.native_location,
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
struct StoredTextLocator {
    document_id: String,
    content_hash: String,
    normalized_document_hash: String,
    owner_section_id: String,
    section_path: Vec<String>,
    paragraph_index: Option<usize>,
    sentence_index: Option<usize>,
    range_start: Option<usize>,
    range_end: Option<usize>,
    segmentation_version: Option<String>,
    native_location: Option<String>,
}

impl From<&TextLocator> for StoredTextLocator {
    fn from(value: &TextLocator) -> Self {
        Self {
            document_id: value.document_id.0.clone(),
            content_hash: value.content_hash.0.clone(),
            normalized_document_hash: value.normalized_document_hash.0.clone(),
            owner_section_id: value.owner_section_id.0.clone(),
            section_path: value.section_path.clone(),
            paragraph_index: value.paragraph_index,
            sentence_index: value.sentence_index,
            range_start: value.normalized_range.map(NormalizedTextRange::start),
            range_end: value.normalized_range.map(NormalizedTextRange::end),
            segmentation_version: value.segmentation_version.clone(),
            native_location: value.native_location.clone(),
        }
    }
}

impl TryFrom<StoredTextLocator> for TextLocator {
    type Error = ApplicationError;

    fn try_from(value: StoredTextLocator) -> Result<Self, Self::Error> {
        let normalized_range = match (value.range_start, value.range_end) {
            (None, None) => None,
            (Some(start), Some(end)) => {
                Some(NormalizedTextRange::new(start, end).map_err(|error| {
                    ApplicationError::IndexFailed(format!("invalid stored locator range: {error}"))
                })?)
            }
            _ => {
                return Err(ApplicationError::IndexFailed(
                    "stored locator has incomplete normalized range".into(),
                ));
            }
        };
        Ok(TextLocator {
            document_id: DocumentId(value.document_id),
            content_hash: ContentHash(value.content_hash),
            normalized_document_hash: NormalizedDocumentHash(value.normalized_document_hash),
            owner_section_id: SectionId(value.owner_section_id),
            section_path: value.section_path,
            paragraph_index: value.paragraph_index,
            sentence_index: value.sentence_index,
            normalized_range,
            segmentation_version: value.segmentation_version,
            native_location: value.native_location,
        })
    }
}
