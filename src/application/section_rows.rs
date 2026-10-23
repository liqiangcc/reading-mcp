//! The section tree and the body order as flat rows.
//!
//! Every question a reader asks about the tree — one section, its children, its ancestors, the
//! window around it in body order — is answered here from a depth-first flattening of the
//! `Document`, which is what the store keeps in `document_sections_v1` and
//! `document_body_order_rows_v1`. The trait defaults in [`crate::application::ports`] and the
//! SQLite fallback for documents stored before those tables existed both call these functions, so
//! the slow path and the fast path answer with the same values.

use std::collections::HashMap;

use crate::domain::{Document, NormalizedDocumentHash, Section, SectionId};

use super::ports::{BodyOrderWindow, DocumentHeader, SectionRow};

/// The sections of `document` in depth-first document order, one row each.
///
/// `section_key` is the index in that order, which is the numbering
/// `section_layout_v2.section_key` uses; `subtree_end_key` is the last key inside the section's
/// subtree, so a subtree is the contiguous range `[section_key, subtree_end_key]`.
pub(crate) fn flatten_document(document: &Document) -> Vec<SectionRow> {
    let mut rows = Vec::with_capacity(document.section_count());
    for section in &document.root_sections {
        flatten_section(section, None, &mut rows);
    }
    rows
}

fn flatten_section(section: &Section, parent_key: Option<i64>, rows: &mut Vec<SectionRow>) {
    let section_key = i64::try_from(rows.len()).unwrap_or(i64::MAX);
    rows.push(SectionRow {
        section_key,
        id: section.id.clone(),
        parent_key,
        level: section.level,
        title: section.title.clone(),
        location: section.location.clone(),
        // Filled in once the subtree is written.
        subtree_end_key: section_key,
    });
    for child in &section.children {
        flatten_section(child, Some(section_key), rows);
    }
    let subtree_end_key = i64::try_from(rows.len() - 1).unwrap_or(i64::MAX);
    rows[usize::try_from(section_key).unwrap_or(0)].subtree_end_key = subtree_end_key;
}

/// The header of `document`, as the store writes it.
pub(crate) fn header_of(
    document: &Document,
    normalized_document_hash: &NormalizedDocumentHash,
) -> DocumentHeader {
    DocumentHeader {
        id: document.id.clone(),
        source: document.source.clone(),
        title: document.title.clone(),
        media_type: document.media_type.clone(),
        content_hash: document.content_hash.clone(),
        normalized_document_hash: Some(normalized_document_hash.clone()),
        section_count: document.section_count(),
        root_count: document.root_sections.len(),
    }
}

/// The row of `section_id`, if the document has that section.
pub(crate) fn section_by_id(document: &Document, section_id: &SectionId) -> Option<SectionRow> {
    flatten_document(document)
        .into_iter()
        .find(|row| row.id == *section_id)
}

/// The row at `section_key`, if the document has that key.
pub(crate) fn section_by_key(document: &Document, section_key: i64) -> Option<SectionRow> {
    row_at(&flatten_document(document), section_key)
}

/// The rows whose keys are in `[from_key, to_key_inclusive]`, in ascending key order.
pub(crate) fn sections_in_key_range(
    document: &Document,
    from_key: i64,
    to_key_inclusive: i64,
    limit: usize,
) -> Vec<SectionRow> {
    flatten_document(document)
        .into_iter()
        .filter(|row| row.section_key >= from_key && row.section_key <= to_key_inclusive)
        .take(limit)
        .collect()
}

/// The children of `parent_key` (the roots when it is `None`), in ascending key order.
pub(crate) fn section_children(
    document: &Document,
    parent_key: Option<i64>,
    limit: usize,
) -> Vec<SectionRow> {
    flatten_document(document)
        .into_iter()
        .filter(|row| row.parent_key == parent_key)
        .take(limit)
        .collect()
}

/// The ancestors of `section_key`, root first and excluding the section itself.
pub(crate) fn section_ancestors(document: &Document, section_key: i64) -> Vec<SectionRow> {
    let rows = flatten_document(document);
    let mut ancestors = Vec::new();
    let mut current = row_at(&rows, section_key).and_then(|row| row.parent_key);
    while let Some(key) = current {
        let Some(row) = row_at(&rows, key) else {
            break;
        };
        current = row.parent_key;
        ancestors.push(row);
    }
    ancestors.reverse();
    ancestors
}

/// The row at `section_key`, if the flattened tree has that key.
fn row_at(rows: &[SectionRow], section_key: i64) -> Option<SectionRow> {
    rows.get(usize::try_from(section_key).ok()?)
        .filter(|row| row.section_key == section_key)
        .cloned()
}

/// The body-order window around `section_key`, or `None` when the section is not in `order`.
pub(crate) fn body_order_window(
    document: &Document,
    order: &[SectionId],
    section_key: i64,
    before: usize,
    after: usize,
) -> Option<BodyOrderWindow> {
    let keys = body_order_keys(document, order);
    let ord = keys.iter().position(|key| *key == section_key)?;
    let start = ord.saturating_sub(before);
    let end = ord.saturating_add(after).min(keys.len().saturating_sub(1));
    Some(BodyOrderWindow {
        ord,
        len: keys.len(),
        window: keys[start..=end]
            .iter()
            .enumerate()
            .map(|(offset, key)| (start + offset, *key))
            .collect(),
    })
}

/// The `section_key` at body-order position `ord`, or `None` when `ord` is past the end.
pub(crate) fn body_order_at(document: &Document, order: &[SectionId], ord: usize) -> Option<i64> {
    body_order_keys(document, order).get(ord).copied()
}

/// The `section_key` of every section id in `order`, in body order.
pub(crate) fn body_order_keys(document: &Document, order: &[SectionId]) -> Vec<i64> {
    let keys_by_id: HashMap<SectionId, i64> = flatten_document(document)
        .into_iter()
        .map(|row| (row.id, row.section_key))
        .collect();
    order
        .iter()
        .map(|id| keys_by_id.get(id).copied().unwrap_or(-1))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{ContentHash, DocumentId, DocumentSource, MediaType};

    fn section(name: &str, parent: Option<&str>, children: Vec<Section>) -> Section {
        Section {
            id: SectionId(format!("section://{name}")),
            parent_id: parent.map(|parent| SectionId(format!("section://{parent}"))),
            title: name.into(),
            level: 1,
            content: format!("{name} body."),
            location: Default::default(),
            children,
        }
    }

    fn document() -> Document {
        Document {
            id: DocumentId("doc:rows".into()),
            source: DocumentSource("memory:rows".into()),
            title: "Rows".into(),
            media_type: MediaType("text/markdown".into()),
            content_hash: ContentHash("sha256:rows".into()),
            metadata: Default::default(),
            root_sections: vec![
                section(
                    "a",
                    None,
                    vec![
                        section("a1", Some("a"), vec![section("a1x", Some("a1"), vec![])]),
                        section("a2", Some("a"), vec![]),
                    ],
                ),
                section("b", None, vec![]),
            ],
        }
    }

    #[test]
    fn the_flattening_numbers_a_depth_first_walk_with_contiguous_subtrees() {
        let rows = flatten_document(&document());
        let keys: Vec<(i64, &str, Option<i64>, i64)> = rows
            .iter()
            .map(|row| {
                (
                    row.section_key,
                    row.id.0.as_str(),
                    row.parent_key,
                    row.subtree_end_key,
                )
            })
            .collect();
        assert_eq!(
            keys,
            vec![
                (0, "section://a", None, 3),
                (1, "section://a1", Some(0), 2),
                (2, "section://a1x", Some(1), 2),
                (3, "section://a2", Some(0), 3),
                (4, "section://b", None, 4),
            ]
        );
    }

    #[test]
    fn the_ancestors_are_root_first_and_exclude_the_section() {
        let document = document();
        let ancestors: Vec<String> = section_ancestors(&document, 2)
            .into_iter()
            .map(|row| row.id.0)
            .collect();
        assert_eq!(ancestors, vec!["section://a", "section://a1"]);
    }
}
