use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use super::sqlite_support::{open_connection, run_sqlite};
use crate::application::ports::{ApplicationError, DocumentRepository, TextUnitIndex};
use crate::domain::{
    ContentHash, Document, DocumentId, DocumentSource, Location, MediaType, NORMALIZATION_VERSION,
    NormalizedDocumentHash, NormalizedTextRange, Section, SectionId, TextUnit, TextUnitId,
    TextUnitKind,
};

pub struct SqliteDocumentRepository {
    path: PathBuf,
    connection: Arc<Mutex<Connection>>,
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
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

#[async_trait]
impl DocumentRepository for SqliteDocumentRepository {
    async fn save(&self, document: Document) -> Result<(), ApplicationError> {
        document
            .validate_ocr_publication()
            .map_err(ApplicationError::RepositoryFailed)?;
        let json = serde_json::to_string(&StoredDocument::from_document(&document))
            .map_err(|error| ApplicationError::RepositoryFailed(error.to_string()))?;
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
                Ok(())
            },
        )
        .await
    }

    async fn get(&self, id: &DocumentId) -> Result<Option<Arc<Document>>, ApplicationError> {
        let id = id.clone();
        run_sqlite(
            &self.connection,
            ApplicationError::RepositoryFailed,
            move |connection| {
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
                    Ok(Arc::new(document))
                })
                .transpose()
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
        Ok(Self {
            path,
            connection: Arc::new(Mutex::new(connection)),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
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
