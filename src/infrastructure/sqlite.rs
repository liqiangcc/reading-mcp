use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use async_trait::async_trait;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::application::ports::{ApplicationError, DocumentRepository, SearchHit, SearchIndex};
use crate::domain::{
    ContentHash, Document, DocumentId, DocumentSource, Location, MediaType, Section, SectionId,
};

pub struct SqliteDocumentRepository {
    path: PathBuf,
    connection: Mutex<Connection>,
}

impl SqliteDocumentRepository {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, ApplicationError> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|error| {
                ApplicationError::RepositoryFailed(format!("{}: {error}", parent.display()))
            })?;
        }
        let connection = Connection::open(&path).map_err(repository_error)?;
        connection
            .execute_batch(
                "PRAGMA journal_mode=WAL;
                 PRAGMA synchronous=NORMAL;
                 CREATE TABLE IF NOT EXISTS documents (
                    id TEXT PRIMARY KEY,
                    source TEXT NOT NULL,
                    content_hash TEXT NOT NULL,
                    document_json TEXT NOT NULL
                 );",
            )
            .map_err(repository_error)?;
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
impl DocumentRepository for SqliteDocumentRepository {
    async fn save(&self, document: Document) -> Result<(), ApplicationError> {
        let json = serde_json::to_string(&StoredDocument::from_document(&document))
            .map_err(|error| ApplicationError::RepositoryFailed(error.to_string()))?;
        let connection = self
            .connection
            .lock()
            .map_err(|_| ApplicationError::RepositoryFailed("SQLite lock poisoned".into()))?;
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
        Ok(())
    }

    async fn get(&self, id: &DocumentId) -> Result<Option<Document>, ApplicationError> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| ApplicationError::RepositoryFailed("SQLite lock poisoned".into()))?;
        let json = connection
            .query_row(
                "SELECT document_json FROM documents WHERE id = ?1",
                params![&id.0],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(repository_error)?;
        json.map(|json| {
            serde_json::from_str::<StoredDocument>(&json)
                .map(StoredDocument::into_document)
                .map_err(|error| ApplicationError::RepositoryFailed(error.to_string()))
        })
        .transpose()
    }
}

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
                 CREATE VIRTUAL TABLE IF NOT EXISTS search_units USING fts5(
                    document_id UNINDEXED,
                    section_id UNINDEXED,
                    title,
                    source UNINDEXED,
                    snippet,
                    location_json UNINDEXED,
                    body
                 );",
            )
            .map_err(index_error)?;
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
    async fn index(&self, document: &Document) -> Result<(), ApplicationError> {
        let mut units = Vec::new();
        for section in &document.root_sections {
            collect_search_units(document, section, &mut units);
        }

        let mut connection = self
            .connection
            .lock()
            .map_err(|_| ApplicationError::IndexFailed("SQLite lock poisoned".into()))?;
        let transaction = connection.transaction().map_err(index_error)?;
        transaction
            .execute(
                "DELETE FROM search_units WHERE document_id = ?1",
                params![&document.id.0],
            )
            .map_err(index_error)?;
        {
            let mut statement = transaction
                .prepare(
                    "INSERT INTO search_units(
                        document_id, section_id, title, source, snippet, location_json, body
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                )
                .map_err(index_error)?;
            for unit in units {
                let location_json = serde_json::to_string(&StoredLocation::from_location(&unit.location))
                    .map_err(|error| ApplicationError::IndexFailed(error.to_string()))?;
                statement
                    .execute(params![
                        &document.id.0,
                        unit.section_id.0,
                        unit.title,
                        document.source.0,
                        unit.snippet,
                        location_json,
                        unit.body
                    ])
                    .map_err(index_error)?;
            }
        }
        transaction.commit().map_err(index_error)?;
        Ok(())
    }

    async fn search(
        &self,
        document_id: &DocumentId,
        query: &str,
        limit: usize,
    ) -> Result<Vec<SearchHit>, ApplicationError> {
        if query.trim().is_empty() {
            return Err(ApplicationError::InvalidRequest(
                "search query must not be empty".into(),
            ));
        }
        if limit == 0 {
            return Ok(vec![]);
        }
        let fts_query = safe_fts_query(query)?;
        let connection = self
            .connection
            .lock()
            .map_err(|_| ApplicationError::IndexFailed("SQLite lock poisoned".into()))?;
        let mut statement = connection
            .prepare(
                "SELECT section_id, title, source, snippet, location_json, bm25(search_units)
                 FROM search_units
                 WHERE search_units MATCH ?1 AND document_id = ?2
                 ORDER BY bm25(search_units) ASC, section_id ASC
                 LIMIT ?3",
            )
            .map_err(index_error)?;
        let rows = statement
            .query_map(params![fts_query, &document_id.0, limit as i64], |row| {
                let location_json: String = row.get(4)?;
                let rank: f64 = row.get(5)?;
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    location_json,
                    rank,
                ))
            })
            .map_err(index_error)?;

        let mut hits = Vec::new();
        for row in rows {
            let (section_id, title, source, snippet, location_json, rank) =
                row.map_err(index_error)?;
            let location = serde_json::from_str::<StoredLocation>(&location_json)
                .map_err(|error| ApplicationError::IndexFailed(error.to_string()))?
                .into_location();
            hits.push(SearchHit {
                section_id: SectionId(section_id),
                title,
                source: DocumentSource(source),
                snippet,
                score: (1.0 / (1.0 + rank.abs())) as f32,
                location,
            });
        }
        Ok(hits)
    }
}

#[derive(Clone)]
struct SearchUnit {
    section_id: SectionId,
    title: String,
    snippet: String,
    body: String,
    location: Location,
}

fn collect_search_units(document: &Document, section: &Section, output: &mut Vec<SearchUnit>) {
    let paragraphs = section
        .content
        .split("\n\n")
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>();

    if paragraphs.is_empty() {
        output.push(SearchUnit {
            section_id: section.id.clone(),
            title: section.title.clone(),
            snippet: truncate(&section.title, 320),
            body: section.title.clone(),
            location: section.location.clone(),
        });
    } else {
        for (index, paragraph) in paragraphs.into_iter().enumerate() {
            let mut location = section.location.clone();
            location.paragraph = Some((index + 1) as u32);
            location.native_location = Some(match &section.location.native_location {
                Some(native) => format!("{native}#search-unit:{}", index + 1),
                None => format!("search-unit:{}", index + 1),
            });
            output.push(SearchUnit {
                section_id: section.id.clone(),
                title: section.title.clone(),
                snippet: truncate(paragraph, 320),
                body: format!("{}\n{paragraph}", section.title),
                location,
            });
        }
    }

    for child in &section.children {
        collect_search_units(document, child, output);
    }
}

fn safe_fts_query(query: &str) -> Result<String, ApplicationError> {
    let terms = query
        .split(|ch: char| ch.is_whitespace() || ch.is_ascii_punctuation())
        .filter(|term| !term.is_empty())
        .map(|term| format!("\"{}\"", term.replace('"', "\"\"")))
        .collect::<Vec<_>>();
    if terms.is_empty() {
        return Err(ApplicationError::InvalidRequest(
            "search query must contain searchable terms".into(),
        ));
    }
    Ok(terms.join(" AND "))
}

fn truncate(value: &str, max_chars: usize) -> String {
    value.chars().take(max_chars).collect()
}

fn repository_error(error: rusqlite::Error) -> ApplicationError {
    ApplicationError::RepositoryFailed(error.to_string())
}

fn index_error(error: rusqlite::Error) -> ApplicationError {
    ApplicationError::IndexFailed(error.to_string())
}

#[derive(Serialize, Deserialize)]
struct StoredDocument {
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

    fn into_document(self) -> Document {
        Document {
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
        }
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
