use std::collections::{HashMap, HashSet};

use serde::Deserialize;

use crate::application::ports::ApplicationError;
use crate::domain::{Document, MediaType, Section, SectionId};

pub(crate) const BODY_ORDER_VERSION: &str = "body-order/v1";
const EPUB_STRUCTURE_MAP_VERSION: &str = "epub-structure-reconciliation/v1";
const EPUB_STRUCTURE_MAP_METADATA_KEY: &str = "epub_structure_map";

pub(crate) fn section_body_order(
    document: &Document,
) -> Result<HashMap<SectionId, usize>, ApplicationError> {
    let ordered_ids = if is_epub(&document.media_type) {
        epub_body_order(document)?
    } else {
        let mut ids = Vec::new();
        for section in &document.root_sections {
            collect_canonical_sequence(section, &mut ids);
        }
        ids
    };

    let all_ids = canonical_section_ids(document);
    let ordered_set = ordered_ids.iter().cloned().collect::<HashSet<_>>();
    if ordered_ids.len() != all_ids.len() || ordered_set != all_ids {
        return Err(ApplicationError::RepositoryFailed(
            "canonical body order does not account for every Section exactly once".into(),
        ));
    }

    Ok(ordered_ids
        .into_iter()
        .enumerate()
        .map(|(order, id)| (id, order))
        .collect())
}

fn is_epub(media_type: &MediaType) -> bool {
    media_type.0.eq_ignore_ascii_case("application/epub+zip")
}

fn collect_canonical_sequence(section: &Section, output: &mut Vec<SectionId>) {
    output.push(section.id.clone());
    for child in &section.children {
        collect_canonical_sequence(child, output);
    }
}

fn canonical_section_ids(document: &Document) -> HashSet<SectionId> {
    let mut ids = HashSet::new();
    for section in &document.root_sections {
        collect_ids(section, &mut ids);
    }
    ids
}

fn collect_ids(section: &Section, output: &mut HashSet<SectionId>) {
    output.insert(section.id.clone());
    for child in &section.children {
        collect_ids(child, output);
    }
}

fn epub_body_order(document: &Document) -> Result<Vec<SectionId>, ApplicationError> {
    let json = document
        .metadata
        .get(EPUB_STRUCTURE_MAP_METADATA_KEY)
        .ok_or_else(|| {
            ApplicationError::RepositoryFailed(
                "EPUB document has no canonical structure map for body order".into(),
            )
        })?;
    let map = serde_json::from_str::<EpubStructureMap>(json).map_err(|error| {
        ApplicationError::RepositoryFailed(format!(
            "EPUB canonical structure map cannot be decoded for body order: {error}"
        ))
    })?;
    if map.schema_version != EPUB_STRUCTURE_MAP_VERSION {
        return Err(ApplicationError::RepositoryFailed(format!(
            "unsupported EPUB structure map {}; expected {EPUB_STRUCTURE_MAP_VERSION}",
            map.schema_version
        )));
    }

    let mut facts = map.sections;
    facts.sort_by_key(|fact| fact.source_order);
    if facts
        .iter()
        .enumerate()
        .any(|(expected, fact)| fact.source_order != expected)
    {
        return Err(ApplicationError::RepositoryFailed(
            "EPUB structure map source order is not contiguous".into(),
        ));
    }
    Ok(facts
        .into_iter()
        .map(|fact| SectionId(fact.section_id))
        .collect())
}

#[derive(Debug, Deserialize)]
struct EpubStructureMap {
    schema_version: String,
    sections: Vec<EpubStructureSectionFact>,
}

#[derive(Debug, Deserialize)]
struct EpubStructureSectionFact {
    section_id: String,
    source_order: usize,
}

#[cfg(test)]
mod tests {
    use super::section_body_order;
    use crate::domain::{
        ContentHash, Document, DocumentId, DocumentSource, MediaType, Section, SectionId,
    };

    #[test]
    fn non_epub_order_is_canonical_body_before_children() {
        let document = document(
            MediaType("text/markdown".into()),
            vec![section("parent", vec![section("child", Vec::new())])],
        );
        let order = section_body_order(&document).expect("body order should be available");
        assert_eq!(order[&SectionId("section://parent".into())], 0);
        assert_eq!(order[&SectionId("section://child".into())], 1);
    }

    #[test]
    fn epub_order_uses_flat_spine_facts_instead_of_tree_preorder() {
        let mut document = document(
            MediaType("application/epub+zip".into()),
            vec![section("parent", vec![section("child", Vec::new())])],
        );
        document.metadata.insert(
            "epub_structure_map".into(),
            r#"{"schema_version":"epub-structure-reconciliation/v1","sections":[{"section_id":"section://parent","source_order":1},{"section_id":"section://parent/child","source_order":0}]}"#.into(),
        );
        let order = section_body_order(&document).expect("EPUB body order should be available");
        assert_eq!(order[&SectionId("section://parent/child".into())], 0);
        assert_eq!(order[&SectionId("section://parent".into())], 1);
    }

    fn document(media_type: MediaType, root_sections: Vec<Section>) -> Document {
        Document {
            id: DocumentId("doc:body-order".into()),
            source: DocumentSource("memory:body-order".into()),
            title: "Body order".into(),
            media_type,
            content_hash: ContentHash("sha256:body-order".into()),
            metadata: Default::default(),
            root_sections,
        }
    }

    fn section(name: &str, children: Vec<Section>) -> Section {
        Section {
            id: SectionId(format!("section://{name}")),
            parent_id: None,
            title: name.into(),
            level: 1,
            content: format!("{name} body"),
            location: Default::default(),
            children,
        }
    }
}
