use std::collections::{HashMap, HashSet};

use serde::Serialize;

use crate::domain::{Section, SectionId};

use super::epub_navigation::{
    EpubNavigationMap, EpubNavigationNode, EpubNavigationProvenance, NavigationResolutionStatus,
};

pub(crate) const EPUB_STRUCTURE_MAP_VERSION: &str = "epub-structure-reconciliation/v1";

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum EpubSpineParseStatus {
    Parsed,
    MissingManifest,
    UnsupportedMedia,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub(crate) struct EpubSpineRecord {
    pub(crate) spine_index: usize,
    pub(crate) idref: String,
    pub(crate) linear: bool,
    pub(crate) manifest_href: Option<String>,
    pub(crate) resolved_entry_path: Option<String>,
    pub(crate) media_type: Option<String>,
    pub(crate) parse_status: EpubSpineParseStatus,
}

#[derive(Clone, Debug)]
pub(crate) struct ParsedSpineDocument {
    pub(crate) spine_index: usize,
    pub(crate) linear: bool,
    pub(crate) entry_path: String,
    pub(crate) sections: Vec<Section>,
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum EpubStructureProvenance {
    EpubNav,
    EpubNcx,
    XhtmlHeading,
    SpineItem,
}

impl EpubStructureProvenance {
    fn from_navigation(value: EpubNavigationProvenance) -> Self {
        match value {
            EpubNavigationProvenance::EpubNav => Self::EpubNav,
            EpubNavigationProvenance::EpubNcx => Self::EpubNcx,
        }
    }
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub(crate) struct EpubStructureDiagnostic {
    pub(crate) code: String,
    pub(crate) message: String,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub(crate) struct EpubNavigationAliasFact {
    pub(crate) source_order: usize,
    pub(crate) label: String,
    pub(crate) provenance: EpubNavigationProvenance,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub(crate) struct EpubStructureSectionFact {
    pub(crate) section_id: String,
    pub(crate) source_order: usize,
    pub(crate) spine_index: usize,
    pub(crate) linear: bool,
    pub(crate) entry_path: String,
    pub(crate) provenance: EpubStructureProvenance,
    pub(crate) source_title: String,
    pub(crate) source_level: u8,
    pub(crate) canonical_title: String,
    pub(crate) canonical_level: u8,
    pub(crate) canonical_parent_id: Option<String>,
    pub(crate) navigation_source_order: Option<usize>,
    pub(crate) navigation_resolution_status: Option<NavigationResolutionStatus>,
    pub(crate) navigation_aliases: Vec<EpubNavigationAliasFact>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub(crate) struct EpubStructureMap {
    pub(crate) schema_version: String,
    pub(crate) navigation_provenance: Option<EpubNavigationProvenance>,
    pub(crate) navigation_nodes: usize,
    pub(crate) applied_navigation_nodes: usize,
    pub(crate) linear_spine_items: usize,
    pub(crate) non_linear_spine_items: usize,
    pub(crate) spine: Vec<EpubSpineRecord>,
    pub(crate) sections: Vec<EpubStructureSectionFact>,
    pub(crate) diagnostics: Vec<EpubStructureDiagnostic>,
}

impl EpubStructureMap {
    pub(crate) fn section_count(&self) -> usize {
        self.sections.len()
    }
}

pub(crate) struct EpubStructureResult {
    pub(crate) root_sections: Vec<Section>,
    pub(crate) structure_map: EpubStructureMap,
}

#[derive(Clone, Debug)]
struct AppliedNavigation {
    source_order: usize,
    provenance: EpubNavigationProvenance,
    resolution_status: NavigationResolutionStatus,
}

#[derive(Clone, Debug)]
struct FlatSection {
    section: Section,
    source_order: usize,
    spine_index: usize,
    linear: bool,
    entry_path: String,
    source_title: String,
    source_level: u8,
    fallback_provenance: EpubStructureProvenance,
    applied_navigation: Option<AppliedNavigation>,
    aliases: Vec<EpubNavigationAliasFact>,
}

#[derive(Clone, Debug)]
struct FlatNavigationNode {
    source_order: usize,
    parent_source_order: Option<usize>,
    label: String,
    resolved_entry_path: Option<String>,
    fragment: Option<String>,
    provenance: EpubNavigationProvenance,
    resolution_status: NavigationResolutionStatus,
}

pub(crate) fn reconcile_epub_structure(
    parsed_spine: Vec<ParsedSpineDocument>,
    spine_records: Vec<EpubSpineRecord>,
    navigation: &EpubNavigationMap,
) -> EpubStructureResult {
    let mut flat_sections = Vec::new();
    let mut source_order = 0usize;
    for parsed in parsed_spine {
        flatten_sections(
            parsed.sections,
            &parsed,
            &mut source_order,
            &mut flat_sections,
        );
    }

    let mut diagnostics = Vec::new();
    let mut navigation_nodes = Vec::new();
    flatten_navigation(&navigation.nodes, None, &mut navigation_nodes);
    let navigation_by_order = navigation_nodes
        .iter()
        .map(|node| (node.source_order, node))
        .collect::<HashMap<_, _>>();

    let mut navigation_targets = HashMap::<usize, usize>::new();
    let mut primary_for_section = HashMap::<usize, usize>::new();

    for node in &navigation_nodes {
        let Some(section_index) =
            resolve_navigation_section(node, &flat_sections, &mut diagnostics)
        else {
            continue;
        };
        navigation_targets.insert(node.source_order, section_index);

        if let Some(primary_source_order) = primary_for_section.get(&section_index).copied() {
            flat_sections[section_index]
                .aliases
                .push(EpubNavigationAliasFact {
                    source_order: node.source_order,
                    label: node.label.clone(),
                    provenance: node.provenance,
                });
            diagnostics.push(EpubStructureDiagnostic {
                code: "duplicate_navigation_target".into(),
                message: format!(
                    "navigation node {} targets the same canonical Section as earlier node {}; the earlier node remains the structural owner and this node is retained as an alias",
                    node.source_order, primary_source_order
                ),
            });
            continue;
        }

        primary_for_section.insert(section_index, node.source_order);
        if node.label.trim().is_empty() {
            diagnostics.push(EpubStructureDiagnostic {
                code: "empty_navigation_label".into(),
                message: format!(
                    "navigation node {} resolves to a Section but has an empty label; the source heading title is retained",
                    node.source_order
                ),
            });
        } else {
            flat_sections[section_index].section.title = node.label.clone();
        }
        flat_sections[section_index].applied_navigation = Some(AppliedNavigation {
            source_order: node.source_order,
            provenance: node.provenance,
            resolution_status: node.resolution_status,
        });
    }

    diagnose_navigation_source_order(
        &navigation_nodes,
        &navigation_targets,
        &flat_sections,
        &mut diagnostics,
    );

    for (&section_index, &navigation_source_order) in &primary_for_section {
        let Some(node) = navigation_by_order.get(&navigation_source_order).copied() else {
            continue;
        };
        match node.parent_source_order {
            None => flat_sections[section_index].section.parent_id = None,
            Some(_) => {
                let mapped_parent = nearest_mapped_navigation_parent(
                    node,
                    &navigation_by_order,
                    &navigation_targets,
                    section_index,
                );
                match mapped_parent {
                    Some(parent_index)
                        if flat_sections[parent_index].source_order
                            < flat_sections[section_index].source_order =>
                    {
                        flat_sections[section_index].section.parent_id =
                            Some(flat_sections[parent_index].section.id.clone());
                    }
                    Some(parent_index) => diagnostics.push(EpubStructureDiagnostic {
                        code: "navigation_parent_conflicts_source_order".into(),
                        message: format!(
                            "navigation parent Section {} occurs at source order {} after/equal to child Section {} at {}; source-derived parentage is retained",
                            flat_sections[parent_index].section.id.0,
                            flat_sections[parent_index].source_order,
                            flat_sections[section_index].section.id.0,
                            flat_sections[section_index].source_order
                        ),
                    }),
                    None => diagnostics.push(EpubStructureDiagnostic {
                        code: "navigation_parent_unmapped".into(),
                        message: format!(
                            "navigation node {} has no ancestor that maps to a canonical Section boundary; source heading parentage is retained",
                            navigation_source_order
                        ),
                    }),
                }
            }
        }
    }

    let applied_navigation_nodes = primary_for_section.len();
    let mut root_sections = build_forest(&flat_sections, &mut diagnostics);
    if applied_navigation_nodes > 0 {
        for root in &mut root_sections {
            rewrite_canonical_paths(root, &[]);
        }
    }

    let final_sections = final_section_facts(&root_sections);
    let sections = flat_sections
        .iter()
        .map(|flat| {
            let final_fact = final_sections
                .get(&flat.section.id)
                .expect("reconciled Section must remain present in final forest");
            EpubStructureSectionFact {
                section_id: flat.section.id.0.clone(),
                source_order: flat.source_order,
                spine_index: flat.spine_index,
                linear: flat.linear,
                entry_path: flat.entry_path.clone(),
                provenance: flat
                    .applied_navigation
                    .as_ref()
                    .map(|value| EpubStructureProvenance::from_navigation(value.provenance))
                    .unwrap_or(flat.fallback_provenance),
                source_title: flat.source_title.clone(),
                source_level: flat.source_level,
                canonical_title: final_fact.title.clone(),
                canonical_level: final_fact.level,
                canonical_parent_id: final_fact.parent_id.as_ref().map(|value| value.0.clone()),
                navigation_source_order: flat
                    .applied_navigation
                    .as_ref()
                    .map(|value| value.source_order),
                navigation_resolution_status: flat
                    .applied_navigation
                    .as_ref()
                    .map(|value| value.resolution_status),
                navigation_aliases: flat.aliases.clone(),
            }
        })
        .collect::<Vec<_>>();

    let linear_spine_items = spine_records.iter().filter(|item| item.linear).count();
    let non_linear_spine_items = spine_records.len().saturating_sub(linear_spine_items);

    EpubStructureResult {
        root_sections,
        structure_map: EpubStructureMap {
            schema_version: EPUB_STRUCTURE_MAP_VERSION.into(),
            navigation_provenance: navigation.provenance,
            navigation_nodes: navigation.node_count(),
            applied_navigation_nodes,
            linear_spine_items,
            non_linear_spine_items,
            spine: spine_records,
            sections,
            diagnostics,
        },
    }
}

fn flatten_sections(
    sections: Vec<Section>,
    parsed: &ParsedSpineDocument,
    source_order: &mut usize,
    output: &mut Vec<FlatSection>,
) {
    for mut section in sections {
        let children = std::mem::take(&mut section.children);
        let fallback_provenance =
            if section.id.0.ends_with("/document") || section.id.0.ends_with("/preamble") {
                EpubStructureProvenance::SpineItem
            } else {
                EpubStructureProvenance::XhtmlHeading
            };
        let source_title = section.title.clone();
        let source_level = section.level;
        output.push(FlatSection {
            section,
            source_order: *source_order,
            spine_index: parsed.spine_index,
            linear: parsed.linear,
            entry_path: parsed.entry_path.clone(),
            source_title,
            source_level,
            fallback_provenance,
            applied_navigation: None,
            aliases: Vec::new(),
        });
        *source_order += 1;
        flatten_sections(children, parsed, source_order, output);
    }
}

fn flatten_navigation(
    nodes: &[EpubNavigationNode],
    parent_source_order: Option<usize>,
    output: &mut Vec<FlatNavigationNode>,
) {
    for node in nodes {
        output.push(FlatNavigationNode {
            source_order: node.source_order,
            parent_source_order,
            label: node.label.clone(),
            resolved_entry_path: node.resolved_entry_path.clone(),
            fragment: node.fragment.clone(),
            provenance: node.provenance,
            resolution_status: node.resolution_status,
        });
        flatten_navigation(&node.children, Some(node.source_order), output);
    }
}

fn resolve_navigation_section(
    node: &FlatNavigationNode,
    sections: &[FlatSection],
    diagnostics: &mut Vec<EpubStructureDiagnostic>,
) -> Option<usize> {
    let entry_path = node.resolved_entry_path.as_deref()?;
    match node.resolution_status {
        NavigationResolutionStatus::ResolvedFragment => {
            let fragment = node.fragment.as_deref()?;
            let matched = sections.iter().position(|section| {
                section.entry_path == entry_path
                    && section.section.location.anchor.as_deref() == Some(fragment)
            });
            if matched.is_none() {
                diagnostics.push(EpubStructureDiagnostic {
                    code: "navigation_fragment_not_section_boundary".into(),
                    message: format!(
                        "navigation node {} resolves fragment {:?} in {:?}, but that fragment is not an existing canonical heading Section boundary; XHTML heading fallback is retained",
                        node.source_order, fragment, entry_path
                    ),
                });
            }
            matched
        }
        NavigationResolutionStatus::ResolvedDocument => {
            first_section_in_entry(entry_path, sections)
        }
        NavigationResolutionStatus::MissingFragment => {
            let result = first_section_in_entry(entry_path, sections);
            if result.is_some() {
                diagnostics.push(EpubStructureDiagnostic {
                    code: "navigation_missing_fragment_document_fallback".into(),
                    message: format!(
                        "navigation node {} references a missing fragment in {:?}; structural mapping degrades to the document's first canonical Section",
                        node.source_order, entry_path
                    ),
                });
            }
            result
        }
        NavigationResolutionStatus::MissingResource
        | NavigationResolutionStatus::UnsupportedResource
        | NavigationResolutionStatus::InvalidPath
        | NavigationResolutionStatus::MalformedResource
        | NavigationResolutionStatus::Unlinked => None,
    }
}

fn first_section_in_entry(entry_path: &str, sections: &[FlatSection]) -> Option<usize> {
    sections
        .iter()
        .position(|section| section.entry_path == entry_path)
}

fn nearest_mapped_navigation_parent(
    node: &FlatNavigationNode,
    navigation_by_order: &HashMap<usize, &FlatNavigationNode>,
    navigation_targets: &HashMap<usize, usize>,
    child_section_index: usize,
) -> Option<usize> {
    let mut parent_order = node.parent_source_order;
    while let Some(order) = parent_order {
        if let Some(section_index) = navigation_targets.get(&order).copied()
            && section_index != child_section_index
        {
            return Some(section_index);
        }
        parent_order = navigation_by_order
            .get(&order)
            .and_then(|parent| parent.parent_source_order);
    }
    None
}

fn diagnose_navigation_source_order(
    navigation: &[FlatNavigationNode],
    navigation_targets: &HashMap<usize, usize>,
    sections: &[FlatSection],
    diagnostics: &mut Vec<EpubStructureDiagnostic>,
) {
    let mut previous: Option<(usize, usize)> = None;
    let mut reported = HashSet::new();
    for node in navigation {
        let Some(section_index) = navigation_targets.get(&node.source_order).copied() else {
            continue;
        };
        let current_source_order = sections[section_index].source_order;
        if let Some((previous_navigation_order, previous_source_order)) = previous
            && current_source_order < previous_source_order
            && reported.insert((previous_navigation_order, node.source_order))
        {
            diagnostics.push(EpubStructureDiagnostic {
                code: "navigation_order_conflicts_spine_order".into(),
                message: format!(
                    "navigation node {} maps to source order {} before node {} at source order {}; canonical sibling/root order remains spine/source order",
                    node.source_order,
                    current_source_order,
                    previous_navigation_order,
                    previous_source_order
                ),
            });
        }
        previous = Some((node.source_order, current_source_order));
    }
}

fn build_forest(
    sections: &[FlatSection],
    diagnostics: &mut Vec<EpubStructureDiagnostic>,
) -> Vec<Section> {
    let by_id = sections
        .iter()
        .enumerate()
        .map(|(index, section)| (section.section.id.clone(), index))
        .collect::<HashMap<_, _>>();
    let mut children = vec![Vec::<usize>::new(); sections.len()];
    let mut roots = Vec::new();

    for (index, flat) in sections.iter().enumerate() {
        match flat
            .section
            .parent_id
            .as_ref()
            .and_then(|id| by_id.get(id).copied())
        {
            Some(parent_index) if parent_index != index => children[parent_index].push(index),
            Some(_) => {
                diagnostics.push(EpubStructureDiagnostic {
                    code: "self_parent_section".into(),
                    message: format!(
                        "Section {} would become its own parent during reconciliation; it remains a root",
                        flat.section.id.0
                    ),
                });
                roots.push(index);
            }
            None => roots.push(index),
        }
    }

    for child_indexes in &mut children {
        child_indexes.sort_by_key(|index| sections[*index].source_order);
    }
    roots.sort_by_key(|index| sections[*index].source_order);

    roots
        .into_iter()
        .map(|index| build_section_tree(index, sections, &children))
        .collect()
}

fn build_section_tree(index: usize, sections: &[FlatSection], children: &[Vec<usize>]) -> Section {
    let mut section = sections[index].section.clone();
    section.children = children[index]
        .iter()
        .map(|child| build_section_tree(*child, sections, children))
        .collect();
    for child in &mut section.children {
        child.parent_id = Some(section.id.clone());
    }
    section
}

fn rewrite_canonical_paths(section: &mut Section, parent_path: &[String]) {
    let mut path = parent_path.to_vec();
    path.push(section.title.clone());
    section.location.section_path = path.clone();
    section.level = u8::try_from(path.len()).unwrap_or(u8::MAX);
    for child in &mut section.children {
        child.parent_id = Some(section.id.clone());
        rewrite_canonical_paths(child, &path);
    }
}

#[derive(Clone)]
struct FinalSectionFact {
    parent_id: Option<SectionId>,
    title: String,
    level: u8,
}

fn final_section_facts(root_sections: &[Section]) -> HashMap<SectionId, FinalSectionFact> {
    fn collect(section: &Section, output: &mut HashMap<SectionId, FinalSectionFact>) {
        output.insert(
            section.id.clone(),
            FinalSectionFact {
                parent_id: section.parent_id.clone(),
                title: section.title.clone(),
                level: section.level,
            },
        );
        for child in &section.children {
            collect(child, output);
        }
    }

    let mut output = HashMap::new();
    for section in root_sections {
        collect(section, &mut output);
    }
    output
}
