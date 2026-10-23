use std::borrow::Cow;
use std::collections::HashMap;
use std::collections::{BTreeMap, HashSet};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

use async_trait::async_trait;
use flate2::Compression;
use flate2::read::ZlibDecoder;
use flate2::write::ZlibEncoder;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use super::lexical::{
    LexicalQuery, ParagraphPrefilters, candidate_kind_order, lowercase_fast,
    score_lowered_with_phrase,
};
use super::observability::{DocumentDecodePhases, emit_document_decode_miss, emit_scan_prewarm};
use super::section_layout::{
    CHUNK_CHARS, LAYOUT_CHUNK_BYTES, LayoutCoverage, LayoutParagraph, LayoutSentence, SectionLayout,
};
use super::sqlite_support::{open_connection, run_sqlite};
use crate::application::get_text_units::{
    EffectiveTextUnitKind, RequestedTextUnitKind, TextUnitCoveragePolicy, TextUnitDirection,
    TextUnitEnumerationCoverage,
};
use crate::application::open_document::OpenDocumentResult;
use crate::application::ports::{
    ApplicationError, BodyOrderWindow, DocumentHeader, DocumentRepository, OpenResultStore,
    OwnedStreamUnit, SearchHitKind, SectionRow, SectionTextUnits, StoredStreamItem, TextUnitIndex,
    TextUnitStreamKey, TextUnitStreamSummary,
};
use crate::application::section_rows;
use crate::application::text_unit_stream::{
    ParagraphCoverageDesc, ParagraphDesc, SectionCoverageDesc, SectionStreamCounts, SentenceDesc,
    UnitRef, walk_section_stream,
};
use crate::domain::{
    ContentHash, Document, DocumentId, DocumentSegmentation, DocumentSource, Location, MediaType,
    NORMALIZATION_VERSION, NormalizedDocumentHash, NormalizedTextIndex, NormalizedTextRange,
    ParagraphContentClass, ParagraphSectionCoverage, ParagraphTextUnitSet, Section, SectionId,
    SectionSegmentation, SentenceEligibility, SentenceParagraphCoverage, SentenceTextUnit,
    SentenceTextUnitSet, TEXT_SEGMENTATION_VERSION, TextUnit, TextUnitId, TextUnitKind,
    paragraph_text_unit_id, sentence_text_unit_id,
};

/// How many MiB of section text the scan text cache keeps by default.
const DEFAULT_SCAN_TEXT_CACHE_MIB: usize = 48;

/// The cap of the scan text cache in bytes, from `READING_MCP_SEARCH_CACHE_MIB`. 0 disables the
/// cache, and a value that is not a number falls back to the default.
fn scan_text_cache_bytes() -> usize {
    let mib = match std::env::var("READING_MCP_SEARCH_CACHE_MIB") {
        Ok(value) => value.trim().parse().unwrap_or(DEFAULT_SCAN_TEXT_CACHE_MIB),
        Err(_) => DEFAULT_SCAN_TEXT_CACHE_MIB,
    };
    mib.saturating_mul(1024 * 1024)
}

/// The fewest normalized chars of section text a scan splits across threads: 1 MiB. UTF-8 bytes are
/// never fewer than chars, so a document below this is below 1 MiB of text as well.
const MIN_PARALLEL_TEXT_CHARS: usize = 1024 * 1024;

/// The most threads a scan uses, whatever the host reports.
const MAX_SCAN_THREADS: usize = 4;

/// The thread count a scan asks for: `READING_MCP_SEARCH_THREADS`, `min(available_parallelism, 4)` by
/// default. 1 is serial, and a value that is not a number falls back to the default.
fn scan_thread_count() -> usize {
    static HOST: OnceLock<usize> = OnceLock::new();
    let host = *HOST.get_or_init(|| {
        std::thread::available_parallelism()
            .map(usize::from)
            .unwrap_or(1)
            .clamp(1, MAX_SCAN_THREADS)
    });
    match std::env::var("READING_MCP_SEARCH_THREADS") {
        Ok(value) => value.trim().parse().unwrap_or(host),
        Err(_) => host,
    }
    .clamp(1, MAX_SCAN_THREADS)
}

/// One section's text as a scan reads it, with the lowercase form every pass over it needs.
struct ScanSectionText {
    text: Arc<str>,
    lowered: LoweredSectionText,
}

enum LoweredSectionText {
    /// `lowercase_fast` borrowed the text: it lowercases to itself, so every slice of it does too.
    Borrowed,
    /// The lowercase copy, kept because lowercasing changes the text.
    Owned(Arc<str>),
}

impl ScanSectionText {
    fn new(text: String) -> Self {
        let lowered = match lowercase_fast(&text) {
            Cow::Borrowed(_) => LoweredSectionText::Borrowed,
            Cow::Owned(lowered) => LoweredSectionText::Owned(Arc::from(lowered)),
        };
        Self {
            text: Arc::from(text),
            lowered,
        }
    }

    fn text(&self) -> &str {
        &self.text
    }

    /// Whether the whole section lowercases to itself, so every slice of it does too.
    fn lowered_is_borrowed(&self) -> bool {
        matches!(self.lowered, LoweredSectionText::Borrowed)
    }

    fn lowered(&self) -> &str {
        match &self.lowered {
            LoweredSectionText::Borrowed => self.text.as_ref(),
            LoweredSectionText::Owned(lowered) => lowered.as_ref(),
        }
    }

    /// The bytes this entry holds: the text plus the owned lowercase copy.
    fn bytes(&self) -> usize {
        self.text.len()
            + match &self.lowered {
                LoweredSectionText::Borrowed => 0,
                LoweredSectionText::Owned(lowered) => lowered.len(),
            }
    }
}

/// The section texts cached for one document, and whether the document outgrew the cap.
#[derive(Default)]
struct ScanTextSections {
    texts: HashMap<i64, Arc<ScanSectionText>>,
    bytes: usize,
    /// The document's text outgrew the cap, so nothing is cached for it any more.
    over_cap: bool,
}

/// The section texts of the most recently scanned document. The store is keyed by the stored document
/// meta and checked against it on every scan, so a replaced segmentation, a re-store or a removal
/// leaves it unused; `write_document_sections` drops it as well.
struct ScanTextStore {
    doc_key: i64,
    normalized_document_hash: NormalizedDocumentHash,
    cap: usize,
    sections: Mutex<ScanTextSections>,
}

impl ScanTextStore {
    fn new(meta: &StoredUnitDocument, cap: usize) -> Self {
        Self {
            doc_key: meta.doc_key,
            normalized_document_hash: meta.normalized_document_hash.clone(),
            cap,
            sections: Mutex::new(ScanTextSections::default()),
        }
    }

    /// Whether this store holds `meta`'s document: the same stored row and the same normalized text.
    fn matches(&self, meta: &StoredUnitDocument) -> bool {
        self.doc_key == meta.doc_key
            && self.normalized_document_hash == meta.normalized_document_hash
    }

    fn get(&self, section_key: i64) -> Option<Arc<ScanSectionText>> {
        let sections = self.sections.lock().ok()?;
        sections.texts.get(&section_key).map(Arc::clone)
    }

    /// Caches one section's text. The section that takes the document over the cap drops everything
    /// cached for it: a document over the cap is not cached at all. A section already held is left
    /// alone, so a warm that races a scan over the same section counts its bytes once.
    fn insert(&self, section_key: i64, text: Arc<ScanSectionText>) {
        let Ok(mut sections) = self.sections.lock() else {
            return;
        };
        if sections.over_cap || sections.texts.contains_key(&section_key) {
            return;
        }
        sections.bytes += text.bytes();
        if sections.bytes > self.cap {
            *sections = ScanTextSections {
                over_cap: true,
                ..Default::default()
            };
            return;
        }
        sections.texts.insert(section_key, text);
    }

    /// Whether the document's text outgrew the cap, so it is scanned one section at a time.
    fn over_cap(&self) -> bool {
        self.sections
            .lock()
            .map(|sections| sections.over_cap)
            .unwrap_or(true)
    }

    /// Records that the document's text cannot be cached at all, without reading any of it: a scan of
    /// it then takes the one-section-at-a-time path from the start, as it does after its first pass.
    fn mark_over_cap(&self) {
        if let Ok(mut sections) = self.sections.lock() {
            *sections = ScanTextSections {
                over_cap: true,
                ..Default::default()
            };
        }
    }
}

/// The scan text store of `meta`'s document: the cached one when it holds that very document, else a
/// new one that replaces it. `None` when the cache is off or its cap is zero.
fn scan_text_store(
    cache: &Mutex<Option<Arc<ScanTextStore>>>,
    meta: &StoredUnitDocument,
    options: ScanOptions,
) -> Option<Arc<ScanTextStore>> {
    if !options.cache {
        return None;
    }
    let cap = scan_text_cache_bytes();
    if cap == 0 {
        return None;
    }
    let mut cached = cache.lock().ok()?;
    if let Some(store) = cached.as_ref()
        && store.matches(meta)
    {
        return Some(Arc::clone(store));
    }
    let store = Arc::new(ScanTextStore::new(meta, cap));
    *cached = Some(Arc::clone(&store));
    Some(store)
}

/// Whether the scan text cache is warmed in the background after an open, from
/// `READING_MCP_SCAN_PREWARM`: on unless it is `0`, `false`, `no` or `off`. A value that is neither
/// form falls back to on here; `RuntimeConfig::from_env` rejects it at startup instead.
fn scan_prewarm_enabled() -> bool {
    match std::env::var("READING_MCP_SCAN_PREWARM") {
        Ok(value) => !matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "0" | "false" | "no" | "off"
        ),
        Err(_) => true,
    }
}

/// Whether `store` is still the scan text store of the cache: no other document took its slot and it
/// was not dropped.
fn scan_text_cache_holds(
    cache: &Mutex<Option<Arc<ScanTextStore>>>,
    store: &Arc<ScanTextStore>,
) -> bool {
    cache
        .lock()
        .ok()
        .and_then(|cached| cached.as_ref().map(|cached| Arc::ptr_eq(cached, store)))
        .unwrap_or(false)
}

/// The scan text store a warm fills: the cached one when it holds `meta`'s document, a new one when
/// the cache is empty, and `None` when another document holds the slot or the cap is 0. A warm never
/// takes the slot from another document: that document's cached text stays until the next search
/// replaces it, exactly as it does today.
fn scan_text_store_for_warm(
    cache: &Mutex<Option<Arc<ScanTextStore>>>,
    meta: &StoredUnitDocument,
) -> Option<Arc<ScanTextStore>> {
    let cap = scan_text_cache_bytes();
    if cap == 0 {
        return None;
    }
    let mut cached = cache.lock().ok()?;
    match cached.as_ref() {
        Some(store) if store.matches(meta) => Some(Arc::clone(store)),
        Some(_) => None,
        None => {
            let store = Arc::new(ScanTextStore::new(meta, cap));
            *cached = Some(Arc::clone(&store));
            Some(store)
        }
    }
}

/// The warm's plan: the store to fill, the document's `doc_key`, the sections a scan would read with
/// the bytes each holds, and the bytes the warm may still read. `None` when there is nothing to warm:
/// the knob is off, the cache is off or its cap is 0, the document's units are not stored for this
/// hash, another document holds the cache slot, or the document's text is larger than the cap.
fn scan_warm_plan(
    connection: &Mutex<Connection>,
    cache: &Mutex<Option<Arc<ScanTextStore>>>,
    document_id: &DocumentId,
    expected_hash: &NormalizedDocumentHash,
) -> Result<Option<WarmPlan>, ApplicationError> {
    if !scan_prewarm_enabled() || scan_text_cache_bytes() == 0 {
        return Ok(None);
    }
    let cap = scan_text_cache_bytes();
    let lock = || {
        connection
            .lock()
            .map_err(|_| ApplicationError::TextUnitIndexFailed("SQLite lock poisoned".into()))
    };
    let (meta, sections) = {
        let connection = lock()?;
        let Some(meta) = load_stored_document(&connection, document_id)? else {
            return Ok(None);
        };
        if meta.segmentation_version != TEXT_SEGMENTATION_VERSION
            || meta.normalized_document_hash != *expected_hash
        {
            return Ok(None);
        }
        let sections = stored_sections(&connection, meta.doc_key)?
            .into_iter()
            .filter(|(_, _, char_len)| *char_len > 0)
            .map(|(section_key, _, char_len)| (section_key, char_len))
            .collect::<Vec<_>>();
        (meta, sections)
    };
    let chars: usize = sections.iter().map(|(_, char_len)| *char_len).sum();
    // A UTF-8 char is at most four bytes, so a document of at most `cap / 4` chars certainly fits in
    // `cap` bytes and the warm needs no byte sizes at all.
    if chars <= cap / 4 {
        // The store a scan would fill when the cache is free or already holds this document; another
        // document's cached text is left alone.
        let Some(store) = scan_text_store_for_warm(cache, &meta) else {
            return Ok(None);
        };
        return Ok(Some(WarmPlan {
            store,
            doc_key: meta.doc_key,
            sections,
            budget: usize::MAX,
        }));
    }
    // A section of more than `cap / 4` chars may alone be larger than the cap, and measuring it would
    // read that much for nothing: such a document is left alone entirely, and its first search does
    // exactly what it does without a warm.
    if sections.iter().any(|(_, char_len)| *char_len > cap / 4) {
        return Ok(None);
    }
    // The document may or may not fit, and no section is large. Measure the stored text bytes of one
    // section at a time, each under its own short connection lock, and give up as soon as the total
    // is over the cap: a document over the cap is then marked over the cap without its text ever
    // being read into memory, which is the state its first scan would end in anyway.
    let mut measured = Vec::with_capacity(sections.len());
    let mut total = 0usize;
    for (section_key, _) in &sections {
        let bytes = {
            let connection = lock()?;
            stored_section_text_bytes(&connection, meta.doc_key, *section_key)?
        };
        total += bytes;
        if total > cap {
            let Some(store) = scan_text_store_for_warm(cache, &meta) else {
                return Ok(None);
            };
            store.mark_over_cap();
            return Ok(None);
        }
        measured.push((*section_key, bytes));
    }
    let Some(store) = scan_text_store_for_warm(cache, &meta) else {
        return Ok(None);
    };
    Ok(Some(WarmPlan {
        store,
        doc_key: meta.doc_key,
        sections: measured,
        budget: cap,
    }))
}

/// What one warm will do: the store to fill, the document, the sections to read with the bytes each
/// holds, and how many bytes of section text it may still read.
struct WarmPlan {
    store: Arc<ScanTextStore>,
    doc_key: i64,
    /// `(section_key, bytes)` in section order. The sections whose text a scan would read.
    sections: Vec<(i64, usize)>,
    /// The bytes the warm may read in total: `usize::MAX` when the document certainly fits.
    budget: usize,
}

/// Fills `plan.store` with the text of its sections, one section at a time, in the order a scan reads
/// them and with the loader a scan uses, so the entries are exactly what a first scan would store.
///
/// Stops at once when the document outgrew the cap (`over_cap`), when the cache no longer holds the
/// store (the document was replaced, or another document took the slot), when the next section does
/// not fit the remaining byte budget, and after `steps` sections. The connection lock is held for one
/// section's read only and dropped before the text is lowered and inserted, so foreground reads
/// interleave with the warm.
fn warm_scan_text(
    connection: &Mutex<Connection>,
    cache: &Mutex<Option<Arc<ScanTextStore>>>,
    plan: &WarmPlan,
    steps: usize,
) -> Result<usize, ApplicationError> {
    let mut warmed = 0;
    let mut budget = plan.budget;
    for (section_key, bytes) in plan.sections.iter().take(steps) {
        if plan.store.over_cap() || !scan_text_cache_holds(cache, &plan.store) {
            break;
        }
        if *bytes > budget {
            break;
        }
        let text = {
            let connection = connection.lock().map_err(|_| {
                ApplicationError::TextUnitIndexFailed("SQLite lock poisoned".into())
            })?;
            load_section_text_unchecked(&connection, plan.doc_key, *section_key)?
        };
        plan.store
            .insert(*section_key, Arc::new(ScanSectionText::new(text)));
        budget = budget.saturating_sub(*bytes);
        warmed += 1;
        // The warm takes the connection lock once per section, and the scan's foreground reads take
        // the same lock. Nothing is locked here, so yielding gives a waiting foreground read the CPU
        // and the lock before the warm asks for it again: without it a foreground read queues behind
        // one section read after another. Sleeping instead would only spread the collisions over more
        // foreground calls, which measured worse.
        std::thread::yield_now();
    }
    Ok(warmed)
}

/// The whole warm of one document: the plan, then every section it names.
fn warm_scan_document(
    connection: &Mutex<Connection>,
    cache: &Mutex<Option<Arc<ScanTextStore>>>,
    document_id: &DocumentId,
    expected_hash: &NormalizedDocumentHash,
) -> Result<usize, ApplicationError> {
    let Some(plan) = scan_warm_plan(connection, cache, document_id, expected_hash)? else {
        return Ok(0);
    };
    warm_scan_text(connection, cache, &plan, usize::MAX)
}

/// The stored text bytes of one section of `doc_key`: what a warm would read for it, which is the
/// text the scan cache will hold. A raw chunk is its own length; a zlib chunk is estimated at
/// [`SECTION_TEXT_ZLIB_EXPANSION_ESTIMATE`] times its stored length, because decompressing it here
/// would read the text the plan exists to avoid reading.
fn stored_section_text_bytes(
    connection: &Connection,
    doc_key: i64,
    section_key: i64,
) -> Result<usize, ApplicationError> {
    let bytes: i64 = connection
        .query_row(
            "SELECT COALESCE(SUM(CASE WHEN encoding = ?3
                                      THEN length(data) * ?4
                                      ELSE length(data) END), 0)
             FROM section_text_v3
             WHERE doc_key = ?1 AND section_key = ?2",
            params![
                doc_key,
                section_key,
                SECTION_TEXT_ENCODING_ZLIB,
                i64::try_from(SECTION_TEXT_ZLIB_EXPANSION_ESTIMATE).unwrap_or(i64::MAX),
            ],
            |row| row.get(0),
        )
        .map_err(text_unit_index_error)?;
    i64_to_usize(bytes, "section text bytes")
}

/// How a lexical scan may use the per-document scan text cache, threads and score pruning.
///
/// The plain options — no cache, one thread, no pruning — are exactly the scan of R2c step 3, and
/// exist so that tests can prove the fast scan equal to it.
#[doc(hidden)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScanOptions {
    /// Whether the per-document scan text cache may be used.
    pub cache: bool,
    /// The most threads a scan may use; 1 is serial.
    pub threads: usize,
    /// Whether units whose score bound cannot reach the current threshold are skipped.
    pub prune: bool,
}

impl ScanOptions {
    /// The scan production uses: the cache and the pruning always, and the thread count the
    /// environment asks for.
    pub fn production() -> Self {
        Self {
            cache: true,
            threads: scan_thread_count(),
            prune: true,
        }
    }

    /// The scan of R2c step 3: no cache, one thread, no pruning.
    pub fn plain() -> Self {
        Self {
            cache: false,
            threads: 1,
            prune: false,
        }
    }
}

impl Default for ScanOptions {
    fn default() -> Self {
        Self::production()
    }
}

pub struct SqliteDocumentRepository {
    path: PathBuf,
    connection: Arc<Mutex<Connection>>,
    /// Whether a full decode reports its phases; only when telemetry is on.
    decode_timing: bool,
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
                 );
                 CREATE TABLE IF NOT EXISTS document_meta_v1 (
                    document_id TEXT NOT NULL,
                    key TEXT NOT NULL,
                    value TEXT NOT NULL,
                    UNIQUE(document_id, key)
                 );
                 CREATE TABLE IF NOT EXISTS document_meta_v2 (
                    document_id TEXT NOT NULL,
                    key TEXT NOT NULL,
                    encoding TEXT NOT NULL,
                    value BLOB NOT NULL,
                    UNIQUE(document_id, key)
                 );
                 CREATE TABLE IF NOT EXISTS document_header_v1 (
                    document_id TEXT NOT NULL UNIQUE,
                    source TEXT NOT NULL,
                    title TEXT NOT NULL,
                    media_type TEXT NOT NULL,
                    content_hash TEXT NOT NULL,
                    normalized_document_hash TEXT,
                    section_count INTEGER NOT NULL,
                    root_count INTEGER NOT NULL,
                    body_order_error TEXT
                 );
                 CREATE TABLE IF NOT EXISTS document_sections_v1 (
                    document_id TEXT NOT NULL,
                    section_key INTEGER NOT NULL,
                    section_id TEXT NOT NULL,
                    parent_key INTEGER,
                    level INTEGER NOT NULL,
                    title TEXT NOT NULL,
                    location_json TEXT NOT NULL,
                    subtree_end_key INTEGER NOT NULL,
                    UNIQUE(document_id, section_key)
                 );
                 CREATE UNIQUE INDEX IF NOT EXISTS idx_document_sections_v1_id
                    ON document_sections_v1(document_id, section_id);
                 CREATE INDEX IF NOT EXISTS idx_document_sections_v1_parent
                    ON document_sections_v1(document_id, parent_key, section_key);
                 CREATE TABLE IF NOT EXISTS document_body_order_rows_v1 (
                    document_id TEXT NOT NULL,
                    ord INTEGER NOT NULL,
                    section_key INTEGER NOT NULL,
                    UNIQUE(document_id, ord)
                 );
                 CREATE UNIQUE INDEX IF NOT EXISTS idx_document_body_order_rows_v1_key
                    ON document_body_order_rows_v1(document_id, section_key);",
            )
            .map_err(repository_error)?;
        ensure_normalized_document_hash_column(&connection)?;
        ensure_body_order_error_column(&connection)?;
        Ok(Self {
            path,
            connection: Arc::new(Mutex::new(connection)),
            decode_timing: telemetry_enabled(),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The stored `document_json` row of one document, byte for byte, or `None` when the row is
    /// absent. Test-only: it is how a test compares a save against a committed golden without
    /// reaching into the database itself.
    #[cfg(test)]
    pub(crate) fn stored_document_json(&self, id: &DocumentId) -> Option<String> {
        let connection = self.connection.lock().expect("repository connection");
        connection
            .query_row(
                "SELECT document_json FROM documents WHERE id = ?1",
                params![id.0],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .ok()
            .flatten()
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
        run_sqlite(
            &self.connection,
            ApplicationError::RepositoryFailed,
            move |connection| {
                // The section text stays out of `document_json` only when the very text of this
                // document version is already stored: a repository used on its own, which never
                // stored it, keeps the text inline exactly as before. The decision and the insert
                // share this connection, so no other write of this process can land between them.
                let content_store = if section_text_is_stored(
                    connection,
                    &document,
                    &normalized_document_hash,
                )? {
                    Some(SECTION_TEXT_CONTENT_STORE)
                } else {
                    None
                };
                let (light_metadata, heavy_metadata) = split_metadata(&document.metadata);
                let metadata_store = (!heavy_metadata.is_empty()).then_some(DOCUMENT_META_STORE);
                // The body order costs an `epub_structure_map` parse, which is the largest metadata
                // value of a book, so it is computed once here — from the document in hand — and
                // stored, instead of on every `read`.
                let body_order = crate::application::body_order_ids(&document);
                let json = crate::json::to_string_exact(&StoredDocument::with_stores(
                    &document,
                    content_store,
                    light_metadata,
                    metadata_store,
                ))
                .map_err(|error| ApplicationError::RepositoryFailed(error.to_string()))?;
                // The `documents` row and its metadata rows are replaced in one
                // transaction, so a document never keeps metadata rows of another version.
                let transaction = connection.transaction().map_err(repository_error)?;
                transaction
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
                // Rows of both metadata stores are deleted, so a document written by this version
                // never keeps the rows of a version that used the other table.
                transaction
                    .execute(
                        "DELETE FROM document_meta_v1 WHERE document_id = ?1",
                        params![document.id.0],
                    )
                    .map_err(repository_error)?;
                transaction
                    .execute(
                        "DELETE FROM document_meta_v2 WHERE document_id = ?1",
                        params![document.id.0],
                    )
                    .map_err(repository_error)?;
                if !heavy_metadata.is_empty() {
                    let mut statement = transaction
                        .prepare_cached(
                            "INSERT INTO document_meta_v2(document_id, key, encoding, value) VALUES (?1, ?2, ?3, ?4)",
                        )
                        .map_err(repository_error)?;
                    for (key, value) in &heavy_metadata {
                        // Borrowed from the document: the compressor reads the value in place.
                        let (key, value) = (*key, *value);
                        let (encoding, blob) = encode_metadata_value(value)?;
                        statement
                            .execute(params![document.id.0, key, encoding, blob])
                            .map_err(repository_error)?;
                    }
                }
                store_section_rows(
                    &transaction,
                    &document,
                    &normalized_document_hash,
                    &body_order,
                )?;
                transaction.commit().map_err(repository_error)?;
                Ok(())
            },
        )
        .await
    }

    async fn get(&self, id: &DocumentId) -> Result<Option<Arc<Document>>, ApplicationError> {
        Ok(self.get_with_hash(id).await?.map(|(document, _)| document))
    }

    /// The full document and its normalized hash. Every call decodes the row: there is no
    /// in-process document cache.
    async fn get_with_hash(
        &self,
        id: &DocumentId,
    ) -> Result<Option<(Arc<Document>, NormalizedDocumentHash)>, ApplicationError> {
        let id = id.clone();
        let decode_timing = self.decode_timing;
        run_sqlite(
            &self.connection,
            ApplicationError::RepositoryFailed,
            move |connection| load_document_row(connection, &id, decode_timing),
        )
        .await
    }

    /// The full document rebuilt from the stored section rows, plus its normalized hash: the one
    /// call a path that needs a whole `Document` uses now that `document_json` keeps no tree. With
    /// `with_text` the section text comes from `section_text_v3`; without it the tree is built and
    /// every section's `content` stays empty.
    async fn get_rebuilt_with_hash(
        &self,
        id: &DocumentId,
        with_text: bool,
    ) -> Result<Option<(Arc<Document>, NormalizedDocumentHash)>, ApplicationError> {
        if with_text {
            return self.get_with_hash(id).await;
        }
        self.get_light_with_hash(id).await
    }

    /// No section text and no metadata-table read: a `Document` that carries only the light
    /// metadata entries and empty section `content`. The section tree comes from
    /// `document_sections_v1` when the row's `section_tree_store` says so, so a light read never
    /// returns an empty tree for a book. A row written before the metadata split existed keeps
    /// every entry inline, so its light metadata is that map split the same way a save splits it; a
    /// row without a stored normalized hash, or a row with OCR metadata, takes the full path
    /// instead, so every field of its result matches `get_with_hash`.
    async fn get_light_with_hash(
        &self,
        id: &DocumentId,
    ) -> Result<Option<(Arc<Document>, NormalizedDocumentHash)>, ApplicationError> {
        let id = id.clone();
        let decode_timing = self.decode_timing;
        run_sqlite(
            &self.connection,
            ApplicationError::RepositoryFailed,
            move |connection| {
                let row = connection
                    .query_row(
                        "SELECT document_json, normalized_document_hash FROM documents WHERE id = ?1",
                        params![&id.0],
                        |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?)),
                    )
                    .optional()
                    .map_err(repository_error)?;
                let Some((json, stored_hash)) = row else {
                    return Ok(None);
                };
                let stored = serde_json::from_str::<StoredDocument>(&json)
                    .map_err(|error| ApplicationError::RepositoryFailed(error.to_string()))?;
                // A row without a stored normalized hash would have to be hashed over the section
                // text, so it takes the full path, which computes and stores the hash.
                let Some(stored_hash) = stored_hash else {
                    return load_document_row(connection, &id, decode_timing);
                };
                // `validate_ocr_publication` reads the OCR metadata, which may be heavy and so
                // live in a metadata table rather than in the light map. A document with any OCR
                // metadata entry therefore takes the full path, so that its validation outcome is
                // the one `get_with_hash` reports. Without OCR metadata there is nothing to
                // validate and the light path skips the call.
                if has_ocr_metadata(connection, &id, &stored.metadata)? {
                    return load_document_row(connection, &id, decode_timing);
                }
                let document = stored.into_document_light(connection)?;
                Ok(Some((Arc::new(document), NormalizedDocumentHash(stored_hash))))
            },
        )
        .await
    }

    async fn load_metadata(
        &self,
        id: &DocumentId,
        key: &str,
    ) -> Result<Option<String>, ApplicationError> {
        let id = id.clone();
        let key = key.to_owned();
        run_sqlite(
            &self.connection,
            ApplicationError::RepositoryFailed,
            move |connection| load_stored_metadata(connection, &id, &key),
        )
        .await
    }

    async fn load_section_text(
        &self,
        id: &DocumentId,
        section_id: &SectionId,
    ) -> Result<Option<String>, ApplicationError> {
        let id = id.clone();
        let section_id = section_id.clone();
        run_sqlite(
            &self.connection,
            ApplicationError::RepositoryFailed,
            move |connection| load_one_section_text(connection, &id, &section_id),
        )
        .await
    }

    async fn load_section_char_len(
        &self,
        id: &DocumentId,
        section_id: &SectionId,
    ) -> Result<Option<usize>, ApplicationError> {
        let id = id.clone();
        let section_id = section_id.clone();
        run_sqlite(
            &self.connection,
            ApplicationError::RepositoryFailed,
            move |connection| load_one_section_char_len(connection, &id, &section_id),
        )
        .await
    }

    async fn body_order(
        &self,
        id: &DocumentId,
    ) -> Result<Option<Result<Vec<SectionId>, ApplicationError>>, ApplicationError> {
        let id = id.clone();
        run_sqlite(
            &self.connection,
            ApplicationError::RepositoryFailed,
            move |connection| stored_or_computed_body_order(connection, &id),
        )
        .await
    }

    /// One `document_header_v1` row. A document saved before that table existed has none, so its
    /// header is built from the document itself, exactly as the trait default builds it.
    async fn document_header(
        &self,
        id: &DocumentId,
    ) -> Result<Option<DocumentHeader>, ApplicationError> {
        let id = id.clone();
        run_sqlite(
            &self.connection,
            ApplicationError::RepositoryFailed,
            move |connection| {
                if let Some(header) = load_document_header(connection, &id)? {
                    return Ok(Some(header));
                }
                Ok(load_document_row(connection, &id, false)?
                    .map(|(document, hash)| section_rows::header_of(&document, &hash)))
            },
        )
        .await
    }

    /// One section row, by id: one unique-index row read.
    async fn section_by_id(
        &self,
        id: &DocumentId,
        section_id: &SectionId,
    ) -> Result<Option<SectionRow>, ApplicationError> {
        let id = id.clone();
        let section_id = section_id.clone();
        run_sqlite(
            &self.connection,
            ApplicationError::RepositoryFailed,
            move |connection| {
                let row = load_section_row_by_id(connection, &id, &section_id)?;
                if row.is_some() || document_has_section_rows(connection, &id)? {
                    return Ok(row);
                }
                Ok(load_document_row(connection, &id, false)?
                    .and_then(|(document, _)| section_rows::section_by_id(&document, &section_id)))
            },
        )
        .await
    }

    /// One section row, by key: one unique-index row read.
    async fn section_by_key(
        &self,
        id: &DocumentId,
        section_key: i64,
    ) -> Result<Option<SectionRow>, ApplicationError> {
        let id = id.clone();
        run_sqlite(
            &self.connection,
            ApplicationError::RepositoryFailed,
            move |connection| {
                let row = load_section_row_by_key(connection, &id, section_key)?;
                if row.is_some() || document_has_section_rows(connection, &id)? {
                    return Ok(row);
                }
                Ok(load_document_row(connection, &id, false)?
                    .and_then(|(document, _)| section_rows::section_by_key(&document, section_key)))
            },
        )
        .await
    }

    /// The rows of one key range: one range scan of the `(document_id, section_key)` index.
    async fn sections_in_key_range(
        &self,
        id: &DocumentId,
        from_key: i64,
        to_key_inclusive: i64,
        limit: usize,
    ) -> Result<Vec<SectionRow>, ApplicationError> {
        let id = id.clone();
        run_sqlite(
            &self.connection,
            ApplicationError::RepositoryFailed,
            move |connection| {
                let rows =
                    load_section_rows_in_range(connection, &id, from_key, to_key_inclusive, limit)?;
                if !rows.is_empty() || document_has_section_rows(connection, &id)? {
                    return Ok(rows);
                }
                Ok(load_document_row(connection, &id, false)?.map_or_else(
                    Vec::new,
                    |(document, _)| {
                        section_rows::sections_in_key_range(
                            &document,
                            from_key,
                            to_key_inclusive,
                            limit,
                        )
                    },
                ))
            },
        )
        .await
    }

    /// The children of one section: one `(document_id, parent_key, section_key)` index range read.
    async fn section_children(
        &self,
        id: &DocumentId,
        parent_key: Option<i64>,
        limit: usize,
    ) -> Result<Vec<SectionRow>, ApplicationError> {
        let id = id.clone();
        run_sqlite(
            &self.connection,
            ApplicationError::RepositoryFailed,
            move |connection| {
                let rows = load_section_children(connection, &id, parent_key, limit)?;
                if !rows.is_empty() || document_has_section_rows(connection, &id)? {
                    return Ok(rows);
                }
                Ok(load_document_row(connection, &id, false)?
                    .map_or_else(Vec::new, |(document, _)| {
                        section_rows::section_children(&document, parent_key, limit)
                    }))
            },
        )
        .await
    }

    /// The ancestors of one section: one row read per level.
    async fn section_ancestors(
        &self,
        id: &DocumentId,
        section_key: i64,
    ) -> Result<Vec<SectionRow>, ApplicationError> {
        let id = id.clone();
        run_sqlite(
            &self.connection,
            ApplicationError::RepositoryFailed,
            move |connection| {
                let rows = load_section_ancestors(connection, &id, section_key)?;
                if !rows.is_empty() || document_has_section_rows(connection, &id)? {
                    return Ok(rows);
                }
                Ok(load_document_row(connection, &id, false)?
                    .map_or_else(Vec::new, |(document, _)| {
                        section_rows::section_ancestors(&document, section_key)
                    }))
            },
        )
        .await
    }

    /// The body-order window around one section: the position, the length and the window rows, each
    /// one `document_body_order_rows_v1` index read.
    async fn body_order_window(
        &self,
        id: &DocumentId,
        section_key: i64,
        before: usize,
        after: usize,
    ) -> Result<Option<BodyOrderWindow>, ApplicationError> {
        let id = id.clone();
        run_sqlite(
            &self.connection,
            ApplicationError::RepositoryFailed,
            move |connection| {
                if let Some(window) =
                    load_body_order_window(connection, &id, section_key, before, after)?
                {
                    return Ok(Some(window));
                }
                // Rows exist but the section is not among them: a document whose body order could
                // not be computed reports that same error, and anything else is simply not in the
                // body order.
                if document_has_section_rows(connection, &id)? {
                    return match stored_or_computed_body_order(connection, &id)? {
                        Some(Err(error)) => Err(error),
                        _ => Ok(None),
                    };
                }
                body_order_window_from_document(connection, &id, section_key, before, after)
            },
        )
        .await
    }

    /// The body-order position of one section: one unique-index row read on
    /// `(document_id, section_key)`, with no `COUNT(*)` over the whole order.
    async fn body_order_position(
        &self,
        id: &DocumentId,
        section_key: i64,
    ) -> Result<Option<usize>, ApplicationError> {
        let id = id.clone();
        run_sqlite(
            &self.connection,
            ApplicationError::RepositoryFailed,
            move |connection| {
                if let Some(ord) = load_body_order_position(connection, &id, section_key)? {
                    return Ok(Some(ord));
                }
                if document_has_section_rows(connection, &id)? {
                    return match stored_or_computed_body_order(connection, &id)? {
                        Some(Err(error)) => Err(error),
                        _ => Ok(None),
                    };
                }
                let Some(order) = stored_or_computed_body_order(connection, &id)? else {
                    return Ok(None);
                };
                let order = order?;
                Ok(
                    load_document_row(connection, &id, false)?.and_then(|(document, _)| {
                        section_rows::body_order_window(&document, &order, section_key, 0, 0)
                            .map(|window| window.ord)
                    }),
                )
            },
        )
        .await
    }

    /// The `section_key` at one body-order position.
    async fn body_order_at(
        &self,
        id: &DocumentId,
        ord: usize,
    ) -> Result<Option<i64>, ApplicationError> {
        let id = id.clone();
        run_sqlite(
            &self.connection,
            ApplicationError::RepositoryFailed,
            move |connection| {
                if let Some(key) = load_body_order_at(connection, &id, ord)? {
                    return Ok(Some(key));
                }
                if document_has_section_rows(connection, &id)? {
                    return match stored_or_computed_body_order(connection, &id)? {
                        Some(Err(error)) => Err(error),
                        _ => Ok(None),
                    };
                }
                let Some(order) = stored_or_computed_body_order(connection, &id)? else {
                    return Ok(None);
                };
                let order = order?;
                Ok(load_document_row(connection, &id, false)?
                    .and_then(|(document, _)| section_rows::body_order_at(&document, &order, ord)))
            },
        )
        .await
    }

    /// The length of the stored body order.
    async fn body_order_len(&self, id: &DocumentId) -> Result<Option<usize>, ApplicationError> {
        let id = id.clone();
        run_sqlite(
            &self.connection,
            ApplicationError::RepositoryFailed,
            move |connection| {
                let len = load_body_order_len(connection, &id)?;
                if len.is_some_and(|len| len > 0) {
                    return Ok(len);
                }
                if document_has_section_rows(connection, &id)? {
                    return match stored_or_computed_body_order(connection, &id)? {
                        Some(Err(error)) => Err(error),
                        _ => Ok(len),
                    };
                }
                let Some(order) = stored_or_computed_body_order(connection, &id)? else {
                    return Ok(None);
                };
                Ok(Some(order?.len()))
            },
        )
        .await
    }
}

/// One `documents` row decoded in full: the row read, the parse, the tree rebuilt from the section
/// rows when the row keeps it there, the section text, the OCR validation and the normalized hash,
/// which is computed and stored when the row has none. `get_with_hash` calls it, and
/// `get_light_with_hash` calls it for the rows that cannot be read light.
fn load_document_row(
    connection: &Connection,
    id: &DocumentId,
    decode_timing: bool,
) -> Result<Option<(Arc<Document>, NormalizedDocumentHash)>, ApplicationError> {
    let row_started = Instant::now();
    let row = connection
        .query_row(
            "SELECT document_json, normalized_document_hash FROM documents WHERE id = ?1",
            params![&id.0],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?)),
        )
        .optional()
        .map_err(repository_error)?;
    let row_read_ms = row_started.elapsed().as_secs_f64() * 1000.0;
    let Some((json, stored_hash)) = row else {
        return Ok(None);
    };
    let json_bytes = json.len();
    let parse_started = Instant::now();
    let stored = serde_json::from_str::<StoredDocument>(&json)
        .map_err(|error| ApplicationError::RepositoryFailed(error.to_string()))?;
    let parse_ms = parse_started.elapsed().as_secs_f64() * 1000.0;
    let load_started = Instant::now();
    let document = stored.into_document(connection, stored_hash.as_deref())?;
    let section_text_load_ms = load_started.elapsed().as_secs_f64() * 1000.0;
    let validate_started = Instant::now();
    document
        .validate_ocr_publication()
        .map_err(ApplicationError::RepositoryFailed)?;
    let validate_ms = validate_started.elapsed().as_secs_f64() * 1000.0;
    let document = Arc::new(document);
    let hash_started = Instant::now();
    let (normalized_document_hash, hash_ms) = match stored_hash {
        Some(hash) => (NormalizedDocumentHash(hash), None),
        None => {
            let hash = document.normalized_document_hash();
            connection
                .execute(
                    "UPDATE documents SET normalized_document_hash = ?1 WHERE id = ?2",
                    params![&hash.0, &id.0],
                )
                .map_err(repository_error)?;
            let hash_ms = hash_started.elapsed().as_secs_f64() * 1000.0;
            (hash, Some(hash_ms))
        }
    };
    if decode_timing {
        emit_document_decode_miss(
            &document,
            DocumentDecodePhases {
                json_bytes,
                row_read_ms,
                parse_ms,
                section_text_load_ms,
                validate_ms,
                hash_ms,
            },
        );
    }
    Ok(Some((document, normalized_document_hash)))
}

/// Whether `document_id` has any OCR metadata entry, in the light map `metadata` or in either
/// metadata table. Every OCR key starts with `ocr_`.
fn has_ocr_metadata(
    connection: &Connection,
    document_id: &DocumentId,
    metadata: &BTreeMap<String, String>,
) -> Result<bool, ApplicationError> {
    if metadata.keys().any(|key| key.starts_with("ocr_")) {
        return Ok(true);
    }
    connection
        .query_row(
            "SELECT 1 FROM document_meta_v1
              WHERE document_id = ?1 AND substr(key, 1, 4) = 'ocr_'
             UNION ALL
             SELECT 1 FROM document_meta_v2
              WHERE document_id = ?1 AND substr(key, 1, 4) = 'ocr_'
             LIMIT 1",
            params![&document_id.0],
            |_row| Ok(()),
        )
        .optional()
        .map(|row| row.is_some())
        .map_err(repository_error)
}

/// One metadata entry of `document_id`: the light map first, then the store the row names.
fn load_stored_metadata(
    connection: &Connection,
    document_id: &DocumentId,
    key: &str,
) -> Result<Option<String>, ApplicationError> {
    let row = connection
        .query_row(
            "SELECT document_json FROM documents WHERE id = ?1",
            params![&document_id.0],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(repository_error)?;
    let Some(json) = row else {
        return Ok(None);
    };
    let stored = serde_json::from_str::<StoredDocument>(&json)
        .map_err(|error| ApplicationError::RepositoryFailed(error.to_string()))?;
    match stored.metadata_store.as_deref() {
        None => Ok(stored.metadata.get(key).cloned()),
        Some(store) if store == DOCUMENT_META_STORE || store == DOCUMENT_META_STORE_V1 => {
            if let Some(value) = stored.metadata.get(key) {
                return Ok(Some(value.clone()));
            }
            load_stored_metadata_entry(connection, document_id, store, key)
        }
        Some(store) => Err(unknown_metadata_store(store)),
    }
}

/// One heavy entry of `document_id` from `store`, decompressed when the row stores it compressed.
fn load_stored_metadata_entry(
    connection: &Connection,
    document_id: &DocumentId,
    store: &str,
    key: &str,
) -> Result<Option<String>, ApplicationError> {
    if store == DOCUMENT_META_STORE_V1 {
        return connection
            .query_row(
                "SELECT value FROM document_meta_v1 WHERE document_id = ?1 AND key = ?2",
                params![&document_id.0, key],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(repository_error);
    }
    let row = connection
        .query_row(
            "SELECT encoding, value FROM document_meta_v2 WHERE document_id = ?1 AND key = ?2",
            params![&document_id.0, key],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, Vec<u8>>(1)?)),
        )
        .optional()
        .map_err(repository_error)?;
    row.map(|(encoding, blob)| decode_metadata_value(document_id, key, &encoding, &blob))
        .transpose()
}

/// Where one section's text of one document lives.
enum SectionTextPlace {
    /// No `documents` row.
    Missing,
    /// The row keeps its section text inline: there is no `text_unit_documents_v2` row for it, so the
    /// text of a section comes from the full document, exactly as the default trait implementation
    /// reads it.
    Inline,
    /// The text is in `section_text_v3` under these keys, and its length is `char_len`.
    Stored {
        doc_key: i64,
        section_key: i64,
        char_len: usize,
    },
}

/// Resolves [`SectionTextPlace`] with the same staleness checks as the whole-document load.
fn section_text_place(
    connection: &Connection,
    document_id: &DocumentId,
    section_id: &SectionId,
) -> Result<SectionTextPlace, ApplicationError> {
    let stored_hash = connection
        .query_row(
            "SELECT normalized_document_hash FROM documents WHERE id = ?1",
            params![&document_id.0],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()
        .map_err(repository_error)?;
    let Some(stored_hash) = stored_hash else {
        return Ok(SectionTextPlace::Missing);
    };
    let meta = connection
        .query_row(
            "SELECT doc_key, normalized_document_hash, segmentation_version
               FROM text_unit_documents_v2
              WHERE document_id = ?1",
            params![&document_id.0],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )
        .optional()
        .map_err(repository_error)?;
    let Some((doc_key, text_hash, segmentation_version)) = meta else {
        // The text-unit store does not know this document, so `save_shared` kept its section text
        // inline in `document_json`.
        return Ok(SectionTextPlace::Inline);
    };
    if stored_hash.is_some_and(|stored_hash| stored_hash != text_hash) {
        return Err(stale_section_text(
            document_id,
            "its section text belongs to another version of the document",
        ));
    }
    if segmentation_version != TEXT_SEGMENTATION_VERSION {
        return Err(stale_section_text(
            document_id,
            "its section text is not stored for the current segmentation version",
        ));
    }
    let Some(section_key) = stored_section_key(connection, document_id, section_id)
        .map_err(|error| stale_section_text(document_id, &error.to_string()))?
    else {
        return Ok(SectionTextPlace::Missing);
    };
    let char_len: i64 = connection
        .query_row(
            "SELECT char_len FROM section_layout_v2 WHERE doc_key = ?1 AND section_key = ?2",
            params![doc_key, section_key],
            |row| row.get(0),
        )
        .map_err(|error| stale_section_text(document_id, &error.to_string()))?;
    let char_len = i64_to_usize(char_len, "char_len")
        .map_err(|error| stale_section_text(document_id, &error.to_string()))?;
    Ok(SectionTextPlace::Stored {
        doc_key,
        section_key,
        char_len,
    })
}

/// The text of one section of a document whose text is not in `section_text_v3`: the inline
/// `document_json` row, decoded in full. Only a row written before the text-unit store held the
/// document takes this path.
fn load_inline_section_text(
    connection: &Connection,
    document_id: &DocumentId,
    section_id: &SectionId,
) -> Result<Option<String>, ApplicationError> {
    Ok(load_document_row(connection, document_id, false)?
        .and_then(|(document, _)| document.find_section(section_id).map(|s| s.content.clone())))
}

/// The text of exactly one section, not its subtree, read from `section_text_v3` with the same
/// staleness checks as the whole-document load.
fn load_one_section_text(
    connection: &Connection,
    document_id: &DocumentId,
    section_id: &SectionId,
) -> Result<Option<String>, ApplicationError> {
    let (doc_key, section_key, char_len) =
        match section_text_place(connection, document_id, section_id)? {
            SectionTextPlace::Missing => return Ok(None),
            SectionTextPlace::Inline => {
                return load_inline_section_text(connection, document_id, section_id);
            }
            SectionTextPlace::Stored {
                doc_key,
                section_key,
                char_len,
            } => (doc_key, section_key, char_len),
        };
    if char_len == 0 {
        return Ok(Some(String::new()));
    }
    let text = load_section_text_unchecked(connection, doc_key, section_key)
        .map_err(|error| stale_section_text(document_id, &error.to_string()))?;
    let text_chars = text.chars().count();
    if text_chars != char_len {
        return Err(stale_section_text(
            document_id,
            &format!("section {section_key} text has {text_chars} chars, expected {char_len}"),
        ));
    }
    Ok(Some(text))
}

/// The length of exactly one section's normalized text, which is the `char_len` the text store
/// keeps next to that text: `write_document_sections` stores `section.normalized_text_len()`, and
/// both the whole-document load and the light load verify it against the text they read.
fn load_one_section_char_len(
    connection: &Connection,
    document_id: &DocumentId,
    section_id: &SectionId,
) -> Result<Option<usize>, ApplicationError> {
    match section_text_place(connection, document_id, section_id)? {
        SectionTextPlace::Missing => Ok(None),
        SectionTextPlace::Inline => Ok(load_document_row(connection, document_id, false)?
            .and_then(|(document, _)| {
                document
                    .find_section(section_id)
                    .map(|section| section.normalized_text_len())
            })),
        SectionTextPlace::Stored { char_len, .. } => Ok(Some(char_len)),
    }
}

// ---------------------------------------------------------------------------------------------
// The section tree and the body order as rows (SEC-1).
//
// `document_sections_v1` keeps one row per section in depth-first document order, the numbering
// `section_layout_v2.section_key` uses, with `subtree_end_key` closing its contiguous key range.
// `document_body_order_rows_v1` keeps the body order as `(ord, section_key)` rows.
// `document_header_v1` marks a document as having both, so a document stored before these tables
// existed (no header row) falls back to the trait defaults, which read the document itself.
// ---------------------------------------------------------------------------------------------

/// Replaces the header, the section rows and the body-order rows of `document`. A failed body
/// order writes no order rows: its exact error text goes in the header's `body_order_error`, which
/// stays the source of truth for it.
fn store_section_rows(
    transaction: &rusqlite::Transaction<'_>,
    document: &Document,
    normalized_document_hash: &NormalizedDocumentHash,
    body_order: &Result<Vec<SectionId>, ApplicationError>,
) -> Result<(), ApplicationError> {
    for table in [
        "document_header_v1",
        "document_sections_v1",
        "document_body_order_rows_v1",
    ] {
        transaction
            .execute(
                &format!("DELETE FROM {table} WHERE document_id = ?1"),
                params![document.id.0],
            )
            .map_err(repository_error)?;
    }

    let rows = crate::application::section_rows::flatten_document(document);
    let section_count = section_row_count(rows.len())?;
    let root_count = section_row_count(document.root_sections.len())?;
    let body_order_error = match body_order {
        Ok(_) => None,
        Err(ApplicationError::RepositoryFailed(message)) => Some(message.clone()),
        Err(other) => Some(other.to_string()),
    };
    transaction
        .execute(
            "INSERT INTO document_header_v1(
                document_id,
                source,
                title,
                media_type,
                content_hash,
                normalized_document_hash,
                section_count,
                root_count,
                body_order_error
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                document.id.0,
                document.source.0,
                document.title,
                document.media_type.0,
                document.content_hash.0,
                normalized_document_hash.0,
                section_count,
                root_count,
                body_order_error,
            ],
        )
        .map_err(repository_error)?;

    {
        let mut statement = transaction
            .prepare_cached(
                "INSERT INTO document_sections_v1(
                    document_id,
                    section_key,
                    section_id,
                    parent_key,
                    level,
                    title,
                    location_json,
                    subtree_end_key
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            )
            .map_err(repository_error)?;
        for row in &rows {
            let location_json =
                serde_json::to_string(&StoredLocation::from_location(&row.location))
                    .map_err(|error| ApplicationError::RepositoryFailed(error.to_string()))?;
            statement
                .execute(params![
                    document.id.0,
                    row.section_key,
                    row.id.0,
                    row.parent_key,
                    i64::from(row.level),
                    row.title,
                    location_json,
                    row.subtree_end_key,
                ])
                .map_err(repository_error)?;
        }
    }

    let Ok(order) = body_order else {
        return Ok(());
    };
    let keys_by_id: HashMap<SectionId, i64> = rows
        .iter()
        .map(|row| (row.id.clone(), row.section_key))
        .collect();
    let mut statement = transaction
        .prepare_cached(
            "INSERT INTO document_body_order_rows_v1(document_id, ord, section_key)
             VALUES (?1, ?2, ?3)",
        )
        .map_err(repository_error)?;
    for (ord, section_id) in order.iter().enumerate() {
        let section_key = keys_by_id.get(section_id).ok_or_else(|| {
            ApplicationError::RepositoryFailed(format!(
                "body order of document {} names a section the tree does not hold",
                document.id.0
            ))
        })?;
        statement
            .execute(params![document.id.0, section_row_count(ord)?, section_key,])
            .map_err(repository_error)?;
    }
    Ok(())
}

/// `count` as the `INTEGER` the section-row tables store, or an error when it does not fit.
fn section_row_count(count: usize) -> Result<i64, ApplicationError> {
    i64::try_from(count).map_err(|_| {
        ApplicationError::RepositoryFailed("document has too many sections for the store".into())
    })
}

/// `limit` as a `LIMIT` value: clamped, so an unlimited request stays unlimited.
fn section_row_limit(limit: usize) -> i64 {
    i64::try_from(limit).unwrap_or(i64::MAX)
}

/// `value` as a `usize`, or an error when it does not fit.
fn section_row_usize(value: i64, field: &str) -> Result<usize, ApplicationError> {
    usize::try_from(value).map_err(|_| {
        ApplicationError::RepositoryFailed(format!(
            "stored {field} value {value} is outside the usize range"
        ))
    })
}

/// Whether `document_id` has a `document_header_v1` row, i.e. was saved with section rows.
fn document_has_section_rows(
    connection: &Connection,
    document_id: &DocumentId,
) -> Result<bool, ApplicationError> {
    connection
        .query_row(
            "SELECT 1 FROM document_header_v1 WHERE document_id = ?1",
            params![&document_id.0],
            |_row| Ok(()),
        )
        .optional()
        .map(|row| row.is_some())
        .map_err(repository_error)
}

/// The columns every section-row read selects, in `StoredSectionRow::read` order.
const SECTION_ROW_COLUMNS: &str =
    "section_key, section_id, parent_key, level, title, location_json, subtree_end_key";

/// One `document_sections_v1` row, with its location still as the stored JSON text.
struct StoredSectionRow {
    section_key: i64,
    section_id: String,
    parent_key: Option<i64>,
    level: i64,
    title: String,
    location_json: String,
    subtree_end_key: i64,
}

impl StoredSectionRow {
    fn read(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            section_key: row.get(0)?,
            section_id: row.get(1)?,
            parent_key: row.get(2)?,
            level: row.get(3)?,
            title: row.get(4)?,
            location_json: row.get(5)?,
            subtree_end_key: row.get(6)?,
        })
    }

    fn into_section_row(self) -> Result<SectionRow, ApplicationError> {
        let location = serde_json::from_str::<StoredLocation>(&self.location_json)
            .map_err(|error| ApplicationError::RepositoryFailed(error.to_string()))?
            .into_location();
        Ok(SectionRow {
            section_key: self.section_key,
            id: SectionId(self.section_id),
            parent_key: self.parent_key,
            level: u8::try_from(self.level).map_err(|_| {
                ApplicationError::RepositoryFailed(format!(
                    "stored section {} has level {} out of the u8 range",
                    self.section_key, self.level
                ))
            })?,
            title: self.title,
            location,
            subtree_end_key: self.subtree_end_key,
        })
    }
}

/// One `document_header_v1` row.
fn load_document_header(
    connection: &Connection,
    document_id: &DocumentId,
) -> Result<Option<DocumentHeader>, ApplicationError> {
    let row = connection
        .query_row(
            "SELECT document_id, source, title, media_type, content_hash,
                    normalized_document_hash, section_count, root_count
               FROM document_header_v1 WHERE document_id = ?1",
            params![&document_id.0],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, i64>(6)?,
                    row.get::<_, i64>(7)?,
                ))
            },
        )
        .optional()
        .map_err(repository_error)?;
    let Some((id, source, title, media_type, content_hash, hash, section_count, root_count)) = row
    else {
        return Ok(None);
    };
    Ok(Some(DocumentHeader {
        id: DocumentId(id),
        source: DocumentSource(source),
        title,
        media_type: MediaType(media_type),
        content_hash: ContentHash(content_hash),
        normalized_document_hash: hash.map(NormalizedDocumentHash),
        section_count: section_row_usize(section_count, "section_count")?,
        root_count: section_row_usize(root_count, "root_count")?,
    }))
}

/// One `document_sections_v1` row, by section id.
fn load_section_row_by_id(
    connection: &Connection,
    document_id: &DocumentId,
    section_id: &SectionId,
) -> Result<Option<SectionRow>, ApplicationError> {
    let row = connection
        .query_row(
            &format!(
                "SELECT {SECTION_ROW_COLUMNS} FROM document_sections_v1
                  WHERE document_id = ?1 AND section_id = ?2"
            ),
            params![&document_id.0, &section_id.0],
            StoredSectionRow::read,
        )
        .optional()
        .map_err(repository_error)?;
    row.map(StoredSectionRow::into_section_row).transpose()
}

/// One `document_sections_v1` row, by section key.
fn load_section_row_by_key(
    connection: &Connection,
    document_id: &DocumentId,
    section_key: i64,
) -> Result<Option<SectionRow>, ApplicationError> {
    let row = connection
        .query_row(
            &format!(
                "SELECT {SECTION_ROW_COLUMNS} FROM document_sections_v1
                  WHERE document_id = ?1 AND section_key = ?2"
            ),
            params![&document_id.0, section_key],
            StoredSectionRow::read,
        )
        .optional()
        .map_err(repository_error)?;
    row.map(StoredSectionRow::into_section_row).transpose()
}

/// The rows whose keys are in `[from_key, to_key_inclusive]`, in ascending key order.
fn load_section_rows_in_range(
    connection: &Connection,
    document_id: &DocumentId,
    from_key: i64,
    to_key_inclusive: i64,
    limit: usize,
) -> Result<Vec<SectionRow>, ApplicationError> {
    let mut statement = connection
        .prepare_cached(&format!(
            "SELECT {SECTION_ROW_COLUMNS} FROM document_sections_v1
              WHERE document_id = ?1 AND section_key BETWEEN ?2 AND ?3
              ORDER BY section_key LIMIT ?4"
        ))
        .map_err(repository_error)?;
    let rows = statement
        .query_map(
            params![
                &document_id.0,
                from_key,
                to_key_inclusive,
                section_row_limit(limit)
            ],
            StoredSectionRow::read,
        )
        .map_err(repository_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(repository_error)?;
    rows.into_iter()
        .map(StoredSectionRow::into_section_row)
        .collect()
}

/// The children of `parent_key` (the roots when it is `None`), in ascending key order.
fn load_section_children(
    connection: &Connection,
    document_id: &DocumentId,
    parent_key: Option<i64>,
    limit: usize,
) -> Result<Vec<SectionRow>, ApplicationError> {
    // Two statements, so the `(document_id, parent_key, section_key)` index answers both the root
    // question and the child question without scanning the document's sections.
    let mut statement = match parent_key {
        None => connection
            .prepare_cached(&format!(
                "SELECT {SECTION_ROW_COLUMNS} FROM document_sections_v1
                  WHERE document_id = ?1 AND parent_key IS NULL
                  ORDER BY section_key LIMIT ?2"
            ))
            .map_err(repository_error)?,
        Some(_) => connection
            .prepare_cached(&format!(
                "SELECT {SECTION_ROW_COLUMNS} FROM document_sections_v1
                  WHERE document_id = ?1 AND parent_key = ?2
                  ORDER BY section_key LIMIT ?3"
            ))
            .map_err(repository_error)?,
    };
    let rows = match parent_key {
        None => statement.query_map(
            params![&document_id.0, section_row_limit(limit)],
            StoredSectionRow::read,
        ),
        Some(parent_key) => statement.query_map(
            params![&document_id.0, parent_key, section_row_limit(limit)],
            StoredSectionRow::read,
        ),
    }
    .map_err(repository_error)?
    .collect::<Result<Vec<_>, _>>()
    .map_err(repository_error)?;
    rows.into_iter()
        .map(StoredSectionRow::into_section_row)
        .collect()
}

/// The ancestors of `section_key`, root first, one indexed row read per level.
fn load_section_ancestors(
    connection: &Connection,
    document_id: &DocumentId,
    section_key: i64,
) -> Result<Vec<SectionRow>, ApplicationError> {
    let mut ancestors = Vec::new();
    let mut current = load_section_row_by_key(connection, document_id, section_key)?
        .and_then(|row| row.parent_key);
    while let Some(key) = current {
        let Some(row) = load_section_row_by_key(connection, document_id, key)? else {
            break;
        };
        current = row.parent_key;
        ancestors.push(row);
    }
    ancestors.reverse();
    Ok(ancestors)
}

/// Every `document_sections_v1` row of `document_id`, in depth-first key order.
fn load_all_section_rows(
    connection: &Connection,
    document_id: &DocumentId,
) -> Result<Vec<SectionRow>, ApplicationError> {
    let mut statement = connection
        .prepare_cached(&format!(
            "SELECT {SECTION_ROW_COLUMNS} FROM document_sections_v1
              WHERE document_id = ?1 ORDER BY section_key"
        ))
        .map_err(repository_error)?;
    let rows = statement
        .query_map(params![&document_id.0], StoredSectionRow::read)
        .map_err(repository_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(repository_error)?;
    rows.into_iter()
        .map(StoredSectionRow::into_section_row)
        .collect()
}

/// The section tree rebuilt from `document_sections_v1`, nested by the stored depth-first keys.
/// A row whose marker says the tree is in the rows but whose header count disagrees with those rows
/// is stale: it is never returned as an empty book.
fn load_section_tree(
    connection: &Connection,
    document_id: &DocumentId,
) -> Result<Vec<Section>, ApplicationError> {
    let rows = load_all_section_rows(connection, document_id)?;
    if let Some(header) = load_document_header(connection, document_id)?
        && header.section_count != rows.len()
    {
        return Err(ApplicationError::StaleDocument(format!(
            "persisted document {} keeps {} section rows but its header says {}; explicit source reopen required",
            document_id.0,
            rows.len(),
            header.section_count
        )));
    }
    let Some(last) = rows.last() else {
        return Ok(Vec::new());
    };
    Ok(build_section_children(&rows, 0, last.section_key, None))
}

/// The children of the node at `start`, up to `end_key`, from the flat depth-first rows.
fn build_section_children(
    rows: &[SectionRow],
    start: usize,
    end_key: i64,
    parent_id: Option<&SectionId>,
) -> Vec<Section> {
    let mut sections = Vec::new();
    let mut index = start;
    while index < rows.len() && rows[index].section_key <= end_key {
        let row = &rows[index];
        let id = row.id.clone();
        let children = build_section_children(rows, index + 1, row.subtree_end_key, Some(&id));
        sections.push(Section {
            id,
            parent_id: parent_id.cloned(),
            title: row.title.clone(),
            level: row.level,
            content: String::new(),
            location: row.location.clone(),
            children,
        });
        // The subtree of `row` is the contiguous key range `[section_key, subtree_end_key]`, so the
        // next sibling starts just past it. `max` keeps a malformed key from looping forever.
        index = usize::try_from(row.subtree_end_key)
            .ok()
            .map_or(rows.len(), |key| key.saturating_add(1))
            .max(index + 1);
    }
    sections
}

/// The body-order window around `section_key`, or `None` when the section is not in the body order.
fn load_body_order_window(
    connection: &Connection,
    document_id: &DocumentId,
    section_key: i64,
    before: usize,
    after: usize,
) -> Result<Option<BodyOrderWindow>, ApplicationError> {
    let ord = connection
        .query_row(
            "SELECT ord FROM document_body_order_rows_v1 WHERE document_id = ?1 AND section_key = ?2",
            params![&document_id.0, section_key],
            |row| row.get::<_, i64>(0),
        )
        .optional()
        .map_err(repository_error)?;
    let Some(ord) = ord else {
        return Ok(None);
    };
    let ord = section_row_usize(ord, "body order position")?;
    let Some(len) = load_body_order_len(connection, document_id)? else {
        return Ok(None);
    };
    let start = ord.saturating_sub(before);
    let end = ord.saturating_add(after).min(len.saturating_sub(1));
    let mut statement = connection
        .prepare_cached(
            "SELECT ord, section_key FROM document_body_order_rows_v1
              WHERE document_id = ?1 AND ord BETWEEN ?2 AND ?3
              ORDER BY ord",
        )
        .map_err(repository_error)?;
    let window = statement
        .query_map(
            params![
                &document_id.0,
                section_row_count(start)?,
                section_row_count(end)?
            ],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
        )
        .map_err(repository_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(repository_error)?
        .into_iter()
        .map(|(ord, key)| Ok((section_row_usize(ord, "body order position")?, key)))
        .collect::<Result<Vec<_>, ApplicationError>>()?;
    Ok(Some(BodyOrderWindow { ord, len, window }))
}

/// The `section_key` at body-order position `ord`.
/// The body-order position of one section: one `(document_id, section_key)` unique-index read.
fn load_body_order_position(
    connection: &Connection,
    document_id: &DocumentId,
    section_key: i64,
) -> Result<Option<usize>, ApplicationError> {
    let ord = connection
        .query_row(
            "SELECT ord FROM document_body_order_rows_v1 WHERE document_id = ?1 AND section_key = ?2",
            params![&document_id.0, section_key],
            |row| row.get::<_, i64>(0),
        )
        .optional()
        .map_err(repository_error)?;
    ord.map(|ord| section_row_usize(ord, "ord")).transpose()
}

fn load_body_order_at(
    connection: &Connection,
    document_id: &DocumentId,
    ord: usize,
) -> Result<Option<i64>, ApplicationError> {
    connection
        .query_row(
            "SELECT section_key FROM document_body_order_rows_v1 WHERE document_id = ?1 AND ord = ?2",
            params![&document_id.0, section_row_count(ord)?],
            |row| row.get::<_, i64>(0),
        )
        .optional()
        .map_err(repository_error)
}

/// The number of rows in the document's body order.
fn load_body_order_len(
    connection: &Connection,
    document_id: &DocumentId,
) -> Result<Option<usize>, ApplicationError> {
    let len = connection
        .query_row(
            "SELECT COUNT(*) FROM document_body_order_rows_v1 WHERE document_id = ?1",
            params![&document_id.0],
            |row| row.get::<_, i64>(0),
        )
        .map_err(repository_error)?;
    section_row_usize(len, "body order length").map(Some)
}

/// The exact error text a failed body order stored in `document_header_v1`, or `None`.
fn load_body_order_error(
    connection: &Connection,
    document_id: &DocumentId,
) -> Result<Option<String>, ApplicationError> {
    connection
        .query_row(
            "SELECT body_order_error FROM document_header_v1 WHERE document_id = ?1",
            params![&document_id.0],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()
        .map_err(repository_error)
        .map(|value| value.flatten())
}

/// The body order as section ids, from the `(ord, section_key)` rows joined to the section rows.
fn load_body_order_ids(
    connection: &Connection,
    document_id: &DocumentId,
) -> Result<Vec<SectionId>, ApplicationError> {
    let mut statement = connection
        .prepare_cached(
            // The `INDEXED BY` pins the join to the unique `(document_id, section_key)` index:
            // without it SQLite can pick the `(document_id, parent_key, section_key)` index, whose
            // `section_key` cannot be seeked when `parent_key` is unconstrained, and scan the whole
            // section table once per order row.
            "SELECT sections.section_id
               FROM document_body_order_rows_v1 AS order_rows
               JOIN document_sections_v1 AS sections INDEXED BY idx_document_sections_v1_id
                 ON sections.document_id = order_rows.document_id
                AND sections.section_key = order_rows.section_key
              WHERE order_rows.document_id = ?1
              ORDER BY order_rows.ord",
        )
        .map_err(repository_error)?;
    let rows = statement
        .query_map(params![&document_id.0], |row| row.get::<_, String>(0))
        .map_err(repository_error)?;
    rows.collect::<Result<Vec<_>, _>>()
        .map(|ids| ids.into_iter().map(SectionId).collect())
        .map_err(repository_error)
}

/// The stored body order of `document_id`: the header's `body_order_error` when the order failed,
/// otherwise the section ids in `document_body_order_rows_v1`. A document saved before the rows
/// existed (no header row) has its order computed from the stored document. `Ok(None)` when the
/// document is unknown.
fn stored_or_computed_body_order(
    connection: &Connection,
    document_id: &DocumentId,
) -> Result<Option<Result<Vec<SectionId>, ApplicationError>>, ApplicationError> {
    if document_has_section_rows(connection, document_id)? {
        if let Some(error) = load_body_order_error(connection, document_id)? {
            return Ok(Some(Err(ApplicationError::RepositoryFailed(error))));
        }
        return Ok(Some(Ok(load_body_order_ids(connection, document_id)?)));
    }
    // A document saved before the section rows existed: compute the order from the full document.
    // The read path never writes, so the next save stores it.
    let Some((document, _)) = load_document_row(connection, document_id, false)? else {
        return Ok(None);
    };
    Ok(Some(crate::application::body_order_ids(&document)))
}

/// The body-order window around `section_key` for a document without body-order rows: the same
/// computation the trait default performs.
fn body_order_window_from_document(
    connection: &Connection,
    document_id: &DocumentId,
    section_key: i64,
    before: usize,
    after: usize,
) -> Result<Option<BodyOrderWindow>, ApplicationError> {
    let Some(order) = stored_or_computed_body_order(connection, document_id)? else {
        return Ok(None);
    };
    let order = order?;
    Ok(
        load_document_row(connection, document_id, false)?.and_then(|(document, _)| {
            section_rows::body_order_window(&document, &order, section_key, before, after)
        }),
    )
}

/// The `content_store` value of a `document_json` row that keeps no section text inline: every
/// section's text is in `section_text_v3` for the same document version.
const SECTION_TEXT_CONTENT_STORE: &str = "section_text_v3";

/// The `section_tree_store` value of a `document_json` row that keeps no section tree inline:
/// `root_sections` is empty and every section, its parent link and its DFS key are in
/// `document_sections_v1`.
const SECTION_TREE_STORE: &str = "document_sections_v1";

/// The `encoding` of a `section_text_v3` chunk that is the raw UTF-8 bytes of the chunk.
const SECTION_TEXT_ENCODING_RAW: i64 = 0;

/// The `encoding` of a `section_text_v3` chunk that is the UTF-8 bytes of the chunk compressed in
/// zlib format (RFC 1950), which `zlib.decompress` of Python reads.
const SECTION_TEXT_ENCODING_ZLIB: i64 = 1;

/// The zlib level a section text chunk is compressed at: the fastest level whose output stays within
/// 15% of the best level on 宋史 and en-novel. Measured in CI; see the level table in the DISK-1 PR.
const SECTION_TEXT_ZLIB_LEVEL: u32 = 6;

/// A chunk is stored raw unless zlib makes it at least this many percent smaller, so text that does
/// not compress never grows on disk.
const SECTION_TEXT_MIN_SAVING_PERCENT: usize = 5;

/// How much larger a compressed chunk is assumed to be once decompressed, for the scan warm's byte
/// budget: a zlib chunk of natural-language text holds about three times its stored size. The budget
/// only decides whether a warm starts, and the scan text store enforces the exact cap on every
/// insert, so an estimate is enough here and no chunk is decompressed just to be measured.
const SECTION_TEXT_ZLIB_EXPANSION_ESTIMATE: usize = 3;

/// The `metadata_store` value of a `document_json` row that keeps its heavy metadata entries in
/// `document_meta_v2`, compressed. A row without the key keeps every entry inline, which is what
/// every row written before the split and a row whose entries are all light write.
const DOCUMENT_META_STORE: &str = "document_meta_v2";

/// The `metadata_store` value #205-#209 wrote: heavy entries in `document_meta_v1`, uncompressed.
/// Rows of that shape are still read, and a save replaces them.
const DOCUMENT_META_STORE_V1: &str = "document_meta_v1";

/// The `encoding` of a `document_meta_v2` value that is the raw UTF-8 bytes of the entry.
const DOCUMENT_META_ENCODING_RAW: &str = "raw";

/// The `encoding` of a `document_meta_v2` value that is the UTF-8 bytes of the entry compressed in
/// zlib format (RFC 1950), which `zlib.decompress` of Python reads.
const DOCUMENT_META_ENCODING_ZLIB: &str = "zlib";

/// The zlib level a heavy entry is compressed at: the fastest level whose output stays within 10%
/// of the best level on 宋史's `normalized_block_map` (19.1 MB raw). Measured in CI; see the level
/// table in the R1c-2 PR.
const DOCUMENT_META_ZLIB_LEVEL: u32 = 6;

/// A metadata value longer than this many bytes goes to `document_meta_v2` instead of
/// `document_json`. The large maps of a book (`normalized_block_map`, `epub_structure_map`,
/// `original_source_binding_map`) are megabytes; the small ones that a reader looks at per call
/// (`content_encoding`, `parser`, page counts) are well under a kilobyte.
const METADATA_HEAVY_BYTES: usize = 1024;

/// The chunk size the metadata compressor is fed in: one stream, no whole-value copy.
const METADATA_COMPRESS_CHUNK_BYTES: usize = 1024 * 1024;

/// The error an unknown `metadata_store` value is: the row needs an explicit source reopen, like
/// every other stale row.
fn unknown_metadata_store(store: &str) -> ApplicationError {
    ApplicationError::StaleDocument(format!(
        "persisted document keeps its metadata in unknown store {store}; explicit source reopen required"
    ))
}

/// The error an unknown `section_tree_store` value is: the row needs an explicit source reopen.
fn unknown_section_tree_store(store: &str) -> ApplicationError {
    ApplicationError::StaleDocument(format!(
        "persisted document keeps its section tree in unknown store {store}; explicit source reopen required"
    ))
}

/// Splits a document's metadata into the entries that stay inline in `document_json` and the heavy
/// ones that `document_meta_v2` holds.
///
/// The heavy entries are borrowed, not cloned: their values are the largest facts a document has
/// (the 19 MB normalized block map of 宋史), they are only ever streamed into the compressor, and a
/// clone of them is a second copy that lives as long as the save does — 3.7 MiB of en-novel's
/// 18.3 MiB heap peak in MEM-2a. The borrow ends with `save_shared`'s call, so nothing outlives the
/// document the caller handed in.
fn split_metadata(
    metadata: &BTreeMap<String, String>,
) -> (BTreeMap<String, String>, Vec<(&str, &str)>) {
    let mut light = BTreeMap::new();
    let mut heavy = Vec::new();
    for (key, value) in metadata {
        if value.len() > METADATA_HEAVY_BYTES {
            heavy.push((key.as_str(), value.as_str()));
        } else {
            light.insert(key.clone(), value.clone());
        }
    }
    (light, heavy)
}

/// The `(encoding, value)` a heavy metadata entry is stored as: the UTF-8 bytes of the entry
/// compressed in zlib format. The encoder streams the value in [`METADATA_COMPRESS_CHUNK_BYTES`]
/// chunks into one stream, so compressing a multi-megabyte map does not allocate a second copy of
/// it.
fn encode_metadata_value(value: &str) -> Result<(&'static str, Vec<u8>), ApplicationError> {
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::new(DOCUMENT_META_ZLIB_LEVEL));
    for chunk in value.as_bytes().chunks(METADATA_COMPRESS_CHUNK_BYTES) {
        encoder.write_all(chunk).map_err(|error| {
            ApplicationError::RepositoryFailed(format!("metadata compression failed: {error}"))
        })?;
    }
    let blob = encoder.finish().map_err(|error| {
        ApplicationError::RepositoryFailed(format!("metadata compression failed: {error}"))
    })?;
    Ok((DOCUMENT_META_ENCODING_ZLIB, blob))
}

/// The exact UTF-8 string of one stored metadata value, decompressed when `encoding` says so. A
/// blob that is not valid zlib data, or does not decode to UTF-8, is a corrupt row: an error that
/// names it, never a panic.
fn decode_metadata_value(
    document_id: &DocumentId,
    key: &str,
    encoding: &str,
    blob: &[u8],
) -> Result<String, ApplicationError> {
    let bytes = match encoding {
        DOCUMENT_META_ENCODING_RAW => blob.to_vec(),
        DOCUMENT_META_ENCODING_ZLIB => {
            let mut bytes = Vec::new();
            ZlibDecoder::new(blob)
                .read_to_end(&mut bytes)
                .map_err(|error| corrupt_metadata_entry(document_id, key, &error.to_string()))?;
            bytes
        }
        encoding => {
            return Err(corrupt_metadata_entry(
                document_id,
                key,
                &format!("unknown encoding {encoding}"),
            ));
        }
    };
    String::from_utf8(bytes)
        .map_err(|error| corrupt_metadata_entry(document_id, key, &error.to_string()))
}

/// The error of a metadata blob that cannot be decoded: the row needs a reopen, but the message
/// says what is broken first.
fn corrupt_metadata_entry(document_id: &DocumentId, key: &str, reason: &str) -> ApplicationError {
    ApplicationError::RepositoryFailed(format!(
        "stored metadata entry {key} of document {} cannot be decoded: {reason}",
        document_id.0
    ))
}

/// The `(encoding, data)` a section text chunk is stored as: the UTF-8 bytes of the chunk compressed
/// in zlib format, or the raw bytes when compression does not make the chunk at least
/// [`SECTION_TEXT_MIN_SAVING_PERCENT`] smaller. One chunk is compressed at a time, so a document's
/// text is never copied in full.
fn encode_text_chunk(text: &str) -> Result<(i64, Vec<u8>), ApplicationError> {
    let raw = text.as_bytes();
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::new(SECTION_TEXT_ZLIB_LEVEL));
    encoder
        .write_all(raw)
        .map_err(|error| text_chunk_error(&format!("compression failed: {error}")))?;
    let blob = encoder
        .finish()
        .map_err(|error| text_chunk_error(&format!("compression failed: {error}")))?;
    if blob.len() * 100 <= raw.len() * (100 - SECTION_TEXT_MIN_SAVING_PERCENT) {
        Ok((SECTION_TEXT_ENCODING_ZLIB, blob))
    } else {
        Ok((SECTION_TEXT_ENCODING_RAW, raw.to_vec()))
    }
}

/// The exact UTF-8 string of one stored section text chunk, decompressed when `encoding` says so. A
/// blob that is not valid zlib data, or does not decode to UTF-8, is a corrupt row: an error that
/// names it, never a panic.
fn decode_text_chunk(encoding: i64, data: &[u8]) -> Result<String, ApplicationError> {
    let bytes = match encoding {
        SECTION_TEXT_ENCODING_RAW => Cow::Borrowed(data),
        SECTION_TEXT_ENCODING_ZLIB => {
            let mut bytes = Vec::new();
            ZlibDecoder::new(data)
                .read_to_end(&mut bytes)
                .map_err(|error| text_chunk_error(&format!("decompression failed: {error}")))?;
            Cow::Owned(bytes)
        }
        encoding => {
            return Err(text_chunk_error(&format!("unknown encoding {encoding}")));
        }
    };
    String::from_utf8(bytes.into_owned())
        .map_err(|error| text_chunk_error(&format!("not UTF-8: {error}")))
}

/// The error of a section text chunk that cannot be encoded or decoded. It carries no document or
/// section id: the caller adds the context its own error style names.
fn text_chunk_error(reason: &str) -> ApplicationError {
    ApplicationError::RepositoryFailed(format!(
        "stored section text chunk cannot be read: {reason}"
    ))
}

/// Whether `document`'s section text is stored in `section_text_v3` for exactly this document
/// version, so that `document_json` may leave it out: the unit row names this document and hash for
/// the current `TEXT_SEGMENTATION_VERSION`, and `section_layout_v2` holds exactly one row per
/// section, in depth-first document order, whose `char_len` is that section's char count.
fn section_text_is_stored(
    connection: &Connection,
    document: &Document,
    normalized_document_hash: &NormalizedDocumentHash,
) -> Result<bool, ApplicationError> {
    if !has_table(connection, "text_unit_documents_v2")?
        || !has_table(connection, "section_layout_v2")?
    {
        return Ok(false);
    }
    let stored = connection
        .query_row(
            "SELECT doc_key, normalized_document_hash, segmentation_version
               FROM text_unit_documents_v2
              WHERE document_id = ?1",
            params![&document.id.0],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )
        .optional()
        .map_err(repository_error)?;
    let Some((doc_key, stored_hash, segmentation_version)) = stored else {
        return Ok(false);
    };
    if stored_hash != normalized_document_hash.as_ref()
        || segmentation_version != TEXT_SEGMENTATION_VERSION
    {
        return Ok(false);
    }
    let mut ordered_sections = Vec::new();
    collect_sections(&document.root_sections, &mut ordered_sections);
    let char_lens = section_char_lens(connection, doc_key)?;
    if char_lens.len() != ordered_sections.len() {
        return Ok(false);
    }
    for (section, char_len) in ordered_sections.iter().zip(char_lens) {
        if usize::try_from(char_len).ok() != Some(section.normalized_text_len()) {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Whether this database has `table`. A repository opened on its own never creates the text-unit
/// tables, so their absence means the section text is not stored.
fn has_table(connection: &Connection, table: &str) -> Result<bool, ApplicationError> {
    connection
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1",
            params![table],
            |_| Ok(()),
        )
        .optional()
        .map(|found| found.is_some())
        .map_err(repository_error)
}

/// The `char_len` of one document's `section_layout_v2` rows, in `section_key` order.
fn section_char_lens(connection: &Connection, doc_key: i64) -> Result<Vec<i64>, ApplicationError> {
    let mut statement = connection
        .prepare("SELECT char_len FROM section_layout_v2 WHERE doc_key = ?1 ORDER BY section_key")
        .map_err(repository_error)?;
    let rows = statement
        .query_map(params![doc_key], |row| row.get::<_, i64>(0))
        .map_err(repository_error)?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(repository_error)
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

/// Adds the nullable `body_order_error` column to a `document_header_v1` written before the body
/// order stopped living in its own table. The header row is where a failed body order is kept now:
/// one string next to the section rows it failed to order, read with the header's one row.
fn ensure_body_order_error_column(connection: &Connection) -> Result<(), ApplicationError> {
    let has_column = {
        let mut statement = connection
            .prepare("PRAGMA table_info(document_header_v1)")
            .map_err(repository_error)?;
        let columns = statement
            .query_map([], |row| row.get::<_, String>(1))
            .map_err(repository_error)?;
        let mut has_column = false;
        for column in columns {
            if column.map_err(repository_error)? == "body_order_error" {
                has_column = true;
            }
        }
        has_column
    };
    if !has_column {
        connection
            .execute_batch("ALTER TABLE document_header_v1 ADD COLUMN body_order_error TEXT")
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
    /// The section texts the last scanned document was scanned from. One document at a time.
    ///
    /// The only in-process cache the store keeps is this #194 scan text cache: no per-call TextUnit
    /// read path holds or consults any other cache.
    scan_text: Arc<Mutex<Option<Arc<ScanTextStore>>>>,
    /// The documents a background warm is filling the scan text store for, so a second `prewarm`
    /// request for a document already being warmed returns at once.
    prewarming: Arc<Mutex<HashSet<DocumentId>>>,
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
        // The v3 store kept one whole-section layout blob and the text in `WITHOUT ROWID` tables,
        // so every per-call read unpacked a whole record at every b-tree level. The v4 store cuts
        // both into rowid tables: chunked layout rows and chunked text rows. Dropping is
        // idempotent, so an old database opens, keeps its `documents` rows, and re-ingests its
        // documents on their next open.
        // The text moved to `section_text_v3` (compressed chunks, DISK-1), so the uncompressed v4
        // text table is dropped too: a row that still names it reads as stale and re-ingests.
        //
        // v10 (SEC-3) keeps the body order only in `document_body_order_rows_v1` and a failed
        // order's exact text in `document_header_v1.body_order_error`, so the old one-row
        // `document_body_order_v1` table (a JSON array of every section id) is dropped here, in
        // the same open that bumps `TEXT_UNIT_STORE_VERSION`. Dropping is idempotent; the bump
        // makes every book stored by the previous version re-ingest once on its next open.
        connection
            .execute_batch("DROP TABLE IF EXISTS document_body_order_v1;")
            .map_err(text_unit_index_error)?;
        connection
            .execute_batch(
                "DROP TABLE IF EXISTS section_layout_v1;
                 DROP TABLE IF EXISTS section_text_v1;
                 DROP TABLE IF EXISTS section_text_v2;
                 CREATE TABLE IF NOT EXISTS text_unit_documents_v2 (
                    doc_key INTEGER PRIMARY KEY,
                    document_id TEXT NOT NULL UNIQUE,
                    content_hash TEXT NOT NULL,
                    normalized_document_hash TEXT NOT NULL,
                    segmentation_version TEXT NOT NULL
                 );
                 CREATE TABLE IF NOT EXISTS section_text_v3 (
                    doc_key INTEGER NOT NULL,
                    section_key INTEGER NOT NULL,
                    chunk_no INTEGER NOT NULL,
                    encoding INTEGER NOT NULL,
                    data BLOB NOT NULL,
                    UNIQUE(doc_key, section_key, chunk_no)
                 );
                 CREATE TABLE IF NOT EXISTS section_layout_v2 (
                    doc_key INTEGER NOT NULL,
                    section_key INTEGER NOT NULL,
                    section_id TEXT NOT NULL,
                    char_len INTEGER NOT NULL,
                    paragraph_coverage_json TEXT,
                    plan_error TEXT,
                    paragraph_count INTEGER NOT NULL,
                    sentence_count INTEGER NOT NULL,
                    chunk_count INTEGER NOT NULL,
                    sorted_flags INTEGER NOT NULL,
                    total_items_paragraph_preserve INTEGER NOT NULL,
                    total_items_paragraph_eligible INTEGER NOT NULL,
                    total_items_sentence_preserve INTEGER NOT NULL,
                    total_items_sentence_eligible INTEGER NOT NULL,
                    coverage_paragraph_preserve_json TEXT NOT NULL,
                    coverage_paragraph_eligible_json TEXT NOT NULL,
                    coverage_sentence_preserve_json TEXT NOT NULL,
                    coverage_sentence_eligible_json TEXT NOT NULL,
                    UNIQUE(doc_key, section_id)
                 );
                 CREATE INDEX IF NOT EXISTS idx_section_layout_v2_section_key
                    ON section_layout_v2(doc_key, section_key);
                 CREATE TABLE IF NOT EXISTS section_layout_chunk_v2 (
                    doc_key INTEGER NOT NULL,
                    section_key INTEGER NOT NULL,
                    chunk_no INTEGER NOT NULL,
                    first_paragraph_index INTEGER NOT NULL,
                    first_sentence_index INTEGER NOT NULL,
                    paragraph_slot INTEGER NOT NULL,
                    sentence_slot INTEGER NOT NULL,
                    paragraph_entry INTEGER NOT NULL,
                    sentence_entry INTEGER NOT NULL,
                    eligible_paragraph_entry INTEGER NOT NULL,
                    eligible_sentence_entry INTEGER NOT NULL,
                    paragraph_count INTEGER NOT NULL,
                    sentence_count INTEGER NOT NULL,
                    blob BLOB NOT NULL,
                    UNIQUE(doc_key, section_key, chunk_no)
                 );
                 CREATE INDEX IF NOT EXISTS idx_section_layout_chunk_v2_paragraph_entry
                    ON section_layout_chunk_v2(doc_key, section_key, paragraph_entry);
                 CREATE INDEX IF NOT EXISTS idx_section_layout_chunk_v2_sentence_entry
                    ON section_layout_chunk_v2(doc_key, section_key, sentence_entry);
                 CREATE INDEX IF NOT EXISTS idx_section_layout_chunk_v2_eligible_paragraph_entry
                    ON section_layout_chunk_v2(doc_key, section_key, eligible_paragraph_entry);
                 CREATE INDEX IF NOT EXISTS idx_section_layout_chunk_v2_eligible_sentence_entry
                    ON section_layout_chunk_v2(doc_key, section_key, eligible_sentence_entry);
                 CREATE INDEX IF NOT EXISTS idx_section_layout_chunk_v2_first
                    ON section_layout_chunk_v2(
                        doc_key, section_key, first_paragraph_index, first_sentence_index
                    );",
            )
            .map_err(text_unit_index_error)?;
        drop_legacy_lexical_search_tables(&connection)?;
        Ok(Self {
            path,
            connection: Arc::new(Mutex::new(connection)),
            scan_text: Arc::new(Mutex::new(None)),
            prewarming: Arc::new(Mutex::new(HashSet::new())),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Warms the scan text store of one document in the background, so the first search after an
    /// open finds the section texts cached instead of reading every one of them. Returns at once:
    /// the work runs on the blocking pool (or on its own small thread when there is no runtime) and
    /// never reports anything to the caller.
    pub(crate) fn prewarm_scan_text(
        &self,
        document_id: &DocumentId,
        expected_hash: &NormalizedDocumentHash,
    ) {
        if !scan_prewarm_enabled() || scan_text_cache_bytes() == 0 {
            return;
        }
        // One warm per document at a time: a second request returns immediately.
        match self.prewarming.lock() {
            Ok(mut warming) => {
                if !warming.insert(document_id.clone()) {
                    return;
                }
            }
            Err(_) => return,
        }
        let connection = Arc::clone(&self.connection);
        let cache = Arc::clone(&self.scan_text);
        let prewarming = Arc::clone(&self.prewarming);
        let document_id = document_id.clone();
        let expected_hash = expected_hash.clone();
        let work = move || {
            let started = Instant::now();
            // A failure or a panic in the warm is the caller's business only through the timing of
            // its next search, so nothing here may propagate.
            let warmed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                warm_scan_document(&connection, &cache, &document_id, &expected_hash)
            }));
            let (sections, ok) = match warmed {
                Ok(Ok(sections)) => (sections, true),
                Ok(Err(_)) | Err(_) => (0, false),
            };
            if telemetry_enabled() {
                emit_scan_prewarm(
                    &document_id,
                    sections,
                    started.elapsed().as_millis() as u64,
                    ok,
                );
            }
            if let Ok(mut warming) = prewarming.lock() {
                warming.remove(&document_id);
            }
        };
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            // The handle is dropped on purpose: the warm must not be waited for, and dropping a
            // blocking task's handle does not cancel it.
            drop(handle.spawn_blocking(work));
        } else {
            // No runtime: a warm is one SQLite read at a time with no deep recursion, so a small
            // stack is enough, and a thread that cannot start simply leaves the cache cold.
            let _ = std::thread::Builder::new()
                .name("scan-prewarm".into())
                .stack_size(256 * 1024)
                .spawn(work);
        }
    }

    /// Benchmark and test only: how many section texts the scan text cache holds, and whether the
    /// document it holds outgrew the cap. `None` when nothing is cached.
    #[doc(hidden)]
    pub fn scan_text_cache_state(&self) -> Option<(usize, bool)> {
        let store = self.scan_text.lock().ok()?.as_ref().map(Arc::clone)?;
        let sections = store.sections.lock().ok()?;
        Some((sections.texts.len(), sections.over_cap))
    }

    /// Test only: the whole warm of one document, synchronously and at most `steps` sections, so a
    /// test can stop it part way and observe what the next call does.
    #[doc(hidden)]
    pub fn prewarm_scan_text_blocking(
        &self,
        document_id: &DocumentId,
        expected_hash: &NormalizedDocumentHash,
        steps: usize,
    ) -> Result<usize, ApplicationError> {
        let Some(plan) = scan_warm_plan(
            &self.connection,
            &self.scan_text,
            document_id,
            expected_hash,
        )?
        else {
            return Ok(0);
        };
        warm_scan_text(&self.connection, &self.scan_text, &plan, steps)
    }

    /// The normalized hash this document's units are stored for, if they are stored for the current
    /// `TEXT_SEGMENTATION_VERSION`.
    pub(crate) async fn stored_unit_hash(
        &self,
        document_id: &DocumentId,
    ) -> Result<Option<NormalizedDocumentHash>, ApplicationError> {
        let document_id = document_id.clone();
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
                Ok(Some(meta.normalized_document_hash))
            },
        )
        .await
    }

    /// Forgets the cached scan text: the document it belongs to was re-stored, so its stored text may
    /// have changed. Only one document is cached, so dropping it always suffices.
    fn drop_scan_text(&self) {
        if let Ok(mut cached) = self.scan_text.lock() {
            *cached = None;
        }
    }

    /// The stored layouts of `document_id`, in `section_key` order, as `(section_id, encoded
    /// whole-section layout)`. Diagnostics only: it proves that ingesting from units and ingesting
    /// from boundaries write identical rows.
    pub async fn stored_section_layouts(
        &self,
        document_id: &DocumentId,
    ) -> Result<Vec<(String, Vec<u8>)>, ApplicationError> {
        let document_id = document_id.clone();
        run_sqlite(
            &self.connection,
            ApplicationError::TextUnitIndexFailed,
            move |connection| {
                let doc_key: i64 = connection
                    .query_row(
                        "SELECT doc_key FROM text_unit_documents_v2 WHERE document_id = ?1",
                        params![&document_id.0],
                        |row| row.get(0),
                    )
                    .map_err(text_unit_index_error)?;
                let mut statement = connection
                    .prepare(
                        "SELECT l.section_id, l.section_key
                           FROM section_layout_v2 l
                          WHERE l.doc_key = ?1
                          ORDER BY l.section_key",
                    )
                    .map_err(text_unit_index_error)?;
                let rows = statement
                    .query_map(params![doc_key], |row| {
                        Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
                    })
                    .map_err(text_unit_index_error)?;
                let mut layouts = Vec::new();
                for row in rows {
                    let (section_id, section_key) = row.map_err(text_unit_index_error)?;
                    let layout = load_whole_section_layout(connection, doc_key, section_key)?;
                    layouts.push((section_id, layout.encode()?));
                }
                Ok(layouts)
            },
        )
        .await
    }

    /// The best `limit` paragraph and sentence candidates, best first, plus the section ids they
    /// index. `Ok(None)` when this document's units are not stored for the current
    /// `TEXT_SEGMENTATION_VERSION`, or were stored for another normalized document hash.
    ///
    /// The scan reads the stored section text and the stored unit boundaries directly. Every layout
    /// chunk is decoded into one whole-section layout, so the scan answers exactly what the v3
    /// whole-layout row answered.
    /// The scan's cache, thread count and pruning are the caller's: production passes
    /// [`ScanOptions::production`], and the differential tests pass the plain scan of R2c step 3.
    pub(crate) async fn scan_lexical(
        &self,
        document_id: &DocumentId,
        expected_hash: &NormalizedDocumentHash,
        query: Arc<LexicalQuery>,
        mode: MatchMode,
        limit: usize,
        options: ScanOptions,
    ) -> Result<Option<(Vec<SectionId>, Vec<ScannedUnitHit>)>, ApplicationError> {
        let document_id = document_id.clone();
        let expected_hash = expected_hash.clone();
        let cache = Arc::clone(&self.scan_text);
        run_sqlite(
            &self.connection,
            ApplicationError::TextUnitIndexFailed,
            move |connection| {
                let Some(meta) = load_stored_document(connection, &document_id)? else {
                    return Ok(None);
                };
                if meta.segmentation_version != TEXT_SEGMENTATION_VERSION
                    || meta.normalized_document_hash != expected_hash
                {
                    return Ok(None);
                }

                let store = scan_text_store(&cache, &meta, options);
                let pass = ScanPass {
                    query: &query,
                    mode,
                    limit,
                    store: store.as_ref(),
                    prune: options.prune,
                    collect_timings: false,
                };
                let (section_ids, hits, _timings) =
                    scan_stored_sections(connection, &meta, &pass, options)?;
                Ok(Some((section_ids, hits)))
            },
        )
        .await
    }

    /// Benchmark only: the phase timings and unit counts of one Strict scan pass, with the hash the
    /// document's units are stored for. `Ok(None)` when the units are not stored for the current
    /// `TEXT_SEGMENTATION_VERSION`.
    #[doc(hidden)]
    pub async fn scan_lexical_profile(
        &self,
        document_id: &DocumentId,
        query: &str,
        limit: usize,
    ) -> Result<Option<ScanTimings>, ApplicationError> {
        let Some(lexical) = LexicalQuery::new(query) else {
            return Err(ApplicationError::InvalidRequest(
                "search query must contain searchable terms".into(),
            ));
        };
        let lexical = Arc::new(lexical);
        let document_id = document_id.clone();
        let cache = Arc::clone(&self.scan_text);
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
                let options = ScanOptions::production();
                let store = scan_text_store(&cache, &meta, options);
                let pass = ScanPass {
                    query: &lexical,
                    mode: MatchMode::Strict,
                    limit,
                    store: store.as_ref(),
                    prune: options.prune,
                    collect_timings: true,
                };
                let (_section_ids, _hits, timings) =
                    scan_stored_sections(connection, &meta, &pass, options)?;
                Ok(Some(timings))
            },
        )
        .await
    }
}

/// Which phase of a scan pass a duration belongs to.
#[derive(Clone, Copy)]
enum ScanPhase {
    ReadText,
    SectionPrefilter,
    LayoutDecode,
    Paragraphs,
    Sentences,
    Sort,
}

/// Where the time of one scan pass went, plus the counts of the work it did. Benchmarks read this;
/// the search path itself passes `false` for `collect_timings` and records nothing.
#[doc(hidden)]
#[derive(Clone, Copy, Debug, Default)]
pub struct ScanTimings {
    /// `load_section_text`, per section.
    pub read_text: std::time::Duration,
    /// `lowercase_fast` plus the prefilter over the whole section, per section.
    pub section_prefilter: std::time::Duration,
    /// The layout blob read and `SectionLayout::decode`, per scanned section.
    pub layout_decode: std::time::Duration,
    /// The paragraph loop, excluding the sorts inside it.
    pub paragraphs: std::time::Duration,
    /// The sentence loop, excluding the sorts inside it.
    pub sentences: std::time::Duration,
    /// Every `sort_scanned_hits` call, wherever it happens.
    pub sort: std::time::Duration,
    /// Sections with text, which the scan read.
    pub sections_scanned: usize,
    /// Sections whose whole-text prefilter passed.
    pub sections_passed_prefilter: usize,
    /// Paragraphs the scan evaluated, whether or not they were hits.
    pub paragraphs_evaluated: usize,
    /// Sentences the scan evaluated, whether or not they were hits.
    pub sentences_evaluated: usize,
    /// Hits pushed, before the final truncate to `limit`.
    pub hits_pushed: usize,
}

impl ScanTimings {
    fn add(&mut self, phase: ScanPhase, elapsed: std::time::Duration) {
        let slot = match phase {
            ScanPhase::ReadText => &mut self.read_text,
            ScanPhase::SectionPrefilter => &mut self.section_prefilter,
            ScanPhase::LayoutDecode => &mut self.layout_decode,
            ScanPhase::Paragraphs => &mut self.paragraphs,
            ScanPhase::Sentences => &mut self.sentences,
            ScanPhase::Sort => &mut self.sort,
        };
        *slot += elapsed;
    }

    /// Adds another thread's timings: the phases and the counts of a pass over several threads add up.
    fn merge(&mut self, other: ScanTimings) {
        self.read_text += other.read_text;
        self.section_prefilter += other.section_prefilter;
        self.layout_decode += other.layout_decode;
        self.paragraphs += other.paragraphs;
        self.sentences += other.sentences;
        self.sort += other.sort;
        self.sections_scanned += other.sections_scanned;
        self.sections_passed_prefilter += other.sections_passed_prefilter;
        self.paragraphs_evaluated += other.paragraphs_evaluated;
        self.sentences_evaluated += other.sentences_evaluated;
        self.hits_pushed += other.hits_pushed;
    }
}

/// Accumulates the phase durations of a scan pass when enabled. Disabled, `begin` stores nothing and
/// `end` takes nothing, so the search path pays one branch per phase and per unit, never a clock read.
struct ScanTimer {
    enabled: bool,
    phase_start: Option<std::time::Instant>,
    timings: ScanTimings,
}

impl ScanTimer {
    fn new(enabled: bool) -> Self {
        Self {
            enabled,
            phase_start: None,
            timings: ScanTimings::default(),
        }
    }

    /// Starts a phase, or nothing when the timer is disabled.
    fn begin(&mut self) {
        if self.enabled {
            self.phase_start = Some(std::time::Instant::now());
        }
    }

    /// Ends the running phase and adds its duration to `phase`.
    fn end(&mut self, phase: ScanPhase) {
        if let Some(start) = self.phase_start.take() {
            self.timings.add(phase, start.elapsed());
        }
    }

    /// A nested phase inside the running one: end the outer phase first, then `add_since`.
    fn now(&self) -> Option<std::time::Instant> {
        if self.enabled {
            Some(std::time::Instant::now())
        } else {
            None
        }
    }

    fn add_since(&mut self, phase: ScanPhase, start: Option<std::time::Instant>) {
        if let Some(start) = start {
            self.timings.add(phase, start.elapsed());
        }
    }

    /// Adds the timings of another thread of the same pass.
    fn merge(&mut self, other: ScanTimings) {
        self.timings.merge(other);
    }

    fn tally(&mut self, tally: impl FnOnce(&mut ScanTimings)) {
        if self.enabled {
            tally(&mut self.timings);
        }
    }

    fn finish(self) -> ScanTimings {
        self.timings
    }
}

/// The text of one section: from the scan text store when it holds it, else from SQLite, in which
/// case it is stored for the next pass. The store is never locked while the caller uses the text.
fn scan_section_text(
    connection: &Connection,
    store: Option<&Arc<ScanTextStore>>,
    doc_key: i64,
    section_key: i64,
) -> Result<Arc<ScanSectionText>, ApplicationError> {
    if let Some(store) = store
        && let Some(text) = store.get(section_key)
    {
        return Ok(text);
    }
    let text = Arc::new(ScanSectionText::new(load_section_text_unchecked(
        connection,
        doc_key,
        section_key,
    )?));
    if let Some(store) = store {
        store.insert(section_key, Arc::clone(&text));
    }
    Ok(text)
}

/// The query, the limits and the text source one scan pass runs under.
struct ScanPass<'a> {
    query: &'a LexicalQuery,
    mode: MatchMode,
    limit: usize,
    store: Option<&'a Arc<ScanTextStore>>,
    /// Whether units whose score bound cannot reach the thread's threshold may be skipped.
    prune: bool,
    collect_timings: bool,
}

impl ScanPass<'_> {
    /// Whether the whole-section prefilter accepts a section whose lowered text is `lowered`.
    fn section_passes_prefilter(&self, lowered: &str) -> bool {
        match self.mode {
            MatchMode::Strict => self.query.may_contain_all(lowered),
            MatchMode::Relaxed => self.query.may_contain_any(lowered),
        }
    }
}

/// One section a planned pass visits, with its index in the returned section id list.
struct PlannedSection {
    /// The index of the section in the section id list the scan returns.
    section: usize,
    section_key: i64,
    char_len: usize,
    /// The most any unit in the section can score; infinite when the section was not planned, so it is
    /// never pruned.
    bound: f32,
}

/// The bound of the best score any unit in a section can reach, or `None` when the whole-section
/// prefilter rejects the section.
fn section_bound(pass: &ScanPass<'_>, lowered: &str) -> Option<f32> {
    let query = pass.query;
    match pass.mode {
        MatchMode::Strict => {
            if !query.may_contain_all(lowered) {
                return None;
            }
            Some(query.container_bound(lowered, query.token_count()))
        }
        MatchMode::Relaxed => {
            let tokens = query.contained_token_count(lowered);
            if tokens == 0 {
                return None;
            }
            Some(query.container_bound(lowered, tokens))
        }
    }
}

/// How many threads a pass over `sections` uses: only a document of at least 1 MiB of text, only when
/// the pass has a text store to read the sections from, and never more threads than sections.
fn scan_threads(
    sections: &[(i64, SectionId, usize)],
    store: Option<&Arc<ScanTextStore>>,
    options: ScanOptions,
) -> usize {
    if options.threads <= 1 || sections.len() < 2 {
        return 1;
    }
    let Some(store) = store else {
        return 1;
    };
    if store.over_cap() {
        return 1;
    }
    let chars: usize = sections.iter().map(|(_, _, char_len)| char_len).sum();
    if chars < MIN_PARALLEL_TEXT_CHARS {
        return 1;
    }
    options.threads.min(MAX_SCAN_THREADS).min(sections.len())
}

/// What each worker of [`parallel_map`] produced: its own state, and its results by item index.
type WorkerResults<S, R> = Vec<(S, Vec<(usize, R)>)>;

/// The sections a pass will visit, with the section ids they index.
type PlannedSections = (Vec<SectionId>, Vec<PlannedSection>);

/// Runs `work` on up to `threads` threads over `count` items, while this thread feeds them in item
/// order: SQLite reads stay on this thread and the workers only compute. Thread `t` takes the items
/// `t, t + threads, ...`, so each thread visits a subsequence of the caller's order, keeps its own
/// state and shares no lock. `Ok(None)` from `produce` stops the pass early.
fn parallel_map<S, T, R>(
    threads: usize,
    count: usize,
    init: impl Fn() -> S + Sync,
    mut produce: impl FnMut(usize) -> Result<Option<T>, ApplicationError>,
    work: impl Fn(&mut S, usize, T) -> R + Sync,
) -> Result<WorkerResults<S, R>, ApplicationError>
where
    S: Send,
    T: Send,
    R: Send,
{
    std::thread::scope(|scope| {
        let mut senders = Vec::with_capacity(threads);
        let mut handles = Vec::with_capacity(threads);
        for _ in 0..threads {
            let (sender, receiver) = std::sync::mpsc::sync_channel::<(usize, T)>(1);
            senders.push(sender);
            let init = &init;
            let work = &work;
            handles.push(scope.spawn(move || {
                let mut state = init();
                let mut results = Vec::new();
                for (index, item) in receiver {
                    results.push((index, work(&mut state, index, item)));
                }
                (state, results)
            }));
        }
        let mut produced = Ok(());
        for index in 0..count {
            match produce(index) {
                Ok(Some(item)) => {
                    if senders[index % threads].send((index, item)).is_err() {
                        break;
                    }
                }
                Ok(None) => break,
                Err(error) => {
                    produced = Err(error);
                    break;
                }
            }
        }
        drop(senders);
        let mut collected = Vec::with_capacity(threads);
        for handle in handles {
            collected.push(handle.join().expect("a scan worker must not panic"));
        }
        produced.map(|()| collected)
    })
}

/// The text of one section from the store only: the parallel workers never read SQLite, so a section
/// the plan did not store cannot be scanned there.
fn stored_section_text(
    store: &Arc<ScanTextStore>,
    section_key: i64,
) -> Result<Arc<ScanSectionText>, ApplicationError> {
    store.get(section_key).ok_or_else(|| {
        ApplicationError::TextUnitIndexFailed(format!(
            "section {section_key} is missing from the scan text store"
        ))
    })
}

/// The scan of one section list by one thread: the hits it keeps and its timings. Each thread of a
/// parallel pass owns one, so no hit vector is shared.
struct SectionScanner<'a> {
    pass: &'a ScanPass<'a>,
    /// How many hits this thread keeps before it sorts and truncates them to `limit`.
    flush_at: usize,
    hits: Vec<ScannedUnitHit>,
    /// The `limit`-th best score this thread holds, once it holds `limit` hits: units that cannot
    /// reach it are skipped, and only those scoring strictly below it.
    threshold: Option<f32>,
    timer: ScanTimer,
}

impl<'a> SectionScanner<'a> {
    fn new(pass: &'a ScanPass<'a>) -> Self {
        Self {
            pass,
            flush_at: pass.limit.saturating_mul(4).saturating_add(64),
            hits: Vec::new(),
            threshold: None,
            timer: ScanTimer::new(pass.collect_timings),
        }
    }

    /// The score a unit must reach to be worth scoring, when this thread already holds enough hits.
    fn prune_below(&self) -> Option<f32> {
        if self.pass.prune {
            self.threshold
        } else {
            None
        }
    }

    /// Whether a unit whose bound is `bound` cannot reach this thread's threshold and may be skipped.
    fn skips(&self, bound: f32) -> bool {
        self.prune_below()
            .is_some_and(|threshold| bound < threshold)
    }

    /// The most a unit inside this paragraph can score, given the prefilter passed on it.
    fn paragraph_bound(&self, lowered: &str) -> f32 {
        let query = self.pass.query;
        let tokens = match self.pass.mode {
            // The prefilter on this very paragraph proved that every query token is in it.
            MatchMode::Strict => query.token_count(),
            MatchMode::Relaxed => query.contained_token_count(lowered),
        };
        query.container_bound(lowered, tokens)
    }

    /// Scans one section's paragraphs and sentences. `section` is its index in the section id list.
    fn scan_section(
        &mut self,
        section: &PlannedSection,
        text: &ScanSectionText,
        layout: &SectionLayout,
        section_ids: &[SectionId],
    ) -> Result<(), ApplicationError> {
        let query = self.pass.query;
        let mode = self.pass.mode;
        let index = NormalizedTextIndex::new(text.text());
        if index.char_len() != section.char_len {
            return Err(ApplicationError::TextUnitIndexFailed(format!(
                "section {} text has {} chars, expected {}",
                section.section_key,
                index.char_len(),
                section.char_len
            )));
        }
        // No unit inside a section can score above its bound, and a unit scoring below this thread's
        // threshold cannot reach the final `limit` either. Only a bound strictly below it may skip.
        if self.skips(section.bound) {
            return Ok(());
        }
        // A borrowed whole-section lowercase means every char of the section lowercases to itself, so
        // every slice of it does too and the per-slice lowercasing can be skipped.
        let borrowed = text.lowered_is_borrowed();
        let mut prefilters = ParagraphPrefilters::new(ParagraphPrefilters::ranges_increase(
            layout
                .paragraphs
                .iter()
                .map(|paragraph| (paragraph.start, paragraph.end)),
        ));

        self.timer.begin();
        for paragraph in &layout.paragraphs {
            self.timer
                .tally(|timings| timings.paragraphs_evaluated += 1);
            let slice =
                slice_section_text_str(&index, paragraph.start, paragraph.end, "paragraph")?;
            let lowered = slice_lowered(slice, borrowed);
            let prefilter = match mode {
                MatchMode::Strict => query.may_contain_all(&lowered),
                MatchMode::Relaxed => query.may_contain_any(&lowered),
            };
            // The paragraph's own bound covers its sentences too, so a pruned paragraph records a
            // failing prefilter and its sentences are skipped with it.
            let pruned = prefilter
                && self
                    .prune_below()
                    .is_some_and(|threshold| self.paragraph_bound(&lowered) < threshold);
            prefilters.insert(paragraph.start, paragraph.end, prefilter && !pruned);
            if !prefilter || pruned {
                continue;
            }
            let score = match mode {
                // The prefilter ran on this very paragraph, so a strict all-CJK query is answered.
                MatchMode::Strict => {
                    unit_score_after_all_prefilter(query, slice, &lowered, SearchHitKind::Paragraph)
                }
                MatchMode::Relaxed => {
                    unit_score(query, mode, slice, &lowered, SearchHitKind::Paragraph)
                }
            };
            let Some(score) = score else {
                continue;
            };
            self.timer.tally(|timings| timings.hits_pushed += 1);
            self.hits.push(ScannedUnitHit {
                section: section.section,
                kind: SearchHitKind::Paragraph,
                paragraph_index: paragraph.paragraph_index,
                sentence_index: None,
                start: paragraph.start,
                score,
            });
            self.flush_if_needed(ScanPhase::Paragraphs, section_ids);
        }
        self.timer.end(ScanPhase::Paragraphs);

        self.timer.begin();
        for sentence in &layout.sentences {
            self.timer.tally(|timings| timings.sentences_evaluated += 1);
            let prefilter = match prefilters.get(sentence.parent_start, sentence.parent_end) {
                Some(prefilter) => prefilter,
                None => {
                    let parent_slice = slice_section_text_str(
                        &index,
                        sentence.parent_start,
                        sentence.parent_end,
                        "paragraph",
                    )?;
                    let parent_lowered = slice_lowered(parent_slice, borrowed);
                    let prefilter = match mode {
                        MatchMode::Strict => query.may_contain_all(&parent_lowered),
                        MatchMode::Relaxed => query.may_contain_any(&parent_lowered),
                    };
                    prefilters.remember(sentence.parent_start, sentence.parent_end, prefilter);
                    prefilter
                }
            };
            if !prefilter {
                continue;
            }
            let slice = slice_section_text_str(&index, sentence.start, sentence.end, "sentence")?;
            let lowered = slice_lowered(slice, borrowed);
            let Some(score) = unit_score(query, mode, slice, &lowered, SearchHitKind::Sentence)
            else {
                continue;
            };
            self.timer.tally(|timings| timings.hits_pushed += 1);
            self.hits.push(ScannedUnitHit {
                section: section.section,
                kind: SearchHitKind::Sentence,
                paragraph_index: sentence.paragraph_index,
                sentence_index: Some(sentence.sentence_index),
                start: sentence.start,
                score,
            });
            self.flush_if_needed(ScanPhase::Sentences, section_ids);
        }
        self.timer.end(ScanPhase::Sentences);
        Ok(())
    }

    /// Sorts and truncates this thread's hits once it holds too many.
    fn flush_if_needed(&mut self, phase: ScanPhase, section_ids: &[SectionId]) {
        if self.hits.len() < self.flush_at {
            return;
        }
        self.timer.end(phase);
        let sort_start = self.timer.now();
        sort_scanned_hits(&mut self.hits, section_ids);
        self.timer.add_since(ScanPhase::Sort, sort_start);
        self.hits.truncate(self.pass.limit);
        self.update_threshold();
        self.timer.begin();
    }

    /// Raises the score a unit must reach once this thread holds `limit` hits: the `limit`-th best of
    /// them, which is at most the final `limit`-th best of the whole scan.
    fn update_threshold(&mut self) {
        self.threshold = if self.hits.len() >= self.pass.limit {
            self.hits.last().map(|hit| hit.score)
        } else {
            None
        };
    }

    /// The hits of this thread, best first and at most `limit` of them.
    fn finish(mut self, section_ids: &[SectionId]) -> (Vec<ScannedUnitHit>, ScanTimings) {
        let sort_start = self.timer.now();
        sort_scanned_hits(&mut self.hits, section_ids);
        self.timer.add_since(ScanPhase::Sort, sort_start);
        self.hits.truncate(self.pass.limit);
        (self.hits, self.timer.finish())
    }
}

/// One pass over the stored section text: the paragraphs and sentences of `meta`'s document that
/// `mode` accepts, best `limit` first, and the section ids they index.
fn scan_stored_sections(
    connection: &Connection,
    meta: &StoredUnitDocument,
    pass: &ScanPass<'_>,
    options: ScanOptions,
) -> Result<(Vec<SectionId>, Vec<ScannedUnitHit>, ScanTimings), ApplicationError> {
    let sections: Vec<(i64, SectionId, usize)> = stored_sections(connection, meta.doc_key)?
        .into_iter()
        .filter(|(_, _, char_len)| *char_len > 0)
        .collect();
    let threads = scan_threads(&sections, pass.store, options);
    // The plain scan of R2c step 3: no cache, one thread and no pruning, section by section in one
    // pass. Every other configuration plans the sections first, which the store makes cheap.
    if threads <= 1 && pass.store.is_none() && !options.prune {
        return scan_sections_interleaved(connection, meta, &sections, pass);
    }
    let mut timer = ScanTimer::new(pass.collect_timings);
    let planned = plan_sections(connection, meta, &sections, pass, threads, &mut timer)?;
    let Some((section_ids, planned)) = planned else {
        // The document outgrew the cache cap while it was planned, so nothing is cached for it and it
        // is scanned one section at a time as before.
        return scan_sections_interleaved(connection, meta, &sections, pass);
    };
    let scanned = if threads <= 1 {
        scan_planned_serial(connection, meta, &planned, &section_ids, pass)
    } else {
        scan_planned_parallel(connection, meta, &planned, &section_ids, pass, threads)
    };
    let (hits, scan_timings) = match scanned? {
        Some(scanned) => scanned,
        // The store lost a section the plan filled, which cannot happen while it holds the document;
        // scanning it one section at a time is still correct.
        None => return scan_sections_interleaved(connection, meta, &sections, pass),
    };
    timer.merge(scan_timings);
    Ok((section_ids, hits, timer.finish()))
}

/// The sections a pass will visit, in the order a thread visits them, plus the section ids they index
/// in section order. `Ok(None)` when the document outgrew the cache cap while it was planned.
fn plan_sections(
    connection: &Connection,
    meta: &StoredUnitDocument,
    sections: &[(i64, SectionId, usize)],
    pass: &ScanPass<'_>,
    threads: usize,
    timer: &mut ScanTimer,
) -> Result<Option<PlannedSections>, ApplicationError> {
    let bounds: Vec<Option<f32>> = if threads <= 1 {
        let mut bounds = Vec::with_capacity(sections.len());
        for (section_key, _, _) in sections {
            timer.tally(|timings| timings.sections_scanned += 1);
            timer.begin();
            let text = scan_section_text(connection, pass.store, meta.doc_key, *section_key)?;
            timer.end(ScanPhase::ReadText);
            timer.begin();
            bounds.push(section_bound(pass, text.lowered()));
            timer.end(ScanPhase::SectionPrefilter);
        }
        bounds
    } else {
        let collect_timings = timer.enabled;
        let mut bounds: Vec<Option<f32>> = vec![None; sections.len()];
        let workers = parallel_map(
            threads,
            sections.len(),
            || ScanTimer::new(collect_timings),
            |index| {
                timer.tally(|timings| timings.sections_scanned += 1);
                timer.begin();
                let text =
                    scan_section_text(connection, pass.store, meta.doc_key, sections[index].0);
                timer.end(ScanPhase::ReadText);
                if pass.store.is_some_and(|store| store.over_cap()) {
                    return Ok(None);
                }
                text.map(Some)
            },
            |worker: &mut ScanTimer, _index, text: Arc<ScanSectionText>| {
                worker.begin();
                let bound = section_bound(pass, text.lowered());
                worker.end(ScanPhase::SectionPrefilter);
                bound
            },
        )?;
        for (worker, results) in workers {
            for (index, bound) in results {
                bounds[index] = bound;
            }
            timer.merge(worker.finish());
        }
        if pass.store.is_some_and(|store| store.over_cap()) {
            return Ok(None);
        }
        bounds
    };

    let mut section_ids = Vec::new();
    let mut planned = Vec::new();
    for ((section_key, section_id, char_len), bound) in sections.iter().zip(bounds) {
        let Some(bound) = bound else {
            continue;
        };
        timer.tally(|timings| timings.sections_passed_prefilter += 1);
        planned.push(PlannedSection {
            section: section_ids.len(),
            section_key: *section_key,
            char_len: *char_len,
            bound,
        });
        section_ids.push(section_id.clone());
    }
    // Visit the highest bound first, so a thread's threshold rises early. The final sort makes the
    // visiting order irrelevant to the result.
    planned.sort_by(|left, right| {
        right
            .bound
            .total_cmp(&left.bound)
            .then_with(|| left.section.cmp(&right.section))
    });
    Ok(Some((section_ids, planned)))
}

/// Scans the planned sections one at a time, in plan order.
fn scan_planned_serial(
    connection: &Connection,
    meta: &StoredUnitDocument,
    planned: &[PlannedSection],
    section_ids: &[SectionId],
    pass: &ScanPass<'_>,
) -> Result<Option<(Vec<ScannedUnitHit>, ScanTimings)>, ApplicationError> {
    let mut timer = ScanTimer::new(pass.collect_timings);
    let mut scanner = SectionScanner::new(pass);
    for section in planned {
        if scanner.skips(section.bound) {
            continue;
        }
        timer.begin();
        let text = scan_section_text(connection, pass.store, meta.doc_key, section.section_key)?;
        timer.end(ScanPhase::ReadText);
        timer.begin();
        let layout = load_whole_section_layout(connection, meta.doc_key, section.section_key)?;
        timer.end(ScanPhase::LayoutDecode);
        scanner.scan_section(section, &text, &layout, section_ids)?;
    }
    let (hits, scan_timings) = scanner.finish(section_ids);
    timer.merge(scan_timings);
    Ok(Some((hits, timer.finish())))
}

/// Scans the planned sections on `threads` threads: this thread reads the stored layouts and the
/// workers decode and scan, each keeping its own hits. `Ok(None)` when the pass has no text store.
fn scan_planned_parallel(
    connection: &Connection,
    meta: &StoredUnitDocument,
    planned: &[PlannedSection],
    section_ids: &[SectionId],
    pass: &ScanPass<'_>,
    threads: usize,
) -> Result<Option<(Vec<ScannedUnitHit>, ScanTimings)>, ApplicationError> {
    let Some(store) = pass.store else {
        return Ok(None);
    };
    let mut timer = ScanTimer::new(pass.collect_timings);
    let workers = parallel_map(
        threads,
        planned.len(),
        || SectionScanner::new(pass),
        |index| {
            let section = &planned[index];
            timer.begin();
            let layout = load_whole_section_layout(connection, meta.doc_key, section.section_key)?;
            timer.end(ScanPhase::LayoutDecode);
            Ok(Some(layout))
        },
        |scanner: &mut SectionScanner<'_>, index, layout: SectionLayout| {
            let section = &planned[index];
            match stored_section_text(store, section.section_key) {
                Ok(text) => scanner.scan_section(section, &text, &layout, section_ids),
                Err(error) => Err(error),
            }
        },
    )?;
    let mut hits: Vec<ScannedUnitHit> = Vec::new();
    let mut merged = ScanTimings::default();
    let mut failed = None;
    for (scanner, results) in workers {
        for (_, result) in results {
            if let Err(error) = result
                && failed.is_none()
            {
                failed = Some(error);
            }
        }
        let (worker_hits, worker_timings) = scanner.finish(section_ids);
        hits.extend(worker_hits);
        merged.merge(worker_timings);
    }
    if let Some(error) = failed {
        return Err(error);
    }
    let sort_start = timer.now();
    sort_scanned_hits(&mut hits, section_ids);
    timer.add_since(ScanPhase::Sort, sort_start);
    hits.truncate(pass.limit);
    timer.merge(merged);
    Ok(Some((hits, timer.finish())))
}

/// Today's scan: one pass over the sections in section order, reading, filtering and scanning each in
/// turn. It is the plain scan the fast scan must equal, and the fallback for a document over the cap.
fn scan_sections_interleaved(
    connection: &Connection,
    meta: &StoredUnitDocument,
    sections: &[(i64, SectionId, usize)],
    pass: &ScanPass<'_>,
) -> Result<(Vec<SectionId>, Vec<ScannedUnitHit>, ScanTimings), ApplicationError> {
    let mut timer = ScanTimer::new(pass.collect_timings);
    let mut section_ids: Vec<SectionId> = Vec::new();
    let mut scanner = SectionScanner::new(pass);

    for (section_key, section_id, char_len) in sections {
        timer.tally(|timings| timings.sections_scanned += 1);
        timer.begin();
        let text = scan_section_text(connection, pass.store, meta.doc_key, *section_key)?;
        timer.end(ScanPhase::ReadText);
        timer.begin();
        let worth_scanning = pass.section_passes_prefilter(text.lowered());
        timer.end(ScanPhase::SectionPrefilter);
        if !worth_scanning {
            continue;
        }
        timer.tally(|timings| timings.sections_passed_prefilter += 1);

        timer.begin();
        let layout = load_whole_section_layout(connection, meta.doc_key, *section_key)?;
        timer.end(ScanPhase::LayoutDecode);
        let section = PlannedSection {
            section: section_ids.len(),
            section_key: *section_key,
            char_len: *char_len,
            // The interleaved scan never prunes: it is the plain scan and the fallback for a document
            // over the cap, and both scan every accepted section.
            bound: f32::INFINITY,
        };
        section_ids.push(section_id.clone());
        scanner.scan_section(&section, &text, &layout, &section_ids)?;
    }

    let (hits, scan_timings) = scanner.finish(&section_ids);
    timer.merge(scan_timings);
    Ok((section_ids, hits, timer.finish()))
}

/// Which candidates a lexical scan accepts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MatchMode {
    /// Every query token is in the candidate.
    Strict,
    /// At least one query token is in the candidate.
    Relaxed,
}

/// One paragraph or sentence hit of a scan; `section` indexes the returned section id list.
#[derive(Debug, PartialEq)]
pub(crate) struct ScannedUnitHit {
    pub section: usize,
    pub kind: SearchHitKind,
    pub paragraph_index: usize,
    pub sentence_index: Option<usize>,
    /// The unit's normalized range start.
    pub start: usize,
    pub score: f32,
}

fn stored_sections(
    connection: &Connection,
    doc_key: i64,
) -> Result<Vec<(i64, SectionId, usize)>, ApplicationError> {
    let mut statement = connection
        .prepare(
            "SELECT section_key, section_id, char_len
             FROM section_layout_v2
             WHERE doc_key = ?1
             ORDER BY section_key ASC",
        )
        .map_err(text_unit_index_error)?;
    let rows = statement
        .query_map(params![doc_key], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })
        .map_err(text_unit_index_error)?;
    let mut sections = Vec::new();
    for row in rows {
        let (section_key, section_id, char_len) = row.map_err(text_unit_index_error)?;
        sections.push((
            section_key,
            SectionId(section_id),
            i64_to_usize(char_len, "char_len")?,
        ));
    }
    Ok(sections)
}

/// Scores one unit as a candidate, or `None` when the mode rejects it.
fn unit_score(
    query: &LexicalQuery,
    mode: MatchMode,
    text: &str,
    lowered: &str,
    kind: SearchHitKind,
) -> Option<f32> {
    match mode {
        MatchMode::Strict => strict_score(query, text, lowered, kind),
        MatchMode::Relaxed => {
            let matched = query.relaxed_count(text, lowered);
            if matched > 0 {
                Some(score_lowered_with_phrase(
                    lowered,
                    &query.raw_lowered,
                    kind,
                    matched,
                    None,
                ))
            } else {
                None
            }
        }
    }
}

/// The strict score of one unit, or `None` when a query token is missing from it.
fn strict_score(
    query: &LexicalQuery,
    text: &str,
    lowered: &str,
    kind: SearchHitKind,
) -> Option<f32> {
    if !query.strict_match(text, lowered) {
        return None;
    }
    Some(score_lowered_with_phrase(
        lowered,
        &query.raw_lowered,
        kind,
        query.token_count(),
        strict_phrase_known(query),
    ))
}

/// The strict score of a paragraph whose `may_contain_all` prefilter passed on the paragraph's own
/// lowered slice.
fn unit_score_after_all_prefilter(
    query: &LexicalQuery,
    text: &str,
    lowered: &str,
    kind: SearchHitKind,
) -> Option<f32> {
    if query.all_cjk() {
        // Every token is a CJK char or an adjacent CJK pair, which `tokenize` emits from the lowered
        // text itself, so the prefilter that passed is exactly `strict_match`.
        return Some(score_lowered_with_phrase(
            lowered,
            &query.raw_lowered,
            kind,
            query.token_count(),
            strict_phrase_known(query),
        ));
    }
    strict_score(query, text, lowered, kind)
}

/// The phrase answer a `strict_match` already gave: it contains-tests every token, so a token equal to
/// the whole lowered query proves the phrase.
fn strict_phrase_known(query: &LexicalQuery) -> Option<bool> {
    query.phrase_is_token().map(|_| true)
}

/// The lowercase form of one unit slice. A whole section that lowercased to itself (`borrowed_section`)
/// lowercases every slice of it to itself too.
fn slice_lowered<'a>(slice: &'a str, borrowed_section: bool) -> Cow<'a, str> {
    if borrowed_section {
        Cow::Borrowed(slice)
    } else {
        lowercase_fast(slice)
    }
}

fn sort_scanned_hits(hits: &mut [ScannedUnitHit], section_ids: &[SectionId]) {
    hits.sort_by(|left, right| {
        right
            .score
            .total_cmp(&left.score)
            .then_with(|| candidate_kind_order(left.kind).cmp(&candidate_kind_order(right.kind)))
            .then_with(|| {
                section_ids[left.section]
                    .0
                    .cmp(&section_ids[right.section].0)
            })
            .then_with(|| left.start.cmp(&right.start))
    });
}

/// The v5 search index kept its own FTS tables. Search now scans the stored section text, so they are
/// dropped, and the file is vacuumed once to give their space back.
fn drop_legacy_lexical_search_tables(connection: &Connection) -> Result<(), ApplicationError> {
    let legacy_tables: i64 = connection
        .query_row(
            "SELECT count(*) FROM sqlite_master
             WHERE type = 'table' AND name LIKE 'lexical!_search!_%' ESCAPE '!'",
            [],
            |row| row.get(0),
        )
        .map_err(text_unit_index_error)?;
    if legacy_tables == 0 {
        return Ok(());
    }
    connection
        .execute_batch(
            "DROP TABLE IF EXISTS lexical_search_fts_v5;
             DROP TABLE IF EXISTS lexical_search_candidates_v5;
             DROP TABLE IF EXISTS lexical_search_documents_v5;
             DROP TABLE IF EXISTS lexical_search_units_v2;
             DROP TABLE IF EXISTS lexical_search_documents_v2;
             DROP TABLE IF EXISTS lexical_search_meta;",
        )
        .map_err(text_unit_index_error)?;
    // Reclaim the space the dropped tables held. A failure here is harmless: the tables are gone and
    // the file merely keeps its size.
    let _ = connection.execute_batch("VACUUM");
    Ok(())
}

/// One section's borrowed units and coverages, grouped in a single pass over the unit sets.
#[derive(Default)]
struct SectionGroups<'a> {
    paragraphs: Vec<&'a TextUnit>,
    paragraph_coverage: Option<&'a ParagraphSectionCoverage>,
    sentences: Vec<&'a SentenceTextUnit>,
    sentence_coverage: Vec<&'a SentenceParagraphCoverage>,
}

/// Where one document's per-section layouts and paragraph coverage records come from: the unit sets
/// of `replace_document_sets`, or the boundaries of `replace_document_segmentation`. Both must produce
/// byte-identical rows.
enum SectionLayoutSource<'a> {
    Units(&'a HashMap<SectionId, SectionGroups<'a>>),
    Segmentation(&'a HashMap<&'a SectionId, &'a SectionSegmentation>),
}

impl SectionLayoutSource<'_> {
    fn paragraph_coverage(&self, section_id: &SectionId) -> Option<&ParagraphSectionCoverage> {
        match self {
            Self::Units(groups) => groups
                .get(section_id)
                .and_then(|group| group.paragraph_coverage),
            Self::Segmentation(sections) => sections
                .get(section_id)
                .copied()
                .map(|section| &section.paragraph_coverage),
        }
    }

    fn paragraph_coverage_json(
        &self,
        section_id: &SectionId,
    ) -> Result<Option<String>, ApplicationError> {
        let coverage = self.paragraph_coverage(section_id);
        match coverage {
            Some(coverage) => Ok(Some(
                serde_json::to_string(&StoredParagraphCoverage::from_coverage(coverage))
                    .map_err(|error| ApplicationError::TextUnitIndexFailed(error.to_string()))?,
            )),
            None => Ok(None),
        }
    }

    fn layout(&self, section_id: &SectionId) -> Result<SectionLayout, ApplicationError> {
        match self {
            Self::Units(groups) => match groups.get(section_id) {
                Some(group) => build_section_layout(group),
                None => Ok(empty_section_layout()),
            },
            Self::Segmentation(sections) => match sections.get(section_id).copied() {
                Some(section) => build_segmentation_section_layout(section),
                None => Ok(empty_section_layout()),
            },
        }
    }
}

fn empty_section_layout() -> SectionLayout {
    SectionLayout {
        paragraphs: Vec::new(),
        sentences: Vec::new(),
        coverage: Vec::new(),
    }
}

/// One stored chunk's byte ranges into the whole-section layout. Paragraphs and sentences are
/// contiguous slices; coverage is a contiguous slice too because the chunker only cuts sorted
/// sections, where coverage is ordered by paragraph index.
#[derive(Clone, Debug)]
struct ChunkRange {
    chunk_no: usize,
    first_paragraph_index: usize,
    first_sentence_index: usize,
    paragraph_slot: usize,
    sentence_slot: usize,
    paragraph_start: usize,
    paragraph_end: usize,
    sentence_start: usize,
    sentence_end: usize,
    coverage_start: usize,
    coverage_end: usize,
    /// A chunk that carries only the next sentence run of one paragraph: no paragraph record and
    /// exactly that paragraph's coverage record.
    mid_paragraph: bool,
}

/// One chunk ready to write: its ranges and the stream entry offsets the planner would assign.
#[derive(Clone, Debug)]
struct ChunkRow {
    range: ChunkRange,
    paragraph_entry: usize,
    sentence_entry: usize,
    eligible_paragraph_entry: usize,
    eligible_sentence_entry: usize,
    paragraph_entries: usize,
    paragraph_eligible_entries: usize,
    sentence_entries: usize,
    sentence_eligible_entries: usize,
}

/// The header row's counters for one kind: summary totals and both coverages.
struct StoredKindSummary {
    total_preserve: usize,
    total_eligible: usize,
    preserve_json: String,
    eligible_json: String,
}

impl StoredKindSummary {
    fn from_counts(counts: &SectionStreamCounts) -> Result<Self, ApplicationError> {
        Ok(Self {
            total_preserve: counts.total_items_preserve,
            total_eligible: counts.total_items_eligible,
            preserve_json: serde_json::to_string(&StoredStreamCoverage::from_coverage(
                &counts.preserve_source,
            ))
            .map_err(|error| ApplicationError::TextUnitIndexFailed(error.to_string()))?,
            eligible_json: serde_json::to_string(&StoredStreamCoverage::from_coverage(
                &counts.eligible_only,
            ))
            .map_err(|error| ApplicationError::TextUnitIndexFailed(error.to_string()))?,
        })
    }

    fn zero() -> Self {
        let coverage = TextUnitEnumerationCoverage {
            owner_chars: 0,
            section_separator_chars: 0,
            sentence_separator_chars: 0,
            paragraph_count: 0,
            sentence_eligible_paragraphs: 0,
            non_prose_paragraphs: 0,
            coarse_structural_paragraphs: 0,
            represented_paragraphs: 0,
            represented_sentences: 0,
            coarse_non_prose_items: 0,
            coarse_structural_items: 0,
            intentionally_skipped: 0,
            unsupported_gaps: 0,
            source_complete: true,
        };
        Self {
            total_preserve: 0,
            total_eligible: 0,
            preserve_json: serde_json::to_string(&StoredStreamCoverage::from_coverage(&coverage))
                .unwrap_or_else(|_| "{}".into()),
            eligible_json: serde_json::to_string(&StoredStreamCoverage::from_coverage(&coverage))
                .unwrap_or_else(|_| "{}".into()),
        }
    }
}

type SectionDescriptors = (
    Vec<ParagraphDesc>,
    Vec<SentenceDesc>,
    Vec<ParagraphCoverageDesc>,
    Option<SectionCoverageDesc>,
);

fn build_section_descriptors(
    layout: &SectionLayout,
    section_coverage: Option<&ParagraphSectionCoverage>,
) -> Result<SectionDescriptors, ApplicationError> {
    let paragraphs = layout
        .paragraphs
        .iter()
        .map(|paragraph| ParagraphDesc {
            paragraph_index: paragraph.paragraph_index,
            char_count: paragraph.end - paragraph.start,
        })
        .collect();
    let sentences = layout
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
    let section_coverage = section_coverage.map(|coverage| SectionCoverageDesc {
        owner_chars: coverage.owner_chars,
        separator_chars: coverage.separator_chars,
        paragraph_count: coverage.paragraph_count,
    });
    Ok((paragraphs, sentences, coverage, section_coverage))
}

fn uvarint_len(value: u64) -> usize {
    let mut value = value;
    let mut len = 1;
    while value >= 0x80 {
        value >>= 7;
        len += 1;
    }
    len
}

fn zigzag_len(value: i64) -> usize {
    let encoded = ((value as u64) << 1) ^ ((value >> 63) as u64);
    uvarint_len(encoded)
}

fn delta_len(value: usize, previous: usize) -> usize {
    zigzag_len(value as i64 - previous as i64)
}

fn paragraph_cost(
    paragraph: &LayoutParagraph,
    first_index: usize,
    previous: Option<&LayoutParagraph>,
) -> usize {
    let previous_source = previous.map_or(0, |value| value.source_order);
    let previous_start = previous.map_or(0, |value| value.start);
    let previous_end = previous.map_or(0, |value| value.end);
    uvarint_len((paragraph.paragraph_index - first_index) as u64)
        + delta_len(paragraph.source_order, previous_source)
        + delta_len(paragraph.start, previous_start)
        + delta_len(paragraph.end, previous_end)
}

fn sentence_cost(
    sentence: &LayoutSentence,
    first_index: usize,
    previous: Option<&LayoutSentence>,
) -> usize {
    let previous_source = previous.map_or(0, |value| value.source_order);
    let previous_start = previous.map_or(0, |value| value.start);
    let previous_end = previous.map_or(0, |value| value.end);
    let previous_parent_start = previous.map_or(0, |value| value.parent_start);
    let previous_parent_end = previous.map_or(0, |value| value.parent_end);
    uvarint_len((sentence.paragraph_index - first_index) as u64)
        + uvarint_len(sentence.sentence_index as u64)
        + delta_len(sentence.source_order, previous_source)
        + delta_len(sentence.start, previous_start)
        + delta_len(sentence.end, previous_end)
        + delta_len(sentence.parent_start, previous_parent_start)
        + delta_len(sentence.parent_end, previous_parent_end)
}

fn coverage_cost(
    coverage: &LayoutCoverage,
    first_index: usize,
    previous: Option<&LayoutCoverage>,
) -> usize {
    let previous_start = previous.map_or(0, |value| value.paragraph_start);
    let previous_end = previous.map_or(0, |value| value.paragraph_end);
    uvarint_len((coverage.paragraph_index - first_index) as u64)
        + delta_len(coverage.paragraph_start, previous_start)
        + delta_len(coverage.paragraph_end, previous_end)
        + uvarint_len(u64::from(coverage.content_class))
        + uvarint_len(u64::from(coverage.eligibility))
        + uvarint_len(coverage.paragraph_chars as u64)
        + uvarint_len(coverage.sentence_chars as u64)
        + uvarint_len(coverage.separator_chars as u64)
        + uvarint_len(coverage.coarse_only_chars as u64)
        + uvarint_len(coverage.sentence_count as u64)
}

/// Cuts a sorted layout into chunks of about [`LAYOUT_CHUNK_BYTES`]. A paragraph whose sentences
/// alone exceed the budget gets one chunk with its paragraph record and then chunks carrying only
/// its coverage record and the next sentence run. An unsorted layout is one chunk, so in-chunk
/// semantics are exactly the linear scan's.
fn build_chunk_ranges(layout: &SectionLayout, sorted: bool) -> Vec<ChunkRange> {
    let whole = !sorted
        || layout.paragraphs.is_empty()
        || layout
            .coverage
            .windows(2)
            .any(|pair| pair[0].paragraph_index >= pair[1].paragraph_index);
    if whole {
        let first_paragraph_index = layout
            .paragraphs
            .iter()
            .map(|paragraph| paragraph.paragraph_index)
            .chain(
                layout
                    .sentences
                    .iter()
                    .map(|sentence| sentence.paragraph_index),
            )
            .chain(
                layout
                    .coverage
                    .iter()
                    .map(|coverage| coverage.paragraph_index),
            )
            .min()
            .unwrap_or(0);
        return vec![ChunkRange {
            chunk_no: 0,
            first_paragraph_index,
            first_sentence_index: 0,
            paragraph_slot: 0,
            sentence_slot: 0,
            paragraph_start: 0,
            paragraph_end: layout.paragraphs.len(),
            sentence_start: 0,
            sentence_end: layout.sentences.len(),
            coverage_start: 0,
            coverage_end: layout.coverage.len(),
            mid_paragraph: false,
        }];
    }

    let mut coverage_slots = HashMap::new();
    for (slot, coverage) in layout.coverage.iter().enumerate() {
        coverage_slots.insert(coverage.paragraph_index, slot);
    }
    let mut sentences_by_paragraph: HashMap<usize, Vec<usize>> = HashMap::new();
    for (slot, sentence) in layout.sentences.iter().enumerate() {
        sentences_by_paragraph
            .entry(sentence.paragraph_index)
            .or_default()
            .push(slot);
    }

    fn regular_range(
        paragraph_index: usize,
        paragraph_slot: usize,
        sentence_slot: usize,
        coverage_slot: Option<usize>,
    ) -> ChunkRange {
        let (coverage_start, coverage_end) =
            coverage_slot.map(|slot| (slot, slot + 1)).unwrap_or((0, 0));
        ChunkRange {
            chunk_no: 0,
            first_paragraph_index: paragraph_index,
            first_sentence_index: 0,
            paragraph_slot,
            sentence_slot,
            paragraph_start: paragraph_slot,
            paragraph_end: paragraph_slot,
            sentence_start: sentence_slot,
            sentence_end: sentence_slot,
            coverage_start,
            coverage_end,
            mid_paragraph: false,
        }
    }

    fn mid_range(
        paragraph_index: usize,
        paragraph_slot: usize,
        sentence_slot: usize,
        sentence_index: usize,
        coverage_slot: Option<usize>,
    ) -> ChunkRange {
        let (coverage_start, coverage_end) =
            coverage_slot.map(|slot| (slot, slot + 1)).unwrap_or((0, 0));
        ChunkRange {
            chunk_no: 0,
            first_paragraph_index: paragraph_index,
            first_sentence_index: sentence_index,
            paragraph_slot,
            sentence_slot,
            paragraph_start: paragraph_slot,
            paragraph_end: paragraph_slot,
            sentence_start: sentence_slot,
            sentence_end: sentence_slot,
            coverage_start,
            coverage_end,
            mid_paragraph: true,
        }
    }

    fn flush(pending: &mut Option<(ChunkRange, usize)>, chunks: &mut Vec<ChunkRange>) {
        if let Some((mut range, _)) = pending.take() {
            range.chunk_no = chunks.len();
            chunks.push(range);
        }
    }

    let mut chunks = Vec::new();
    let mut pending: Option<(ChunkRange, usize)> = None;
    let mut sentence_cursor = 0usize;

    for (p_slot, paragraph) in layout.paragraphs.iter().enumerate() {
        let sentence_slots = sentences_by_paragraph
            .get(&paragraph.paragraph_index)
            .cloned()
            .unwrap_or_default();
        let coverage_slot = coverage_slots.get(&paragraph.paragraph_index).copied();

        if let Some((range, estimated)) = pending.as_ref() {
            let paragraph_estimate = paragraph_cost(paragraph, range.first_paragraph_index, None);
            let coverage_estimate = coverage_slot.map_or(0, |slot| {
                coverage_cost(&layout.coverage[slot], range.first_paragraph_index, None)
            });
            let non_empty = range.paragraph_end > range.paragraph_start
                || range.sentence_end > range.sentence_start;
            if range.mid_paragraph
                || (non_empty
                    && *estimated + paragraph_estimate + coverage_estimate > LAYOUT_CHUNK_BYTES)
            {
                flush(&mut pending, &mut chunks);
            }
        }
        if pending.is_none() {
            pending = Some((
                regular_range(
                    paragraph.paragraph_index,
                    p_slot,
                    sentence_cursor,
                    coverage_slot,
                ),
                8,
            ));
        }

        {
            let (range, estimated) = pending.as_mut().expect("a chunk must be open");
            *estimated += paragraph_cost(paragraph, range.first_paragraph_index, None);
            if let Some(slot) = coverage_slot {
                *estimated +=
                    coverage_cost(&layout.coverage[slot], range.first_paragraph_index, None);
                if range.coverage_end == 0 {
                    range.coverage_start = slot;
                    range.coverage_end = slot + 1;
                } else {
                    range.coverage_start = range.coverage_start.min(slot);
                    range.coverage_end = range.coverage_end.max(slot + 1);
                }
            }
            range.paragraph_end = p_slot + 1;
        }

        let mut previous_sentence: Option<&LayoutSentence> = None;
        for sentence_slot in sentence_slots {
            let sentence = &layout.sentences[sentence_slot];

            if let Some((range, estimated)) = pending.as_ref() {
                let sentence_estimate =
                    sentence_cost(sentence, range.first_paragraph_index, previous_sentence);
                let non_empty = range.paragraph_end > range.paragraph_start
                    || range.sentence_end > range.sentence_start;
                if !range.mid_paragraph
                    && non_empty
                    && *estimated + sentence_estimate > LAYOUT_CHUNK_BYTES
                {
                    flush(&mut pending, &mut chunks);
                }
            }
            if pending.is_none() {
                pending = Some((
                    mid_range(
                        paragraph.paragraph_index,
                        p_slot,
                        sentence_slot,
                        sentence.sentence_index,
                        coverage_slot,
                    ),
                    8,
                ));
            }

            let flush_mid = {
                let (range, estimated) = pending.as_mut().expect("a chunk must be open");
                let shrunk =
                    sentence_cost(sentence, range.first_paragraph_index, previous_sentence);
                *estimated += shrunk;
                range.sentence_end = sentence_slot + 1;
                range.mid_paragraph && *estimated + 64 > LAYOUT_CHUNK_BYTES
            };
            previous_sentence = Some(sentence);
            sentence_cursor = sentence_slot + 1;
            if flush_mid {
                flush(&mut pending, &mut chunks);
            }
        }

        if pending
            .as_ref()
            .is_some_and(|(range, _)| range.mid_paragraph)
        {
            flush(&mut pending, &mut chunks);
        }
    }
    flush(&mut pending, &mut chunks);
    chunks
}

/// Encodes one chunk's local layout: paragraph indices are relative to the chunk's first paragraph
/// and every delta base starts at zero, which `SectionLayout::encode` does for any slice.
fn chunk_layout(layout: &SectionLayout, range: &ChunkRange) -> SectionLayout {
    let first = range.first_paragraph_index;
    let relative = |paragraph_index: usize| paragraph_index - first;
    let mut paragraphs = Vec::new();
    let mut sentences = Vec::new();
    if range.mid_paragraph {
        // A mid chunk keeps the paragraph record so the stream walk can emit its sentence run.
        // The write path still records `paragraph_count = 0`, and reassembly drops the duplicate.
        let mut paragraph = layout.paragraphs[range.paragraph_start].clone();
        paragraph.paragraph_index = relative(paragraph.paragraph_index);
        paragraphs.push(paragraph);
    } else {
        for paragraph in &layout.paragraphs[range.paragraph_start..range.paragraph_end] {
            let mut paragraph = paragraph.clone();
            paragraph.paragraph_index = relative(paragraph.paragraph_index);
            paragraphs.push(paragraph);
        }
    }
    for sentence in &layout.sentences[range.sentence_start..range.sentence_end] {
        let mut sentence = sentence.clone();
        sentence.paragraph_index = relative(sentence.paragraph_index);
        sentences.push(sentence);
    }
    let coverage = layout.coverage[range.coverage_start..range.coverage_end]
        .iter()
        .map(|coverage| {
            let mut coverage = coverage.clone();
            coverage.paragraph_index = relative(coverage.paragraph_index);
            coverage
        })
        .collect();
    SectionLayout {
        paragraphs,
        sentences,
        coverage,
    }
}

/// Assigns every planner entry to the chunk that holds its unit, and returns the two kinds'
/// header summaries plus the per-chunk stream starts.
fn plan_chunk_starts(
    section_id: &SectionId,
    layout: &SectionLayout,
    section_coverage: Option<&ParagraphSectionCoverage>,
    chunks: &mut [ChunkRow],
) -> Result<(StoredKindSummary, StoredKindSummary, Option<String>), ApplicationError> {
    let (paragraphs, sentences, coverage, section_coverage) =
        build_section_descriptors(layout, section_coverage)?;
    let mut summaries = Vec::with_capacity(2);
    for kind in [
        RequestedTextUnitKind::Paragraph,
        RequestedTextUnitKind::Sentence,
    ] {
        let mut cursor = 0usize;
        let counts = walk_section_stream(
            section_id,
            &paragraphs,
            section_coverage.as_ref(),
            &sentences,
            &coverage,
            kind,
            &mut |entry| {
                let slot = match entry.unit {
                    UnitRef::Paragraph(slot) | UnitRef::Sentence(slot) => slot,
                };
                // Entries arrive in array order, so the cursor only moves forward.
                loop {
                    if cursor >= chunks.len() {
                        break;
                    }
                    let (start, end) = match entry.unit {
                        UnitRef::Paragraph(_) => (
                            chunks[cursor].range.paragraph_start,
                            chunks[cursor].range.paragraph_end,
                        ),
                        UnitRef::Sentence(_) => (
                            chunks[cursor].range.sentence_start,
                            chunks[cursor].range.sentence_end,
                        ),
                    };
                    if start <= slot && slot < end {
                        break;
                    }
                    if slot >= end {
                        cursor += 1;
                        continue;
                    }
                    cursor += 1;
                }
                if let Some(chunk) = chunks.get_mut(cursor) {
                    let counts = match (kind, entry.unit) {
                        (RequestedTextUnitKind::Paragraph, _) => (
                            &mut chunk.paragraph_entries,
                            &mut chunk.paragraph_eligible_entries,
                        ),
                        (RequestedTextUnitKind::Sentence, _) => (
                            &mut chunk.sentence_entries,
                            &mut chunk.sentence_eligible_entries,
                        ),
                    };
                    *counts.0 += 1;
                    if entry.eligible {
                        *counts.1 += 1;
                    }
                }
            },
        );
        match counts {
            Ok(counts) => summaries.push(StoredKindSummary::from_counts(&counts)?),
            Err(error) => {
                let message = match error {
                    ApplicationError::InvalidRequest(message) => message,
                    other => other.to_string(),
                };
                return Ok((
                    StoredKindSummary::zero(),
                    StoredKindSummary::zero(),
                    Some(message),
                ));
            }
        }
    }
    let paragraph_summary = summaries.remove(0);
    let sentence_summary = summaries.remove(0);

    let mut paragraph_entry = 0usize;
    let mut eligible_paragraph_entry = 0usize;
    let mut sentence_entry = 0usize;
    let mut eligible_sentence_entry = 0usize;
    for chunk in chunks.iter_mut() {
        chunk.paragraph_entry = paragraph_entry;
        chunk.eligible_paragraph_entry = eligible_paragraph_entry;
        chunk.sentence_entry = sentence_entry;
        chunk.eligible_sentence_entry = eligible_sentence_entry;
        paragraph_entry += chunk.paragraph_entries;
        eligible_paragraph_entry += chunk.paragraph_eligible_entries;
        sentence_entry += chunk.sentence_entries;
        eligible_sentence_entry += chunk.sentence_eligible_entries;
    }
    Ok((paragraph_summary, sentence_summary, None))
}

/// Writes one document's `section_text_v3` chunks, `section_layout_v2` headers and
/// `section_layout_chunk_v2` rows in a transaction.
fn write_document_sections(
    connection: &mut Connection,
    document: &Document,
    normalized_document_hash: &NormalizedDocumentHash,
    source: &SectionLayoutSource<'_>,
) -> Result<(), ApplicationError> {
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
                normalized_document_hash.as_ref(),
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
            "DELETE FROM section_text_v3 WHERE doc_key = ?1",
            params![doc_key],
        )
        .map_err(text_unit_index_error)?;
    transaction
        .execute(
            "DELETE FROM section_layout_chunk_v2 WHERE doc_key = ?1",
            params![doc_key],
        )
        .map_err(text_unit_index_error)?;
    transaction
        .execute(
            "DELETE FROM section_layout_v2 WHERE doc_key = ?1",
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
                ApplicationError::TextUnitIndexFailed("document has too many sections".into())
            })?,
        );
    }

    {
        let mut chunk_statement = transaction
            .prepare(
                "INSERT INTO section_text_v3(doc_key, section_key, chunk_no, encoding, data)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
            )
            .map_err(text_unit_index_error)?;
        let mut header_statement = transaction
            .prepare(
                "INSERT INTO section_layout_v2(
                    doc_key,
                    section_key,
                    section_id,
                    char_len,
                    paragraph_coverage_json,
                    plan_error,
                    paragraph_count,
                    sentence_count,
                    chunk_count,
                    sorted_flags,
                    total_items_paragraph_preserve,
                    total_items_paragraph_eligible,
                    total_items_sentence_preserve,
                    total_items_sentence_eligible,
                    coverage_paragraph_preserve_json,
                    coverage_paragraph_eligible_json,
                    coverage_sentence_preserve_json,
                    coverage_sentence_eligible_json
                 ) VALUES (
                    ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10,
                    ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18
                 )",
            )
            .map_err(text_unit_index_error)?;
        let mut chunk_row_statement = transaction
            .prepare(
                "INSERT INTO section_layout_chunk_v2(
                    doc_key,
                    section_key,
                    chunk_no,
                    first_paragraph_index,
                    first_sentence_index,
                    paragraph_slot,
                    sentence_slot,
                    paragraph_entry,
                    sentence_entry,
                    eligible_paragraph_entry,
                    eligible_sentence_entry,
                    paragraph_count,
                    sentence_count,
                    blob
                 ) VALUES (
                    ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14
                 )",
            )
            .map_err(text_unit_index_error)?;

        for section in &ordered_sections {
            let section_key = section_keys[&section.id];
            insert_section_text(&mut chunk_statement, doc_key, section_key, &section.content)?;

            let paragraph_coverage = source.paragraph_coverage(&section.id);
            let paragraph_coverage_json = source.paragraph_coverage_json(&section.id)?;
            let layout = source.layout(&section.id)?;
            let paragraphs_sorted = layout
                .paragraphs
                .windows(2)
                .all(|pair| pair[0].paragraph_index < pair[1].paragraph_index);
            let sentences_sorted = layout.sentences.windows(2).all(|pair| {
                (pair[0].paragraph_index, pair[0].sentence_index)
                    < (pair[1].paragraph_index, pair[1].sentence_index)
            });
            let sorted = paragraphs_sorted && sentences_sorted;
            let ranges = build_chunk_ranges(&layout, sorted);
            let mut chunks = ranges
                .into_iter()
                .map(|range| ChunkRow {
                    range,
                    paragraph_entry: 0,
                    sentence_entry: 0,
                    eligible_paragraph_entry: 0,
                    eligible_sentence_entry: 0,
                    paragraph_entries: 0,
                    paragraph_eligible_entries: 0,
                    sentence_entries: 0,
                    sentence_eligible_entries: 0,
                })
                .collect::<Vec<_>>();
            let (paragraph_summary, sentence_summary, plan_error) =
                plan_chunk_starts(&section.id, &layout, paragraph_coverage, &mut chunks)?;

            let mut sorted_flags = 0i64;
            if paragraphs_sorted {
                sorted_flags |= 1;
            }
            if sentences_sorted {
                sorted_flags |= 2;
            }
            header_statement
                .execute(params![
                    doc_key,
                    section_key,
                    &section.id.0,
                    usize_to_i64(section.normalized_text_len())?,
                    paragraph_coverage_json,
                    plan_error,
                    usize_to_i64(layout.paragraphs.len())?,
                    usize_to_i64(layout.sentences.len())?,
                    usize_to_i64(chunks.len())?,
                    sorted_flags,
                    usize_to_i64(paragraph_summary.total_preserve)?,
                    usize_to_i64(paragraph_summary.total_eligible)?,
                    usize_to_i64(sentence_summary.total_preserve)?,
                    usize_to_i64(sentence_summary.total_eligible)?,
                    paragraph_summary.preserve_json,
                    paragraph_summary.eligible_json,
                    sentence_summary.preserve_json,
                    sentence_summary.eligible_json,
                ])
                .map_err(text_unit_index_error)?;

            for chunk in &chunks {
                let blob = chunk_layout(&layout, &chunk.range).encode()?;
                chunk_row_statement
                    .execute(params![
                        doc_key,
                        section_key,
                        usize_to_i64(chunk.range.chunk_no)?,
                        usize_to_i64(chunk.range.first_paragraph_index)?,
                        usize_to_i64(chunk.range.first_sentence_index)?,
                        usize_to_i64(chunk.range.paragraph_slot)?,
                        usize_to_i64(chunk.range.sentence_slot)?,
                        usize_to_i64(chunk.paragraph_entry)?,
                        usize_to_i64(chunk.sentence_entry)?,
                        usize_to_i64(chunk.eligible_paragraph_entry)?,
                        usize_to_i64(chunk.eligible_sentence_entry)?,
                        usize_to_i64(chunk.range.paragraph_end - chunk.range.paragraph_start)?,
                        usize_to_i64(chunk.range.sentence_end - chunk.range.sentence_start)?,
                        blob,
                    ])
                    .map_err(text_unit_index_error)?;
            }
        }
    }

    transaction.commit().map_err(text_unit_index_error)?;
    Ok(())
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
                    return paragraph_units_from_layout(connection, &meta);
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
        run_sqlite(
            &self.connection,
            ApplicationError::TextUnitIndexFailed,
            move |connection| {
                let normalized_document_hash = paragraphs.normalized_document_hash.clone();

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

                write_document_sections(
                    connection,
                    &document,
                    &normalized_document_hash,
                    &SectionLayoutSource::Units(&groups),
                )
            },
        )
        .await?;
        self.drop_scan_text();
        Ok(())
    }

    async fn replace_document_segmentation(
        &self,
        document: &Arc<Document>,
        segmentation: Arc<DocumentSegmentation>,
    ) -> Result<(), ApplicationError> {
        let document = Arc::clone(document);
        run_sqlite(
            &self.connection,
            ApplicationError::TextUnitIndexFailed,
            move |connection| {
                let normalized_document_hash = document.normalized_document_hash();
                let sections = segmentation
                    .sections
                    .iter()
                    .map(|section| (&section.section_id, section))
                    .collect::<HashMap<_, _>>();
                write_document_sections(
                    connection,
                    &document,
                    &normalized_document_hash,
                    &SectionLayoutSource::Segmentation(&sections),
                )
            },
        )
        .await?;
        self.drop_scan_text();
        Ok(())
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
                let Some(header) = load_section_header(connection, &meta, &section_id)? else {
                    return Ok(Some(SectionTextUnits {
                        paragraphs: Vec::new(),
                        paragraph_coverage: None,
                        sentences: Vec::new(),
                        sentence_coverage: Vec::new(),
                    }));
                };

                let layout =
                    load_whole_section_layout(connection, meta.doc_key, header.section_key)?;
                let text = load_section_text(
                    connection,
                    meta.doc_key,
                    header.section_key,
                    header.char_len,
                )?;
                let index = NormalizedTextIndex::new(&text);

                let mut paragraphs = Vec::with_capacity(layout.paragraphs.len());
                for paragraph in &layout.paragraphs {
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

                let mut sentences = Vec::with_capacity(layout.sentences.len());
                for sentence in &layout.sentences {
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

                let mut sentence_coverage = Vec::with_capacity(layout.coverage.len());
                for coverage in &layout.coverage {
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
                    paragraph_coverage: header.paragraph_coverage.clone(),
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
                let Some(header) = load_section_header(connection, &meta, &section_id)? else {
                    return Err(stale_text_units(&document_id));
                };

                let (chunk, unit, start, end) = match unit_kind {
                    EffectiveTextUnitKind::Paragraph => {
                        let Some(chunk) = load_section_chunk_at_paragraph(
                            connection,
                            meta.doc_key,
                            header.section_key,
                            paragraph_index,
                        )?
                        else {
                            return Ok(None);
                        };
                        let Some(local_index) =
                            paragraph_index.checked_sub(chunk.first_paragraph_index)
                        else {
                            return Ok(None);
                        };
                        let Some(slot) = chunk
                            .layout
                            .paragraphs
                            .iter()
                            .position(|paragraph| paragraph.paragraph_index == local_index)
                        else {
                            return Ok(None);
                        };
                        let start = chunk.layout.paragraphs[slot].start;
                        let end = chunk.layout.paragraphs[slot].end;
                        (chunk, UnitRef::Paragraph(slot), start, end)
                    }
                    EffectiveTextUnitKind::Sentence => {
                        let Some(sentence_index) = sentence_index else {
                            return Ok(None);
                        };
                        let Some(chunk) = load_section_chunk_at_sentence(
                            connection,
                            meta.doc_key,
                            header.section_key,
                            paragraph_index,
                            sentence_index,
                        )?
                        else {
                            return Ok(None);
                        };
                        let Some(local_index) =
                            paragraph_index.checked_sub(chunk.first_paragraph_index)
                        else {
                            return Ok(None);
                        };
                        let Some(slot) = chunk.layout.sentences.iter().position(|sentence| {
                            sentence.paragraph_index == local_index
                                && sentence.sentence_index == sentence_index
                        }) else {
                            return Ok(None);
                        };
                        let start = chunk.layout.sentences[slot].start;
                        let end = chunk.layout.sentences[slot].end;
                        (chunk, UnitRef::Sentence(slot), start, end)
                    }
                };

                let text =
                    load_text_slice(connection, meta.doc_key, header.section_key, start, end)?;
                let index = NormalizedTextIndex::new(&text);
                Ok(Some(build_owned_unit(
                    &meta,
                    &section_id,
                    &chunk.layout,
                    chunk.first_paragraph_index,
                    unit,
                    &index,
                    start,
                )?))
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
                let Some(header) = load_section_header(connection, &meta, &section_id)? else {
                    return Err(ApplicationError::InvalidRequest(format!(
                        "paragraph coverage is unavailable for section {}",
                        section_id.0
                    )));
                };
                if let Some(error) = header.plan_error.as_deref() {
                    return Err(ApplicationError::InvalidRequest(error.to_owned()));
                }
                if header.paragraph_coverage.is_none() {
                    return Err(ApplicationError::InvalidRequest(format!(
                        "paragraph coverage is unavailable for section {}",
                        section_id.0
                    )));
                }
                let (total_items, coverage) = match (kind, policy) {
                    (RequestedTextUnitKind::Paragraph, TextUnitCoveragePolicy::PreserveSource) => (
                        header.paragraph_total_preserve,
                        header.paragraph_preserve_coverage,
                    ),
                    (RequestedTextUnitKind::Paragraph, TextUnitCoveragePolicy::EligibleOnly) => (
                        header.paragraph_total_eligible,
                        header.paragraph_eligible_coverage,
                    ),
                    (RequestedTextUnitKind::Sentence, TextUnitCoveragePolicy::PreserveSource) => (
                        header.sentence_total_preserve,
                        header.sentence_preserve_coverage,
                    ),
                    (RequestedTextUnitKind::Sentence, TextUnitCoveragePolicy::EligibleOnly) => (
                        header.sentence_total_eligible,
                        header.sentence_eligible_coverage,
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
        run_sqlite(
            &self.connection,
            ApplicationError::TextUnitIndexFailed,
            move |connection| {
                let Some(meta) = load_stored_document(connection, &document_id)? else {
                    return Ok(Vec::new());
                };
                let Some(header) = load_section_header(connection, &meta, &section_id)? else {
                    return Ok(Vec::new());
                };
                if header.paragraph_coverage.is_none() {
                    return Ok(Vec::new());
                }
                if let Some(error) = header.plan_error.as_deref() {
                    return Err(ApplicationError::InvalidRequest(error.to_owned()));
                }
                let total = match (kind, policy) {
                    (RequestedTextUnitKind::Paragraph, TextUnitCoveragePolicy::PreserveSource) => {
                        header.paragraph_total_preserve
                    }
                    (RequestedTextUnitKind::Paragraph, TextUnitCoveragePolicy::EligibleOnly) => {
                        header.paragraph_total_eligible
                    }
                    (RequestedTextUnitKind::Sentence, TextUnitCoveragePolicy::PreserveSource) => {
                        header.sentence_total_preserve
                    }
                    (RequestedTextUnitKind::Sentence, TextUnitCoveragePolicy::EligibleOnly) => {
                        header.sentence_total_eligible
                    }
                };
                let (start, end) = match direction {
                    TextUnitDirection::Forward => {
                        (position.min(total), (position + limit).min(total))
                    }
                    TextUnitDirection::Backward => {
                        let end = position.min(total);
                        (end.saturating_sub(limit), end)
                    }
                };
                collect_chunk_stream_items(
                    connection,
                    &meta,
                    &section_id,
                    &header,
                    kind,
                    policy,
                    start,
                    end,
                )
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
                let Some(header) = load_section_header(connection, &meta, &section_id)? else {
                    return Ok(None);
                };
                if header.paragraph_coverage.is_none() {
                    return Ok(None);
                }
                if let Some(error) = header.plan_error.as_deref() {
                    return Err(ApplicationError::InvalidRequest(error.to_owned()));
                }
                let (chunk, target) = match unit_kind {
                    EffectiveTextUnitKind::Paragraph => {
                        if sentence_index.is_some() {
                            return Ok(None);
                        }
                        let Some(chunk) = load_section_chunk_at_paragraph(
                            connection,
                            meta.doc_key,
                            header.section_key,
                            paragraph_index,
                        )?
                        else {
                            return Ok(None);
                        };
                        let Some(local_index) =
                            paragraph_index.checked_sub(chunk.first_paragraph_index)
                        else {
                            return Ok(None);
                        };
                        let Some(slot) = chunk
                            .layout
                            .paragraphs
                            .iter()
                            .position(|paragraph| paragraph.paragraph_index == local_index)
                        else {
                            return Ok(None);
                        };
                        (chunk, UnitRef::Paragraph(slot))
                    }
                    EffectiveTextUnitKind::Sentence => {
                        let Some(sentence_index) = sentence_index else {
                            return Ok(None);
                        };
                        let Some(chunk) = load_section_chunk_at_sentence(
                            connection,
                            meta.doc_key,
                            header.section_key,
                            paragraph_index,
                            sentence_index,
                        )?
                        else {
                            return Ok(None);
                        };
                        let Some(local_index) =
                            paragraph_index.checked_sub(chunk.first_paragraph_index)
                        else {
                            return Ok(None);
                        };
                        let Some(slot) = chunk.layout.sentences.iter().position(|sentence| {
                            sentence.paragraph_index == local_index
                                && sentence.sentence_index == sentence_index
                        }) else {
                            return Ok(None);
                        };
                        (chunk, UnitRef::Sentence(slot))
                    }
                };

                let (paragraphs, sentences, coverage, section_coverage) =
                    build_section_descriptors(&chunk.layout, header.paragraph_coverage.as_ref())?;
                let mut entries = Vec::new();
                walk_section_stream(
                    &section_id,
                    &paragraphs,
                    section_coverage.as_ref(),
                    &sentences,
                    &coverage,
                    kind,
                    &mut |entry| entries.push(entry),
                )?;
                let mut eligible_seen = 0usize;
                for (local_index, entry) in entries.iter().enumerate() {
                    if entry.unit != target {
                        if entry.eligible {
                            eligible_seen += 1;
                        }
                        continue;
                    }
                    return Ok(match policy {
                        TextUnitCoveragePolicy::PreserveSource => Some(match kind {
                            RequestedTextUnitKind::Paragraph => chunk.paragraph_entry + local_index,
                            RequestedTextUnitKind::Sentence => chunk.sentence_entry + local_index,
                        }),
                        TextUnitCoveragePolicy::EligibleOnly => {
                            if !entry.eligible {
                                None
                            } else {
                                Some(match kind {
                                    RequestedTextUnitKind::Paragraph => {
                                        chunk.eligible_paragraph_entry + eligible_seen
                                    }
                                    RequestedTextUnitKind::Sentence => {
                                        chunk.eligible_sentence_entry + eligible_seen
                                    }
                                })
                            }
                        }
                    });
                }
                Ok(None)
            },
        )
        .await
    }

    async fn document_key(
        &self,
        document_id: &DocumentId,
    ) -> Result<Option<i64>, ApplicationError> {
        let document_id = document_id.clone();
        run_sqlite(
            &self.connection,
            ApplicationError::TextUnitIndexFailed,
            move |connection| stored_document_key(connection, &document_id),
        )
        .await
    }

    async fn document_id_for_key(
        &self,
        doc_key: i64,
    ) -> Result<Option<DocumentId>, ApplicationError> {
        run_sqlite(
            &self.connection,
            ApplicationError::TextUnitIndexFailed,
            move |connection| stored_document_id_at_key(connection, doc_key),
        )
        .await
    }

    async fn section_key(
        &self,
        document_id: &DocumentId,
        section_id: &SectionId,
    ) -> Result<Option<i64>, ApplicationError> {
        let document_id = document_id.clone();
        let section_id = section_id.clone();
        run_sqlite(
            &self.connection,
            ApplicationError::TextUnitIndexFailed,
            move |connection| stored_section_key(connection, &document_id, &section_id),
        )
        .await
    }

    async fn section_id_for_key(
        &self,
        document_id: &DocumentId,
        section_key: i64,
    ) -> Result<Option<SectionId>, ApplicationError> {
        let document_id = document_id.clone();
        run_sqlite(
            &self.connection,
            ApplicationError::TextUnitIndexFailed,
            move |connection| stored_section_id_at_key(connection, &document_id, section_key),
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
                    SELECT 1 FROM section_layout_v2
                    WHERE section_layout_v2.doc_key = text_unit_documents_v2.doc_key
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

/// One `section_layout_v2` row: the counters and coverages the stream reads without touching the
/// layout chunks.
struct SectionHeader {
    section_key: i64,
    char_len: usize,
    paragraph_coverage: Option<ParagraphSectionCoverage>,
    plan_error: Option<String>,
    chunk_count: usize,
    paragraph_total_preserve: usize,
    paragraph_total_eligible: usize,
    sentence_total_preserve: usize,
    sentence_total_eligible: usize,
    paragraph_preserve_coverage: TextUnitEnumerationCoverage,
    paragraph_eligible_coverage: TextUnitEnumerationCoverage,
    sentence_preserve_coverage: TextUnitEnumerationCoverage,
    sentence_eligible_coverage: TextUnitEnumerationCoverage,
}

/// One decoded `section_layout_chunk_v2` row. `layout` holds indices relative to
/// `first_paragraph_index`; offsets remain section-global.
#[derive(Clone)]
struct SectionChunk {
    chunk_no: usize,
    first_paragraph_index: usize,
    paragraph_entry: usize,
    sentence_entry: usize,
    eligible_paragraph_entry: usize,
    eligible_sentence_entry: usize,
    paragraph_count: usize,
    layout: SectionLayout,
}

impl SectionChunk {
    /// A mid chunk carries the next sentence run of a paragraph and no paragraph entry. The whole
    /// fallback of an empty section also has `paragraph_count == 0` but is chunk 0, never a mid.
    fn is_mid_paragraph(&self) -> bool {
        self.paragraph_count == 0 && self.chunk_no > 0
    }
}

fn decode_stream_coverage(json: &str) -> Result<TextUnitEnumerationCoverage, ApplicationError> {
    let stored: StoredStreamCoverage = serde_json::from_str(json)
        .map_err(|error| ApplicationError::TextUnitIndexFailed(error.to_string()))?;
    Ok(stored.into_coverage())
}

fn load_section_header(
    connection: &Connection,
    meta: &StoredUnitDocument,
    section_id: &SectionId,
) -> Result<Option<SectionHeader>, ApplicationError> {
    connection
        .query_row(
            "SELECT section_key, char_len, paragraph_coverage_json, plan_error,
                    paragraph_count, sentence_count, chunk_count,
                    total_items_paragraph_preserve, total_items_paragraph_eligible,
                    total_items_sentence_preserve, total_items_sentence_eligible,
                    coverage_paragraph_preserve_json, coverage_paragraph_eligible_json,
                    coverage_sentence_preserve_json, coverage_sentence_eligible_json
               FROM section_layout_v2
              WHERE doc_key = ?1 AND section_id = ?2",
            params![meta.doc_key, &section_id.0],
            |row| {
                let paragraph_coverage_json: Option<String> = row.get(2)?;
                let paragraph_preserve: String = row.get(11)?;
                let paragraph_eligible: String = row.get(12)?;
                let sentence_preserve: String = row.get(13)?;
                let sentence_eligible: String = row.get(14)?;
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    paragraph_coverage_json,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, i64>(6)?,
                    row.get::<_, i64>(7)?,
                    row.get::<_, i64>(8)?,
                    row.get::<_, i64>(9)?,
                    row.get::<_, i64>(10)?,
                    paragraph_preserve,
                    paragraph_eligible,
                    sentence_preserve,
                    sentence_eligible,
                ))
            },
        )
        .optional()
        .map_err(text_unit_index_error)?
        .map(|row| -> Result<SectionHeader, ApplicationError> {
            let _ = (row.4, row.5);
            Ok(SectionHeader {
                section_key: row.0,
                char_len: i64_to_usize(row.1, "char_len")?,
                paragraph_coverage: row
                    .2
                    .map(
                        |json| -> Result<ParagraphSectionCoverage, ApplicationError> {
                            let stored: StoredParagraphCoverage = serde_json::from_str(&json)
                                .map_err(|error| {
                                    ApplicationError::TextUnitIndexFailed(error.to_string())
                                })?;
                            Ok(stored.into_coverage())
                        },
                    )
                    .transpose()?,
                plan_error: row.3,
                chunk_count: i64_to_usize(row.6, "chunk_count")?,
                paragraph_total_preserve: i64_to_usize(row.7, "paragraph total")?,
                paragraph_total_eligible: i64_to_usize(row.8, "paragraph eligible total")?,
                sentence_total_preserve: i64_to_usize(row.9, "sentence total")?,
                sentence_total_eligible: i64_to_usize(row.10, "sentence eligible total")?,
                paragraph_preserve_coverage: decode_stream_coverage(&row.11)?,
                paragraph_eligible_coverage: decode_stream_coverage(&row.12)?,
                sentence_preserve_coverage: decode_stream_coverage(&row.13)?,
                sentence_eligible_coverage: decode_stream_coverage(&row.14)?,
            })
        })
        .transpose()
}

const SECTION_CHUNK_COLUMNS: &str = "chunk_no, first_paragraph_index, first_sentence_index, paragraph_slot, sentence_slot, paragraph_entry, sentence_entry, eligible_paragraph_entry, eligible_sentence_entry, paragraph_count, sentence_count, blob";

fn decode_section_chunk_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<SectionChunk> {
    let _: i64 = row.get(2)?;
    let _: i64 = row.get(3)?;
    let _: i64 = row.get(4)?;
    let _: i64 = row.get(10)?;
    Ok(SectionChunk {
        chunk_no: row.get::<_, i64>(0)?.try_into().unwrap_or(usize::MAX),
        first_paragraph_index: row.get::<_, i64>(1)?.try_into().unwrap_or(usize::MAX),
        paragraph_entry: row.get::<_, i64>(5)?.try_into().unwrap_or(usize::MAX),
        sentence_entry: row.get::<_, i64>(6)?.try_into().unwrap_or(usize::MAX),
        eligible_paragraph_entry: row.get::<_, i64>(7)?.try_into().unwrap_or(usize::MAX),
        eligible_sentence_entry: row.get::<_, i64>(8)?.try_into().unwrap_or(usize::MAX),
        paragraph_count: row.get::<_, i64>(9)?.try_into().unwrap_or(usize::MAX),
        layout: SectionLayout {
            paragraphs: Vec::new(),
            sentences: Vec::new(),
            coverage: Vec::new(),
        },
    })
}

fn decode_section_chunk(
    mut chunk: SectionChunk,
    blob: &[u8],
) -> Result<SectionChunk, ApplicationError> {
    chunk.layout = SectionLayout::decode(blob)?;
    Ok(chunk)
}

fn read_section_chunk(
    connection: &Connection,
    sql: &str,
    params: impl rusqlite::Params,
) -> Result<Option<SectionChunk>, ApplicationError> {
    let mut statement = connection
        .prepare_cached(sql)
        .map_err(text_unit_index_error)?;
    let row = statement
        .query_row(params, |row| {
            let chunk = decode_section_chunk_row(row)?;
            let blob: Vec<u8> = row.get(11)?;
            Ok((chunk, blob))
        })
        .optional()
        .map_err(text_unit_index_error)?;
    row.map(|(chunk, blob)| decode_section_chunk(chunk, &blob))
        .transpose()
}

fn load_section_chunk_at_paragraph(
    connection: &Connection,
    doc_key: i64,
    section_key: i64,
    paragraph_index: usize,
) -> Result<Option<SectionChunk>, ApplicationError> {
    read_section_chunk(
        connection,
        "SELECT chunk_no, first_paragraph_index, first_sentence_index, paragraph_slot,
                sentence_slot, paragraph_entry, sentence_entry, eligible_paragraph_entry,
                eligible_sentence_entry, paragraph_count, sentence_count, blob
           FROM section_layout_chunk_v2
          WHERE doc_key = ?1 AND section_key = ?2
            AND first_paragraph_index <= ?3 AND paragraph_count > 0
          ORDER BY first_paragraph_index DESC, chunk_no DESC
          LIMIT 1",
        params![doc_key, section_key, usize_to_i64(paragraph_index)?],
    )
}

fn load_section_chunk_at_sentence(
    connection: &Connection,
    doc_key: i64,
    section_key: i64,
    paragraph_index: usize,
    sentence_index: usize,
) -> Result<Option<SectionChunk>, ApplicationError> {
    read_section_chunk(
        connection,
        "SELECT chunk_no, first_paragraph_index, first_sentence_index, paragraph_slot,
                sentence_slot, paragraph_entry, sentence_entry, eligible_paragraph_entry,
                eligible_sentence_entry, paragraph_count, sentence_count, blob
           FROM section_layout_chunk_v2
          WHERE doc_key = ?1 AND section_key = ?2
            AND first_paragraph_index <= ?3
            AND (first_paragraph_index < ?3 OR first_sentence_index <= ?4)
          ORDER BY first_paragraph_index DESC, first_sentence_index DESC, chunk_no DESC
          LIMIT 1",
        params![
            doc_key,
            section_key,
            usize_to_i64(paragraph_index)?,
            usize_to_i64(sentence_index)?
        ],
    )
}

fn load_section_chunks_from(
    connection: &Connection,
    doc_key: i64,
    section_key: i64,
    from_chunk: usize,
) -> Result<Vec<SectionChunk>, ApplicationError> {
    let mut statement = connection
        .prepare_cached(
            "SELECT chunk_no, first_paragraph_index, first_sentence_index, paragraph_slot,
                    sentence_slot, paragraph_entry, sentence_entry, eligible_paragraph_entry,
                    eligible_sentence_entry, paragraph_count, sentence_count, blob
               FROM section_layout_chunk_v2
              WHERE doc_key = ?1 AND section_key = ?2 AND chunk_no >= ?3
              ORDER BY chunk_no ASC",
        )
        .map_err(text_unit_index_error)?;
    let rows = statement
        .query_map(
            params![doc_key, section_key, usize_to_i64(from_chunk)?],
            |row| {
                let chunk = decode_section_chunk_row(row)?;
                let blob: Vec<u8> = row.get(11)?;
                Ok((chunk, blob))
            },
        )
        .map_err(text_unit_index_error)?;
    let mut chunks = Vec::new();
    for row in rows {
        let (chunk, blob) = row.map_err(text_unit_index_error)?;
        chunks.push(decode_section_chunk(chunk, &blob)?);
    }
    Ok(chunks)
}

fn load_section_chunk_at_no(
    connection: &Connection,
    doc_key: i64,
    section_key: i64,
    chunk_no: usize,
) -> Result<Option<SectionChunk>, ApplicationError> {
    read_section_chunk(
        connection,
        "SELECT chunk_no, first_paragraph_index, first_sentence_index, paragraph_slot,
                sentence_slot, paragraph_entry, sentence_entry, eligible_paragraph_entry,
                eligible_sentence_entry, paragraph_count, sentence_count, blob
           FROM section_layout_chunk_v2
          WHERE doc_key = ?1 AND section_key = ?2 AND chunk_no = ?3",
        params![doc_key, section_key, usize_to_i64(chunk_no)?],
    )
}

fn reassemble_section_layout(chunks: &[SectionChunk]) -> SectionLayout {
    let mut paragraphs = Vec::new();
    let mut sentences = Vec::new();
    let mut coverage = Vec::new();
    let mut seen_paragraphs = std::collections::HashSet::new();
    let mut seen_coverage = std::collections::HashSet::new();
    for chunk in chunks {
        let base = chunk.first_paragraph_index;
        for paragraph in &chunk.layout.paragraphs {
            let mut paragraph = paragraph.clone();
            paragraph.paragraph_index += base;
            if chunk.is_mid_paragraph() && !seen_paragraphs.insert(paragraph.paragraph_index) {
                continue;
            }
            if !chunk.is_mid_paragraph() {
                seen_paragraphs.insert(paragraph.paragraph_index);
            }
            paragraphs.push(paragraph);
        }
        for sentence in &chunk.layout.sentences {
            let mut sentence = sentence.clone();
            sentence.paragraph_index += base;
            sentences.push(sentence);
        }
        for record in &chunk.layout.coverage {
            let mut record = record.clone();
            record.paragraph_index += base;
            if chunk.is_mid_paragraph() && !seen_coverage.insert(record.paragraph_index) {
                continue;
            }
            if !chunk.is_mid_paragraph() {
                seen_coverage.insert(record.paragraph_index);
            }
            coverage.push(record);
        }
    }
    SectionLayout {
        paragraphs,
        sentences,
        coverage,
    }
}

/// Reads exactly the entries of `[start, end)` from the chunks those entries live in. Each chunk is
/// decoded once and walked with the same planner rules as ingest, so page order and positions match
/// the whole-section plan.
#[allow(clippy::too_many_arguments)]
fn collect_chunk_stream_items(
    connection: &Connection,
    meta: &StoredUnitDocument,
    section_id: &SectionId,
    header: &SectionHeader,
    kind: RequestedTextUnitKind,
    policy: TextUnitCoveragePolicy,
    start: usize,
    end: usize,
) -> Result<Vec<StoredStreamItem>, ApplicationError> {
    if start >= end {
        return Ok(Vec::new());
    }
    let Some(mut chunk) = load_section_chunk_for_stream_position(
        connection,
        meta.doc_key,
        header.section_key,
        kind,
        policy,
        start,
    )?
    else {
        return Ok(Vec::new());
    };

    struct PageUnit {
        position: usize,
        chunk_index: usize,
        unit: UnitRef,
        unit_start: usize,
        unit_end: usize,
        content_class: ParagraphContentClass,
        degradation: Option<&'static str>,
        char_count: usize,
    }

    // A page loads its text once, from the first selected unit's start to the last one's end, as
    // the planner-backed index does; each unit keeps its own range into that slice.
    let mut chunks: Vec<SectionChunk> = Vec::new();
    let mut pending: Vec<PageUnit> = Vec::with_capacity(end - start);
    let mut chunk_no = chunk.chunk_no;
    'chunks: loop {
        if kind == RequestedTextUnitKind::Paragraph && chunk.is_mid_paragraph() {
            match load_section_chunk_at_no(
                connection,
                meta.doc_key,
                header.section_key,
                chunk_no + 1,
            )? {
                Some(next) => {
                    chunk = next;
                    chunk_no += 1;
                    continue;
                }
                None => break,
            }
        }
        let chunk_index = chunks.len();
        chunks.push(chunk.clone());
        let (paragraphs, sentences, coverage, section_coverage) =
            build_section_descriptors(&chunk.layout, header.paragraph_coverage.as_ref())?;
        let mut entries = Vec::new();
        walk_section_stream(
            section_id,
            &paragraphs,
            section_coverage.as_ref(),
            &sentences,
            &coverage,
            kind,
            &mut |entry| entries.push(entry),
        )?;

        let mut eligible_local = 0usize;
        for (local_index, entry) in entries.iter().enumerate() {
            if matches!(policy, TextUnitCoveragePolicy::EligibleOnly) && !entry.eligible {
                continue;
            }
            let stream_position = match policy {
                TextUnitCoveragePolicy::PreserveSource => match kind {
                    RequestedTextUnitKind::Paragraph => chunk.paragraph_entry + local_index,
                    RequestedTextUnitKind::Sentence => chunk.sentence_entry + local_index,
                },
                TextUnitCoveragePolicy::EligibleOnly => {
                    let position = match kind {
                        RequestedTextUnitKind::Paragraph => {
                            chunk.eligible_paragraph_entry + eligible_local
                        }
                        RequestedTextUnitKind::Sentence => {
                            chunk.eligible_sentence_entry + eligible_local
                        }
                    };
                    if entry.eligible {
                        eligible_local += 1;
                    }
                    position
                }
            };
            if stream_position >= end {
                break 'chunks;
            }
            if stream_position < start {
                continue;
            }
            let (unit_start, unit_end) = unit_char_range(&chunk.layout, entry.unit)?;
            pending.push(PageUnit {
                position: stream_position,
                chunk_index,
                unit: entry.unit,
                unit_start,
                unit_end,
                content_class: entry.content_class,
                degradation: entry.degradation,
                char_count: entry.char_count,
            });
        }

        chunk_no += 1;
        if chunk_no >= header.chunk_count {
            break;
        }
        let Some(next) =
            load_section_chunk_at_no(connection, meta.doc_key, header.section_key, chunk_no)?
        else {
            break;
        };
        chunk = next;
    }

    let Some((range_start, range_end)) =
        pending.iter().fold(None::<(usize, usize)>, |range, unit| {
            Some(match range {
                None => (unit.unit_start, unit.unit_end),
                Some((start, end)) => (start.min(unit.unit_start), end.max(unit.unit_end)),
            })
        })
    else {
        return Ok(Vec::new());
    };
    let text = load_text_slice(
        connection,
        meta.doc_key,
        header.section_key,
        range_start,
        range_end,
    )?;
    let index = NormalizedTextIndex::new(&text);
    let mut items = Vec::with_capacity(pending.len());
    for unit in pending {
        let chunk = &chunks[unit.chunk_index];
        items.push(StoredStreamItem {
            position: unit.position,
            unit: build_owned_unit(
                meta,
                section_id,
                &chunk.layout,
                chunk.first_paragraph_index,
                unit.unit,
                &index,
                range_start,
            )?,
            content_class: unit.content_class,
            degradation: unit.degradation.map(str::to_owned),
            char_count: unit.char_count,
        });
    }
    Ok(items)
}

fn load_whole_section_layout(
    connection: &Connection,
    doc_key: i64,
    section_key: i64,
) -> Result<SectionLayout, ApplicationError> {
    let chunks = load_section_chunks_from(connection, doc_key, section_key, 0)?;
    Ok(reassemble_section_layout(&chunks))
}

fn stream_choice_field(
    kind: RequestedTextUnitKind,
    policy: TextUnitCoveragePolicy,
) -> &'static str {
    match (kind, policy) {
        (RequestedTextUnitKind::Paragraph, TextUnitCoveragePolicy::PreserveSource) => {
            "paragraph_entry"
        }
        (RequestedTextUnitKind::Paragraph, TextUnitCoveragePolicy::EligibleOnly) => {
            "eligible_paragraph_entry"
        }
        (RequestedTextUnitKind::Sentence, TextUnitCoveragePolicy::PreserveSource) => {
            "sentence_entry"
        }
        (RequestedTextUnitKind::Sentence, TextUnitCoveragePolicy::EligibleOnly) => {
            "eligible_sentence_entry"
        }
    }
}

fn stream_count_field(kind: RequestedTextUnitKind) -> &'static str {
    match kind {
        RequestedTextUnitKind::Paragraph => "paragraph_count > 0",
        // A coarse paragraph is a sentence-stream entry without a sentence record, so a chunk can
        // have stream entries with `sentence_count == 0` but `paragraph_count > 0`.
        RequestedTextUnitKind::Sentence => "(sentence_count > 0 OR paragraph_count > 0)",
    }
}

fn load_section_chunk_for_stream_position(
    connection: &Connection,
    doc_key: i64,
    section_key: i64,
    kind: RequestedTextUnitKind,
    policy: TextUnitCoveragePolicy,
    position: usize,
) -> Result<Option<SectionChunk>, ApplicationError> {
    let choice = stream_choice_field(kind, policy);
    let count = stream_count_field(kind);
    let sql = format!(
        "SELECT {SECTION_CHUNK_COLUMNS}
           FROM section_layout_chunk_v2
          WHERE doc_key = ?1 AND section_key = ?2
            AND {choice} <= ?3 AND {count}
          ORDER BY {choice} DESC, chunk_no DESC
          LIMIT 1"
    );
    read_section_chunk(
        connection,
        &sql,
        params![doc_key, section_key, usize_to_i64(position)?],
    )
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
    let text = load_section_text_unchecked(connection, doc_key, section_key)?;
    let text_chars = text.chars().count();
    if text_chars != char_len {
        return Err(ApplicationError::TextUnitIndexFailed(format!(
            "section {section_key} text has {text_chars} chars, expected {char_len}"
        )));
    }
    Ok(text)
}

/// The stored text of one section, without counting its chars. The scan counts them once through the
/// `NormalizedTextIndex` it builds anyway, and reports the same error `load_section_text` would.
fn load_section_text_unchecked(
    connection: &Connection,
    doc_key: i64,
    section_key: i64,
) -> Result<String, ApplicationError> {
    let mut statement = connection
        .prepare_cached(
            "SELECT chunk_no, encoding, data
             FROM section_text_v3
             WHERE doc_key = ?1 AND section_key = ?2
             ORDER BY chunk_no ASC",
        )
        .map_err(text_unit_index_error)?;
    let rows = statement
        .query_map(params![doc_key, section_key], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, Vec<u8>>(2)?,
            ))
        })
        .map_err(text_unit_index_error)?;
    let mut text = String::new();
    for (expected_chunk, row) in rows.enumerate() {
        let (chunk_no, encoding, data) = row.map_err(text_unit_index_error)?;
        let chunk_no = i64_to_usize(chunk_no, "chunk_no")?;
        if chunk_no != expected_chunk {
            return Err(ApplicationError::TextUnitIndexFailed(format!(
                "section {section_key} is missing text chunk {expected_chunk}"
            )));
        }
        let chunk = decode_text_chunk(encoding, &data).map_err(|error| {
            ApplicationError::TextUnitIndexFailed(format!(
                "section {section_key} text chunk {chunk_no}: {error}"
            ))
        })?;
        text.push_str(&chunk);
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
            "SELECT chunk_no, encoding, data
             FROM section_text_v3
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
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, Vec<u8>>(2)?,
                ))
            },
        )
        .map_err(text_unit_index_error)?;

    let mut text = String::new();
    for (offset, row) in rows.enumerate() {
        let (chunk_no, encoding, data) = row.map_err(text_unit_index_error)?;
        let chunk_no = i64_to_usize(chunk_no, "chunk_no")?;
        let expected_chunk = first_chunk + offset;
        if chunk_no != expected_chunk {
            return Err(ApplicationError::TextUnitIndexFailed(format!(
                "section {section_key} is missing text chunk {expected_chunk}"
            )));
        }
        let chunk = decode_text_chunk(encoding, &data).map_err(|error| {
            ApplicationError::TextUnitIndexFailed(format!(
                "section {section_key} text chunk {chunk_no}: {error}"
            ))
        })?;
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
    slice_section_text_str(index, start, end, what).map(str::to_owned)
}

fn slice_section_text_str<'a>(
    index: &NormalizedTextIndex<'a>,
    start: usize,
    end: usize,
    what: &str,
) -> Result<&'a str, ApplicationError> {
    let range = stored_normalized_range(start, end)?;
    index.slice(range).map_err(|error| {
        ApplicationError::TextUnitIndexFailed(format!(
            "persisted {what} range is not a valid section slice: {error}"
        ))
    })
}

fn paragraph_units_from_layout(
    connection: &Connection,
    meta: &StoredUnitDocument,
) -> Result<Vec<TextUnit>, ApplicationError> {
    let mut statement = connection
        .prepare(
            "SELECT section_key, section_id
             FROM section_layout_v2
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
        let Some(header) = load_section_header(connection, meta, &section_id)? else {
            continue;
        };
        let layout = load_whole_section_layout(connection, meta.doc_key, header.section_key)?;
        let text = load_section_text(
            connection,
            meta.doc_key,
            header.section_key,
            header.char_len,
        )?;
        let index = NormalizedTextIndex::new(&text);
        for paragraph in &layout.paragraphs {
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

/// Writes one section's text as `section_text_v3` chunks of at most [`CHUNK_CHARS`] chars each,
/// compressing one chunk at a time so a document's text is never copied in full.
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
            insert_text_chunk(
                statement,
                doc_key,
                section_key,
                chunk_no,
                &text[start..index],
            )?;
            chunk_no += 1;
            start = index;
            chars = 0;
        }
        chars += 1;
    }
    if start < text.len() {
        insert_text_chunk(statement, doc_key, section_key, chunk_no, &text[start..])?;
    }
    Ok(())
}

fn insert_text_chunk(
    statement: &mut rusqlite::Statement<'_>,
    doc_key: i64,
    section_key: i64,
    chunk_no: i64,
    text: &str,
) -> Result<(), ApplicationError> {
    let (encoding, data) = encode_text_chunk(text)?;
    statement
        .execute(params![doc_key, section_key, chunk_no, encoding, data])
        .map_err(text_unit_index_error)?;
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

/// The same layout `build_section_layout` derives from units, built straight from the boundaries.
fn build_segmentation_section_layout(
    section: &SectionSegmentation,
) -> Result<SectionLayout, ApplicationError> {
    let paragraphs = section
        .paragraphs
        .iter()
        .map(|boundary| LayoutParagraph {
            paragraph_index: boundary.paragraph_index(),
            source_order: boundary.source_order(),
            start: boundary.range().start(),
            end: boundary.range().end(),
        })
        .collect();

    // Both lists are fallible and their length is known: push into an exactly sized Vec instead of
    // collecting into a Result, whose size hint is a lower bound and grows by doubling.
    let mut sentences = Vec::with_capacity(section.sentences.len());
    for boundary in &section.sentences {
        // The parent range is not stored on the boundary: it is always the range of the
        // paragraph this sentence belongs to.
        let parent = section
            .paragraph_range(boundary.paragraph_index())
            .ok_or_else(|| {
                ApplicationError::TextUnitIndexFailed(format!(
                    "Sentence {}#p{}.s{} has no parent Paragraph boundary",
                    section.section_id.0,
                    boundary.paragraph_index(),
                    boundary.sentence_index()
                ))
            })?;
        sentences.push(LayoutSentence {
            paragraph_index: boundary.paragraph_index(),
            sentence_index: boundary.sentence_index(),
            source_order: boundary.source_order(),
            start: boundary.range().start(),
            end: boundary.range().end(),
            parent_start: parent.start(),
            parent_end: parent.end(),
        });
    }

    let mut coverage = Vec::with_capacity(section.sentence_coverage.len());
    for boundary in &section.sentence_coverage {
        coverage.push(LayoutCoverage {
            paragraph_index: boundary.paragraph_index(),
            paragraph_start: boundary.paragraph_range().start(),
            paragraph_end: boundary.paragraph_range().end(),
            content_class: u8::try_from(content_class_code(boundary.content_class())).map_err(
                |_| {
                    ApplicationError::TextUnitIndexFailed(
                        "paragraph content class code does not fit u8".into(),
                    )
                },
            )?,
            eligibility: u8::try_from(sentence_eligibility_code(boundary.eligibility())).map_err(
                |_| {
                    ApplicationError::TextUnitIndexFailed(
                        "sentence eligibility code does not fit u8".into(),
                    )
                },
            )?,
            paragraph_chars: boundary.paragraph_chars(),
            sentence_chars: boundary.sentence_chars(),
            separator_chars: boundary.separator_chars(),
            coarse_only_chars: boundary.coarse_only_chars(),
            sentence_count: boundary.sentence_count(),
        });
    }

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
    section_id: &SectionId,
    layout: &SectionLayout,
    first_paragraph_index: usize,
    unit: UnitRef,
    index: &NormalizedTextIndex<'_>,
    base: usize,
) -> Result<OwnedStreamUnit, ApplicationError> {
    match unit {
        UnitRef::Paragraph(unit_index) => {
            let paragraph = layout.paragraphs.get(unit_index).ok_or_else(|| {
                ApplicationError::TextUnitIndexFailed(format!(
                    "stream references missing paragraph descriptor {unit_index}"
                ))
            })?;
            let text = slice_loaded_text(index, base, paragraph.start, paragraph.end, "paragraph")?;
            Ok(OwnedStreamUnit::Paragraph(rebuild_paragraph_unit(
                meta,
                section_id,
                usize_to_i64(paragraph.paragraph_index + first_paragraph_index)?,
                usize_to_i64(paragraph.source_order)?,
                usize_to_i64(paragraph.start)?,
                usize_to_i64(paragraph.end)?,
                text,
            )?))
        }
        UnitRef::Sentence(unit_index) => {
            let sentence = layout.sentences.get(unit_index).ok_or_else(|| {
                ApplicationError::TextUnitIndexFailed(format!(
                    "stream references missing sentence descriptor {unit_index}"
                ))
            })?;
            let text = slice_loaded_text(index, base, sentence.start, sentence.end, "sentence")?;
            Ok(OwnedStreamUnit::Sentence(rebuild_sentence_unit(
                meta,
                section_id,
                usize_to_i64(sentence.paragraph_index + first_paragraph_index)?,
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
    /// Where the section text lives. Absent when every section carries its text inline, which is
    /// what an old row and a repository used on its own write.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    content_store: Option<String>,
    /// Where the heavy metadata entries live. Absent when every entry is inline, which is what an
    /// old row and a document whose metadata is all light write.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    metadata_store: Option<String>,
    /// Where the section tree lives when `root_sections` is empty. Absent when the tree is inline,
    /// which is what an old row and a repository used on its own write.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    section_tree_store: Option<String>,
    id: String,
    source: String,
    title: String,
    media_type: String,
    content_hash: String,
    metadata: BTreeMap<String, String>,
    root_sections: Vec<StoredSection>,
}

impl StoredDocument {
    /// `content_store` names where the section text is kept. With `Some`, every section's `content`
    /// is written empty and the text is read back from that store on load; with `None` the text is
    /// inline, byte for byte as before. When the text is stored, the tree lives in the section
    /// rows too, so `root_sections` is written empty and `section_tree_store` names the rows.
    /// `metadata` is the light half of the document's metadata; `metadata_store` names where the
    /// heavy half is kept, and is `None` when there is no heavy entry.
    fn with_stores(
        document: &Document,
        content_store: Option<&str>,
        metadata: BTreeMap<String, String>,
        metadata_store: Option<&str>,
    ) -> Self {
        let tree_in_rows = content_store.is_some();
        Self {
            normalization_version: Some(NORMALIZATION_VERSION.into()),
            content_store: content_store.map(str::to_owned),
            metadata_store: metadata_store.map(str::to_owned),
            section_tree_store: tree_in_rows.then(|| SECTION_TREE_STORE.to_owned()),
            id: document.id.0.clone(),
            source: document.source.0.clone(),
            title: document.title.clone(),
            media_type: document.media_type.0.clone(),
            content_hash: document.content_hash.0.clone(),
            metadata,
            root_sections: if tree_in_rows {
                Vec::new()
            } else {
                document
                    .root_sections
                    .iter()
                    .map(|section| StoredSection::from_section(section, false))
                    .collect()
            },
        }
    }

    /// Decodes the document. `connection` and `stored_hash` are read only when the row keeps its
    /// section text in `section_text_v3`; `stored_hash` is the hash of the `documents` row, if it
    /// has one.
    fn into_document(
        self,
        connection: &Connection,
        stored_hash: Option<&str>,
    ) -> Result<Document, ApplicationError> {
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
        let content_store = self.content_store;
        let metadata_store = self.metadata_store;
        let tree_store = self.section_tree_store;
        let mut metadata = self.metadata;
        let mut document = Document {
            id: DocumentId(self.id),
            source: DocumentSource(self.source),
            title: self.title,
            media_type: MediaType(self.media_type),
            content_hash: ContentHash(self.content_hash),
            metadata: BTreeMap::new(),
            root_sections: self
                .root_sections
                .into_iter()
                .map(StoredSection::into_section)
                .collect(),
        };
        // The tree lives in the section rows when the row says so; `root_sections` is empty then.
        // A marker without a complete set of rows is a stale row, never an empty book.
        match tree_store.as_deref() {
            None => {}
            Some(store) if store == SECTION_TREE_STORE => {
                document.root_sections = load_section_tree(connection, &document.id)?;
            }
            Some(store) => return Err(unknown_section_tree_store(store)),
        }
        // The decoded document carries every metadata entry, inline and heavy, exactly as a row that
        // kept all of them inline did before the split.
        match metadata_store.as_deref() {
            None => {}
            Some(store) if store == DOCUMENT_META_STORE || store == DOCUMENT_META_STORE_V1 => {
                load_document_metadata(connection, &document.id, store, &mut metadata)?;
            }
            Some(store) => return Err(unknown_metadata_store(store)),
        }
        document.metadata = metadata;
        match content_store.as_deref() {
            None => {}
            Some(store) if store == SECTION_TEXT_CONTENT_STORE => {
                load_document_section_text(connection, &mut document, stored_hash)?;
            }
            Some(store) => {
                return Err(ApplicationError::StaleDocument(format!(
                    "persisted document keeps its section text in unknown store {store}; explicit source reopen required"
                )));
            }
        }
        Ok(document)
    }

    /// The light decode: the same checks as [`Self::into_document`], but no section text and no
    /// metadata-table read. The tree is rebuilt from `document_sections_v1` when the row keeps it
    /// there, so the light form carries the whole tree and no text. The caller has already
    /// established that this row has a stored normalized hash and no OCR metadata.
    fn into_document_light(self, connection: &Connection) -> Result<Document, ApplicationError> {
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
        // A row with a `metadata_store` holds only its light entries inline. A row without one
        // holds every entry inline, which is what every row written before the split and every
        // document whose entries are all light write: its light form is that map split the way a
        // save splits it, so the light read stays one row read for those documents too.
        let metadata = match self.metadata_store.as_deref() {
            Some(store) if store == DOCUMENT_META_STORE || store == DOCUMENT_META_STORE_V1 => {
                self.metadata
            }
            None => split_metadata(&self.metadata).0,
            Some(store) => return Err(unknown_metadata_store(store)),
        };
        match self.content_store.as_deref() {
            None => {}
            Some(store) if store == SECTION_TEXT_CONTENT_STORE => {}
            Some(store) => {
                return Err(ApplicationError::StaleDocument(format!(
                    "persisted document keeps its section text in unknown store {store}; explicit source reopen required"
                )));
            }
        }
        let keep_inline_text = self.content_store.is_none();
        let mut root_sections: Vec<Section> = match self.section_tree_store.as_deref() {
            None => self
                .root_sections
                .into_iter()
                .map(StoredSection::into_section)
                .collect(),
            Some(store) if store == SECTION_TREE_STORE => {
                load_section_tree(connection, &DocumentId(self.id.clone()))?
            }
            Some(store) => return Err(unknown_section_tree_store(store)),
        };
        if keep_inline_text {
            // A row that keeps its text inline would otherwise hand the light read the whole
            // section text: drop it here, so a light document never carries text.
            clear_section_text(&mut root_sections);
        }
        Ok(Document {
            id: DocumentId(self.id),
            source: DocumentSource(self.source),
            title: self.title,
            media_type: MediaType(self.media_type),
            content_hash: ContentHash(self.content_hash),
            metadata,
            root_sections,
        })
    }
}

/// Fills `metadata` with every heavy entry of `document_id` from `store`, so the decoded document
/// carries the metadata a row that kept all of it inline carried. A `document_meta_v2` entry is
/// decompressed on demand, so the decoded strings are byte-identical to the ones that were saved.
fn load_document_metadata(
    connection: &Connection,
    document_id: &DocumentId,
    store: &str,
    metadata: &mut BTreeMap<String, String>,
) -> Result<(), ApplicationError> {
    if store == DOCUMENT_META_STORE_V1 {
        let mut statement = connection
            .prepare_cached("SELECT key, value FROM document_meta_v1 WHERE document_id = ?1")
            .map_err(repository_error)?;
        let rows = statement
            .query_map(params![&document_id.0], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(repository_error)?;
        for row in rows {
            let (key, value) = row.map_err(repository_error)?;
            metadata.insert(key, value);
        }
        return Ok(());
    }
    let mut statement = connection
        .prepare_cached("SELECT key, encoding, value FROM document_meta_v2 WHERE document_id = ?1")
        .map_err(repository_error)?;
    let rows = statement
        .query_map(params![&document_id.0], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Vec<u8>>(2)?,
            ))
        })
        .map_err(repository_error)?;
    for row in rows {
        let (key, encoding, blob) = row.map_err(repository_error)?;
        let value = decode_metadata_value(document_id, &key, &encoding, &blob)?;
        metadata.insert(key, value);
    }
    Ok(())
}

/// Empties every section's `content`, in place.
fn clear_section_text(sections: &mut [Section]) {
    for section in sections.iter_mut() {
        section.content.clear();
        clear_section_text(&mut section.children);
    }
}

fn stale_section_text(document_id: &DocumentId, reason: &str) -> ApplicationError {
    ApplicationError::StaleDocument(format!(
        "stored section text of document {} is stale: {reason}; explicit source reopen required",
        document_id.0
    ))
}

/// Fills every section's `content` from `section_text_v3`, in depth-first section order, which is
/// the order `write_document_sections` assigns `section_key` in.
fn load_document_section_text(
    connection: &Connection,
    document: &mut Document,
    stored_hash: Option<&str>,
) -> Result<(), ApplicationError> {
    let document_id = document.id.clone();
    if !has_table(connection, "text_unit_documents_v2")
        .map_err(|error| stale_section_text(&document_id, &error.to_string()))?
    {
        return Err(stale_section_text(
            &document_id,
            "its section text is not stored",
        ));
    }
    let stored = connection
        .query_row(
            "SELECT doc_key, normalized_document_hash
               FROM text_unit_documents_v2
              WHERE document_id = ?1",
            params![&document_id.0],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()
        .map_err(repository_error)?;
    let Some((doc_key, text_hash)) = stored else {
        return Err(stale_section_text(
            &document_id,
            "its section text is not stored",
        ));
    };
    if stored_hash.is_some_and(|stored_hash| stored_hash != text_hash) {
        return Err(stale_section_text(
            &document_id,
            "its section text belongs to another version of the document",
        ));
    }
    let char_lens = section_char_lens(connection, doc_key)
        .map_err(|error| stale_section_text(&document_id, &error.to_string()))?;
    let section_count = document.section_count();
    if char_lens.len() != section_count {
        return Err(stale_section_text(
            &document_id,
            &format!(
                "its section layout has {} rows for {section_count} sections",
                char_lens.len()
            ),
        ));
    }

    let mut texts = Vec::with_capacity(char_lens.len());
    for (index, char_len) in char_lens.into_iter().enumerate() {
        let section_key = i64::try_from(index).map_err(|_| {
            stale_section_text(&document_id, "its section count exceeds the stored range")
        })?;
        let char_len = i64_to_usize(char_len, "char_len")
            .map_err(|error| stale_section_text(&document_id, &error.to_string()))?;
        let text = load_section_text_unchecked(connection, doc_key, section_key)
            .map_err(|error| stale_section_text(&document_id, &error.to_string()))?;
        let text_chars = text.chars().count();
        if text_chars != char_len {
            return Err(stale_section_text(
                &document_id,
                &format!("section {section_key} text has {text_chars} chars, expected {char_len}"),
            ));
        }
        texts.push(text);
    }
    let mut texts = texts.into_iter();
    fill_section_text(&mut document.root_sections, &mut texts);
    if texts.next().is_some() {
        return Err(stale_section_text(
            &document_id,
            "its section text has more sections than the document",
        ));
    }
    Ok(())
}

/// Assigns the stored texts to the sections in depth-first order, one text per section.
fn fill_section_text(sections: &mut [Section], texts: &mut std::vec::IntoIter<String>) {
    for section in sections.iter_mut() {
        section.content = texts.next().unwrap_or_default();
        fill_section_text(&mut section.children, texts);
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
    /// `omit_content` writes an empty `content` instead of a copy of the section's text, for rows
    /// whose text is stored in `section_text_v3`.
    fn from_section(section: &Section, omit_content: bool) -> Self {
        Self {
            id: section.id.0.clone(),
            parent_id: section.parent_id.as_ref().map(|value| value.0.clone()),
            title: section.title.clone(),
            level: section.level,
            content: if omit_content {
                String::new()
            } else {
                section.content.clone()
            },
            location: StoredLocation::from_location(&section.location),
            children: section
                .children
                .iter()
                .map(|child| StoredSection::from_section(child, omit_content))
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

// ---------------------------------------------------------------------------------------------
// Short-handle key lookups (v2).
//
// The v2 tools address a document by `text_unit_documents_v2.doc_key` and a section by
// `section_layout_v2.section_key` instead of by the long canonical ids. These four lookups are the
// only bridge between the two namings. They are appended here, after the rest of the store, so they
// never conflict with in-flight work on the scan and layout code above.
// ---------------------------------------------------------------------------------------------

/// `text_unit_documents_v2.doc_key` of `document_id`.
fn stored_document_key(
    connection: &Connection,
    document_id: &DocumentId,
) -> Result<Option<i64>, ApplicationError> {
    connection
        .query_row(
            "SELECT doc_key FROM text_unit_documents_v2 WHERE document_id = ?1",
            params![&document_id.0],
            |row| row.get::<_, i64>(0),
        )
        .optional()
        .map_err(text_unit_index_error)
}

/// The document stored under `doc_key`.
fn stored_document_id_at_key(
    connection: &Connection,
    doc_key: i64,
) -> Result<Option<DocumentId>, ApplicationError> {
    connection
        .query_row(
            "SELECT document_id FROM text_unit_documents_v2 WHERE doc_key = ?1",
            params![doc_key],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map(|row| row.map(DocumentId))
        .map_err(text_unit_index_error)
}

/// `section_layout_v2.section_key` of one section of `document_id`.
fn stored_section_key(
    connection: &Connection,
    document_id: &DocumentId,
    section_id: &SectionId,
) -> Result<Option<i64>, ApplicationError> {
    connection
        .query_row(
            "SELECT l.section_key
               FROM section_layout_v2 l
               JOIN text_unit_documents_v2 d ON d.doc_key = l.doc_key
              WHERE d.document_id = ?1 AND l.section_id = ?2",
            params![&document_id.0, &section_id.0],
            |row| row.get::<_, i64>(0),
        )
        .optional()
        .map_err(text_unit_index_error)
}

/// The section stored at `section_key` of `document_id`.
fn stored_section_id_at_key(
    connection: &Connection,
    document_id: &DocumentId,
    section_key: i64,
) -> Result<Option<SectionId>, ApplicationError> {
    connection
        .query_row(
            "SELECT l.section_id
               FROM section_layout_v2 l
               JOIN text_unit_documents_v2 d ON d.doc_key = l.doc_key
              WHERE d.document_id = ?1 AND l.section_key = ?2",
            params![&document_id.0, section_key],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map(|row| row.map(SectionId))
        .map_err(text_unit_index_error)
}

/// Whether the process emits telemetry events, from `READING_MCP_TELEMETRY`. Only the decode-phase
/// event of a full document load looks at this itself; every other event is emitted by the observed
/// wrappers, which the runtime installs only when telemetry is on.
fn telemetry_enabled() -> bool {
    match std::env::var("READING_MCP_TELEMETRY") {
        Ok(value) => !matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "0" | "false" | "no" | "off"
        ),
        Err(_) => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The fast unit scan — cache, threads and pruning — must return exactly the units of the plain
    /// one, as whole vectors.
    #[tokio::test]
    async fn the_fast_unit_scan_matches_the_plain_one() {
        use crate::application::ports::{Parser, RetrievedResource};

        let directory = tempfile::tempdir().expect("temp directory");
        let path = directory.path().join("scan.sqlite");
        let repository = SqliteDocumentRepository::open(&path).expect("repository");
        let text = "皇帝在朝堂上讨论虚拟内存机制与页表映射。\n\n\
                    群臣议事，節度使入朝。Memory layout of the tables.\n\n\
                    ΟΔΟΣ.ΟΔΟΣ closes the paragraph.\n\n\
                    皇帝又论页表与地址空间。\n";
        let document = crate::parsing::TextParser
            .parse(RetrievedResource {
                source: DocumentSource("memory:unit-scan.txt".into()),
                final_source: DocumentSource("memory:unit-scan.txt".into()),
                media_type: MediaType("text/plain".into()),
                bytes: text.as_bytes().to_vec(),
                etag: None,
                last_modified: None,
                metadata: Default::default(),
            })
            .await
            .expect("parse text");
        repository.save(document.clone()).await.expect("save");
        let units = SqliteTextUnitIndex::open(&path).expect("units");
        let document = Arc::new(document);
        units
            .replace_document_segmentation(&document, Arc::new(document.segmentation()))
            .await
            .expect("store segmentation");
        let hash = document.normalized_document_hash();

        for query in ["皇帝", "memory", "οδος", "皇帝 内存", "的"] {
            let lexical = Arc::new(LexicalQuery::new(query).expect("query tokens"));
            for mode in [MatchMode::Strict, MatchMode::Relaxed] {
                for limit in [1, 3, 10] {
                    let plain = units
                        .scan_lexical(
                            &document.id,
                            &hash,
                            Arc::clone(&lexical),
                            mode,
                            limit,
                            ScanOptions::plain(),
                        )
                        .await
                        .expect("plain scan")
                        .expect("stored units");
                    let fast = units
                        .scan_lexical(
                            &document.id,
                            &hash,
                            Arc::clone(&lexical),
                            mode,
                            limit,
                            ScanOptions {
                                cache: true,
                                threads: 4,
                                prune: true,
                            },
                        )
                        .await
                        .expect("fast scan")
                        .expect("stored units");
                    assert_eq!(
                        fast.0, plain.0,
                        "section ids for {query:?} {mode:?} limit {limit}"
                    );
                    assert_eq!(
                        fast.1, plain.1,
                        "units for {query:?} {mode:?} limit {limit}"
                    );
                }
            }
        }
    }
}
