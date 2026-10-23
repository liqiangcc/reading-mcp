//! The point-query view of one stored document: its header plus one section row at a time.
//!
//! SEC-1 stores the section tree as indexed rows (`document_sections_v1`) and the body order as
//! indexed rows (`document_body_order_rows_v1`) and exposes them through nine `DocumentRepository`
//! calls. This module is the small view layer the readers use instead of decoding the whole
//! document: a [`DocumentView`] holds the header and answers section questions one row at a time,
//! so a call costs O(the page it returns) and does not depend on the book's size.
//!
//! A row is turned into the domain values the existing renderers and locator constructors take by
//! [`carrier_document`] and [`carrier_section`]: a `Document` that carries only identity and source,
//! and a `Section` with empty `content` and empty `children`. They reproduce the bytes those
//! constructors read (`id`, `content_hash`, `location.section_path`, `location.native_location`)
//! without a second constructor that could drift from the domain one. **A carrier is never
//! rendered, walked or searched**: a path that renders a tree loads the subtree's rows first with
//! [`DocumentView::subtree_section`].

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::application::ports::{ApplicationError, DocumentHeader, DocumentRepository, SectionRow};
use crate::application::section_rows;
use crate::domain::{Document, DocumentId, NormalizedDocumentHash, Section, SectionId};

/// One stored document as the readers see it.
///
/// Normally it is the header plus one row per question. A caller that already holds the whole
/// document — the `DerivedTextUnits` fallback, which segments the document's own text — keeps its
/// single load and answers from that document instead, so both modes run the same reader code.
pub(crate) struct DocumentView {
    repository: Arc<dyn DocumentRepository>,
    header: DocumentHeader,
    /// The whole document, when the caller already loaded it.
    full: Option<Arc<Document>>,
}

impl DocumentView {
    /// The header of `id`, or `Ok(None)` when the store has no such document. A document stored
    /// before SEC-1 has no header row; the port's default (and the SQLite fallback) builds one from
    /// the document itself, so this still answers.
    pub(crate) async fn load(
        repository: Arc<dyn DocumentRepository>,
        id: &DocumentId,
    ) -> Result<Option<Self>, ApplicationError> {
        Ok(repository.document_header(id).await?.map(|header| Self {
            repository,
            header,
            full: None,
        }))
    }

    /// A view over a document the caller already holds: every question is answered from that
    /// document, and nothing is loaded twice.
    pub(crate) fn from_document(
        repository: Arc<dyn DocumentRepository>,
        document: Arc<Document>,
        normalized_document_hash: &NormalizedDocumentHash,
    ) -> Self {
        Self {
            header: section_rows::header_of(&document, normalized_document_hash),
            repository,
            full: Some(document),
        }
    }

    /// The header and the normalized document hash the readers bind handles and cursors to.
    ///
    /// The hash comes from the header. A store whose header carries no hash falls back to
    /// `get_light_with_hash` — the only way to learn it — which is the pre-SEC-1 cost and only for
    /// such a store.
    pub(crate) async fn load_with_hash(
        repository: Arc<dyn DocumentRepository>,
        id: &DocumentId,
    ) -> Result<Option<(Self, NormalizedDocumentHash)>, ApplicationError> {
        let Some(view) = Self::load(Arc::clone(&repository), id).await? else {
            return Ok(None);
        };
        let hash = match view.header.normalized_document_hash.clone() {
            Some(hash) => hash,
            None => match repository.get_light_with_hash(id).await? {
                Some((_, hash)) => hash,
                None => return Ok(None),
            },
        };
        Ok(Some((view, hash)))
    }

    pub(crate) fn header(&self) -> &DocumentHeader {
        &self.header
    }

    pub(crate) fn id(&self) -> &DocumentId {
        &self.header.id
    }

    pub(crate) fn repository(&self) -> &Arc<dyn DocumentRepository> {
        &self.repository
    }

    /// The identity-only `Document` the locator constructors and the text loaders take. Never a
    /// tree to walk or render.
    pub(crate) fn document(&self) -> Document {
        carrier_document(&self.header)
    }

    /// One section row by id: one unique-index read.
    pub(crate) async fn section_by_id(
        &self,
        section_id: &SectionId,
    ) -> Result<Option<SectionRow>, ApplicationError> {
        if let Some(document) = &self.full {
            return Ok(section_rows::section_by_id(document, section_id));
        }
        self.repository
            .section_by_id(&self.header.id, section_id)
            .await
    }

    /// One section row by its depth-first key: one unique-index read.
    pub(crate) async fn section_by_key(
        &self,
        section_key: i64,
    ) -> Result<Option<SectionRow>, ApplicationError> {
        if let Some(document) = &self.full {
            return Ok(section_rows::section_by_key(document, section_key));
        }
        self.repository
            .section_by_key(&self.header.id, section_key)
            .await
    }

    /// The children of `parent_key` (the roots when it is `None`), in ascending key order.
    pub(crate) async fn children(
        &self,
        parent_key: Option<i64>,
        limit: usize,
    ) -> Result<Vec<SectionRow>, ApplicationError> {
        if let Some(document) = &self.full {
            return Ok(section_rows::section_children(document, parent_key, limit));
        }
        self.repository
            .section_children(&self.header.id, parent_key, limit)
            .await
    }

    /// The ancestors of `section_key`, root first and excluding the section itself.
    pub(crate) async fn ancestors(
        &self,
        section_key: i64,
    ) -> Result<Vec<SectionRow>, ApplicationError> {
        if let Some(document) = &self.full {
            return Ok(section_rows::section_ancestors(document, section_key));
        }
        self.repository
            .section_ancestors(&self.header.id, section_key)
            .await
    }

    /// The rows whose keys are in `[from_key, to_key_inclusive]`, in ascending key order.
    pub(crate) async fn key_range(
        &self,
        from_key: i64,
        to_key_inclusive: i64,
    ) -> Result<Vec<SectionRow>, ApplicationError> {
        let limit = usize::try_from(to_key_inclusive.saturating_sub(from_key).saturating_add(1))
            .unwrap_or(usize::MAX);
        if let Some(document) = &self.full {
            return Ok(section_rows::sections_in_key_range(
                document,
                from_key,
                to_key_inclusive,
                limit,
            ));
        }
        self.repository
            .sections_in_key_range(&self.header.id, from_key, to_key_inclusive, limit)
            .await
    }

    /// The rows of `row`'s subtree in key order: one key-range read.
    pub(crate) async fn subtree(
        &self,
        row: &SectionRow,
    ) -> Result<Vec<SectionRow>, ApplicationError> {
        self.key_range(row.section_key, row.subtree_end_key).await
    }

    /// One section as a `Section` value: the full document's own section when the caller holds it,
    /// otherwise the row carrier (which a renderer must not use).
    pub(crate) async fn section_of(
        &self,
        section_id: &SectionId,
    ) -> Result<Option<Section>, ApplicationError> {
        if let Some(document) = &self.full {
            return Ok(document.find_section(section_id).cloned());
        }
        Ok(self
            .section_by_id(section_id)
            .await?
            .map(|row| carrier_section(&row)))
    }

    /// The subtree of `section_id` nested into a `Section` tree, the only form a renderer may see:
    /// the full document's own section when the caller holds it, otherwise one key-range read.
    pub(crate) async fn subtree_of(
        &self,
        section_id: &SectionId,
    ) -> Result<Option<Section>, ApplicationError> {
        if let Some(document) = &self.full {
            return Ok(document.find_section(section_id).cloned());
        }
        match self.section_by_id(section_id).await? {
            Some(row) => Ok(Some(self.subtree_section(&row).await?)),
            None => Ok(None),
        }
    }

    /// `row`'s subtree nested back into a `Section`, the only form a renderer may see. One key-range
    /// read of the subtree, which is exactly what rendering it costs.
    pub(crate) async fn subtree_section(
        &self,
        row: &SectionRow,
    ) -> Result<Section, ApplicationError> {
        let rows = self.subtree(row).await?;
        subtree_from_rows(&rows).ok_or_else(|| {
            ApplicationError::RepositoryFailed(format!(
                "section {} of document {} is not in the stored section rows",
                row.id.0, self.header.id.0
            ))
        })
    }
}

/// The identity-only `Document` of a header: `id`, `source`, `title`, `media_type`, `content_hash`
/// and no tree. Every field a `TextLocator` constructor or a cursor claim reads is here; nothing a
/// renderer or a search reads is.
pub(crate) fn carrier_document(header: &DocumentHeader) -> Document {
    Document {
        id: header.id.clone(),
        source: header.source.clone(),
        title: header.title.clone(),
        media_type: header.media_type.clone(),
        content_hash: header.content_hash.clone(),
        metadata: BTreeMap::new(),
        root_sections: Vec::new(),
    }
}

/// The `Section` of one row: `id`, `parent_id`, `title`, `level`, `location` and empty `content`
/// and `children`. Never rendered, walked or searched.
pub(crate) fn carrier_section(row: &SectionRow) -> Section {
    Section {
        id: row.id.clone(),
        // The row carries its parent's key, not its id; only the locator constructors and the
        // renderers read a `Section`, and neither reads `parent_id`.
        parent_id: None,
        title: row.title.clone(),
        level: row.level,
        content: String::new(),
        location: row.location.clone(),
        children: Vec::new(),
    }
}

/// `rows` nested into a `Section` tree: `rows[0]` is the root of the subtree, every later row is
/// inside its subtree, and each child's subtree is contiguous, so one pass rebuilds the nesting.
fn subtree_from_rows(rows: &[SectionRow]) -> Option<Section> {
    fn build(rows: &[SectionRow], index: usize) -> (Section, usize) {
        let row = &rows[index];
        let mut section = carrier_section(row);
        let mut next = index + 1;
        while next < rows.len() && rows[next].parent_key == Some(row.section_key) {
            let (child, after) = build(rows, next);
            section.children.push(child);
            next = after;
        }
        (section, next)
    }
    if rows.is_empty() {
        return None;
    }
    Some(build(rows, 0).0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{ContentHash, DocumentSource, Location, MediaType};

    fn row(key: i64, parent: Option<i64>, end: i64) -> SectionRow {
        SectionRow {
            section_key: key,
            id: SectionId(format!("section://{key}")),
            parent_key: parent,
            level: 1,
            title: format!("Title {key}"),
            location: Location::default(),
            subtree_end_key: end,
        }
    }

    #[test]
    fn a_carrier_section_keeps_the_fields_a_locator_reads_and_nothing_else() {
        let header = DocumentHeader {
            id: DocumentId("doc:carrier".into()),
            source: DocumentSource("memory:carrier".into()),
            title: "Carrier".into(),
            media_type: MediaType("text/markdown".into()),
            content_hash: ContentHash("sha256:carrier".into()),
            normalized_document_hash: None,
            section_count: 3,
            root_count: 1,
        };
        let document = carrier_document(&header);
        assert_eq!(document.id, header.id);
        assert_eq!(document.content_hash, header.content_hash);
        assert!(document.root_sections.is_empty());

        let section = carrier_section(&row(0, None, 2));
        assert_eq!(section.id, SectionId("section://0".into()));
        assert!(section.content.is_empty());
        assert!(section.children.is_empty());
    }

    #[test]
    fn a_subtree_is_rebuilt_from_its_contiguous_rows() {
        let rows = vec![
            row(0, None, 3),
            row(1, Some(0), 2),
            row(2, Some(1), 2),
            row(3, Some(0), 3),
        ];
        let section = subtree_from_rows(&rows).expect("a non-empty subtree");
        assert_eq!(section.id, SectionId("section://0".into()));
        let children: Vec<SectionId> = section.children.iter().map(|s| s.id.clone()).collect();
        assert_eq!(
            children,
            vec![
                SectionId("section://1".into()),
                SectionId("section://3".into())
            ]
        );
        assert_eq!(
            section.children[0].children[0].id,
            SectionId("section://2".into())
        );
        assert!(subtree_from_rows(&[]).is_none());
    }
}
