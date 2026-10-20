use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;

use super::lexical::{
    LEXICAL_SEARCH_INDEX_VERSION, LexicalQuery, MAX_SNIPPET_CHARS, compare_hits, lowercase_fast,
    match_snippet, score_lowered, text_unit_location,
};
use super::sqlite::{MatchMode, ScannedUnitHit, SqliteDocumentRepository, SqliteTextUnitIndex};
use crate::application::get_text_units::EffectiveTextUnitKind;
use crate::application::ports::{
    ApplicationError, DocumentRepository, LEXICAL_TOKENIZER_VERSION, LexicalSearchHit,
    OwnedStreamUnit, SearchHit, SearchHitKind, SearchIndex, TextUnitIndex,
};
use crate::domain::{
    Document, DocumentId, DocumentSegmentation, NormalizedDocumentHash, Section, TextLocator,
};

/// Lexical search over the stored section text: no full-text index is built, the candidates are found by
/// scanning `section_text_v1` and the unit boundaries of `section_layout_v1`.
pub struct SqliteSearchIndex {
    repository: Arc<dyn DocumentRepository>,
    units: Arc<SqliteTextUnitIndex>,
}

impl SqliteSearchIndex {
    pub fn new(repository: Arc<dyn DocumentRepository>, units: Arc<SqliteTextUnitIndex>) -> Self {
        Self { repository, units }
    }

    /// Opens its own repository and text-unit store on `path` (tests, tools).
    pub fn open(path: impl AsRef<Path>) -> Result<Self, ApplicationError> {
        let path = path.as_ref();
        let repository = Arc::new(SqliteDocumentRepository::open(path)?);
        let units = Arc::new(SqliteTextUnitIndex::open(path)?);
        Ok(Self::new(repository, units))
    }

    /// Stores the segmentation of `document` unless its units are already stored for the same
    /// normalized hash.
    async fn ensure_segmentation(
        &self,
        document: Arc<Document>,
        normalized_document_hash: NormalizedDocumentHash,
        segmentation: Arc<DocumentSegmentation>,
    ) -> Result<(), ApplicationError> {
        if self.units.stored_unit_hash(&document.id).await?.as_ref()
            == Some(&normalized_document_hash)
        {
            return Ok(());
        }
        self.units
            .replace_document_segmentation(&document, segmentation)
            .await
    }

    /// One strict or relaxed pass: the section title hits and the paragraph and sentence hits.
    async fn scan_pass(
        &self,
        document: &Document,
        normalized_document_hash: &NormalizedDocumentHash,
        raw_query: &str,
        query: &Arc<LexicalQuery>,
        mode: MatchMode,
        limit: usize,
    ) -> Result<Vec<LexicalSearchHit>, ApplicationError> {
        let mut hits = Vec::new();
        for section in &document.root_sections {
            collect_section_hits(
                document,
                section,
                normalized_document_hash,
                raw_query,
                query,
                mode,
                &mut hits,
            );
        }

        let Some((section_ids, scanned)) = self
            .units
            .scan_lexical(
                &document.id,
                normalized_document_hash,
                Arc::clone(query),
                mode,
                limit,
            )
            .await?
        else {
            return Err(ApplicationError::DocumentNotFound);
        };

        for ScannedUnitHit {
            section,
            kind,
            paragraph_index,
            sentence_index,
            score,
            ..
        } in scanned
        {
            let section_id = section_ids[section].clone();
            let found = document.find_section(&section_id).ok_or_else(|| {
                ApplicationError::IndexFailed(format!(
                    "search hit section {} is missing from the canonical document",
                    section_id.0
                ))
            })?;
            let (unit_kind, wanted_sentence) = match kind {
                SearchHitKind::Paragraph => (EffectiveTextUnitKind::Paragraph, None),
                SearchHitKind::Sentence => (EffectiveTextUnitKind::Sentence, sentence_index),
                SearchHitKind::Section => {
                    return Err(ApplicationError::IndexFailed(
                        "a lexical scan must not report a section unit".into(),
                    ));
                }
            };
            let unit = self
                .units
                .section_unit(
                    &document.id,
                    &section_id,
                    unit_kind,
                    paragraph_index,
                    wanted_sentence,
                )
                .await?
                .ok_or_else(|| {
                    ApplicationError::IndexFailed(format!(
                        "search unit {paragraph_index}{} of section {} is not stored",
                        wanted_sentence
                            .map(|sentence| format!("#sentence:{sentence}"))
                            .unwrap_or_default(),
                        section_id.0
                    ))
                })?;
            hits.push(unit_hit(document, found, unit, raw_query, score));
        }
        Ok(hits)
    }
}

fn collect_section_hits(
    document: &Document,
    section: &Section,
    normalized_document_hash: &NormalizedDocumentHash,
    raw_query: &str,
    query: &LexicalQuery,
    mode: MatchMode,
    hits: &mut Vec<LexicalSearchHit>,
) {
    let lowered = lowercase_fast(&section.title);
    let score = match mode {
        MatchMode::Strict => {
            if query.strict_match(&section.title, &lowered) {
                Some(score_lowered(
                    &lowered,
                    &query.raw_lowered,
                    SearchHitKind::Section,
                    query.token_count(),
                ))
            } else {
                None
            }
        }
        MatchMode::Relaxed => {
            let matched = query.relaxed_count(&section.title, &lowered);
            if matched > 0 {
                Some(score_lowered(
                    &lowered,
                    &query.raw_lowered,
                    SearchHitKind::Section,
                    matched,
                ))
            } else {
                None
            }
        }
    };
    if let Some(score) = score {
        hits.push(LexicalSearchHit {
            section_id: section.id.clone(),
            title: section.title.clone(),
            source: document.source.clone(),
            snippet: match_snippet(&section.title, raw_query, MAX_SNIPPET_CHARS),
            score,
            location: section.location.clone(),
            candidate_kind: SearchHitKind::Section,
            text_locator: TextLocator::for_section_with_hash(
                document,
                section,
                normalized_document_hash,
            ),
            tokenizer_version: LEXICAL_TOKENIZER_VERSION.into(),
        });
    }
    for child in &section.children {
        collect_section_hits(
            document,
            child,
            normalized_document_hash,
            raw_query,
            query,
            mode,
            hits,
        );
    }
}

fn unit_hit(
    document: &Document,
    section: &Section,
    unit: OwnedStreamUnit,
    raw_query: &str,
    score: f32,
) -> LexicalSearchHit {
    match unit {
        OwnedStreamUnit::Paragraph(paragraph) => LexicalSearchHit {
            section_id: section.id.clone(),
            title: section.title.clone(),
            source: document.source.clone(),
            snippet: match_snippet(&paragraph.text, raw_query, MAX_SNIPPET_CHARS),
            score,
            location: text_unit_location(section, paragraph.paragraph_index, None),
            candidate_kind: SearchHitKind::Paragraph,
            text_locator: TextLocator::for_paragraph(document, section, &paragraph),
            tokenizer_version: LEXICAL_TOKENIZER_VERSION.into(),
        },
        OwnedStreamUnit::Sentence(sentence) => LexicalSearchHit {
            section_id: section.id.clone(),
            title: section.title.clone(),
            source: document.source.clone(),
            snippet: match_snippet(&sentence.text, raw_query, MAX_SNIPPET_CHARS),
            score,
            location: text_unit_location(
                section,
                sentence.paragraph_index,
                Some(sentence.sentence_index),
            ),
            candidate_kind: SearchHitKind::Sentence,
            text_locator: TextLocator::for_sentence(document, section, &sentence),
            tokenizer_version: LEXICAL_TOKENIZER_VERSION.into(),
        },
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
        let segmentation = Arc::new(document.try_segmentation().map_err(|error| {
            ApplicationError::IndexFailed(format!(
                "cannot build lexical candidates from persisted block evidence: {error}"
            ))
        })?);
        let normalized_document_hash = document.normalized_document_hash();
        self.ensure_segmentation(
            Arc::new(document.clone()),
            normalized_document_hash,
            segmentation,
        )
        .await
    }

    async fn index_with_segmentation(
        &self,
        document: Arc<Document>,
        normalized_document_hash: NormalizedDocumentHash,
        segmentation: Arc<DocumentSegmentation>,
    ) -> Result<(), ApplicationError> {
        self.ensure_segmentation(document, normalized_document_hash, segmentation)
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
        let Some(lexical) = LexicalQuery::new(query) else {
            return Err(ApplicationError::InvalidRequest(
                "search query must contain searchable terms".into(),
            ));
        };
        let lexical = Arc::new(lexical);
        // The repository caches the decoded document, and the caller has just loaded it, so this is a
        // cache hit in the runtime.
        let Some((document, normalized_document_hash)) =
            self.repository.get_with_hash(document_id).await?
        else {
            return Err(ApplicationError::DocumentNotFound);
        };

        let mut hits = self
            .scan_pass(
                &document,
                &normalized_document_hash,
                query,
                &lexical,
                MatchMode::Strict,
                limit,
            )
            .await?;
        // Strict all-token matching is authoritative. Only a zero-hit multi-token query falls back to
        // the any-token ranking; single-token queries never relax.
        if hits.is_empty() && lexical.token_count() > 1 {
            hits = self
                .scan_pass(
                    &document,
                    &normalized_document_hash,
                    query,
                    &lexical,
                    MatchMode::Relaxed,
                    limit,
                )
                .await?;
        }
        hits.sort_by(compare_hits);
        hits.truncate(limit);
        Ok(hits)
    }
}
