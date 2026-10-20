use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use super::sqlite_support::{open_connection, run_sqlite};
use crate::application::ports::{
    ApplicationError, LEXICAL_TOKENIZER_VERSION, LexicalSearchHit, SearchHit, SearchHitKind,
    SearchIndex,
};
use crate::domain::{
    ContentHash, Document, DocumentId, DocumentSource, Location, NormalizedDocumentHash,
    NormalizedTextRange, ParagraphTextUnitSet, SectionId, SentenceTextUnitSet, TextLocator,
};

use super::lexical::{
    LEXICAL_SEARCH_INDEX_VERSION, LexicalCandidate, MAX_SNIPPET_CHARS, build_lexical_candidates,
    encoded_lexemes, encoded_query, encoded_relaxed_query, for_each_lexical_candidate,
    match_snippet, tokenize,
};

const META_INDEX_VERSION: &str = "lexical_search_index_version";
const META_TOKENIZER_VERSION: &str = "lexical_tokenizer_version";

pub struct SqliteSearchIndex {
    path: PathBuf,
    connection: Arc<Mutex<Connection>>,
}

impl SqliteSearchIndex {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, ApplicationError> {
        let path = path.as_ref().to_path_buf();
        let connection = open_connection(&path, ApplicationError::IndexFailed)?;
        connection
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS lexical_search_meta (
                    key TEXT PRIMARY KEY,
                    value TEXT NOT NULL
                 );",
            )
            .map_err(index_error)?;
        ensure_schema(&connection)?;
        Ok(Self {
            path,
            connection: Arc::new(Mutex::new(connection)),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    async fn store_candidates(
        &self,
        document_id: DocumentId,
        document_source: DocumentSource,
        normalized_document_hash: NormalizedDocumentHash,
        content_hash: ContentHash,
        produce: impl FnOnce(
            &mut dyn FnMut(LexicalCandidate) -> Result<(), ApplicationError>,
        ) -> Result<(), ApplicationError>
        + Send
        + 'static,
    ) -> Result<(), ApplicationError> {
        run_sqlite(
            &self.connection,
            ApplicationError::IndexFailed,
            move |connection| {
                let transaction = connection.transaction().map_err(index_error)?;
                let source = document_source.0.as_str();
                transaction
                    .execute(
                        "INSERT INTO lexical_search_documents_v5(
                            document_id,
                            source,
                            normalized_document_hash,
                            content_hash,
                            index_version,
                            tokenizer_version
                         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                         ON CONFLICT(document_id) DO UPDATE SET
                            source = excluded.source,
                            normalized_document_hash = excluded.normalized_document_hash,
                            content_hash = excluded.content_hash,
                            index_version = excluded.index_version,
                            tokenizer_version = excluded.tokenizer_version",
                        params![
                            &document_id.0,
                            source,
                            normalized_document_hash.0,
                            content_hash.0,
                            LEXICAL_SEARCH_INDEX_VERSION,
                            LEXICAL_TOKENIZER_VERSION,
                        ],
                    )
                    .map_err(index_error)?;
                let doc_key: i64 = transaction
                    .query_row(
                        "SELECT doc_key FROM lexical_search_documents_v5 WHERE document_id = ?1",
                        params![&document_id.0],
                        |row| row.get(0),
                    )
                    .map_err(index_error)?;

                transaction
                    .execute(
                        "DELETE FROM lexical_search_fts_v5
                         WHERE rowid IN (
                            SELECT rowid FROM lexical_search_candidates_v5 WHERE doc_key = ?1
                         )",
                        params![doc_key],
                    )
                    .map_err(index_error)?;
                transaction
                    .execute(
                        "DELETE FROM lexical_search_candidates_v5 WHERE doc_key = ?1",
                        params![doc_key],
                    )
                    .map_err(index_error)?;

                {
                    let mut candidate_statement = transaction
                        .prepare(
                            "INSERT INTO lexical_search_candidates_v5(
                                doc_key,
                                source_order,
                                candidate_kind,
                                section_id,
                                title,
                                searchable_text,
                                location_json,
                                locator_json
                             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                        )
                        .map_err(index_error)?;
                    let mut fts_statement = transaction
                        .prepare(
                            "INSERT INTO lexical_search_fts_v5(rowid, lexemes, doc)
                             VALUES (?1, ?2, ?3)",
                        )
                        .map_err(index_error)?;
                    let doc_token = format!("d{doc_key}");

                    produce(&mut |candidate| {
                        let location_json =
                            serde_json::to_string(&StoredLocation::from(&candidate.location))
                                .map_err(|error| {
                                    ApplicationError::IndexFailed(error.to_string())
                                })?;
                        let locator_json = serde_json::to_string(&StoredCompactLocator::from(
                            &candidate.text_locator,
                        ))
                        .map_err(|error| ApplicationError::IndexFailed(error.to_string()))?;
                        candidate_statement
                            .execute(params![
                                doc_key,
                                usize_to_i64(candidate.source_order)?,
                                candidate.candidate_kind.as_str(),
                                &candidate.section_id.0,
                                &candidate.title,
                                &candidate.searchable_text,
                                location_json,
                                locator_json,
                            ])
                            .map_err(index_error)?;
                        let rowid = transaction.last_insert_rowid();
                        fts_statement
                            .execute(params![
                                rowid,
                                encoded_lexemes(&candidate.tokens),
                                &doc_token,
                            ])
                            .map_err(index_error)?;
                        Ok(())
                    })?;
                }

                transaction.commit().map_err(index_error)?;
                Ok(())
            },
        )
        .await
    }
}

#[async_trait]
impl SearchIndex for SqliteSearchIndex {
    fn tokenizer_version(&self) -> &'static str {
        LEXICAL_TOKENIZER_VERSION
    }

    fn index_version(&self) -> &'static str {
        LEXICAL_SEARCH_INDEX_VERSION
    }

    async fn index(&self, document: &Document) -> Result<(), ApplicationError> {
        let document_id = document.id.clone();
        let document_source = document.source.clone();
        let normalized_document_hash = document.normalized_document_hash();
        let content_hash = document.content_hash.clone();
        let candidates = build_lexical_candidates(document)?;
        self.store_candidates(
            document_id,
            document_source,
            normalized_document_hash,
            content_hash,
            move |sink| {
                for candidate in candidates {
                    sink(candidate)?;
                }
                Ok(())
            },
        )
        .await
    }

    async fn index_with_units(
        &self,
        document: Arc<Document>,
        paragraphs: Arc<ParagraphTextUnitSet>,
        sentences: Arc<SentenceTextUnitSet>,
    ) -> Result<(), ApplicationError> {
        let document_id = document.id.clone();
        let document_source = document.source.clone();
        let normalized_document_hash = document.normalized_document_hash();
        let content_hash = document.content_hash.clone();
        self.store_candidates(
            document_id,
            document_source,
            normalized_document_hash,
            content_hash,
            move |sink| {
                for_each_lexical_candidate(&document, &paragraphs.units, &sentences.units, sink)
            },
        )
        .await
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
        let document_id = document_id.clone();
        let query = query.to_owned();
        run_sqlite(
            &self.connection,
            ApplicationError::IndexFailed,
            move |connection| {
                let document = connection
                    .query_row(
                        "SELECT doc_key, source, normalized_document_hash, content_hash, tokenizer_version
                         FROM lexical_search_documents_v5 WHERE document_id = ?1",
                        params![&document_id.0],
                        |row| {
                            Ok(StoredDocument {
                                doc_key: row.get(0)?,
                                source: row.get(1)?,
                                normalized_document_hash: row.get(2)?,
                                content_hash: row.get(3)?,
                                tokenizer_version: row.get(4)?,
                            })
                        },
                    )
                    .optional()
                    .map_err(index_error)?;
                let Some(document) = document else {
                    return Err(ApplicationError::DocumentNotFound);
                };

                let mut hits = fts_search(
                    connection,
                    &fts_query,
                    &document_id,
                    &document,
                    &query,
                    limit,
                )?;
                // Strict AND is authoritative; only a zero-hit multi-token query falls
                // back to a deterministic OR pass. Single-token queries never relax.
                if hits.is_empty() {
                    let query_tokens = tokenize(&query);
                    if query_tokens.len() > 1 {
                        hits = fts_search(
                            connection,
                            &encoded_relaxed_query(&query_tokens),
                            &document_id,
                            &document,
                            &query,
                            limit,
                        )?;
                    }
                }
                Ok(hits)
            },
        )
        .await
    }
}

struct StoredDocument {
    doc_key: i64,
    source: String,
    normalized_document_hash: String,
    content_hash: String,
    tokenizer_version: String,
}

fn fts_search(
    connection: &Connection,
    fts_query: &str,
    document_id: &DocumentId,
    document: &StoredDocument,
    raw_query: &str,
    limit: usize,
) -> Result<Vec<LexicalSearchHit>, ApplicationError> {
    if document.tokenizer_version != LEXICAL_TOKENIZER_VERSION {
        return Err(ApplicationError::IndexFailed(format!(
            "persisted tokenizer version {} does not match {LEXICAL_TOKENIZER_VERSION}",
            document.tokenizer_version
        )));
    }
    let scoped_query = format!("doc : d{} AND lexemes : ({fts_query})", document.doc_key);
    let mut statement = connection
        .prepare(
            "SELECT c.candidate_kind, c.section_id, c.title, c.searchable_text, c.location_json,
                    c.locator_json, bm25(lexical_search_fts_v5, 1.0, 0.0) AS rank
             FROM lexical_search_fts_v5
             JOIN lexical_search_candidates_v5 c ON c.rowid = lexical_search_fts_v5.rowid
             WHERE lexical_search_fts_v5 MATCH ?1
             ORDER BY rank ASC, c.source_order ASC
             LIMIT ?2",
        )
        .map_err(index_error)?;
    let rows = statement
        .query_map(params![scoped_query, usize_to_i64(limit)?], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, f64>(6)?,
            ))
        })
        .map_err(index_error)?;

    let mut hits = Vec::new();
    for row in rows {
        let (candidate_kind, section_id, title, searchable_text, location_json, locator_json, rank) =
            row.map_err(index_error)?;
        let locator: StoredCompactLocator = serde_json::from_str(&locator_json)
            .map_err(|error| ApplicationError::IndexFailed(error.to_string()))?;
        hits.push(LexicalSearchHit {
            section_id: SectionId(section_id),
            title,
            source: DocumentSource(document.source.clone()),
            snippet: match_snippet(&searchable_text, raw_query, MAX_SNIPPET_CHARS),
            score: (1.0 / (1.0 + rank.abs())) as f32,
            location: serde_json::from_str::<StoredLocation>(&location_json)
                .map_err(|error| ApplicationError::IndexFailed(error.to_string()))?
                .into(),
            candidate_kind: parse_kind(&candidate_kind)?,
            text_locator: locator.rebuild(
                &document_id.0,
                &document.content_hash,
                &document.normalized_document_hash,
            )?,
            tokenizer_version: document.tokenizer_version.clone(),
        });
    }
    Ok(hits)
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
                 DROP TABLE IF EXISTS lexical_search_documents_v2;
                 DROP TABLE IF EXISTS lexical_search_fts_v5;
                 DROP TABLE IF EXISTS lexical_search_candidates_v5;
                 DROP TABLE IF EXISTS lexical_search_documents_v5;",
            )
            .map_err(index_error)?;
    }

    connection
        .execute_batch(
            "CREATE TABLE IF NOT EXISTS lexical_search_documents_v5 (
                doc_key INTEGER PRIMARY KEY,
                document_id TEXT NOT NULL UNIQUE,
                source TEXT NOT NULL,
                normalized_document_hash TEXT NOT NULL,
                content_hash TEXT NOT NULL,
                index_version TEXT NOT NULL,
                tokenizer_version TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS lexical_search_candidates_v5 (
                rowid INTEGER PRIMARY KEY,
                doc_key INTEGER NOT NULL,
                source_order INTEGER NOT NULL,
                candidate_kind TEXT NOT NULL,
                section_id TEXT NOT NULL,
                title TEXT NOT NULL,
                searchable_text TEXT NOT NULL,
                location_json TEXT NOT NULL,
                locator_json TEXT NOT NULL
             );
             CREATE INDEX IF NOT EXISTS idx_lexical_search_candidates_v5_doc
                ON lexical_search_candidates_v5(doc_key);
             CREATE VIRTUAL TABLE IF NOT EXISTS lexical_search_fts_v5 USING fts5(
                lexemes,
                doc,
                content='',
                contentless_delete=1
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
struct StoredCompactLocator {
    owner_section_id: String,
    section_path: Vec<String>,
    paragraph_index: Option<usize>,
    sentence_index: Option<usize>,
    range_start: Option<usize>,
    range_end: Option<usize>,
    segmentation_version: Option<String>,
    native_location: Option<String>,
}

impl From<&TextLocator> for StoredCompactLocator {
    fn from(value: &TextLocator) -> Self {
        Self {
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

impl StoredCompactLocator {
    fn rebuild(
        self,
        document_id: &str,
        content_hash: &str,
        normalized_document_hash: &str,
    ) -> Result<TextLocator, ApplicationError> {
        let normalized_range = match (self.range_start, self.range_end) {
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
            document_id: DocumentId(document_id.to_owned()),
            content_hash: ContentHash(content_hash.to_owned()),
            normalized_document_hash: NormalizedDocumentHash(normalized_document_hash.to_owned()),
            owner_section_id: SectionId(self.owner_section_id),
            section_path: self.section_path,
            paragraph_index: self.paragraph_index,
            sentence_index: self.sentence_index,
            normalized_range,
            segmentation_version: self.segmentation_version,
            native_location: self.native_location,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use rusqlite::params;

    use super::*;
    use crate::domain::{MediaType, Section};

    fn fixture() -> Document {
        Document {
            id: DocumentId("doc:locator-v5".into()),
            source: DocumentSource("memory:locator-v5".into()),
            title: "系统机制".into(),
            media_type: MediaType("text/markdown".into()),
            content_hash: ContentHash("sha256:locator-v5".into()),
            metadata: BTreeMap::new(),
            root_sections: vec![Section {
                id: SectionId("section://virtual-memory".into()),
                parent_id: None,
                title: "虚拟内存机制".into(),
                level: 1,
                content: "地址空间隔离进程内存。页表把虚拟页映射到物理帧。\n\nUse read-cursor/v2 with std::sync::Arc safely.".into(),
                location: Location {
                    section_path: vec!["虚拟内存机制".into()],
                    native_location: Some("markdown:#virtual-memory".into()),
                    ..Location::default()
                },
                children: vec![],
            }],
        }
    }

    #[tokio::test]
    async fn compact_locator_round_trips_every_candidate() {
        let directory = tempfile::tempdir().expect("temp directory");
        let database = directory.path().join("locators.sqlite");
        let document = fixture();
        let expected = build_lexical_candidates(&document).expect("candidates");
        assert!(!expected.is_empty());

        let index = SqliteSearchIndex::open(&database).expect("index");
        index.index(&document).await.expect("index document");

        let connection = Connection::open(&database).expect("inspect");
        let document_row: (String, String) = connection
            .query_row(
                "SELECT content_hash, normalized_document_hash
                 FROM lexical_search_documents_v5 WHERE document_id = ?1",
                params![&document.id.0],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("document row");
        let mut statement = connection
            .prepare(
                "SELECT locator_json, source_order FROM lexical_search_candidates_v5
                 ORDER BY source_order",
            )
            .expect("candidate statement");
        let rows = statement
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            })
            .expect("candidate rows");

        let mut count = 0usize;
        for row in rows {
            let (locator_json, source_order) = row.expect("candidate row");
            let compact: StoredCompactLocator =
                serde_json::from_str(&locator_json).expect("compact locator");
            let rebuilt = compact
                .rebuild(&document.id.0, &document_row.0, &document_row.1)
                .expect("rebuilt locator");
            let expected_candidate = expected
                .iter()
                .find(|candidate| candidate.source_order == source_order as usize)
                .expect("expected candidate");
            assert_eq!(rebuilt, expected_candidate.text_locator);
            count += 1;
        }
        assert_eq!(count, expected.len());
    }
}
