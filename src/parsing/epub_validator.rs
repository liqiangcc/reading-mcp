use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::domain::{
    Document, NormalizedBlockKind, Section, SectionId, SentenceEligibility, TextUnit,
};

use super::epub_navigation::{
    EPUB_NAVIGATION_MAP_VERSION, EpubNavigationMap, EpubNavigationNode, EpubNavigationProvenance,
    NavigationResolutionStatus,
};
use super::epub_structure::EPUB_STRUCTURE_MAP_VERSION;

pub const EPUB_VALIDATION_REPORT_VERSION: &str = "epub-structure-validator/v1";
pub const EPUB_VALIDATION_REPORT_METADATA_KEY: &str = "epub_validation_report";
pub const EPUB_VALIDATION_REPORT_VERSION_METADATA_KEY: &str = "epub_validation_report_version";
pub const EPUB_VALIDATION_INTEGRITY_METADATA_KEY: &str = "epub_validation_integrity";
pub const EPUB_VALIDATION_ERRORS_METADATA_KEY: &str = "epub_validation_errors";
pub const EPUB_VALIDATION_DEGRADATIONS_METADATA_KEY: &str = "epub_validation_degradations";

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EpubValidationSeverity {
    Error,
    Degradation,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EpubValidationIntegrity {
    Valid,
    Invalid,
}

impl EpubValidationIntegrity {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Valid => "valid",
            Self::Invalid => "invalid",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct EpubValidationFinding {
    pub severity: EpubValidationSeverity,
    pub plane: String,
    pub code: String,
    pub message: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct EpubPackageSpineCoverage {
    pub manifest_items_total: usize,
    pub spine_items_total: usize,
    pub spine_items_parsed: usize,
    pub spine_items_missing_manifest: usize,
    pub spine_items_unsupported_media: usize,
    pub linear_spine_items: usize,
    pub non_linear_spine_items: usize,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct EpubNavigationCoverage {
    pub nodes_total: usize,
    pub resolved_nodes: usize,
    pub resolved_document: usize,
    pub resolved_fragment: usize,
    pub missing_fragment: usize,
    pub missing_resource: usize,
    pub unsupported_resource: usize,
    pub invalid_path: usize,
    pub malformed_resource: usize,
    pub unlinked: usize,
    pub fragment_targets_total: usize,
    pub fragment_targets_resolved: usize,
    pub diagnostics: usize,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct EpubStructureCoverage {
    pub sections_total: usize,
    pub sections_epub_nav: usize,
    pub sections_epub_ncx: usize,
    pub sections_xhtml_heading: usize,
    pub sections_spine_item: usize,
    pub applied_navigation_nodes: usize,
    pub diagnostics: usize,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct EpubBlockCoverage {
    pub blocks_total: usize,
    pub paragraph_blocks: usize,
    pub blockquote_blocks: usize,
    pub list_item_blocks: usize,
    pub preformatted_blocks: usize,
    pub table_blocks: usize,
    pub sections_with_nonempty_content: usize,
    pub sections_with_blocks: usize,
    pub nonempty_sections_without_blocks: usize,
    pub section_content_chars: usize,
    pub block_chars: usize,
    pub separator_or_unmodeled_chars: usize,
    pub blocks_with_exact_paragraph_match: usize,
    pub blocks_without_exact_paragraph_match: usize,
    pub native_non_prose_blocks_with_sentence_units: usize,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct EpubTextUnitCoverage {
    pub paragraph_units: usize,
    pub paragraph_chars: usize,
    pub paragraph_separator_chars: usize,
    pub sentence_units: usize,
    pub sentence_chars: usize,
    pub sentence_separator_chars: usize,
    pub sentence_coarse_only_chars: usize,
    pub coarse_paragraphs: usize,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct EpubValidationCoverage {
    pub package_spine: EpubPackageSpineCoverage,
    pub navigation: EpubNavigationCoverage,
    pub structure: EpubStructureCoverage,
    pub blocks: EpubBlockCoverage,
    pub text_units: EpubTextUnitCoverage,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct EpubValidationReport {
    pub schema_version: String,
    pub integrity: EpubValidationIntegrity,
    pub error_count: usize,
    pub degradation_count: usize,
    pub findings: Vec<EpubValidationFinding>,
    pub coverage: EpubValidationCoverage,
}

impl EpubValidationReport {
    pub fn has_errors(&self) -> bool {
        self.error_count > 0
    }

    pub fn error_codes(&self) -> Vec<&str> {
        self.findings
            .iter()
            .filter(|finding| finding.severity == EpubValidationSeverity::Error)
            .map(|finding| finding.code.as_str())
            .collect()
    }
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum StoredSpineParseStatus {
    Parsed,
    MissingManifest,
    UnsupportedMedia,
}

#[derive(Clone, Debug, Deserialize)]
struct StoredSpineRecord {
    spine_index: usize,
    idref: String,
    linear: bool,
    manifest_href: Option<String>,
    resolved_entry_path: Option<String>,
    media_type: Option<String>,
    parse_status: StoredSpineParseStatus,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum StoredStructureProvenance {
    EpubNav,
    EpubNcx,
    XhtmlHeading,
    SpineItem,
}

#[derive(Clone, Debug, Deserialize)]
struct StoredStructureDiagnostic {
    code: String,
    message: String,
}

#[derive(Clone, Debug, Deserialize)]
struct StoredStructureSectionFact {
    section_id: String,
    source_order: usize,
    spine_index: usize,
    linear: bool,
    entry_path: String,
    provenance: StoredStructureProvenance,
    canonical_title: String,
    canonical_level: u8,
    canonical_parent_id: Option<String>,
    navigation_source_order: Option<usize>,
    navigation_resolution_status: Option<NavigationResolutionStatus>,
}

#[derive(Clone, Debug, Deserialize)]
struct StoredStructureMap {
    schema_version: String,
    navigation_provenance: Option<EpubNavigationProvenance>,
    navigation_nodes: usize,
    applied_navigation_nodes: usize,
    linear_spine_items: usize,
    non_linear_spine_items: usize,
    spine: Vec<StoredSpineRecord>,
    sections: Vec<StoredStructureSectionFact>,
    diagnostics: Vec<StoredStructureDiagnostic>,
}

pub fn validate_epub_document(document: &Document) -> EpubValidationReport {
    let mut findings = Vec::new();
    let mut coverage = EpubValidationCoverage::default();

    let navigation = decode_navigation_map(document, &mut findings);
    let structure = decode_structure_map(document, &mut findings);

    let canonical_sections = validate_canonical_sections(document, &mut findings);

    if let Some(navigation) = navigation.as_ref() {
        validate_navigation(document, navigation, &mut coverage.navigation, &mut findings);
    }

    if let Some(structure) = structure.as_ref() {
        validate_structure(
            document,
            structure,
            navigation.as_ref(),
            &canonical_sections,
            &mut coverage,
            &mut findings,
        );
    }

    validate_blocks_and_text_units(document, &mut coverage, &mut findings);

    let error_count = findings
        .iter()
        .filter(|finding| finding.severity == EpubValidationSeverity::Error)
        .count();
    let degradation_count = findings.len().saturating_sub(error_count);
    let integrity = if error_count == 0 {
        EpubValidationIntegrity::Valid
    } else {
        EpubValidationIntegrity::Invalid
    };

    EpubValidationReport {
        schema_version: EPUB_VALIDATION_REPORT_VERSION.into(),
        integrity,
        error_count,
        degradation_count,
        findings,
        coverage,
    }
}

pub(crate) fn attach_epub_validation_report(
    document: &mut Document,
) -> Result<EpubValidationReport, serde_json::Error> {
    let report = validate_epub_document(document);
    let json = serde_json::to_string(&report)?;
    document.metadata.insert(
        EPUB_VALIDATION_REPORT_VERSION_METADATA_KEY.into(),
        report.schema_version.clone(),
    );
    document.metadata.insert(
        EPUB_VALIDATION_INTEGRITY_METADATA_KEY.into(),
        report.integrity.as_str().into(),
    );
    document.metadata.insert(
        EPUB_VALIDATION_ERRORS_METADATA_KEY.into(),
        report.error_count.to_string(),
    );
    document.metadata.insert(
        EPUB_VALIDATION_DEGRADATIONS_METADATA_KEY.into(),
        report.degradation_count.to_string(),
    );
    document
        .metadata
        .insert(EPUB_VALIDATION_REPORT_METADATA_KEY.into(), json);
    Ok(report)
}

fn decode_navigation_map(
    document: &Document,
    findings: &mut Vec<EpubValidationFinding>,
) -> Option<EpubNavigationMap> {
    let Some(json) = document.metadata.get("epub_navigation_map") else {
        push_error(
            findings,
            "navigation",
            "missing_navigation_map",
            "persisted EPUB Document has no epub_navigation_map fact",
        );
        return None;
    };
    match serde_json::from_str::<EpubNavigationMap>(json) {
        Ok(map) => Some(map),
        Err(error) => {
            push_error(
                findings,
                "navigation",
                "invalid_navigation_map_json",
                format!("persisted epub_navigation_map cannot be decoded: {error}"),
            );
            None
        }
    }
}

fn decode_structure_map(
    document: &Document,
    findings: &mut Vec<EpubValidationFinding>,
) -> Option<StoredStructureMap> {
    let Some(json) = document.metadata.get("epub_structure_map") else {
        push_error(
            findings,
            "structure",
            "missing_structure_map",
            "persisted EPUB Document has no epub_structure_map fact",
        );
        return None;
    };
    match serde_json::from_str::<StoredStructureMap>(json) {
        Ok(map) => Some(map),
        Err(error) => {
            push_error(
                findings,
                "structure",
                "invalid_structure_map_json",
                format!("persisted epub_structure_map cannot be decoded: {error}"),
            );
            None
        }
    }
}

#[derive(Clone)]
struct CanonicalSectionFact {
    parent_id: Option<SectionId>,
    title: String,
    level: u8,
    native_location: Option<String>,
    chapter: Option<String>,
}

fn validate_canonical_sections(
    document: &Document,
    findings: &mut Vec<EpubValidationFinding>,
) -> HashMap<SectionId, CanonicalSectionFact> {
    let mut facts = HashMap::new();
    for root in &document.root_sections {
        collect_canonical_section(root, None, &mut facts, findings);
    }

    for (id, fact) in &facts {
        let mut seen = HashSet::new();
        let mut current = fact.parent_id.as_ref();
        while let Some(parent_id) = current {
            if !seen.insert(parent_id.clone()) {
                push_error(
                    findings,
                    "structure",
                    "section_parent_cycle",
                    format!("Section {:?} participates in a parent-reference cycle", id.0),
                );
                break;
            }
            let Some(parent) = facts.get(parent_id) else {
                push_error(
                    findings,
                    "structure",
                    "unknown_section_parent",
                    format!(
                        "Section {:?} references missing parent {:?}",
                        id.0, parent_id.0
                    ),
                );
                break;
            };
            current = parent.parent_id.as_ref();
        }
    }

    facts
}

fn collect_canonical_section(
    section: &Section,
    expected_parent: Option<&SectionId>,
    facts: &mut HashMap<SectionId, CanonicalSectionFact>,
    findings: &mut Vec<EpubValidationFinding>,
) {
    if facts.contains_key(&section.id) {
        push_error(
            findings,
            "structure",
            "duplicate_section_id",
            format!("canonical Section id {:?} occurs more than once", section.id.0),
        );
        return;
    }
    if section.parent_id.as_ref() != expected_parent {
        push_error(
            findings,
            "structure",
            "section_parent_pointer_mismatch",
            format!(
                "Section {:?} nested parent is {:?}, but parent_id is {:?}",
                section.id.0,
                expected_parent.map(|value| value.0.as_str()),
                section.parent_id.as_ref().map(|value| value.0.as_str())
            ),
        );
    }
    facts.insert(
        section.id.clone(),
        CanonicalSectionFact {
            parent_id: section.parent_id.clone(),
            title: section.title.clone(),
            level: section.level,
            native_location: section.location.native_location.clone(),
            chapter: section.location.chapter.clone(),
        },
    );
    for child in &section.children {
        collect_canonical_section(child, Some(&section.id), facts, findings);
    }
}

fn validate_navigation(
    document: &Document,
    map: &EpubNavigationMap,
    coverage: &mut EpubNavigationCoverage,
    findings: &mut Vec<EpubValidationFinding>,
) {
    if map.schema_version != EPUB_NAVIGATION_MAP_VERSION {
        push_error(
            findings,
            "navigation",
            "navigation_map_version_mismatch",
            format!(
                "navigation map version {:?} does not match {:?}",
                map.schema_version, EPUB_NAVIGATION_MAP_VERSION
            ),
        );
    }

    if document
        .metadata
        .get("epub_navigation_map_version")
        .is_some_and(|value| value != &map.schema_version)
    {
        push_error(
            findings,
            "navigation",
            "navigation_summary_version_mismatch",
            "epub_navigation_map_version summary disagrees with persisted map",
        );
    }

    let mut flat = Vec::new();
    flatten_navigation_nodes(&map.nodes, 1, &mut flat, findings);
    coverage.nodes_total = flat.len();
    coverage.diagnostics = map.diagnostics.len();

    for (expected_order, node) in flat.iter().enumerate() {
        if node.source_order != expected_order {
            push_error(
                findings,
                "navigation",
                "navigation_source_order_gap",
                format!(
                    "navigation node has source_order {}, expected {}",
                    node.source_order, expected_order
                ),
            );
        }
        if node.provenance != map.provenance.unwrap_or(node.provenance) {
            push_error(
                findings,
                "navigation",
                "navigation_provenance_mismatch",
                format!(
                    "navigation node {} provenance does not match map provenance",
                    node.source_order
                ),
            );
        }
        if node.fragment.is_some() {
            coverage.fragment_targets_total += 1;
        }
        match node.resolution_status {
            NavigationResolutionStatus::ResolvedDocument => {
                coverage.resolved_nodes += 1;
                coverage.resolved_document += 1;
                if node.resolved_entry_path.is_none() {
                    push_error(
                        findings,
                        "navigation",
                        "resolved_document_without_path",
                        format!(
                            "navigation node {} claims resolved_document without a resolved path",
                            node.source_order
                        ),
                    );
                }
            }
            NavigationResolutionStatus::ResolvedFragment => {
                coverage.resolved_nodes += 1;
                coverage.resolved_fragment += 1;
                coverage.fragment_targets_resolved += 1;
                if node.resolved_entry_path.is_none() || node.fragment.is_none() {
                    push_error(
                        findings,
                        "navigation",
                        "resolved_fragment_without_evidence",
                        format!(
                            "navigation node {} claims resolved_fragment without path+fragment evidence",
                            node.source_order
                        ),
                    );
                }
            }
            NavigationResolutionStatus::MissingFragment => {
                coverage.missing_fragment += 1;
                push_degradation(
                    findings,
                    "navigation",
                    "navigation_target_missing_fragment",
                    format!("navigation node {} references a missing fragment", node.source_order),
                );
            }
            NavigationResolutionStatus::MissingResource => {
                coverage.missing_resource += 1;
                push_degradation(
                    findings,
                    "navigation",
                    "navigation_target_missing_resource",
                    format!("navigation node {} references a missing resource", node.source_order),
                );
            }
            NavigationResolutionStatus::UnsupportedResource => {
                coverage.unsupported_resource += 1;
                push_degradation(
                    findings,
                    "navigation",
                    "navigation_target_unsupported_resource",
                    format!("navigation node {} targets unsupported media", node.source_order),
                );
            }
            NavigationResolutionStatus::InvalidPath => {
                coverage.invalid_path += 1;
                push_degradation(
                    findings,
                    "navigation",
                    "navigation_target_invalid_path",
                    format!("navigation node {} has an invalid archive path", node.source_order),
                );
            }
            NavigationResolutionStatus::MalformedResource => {
                coverage.malformed_resource += 1;
                push_degradation(
                    findings,
                    "navigation",
                    "navigation_target_malformed_resource",
                    format!("navigation node {} targets malformed content", node.source_order),
                );
            }
            NavigationResolutionStatus::Unlinked => {
                coverage.unlinked += 1;
                push_degradation(
                    findings,
                    "navigation",
                    "navigation_node_unlinked",
                    format!("navigation node {} has no target href", node.source_order),
                );
            }
        }
    }

    for diagnostic in &map.diagnostics {
        push_degradation(
            findings,
            "navigation",
            diagnostic.code.clone(),
            diagnostic.message.clone(),
        );
    }

    check_summary_count(
        document,
        "epub_navigation_nodes",
        coverage.nodes_total,
        "navigation",
        findings,
    );
    check_summary_count(
        document,
        "epub_navigation_resolved_nodes",
        coverage.resolved_nodes,
        "navigation",
        findings,
    );
    check_summary_count(
        document,
        "epub_navigation_diagnostics",
        coverage.diagnostics,
        "navigation",
        findings,
    );
}

fn flatten_navigation_nodes<'a>(
    nodes: &'a [EpubNavigationNode],
    expected_depth: usize,
    output: &mut Vec<&'a EpubNavigationNode>,
    findings: &mut Vec<EpubValidationFinding>,
) {
    for node in nodes {
        if node.depth != expected_depth {
            push_error(
                findings,
                "navigation",
                "navigation_depth_mismatch",
                format!(
                    "navigation node {} depth is {}, expected {}",
                    node.source_order, node.depth, expected_depth
                ),
            );
        }
        output.push(node);
        flatten_navigation_nodes(&node.children, expected_depth + 1, output, findings);
    }
}

fn validate_structure(
    document: &Document,
    map: &StoredStructureMap,
    navigation: Option<&EpubNavigationMap>,
    canonical_sections: &HashMap<SectionId, CanonicalSectionFact>,
    coverage: &mut EpubValidationCoverage,
    findings: &mut Vec<EpubValidationFinding>,
) {
    if map.schema_version != EPUB_STRUCTURE_MAP_VERSION {
        push_error(
            findings,
            "structure",
            "structure_map_version_mismatch",
            format!(
                "structure map version {:?} does not match {:?}",
                map.schema_version, EPUB_STRUCTURE_MAP_VERSION
            ),
        );
    }

    coverage.package_spine.manifest_items_total = metadata_usize(
        document,
        "epub_manifest_items",
        "package",
        findings,
    )
    .unwrap_or_default();
    coverage.package_spine.spine_items_total = map.spine.len();
    coverage.package_spine.linear_spine_items = map.spine.iter().filter(|item| item.linear).count();
    coverage.package_spine.non_linear_spine_items = map.spine.len().saturating_sub(
        coverage.package_spine.linear_spine_items,
    );

    for (offset, spine) in map.spine.iter().enumerate() {
        let expected_index = offset + 1;
        if spine.spine_index != expected_index {
            push_error(
                findings,
                "spine",
                "spine_index_gap",
                format!(
                    "spine record {:?} has index {}, expected {}",
                    spine.idref, spine.spine_index, expected_index
                ),
            );
        }
        match spine.parse_status {
            StoredSpineParseStatus::Parsed => {
                coverage.package_spine.spine_items_parsed += 1;
                if spine.manifest_href.is_none()
                    || spine.resolved_entry_path.is_none()
                    || spine.media_type.is_none()
                {
                    push_error(
                        findings,
                        "spine",
                        "parsed_spine_missing_resolution_facts",
                        format!(
                            "parsed spine item {:?} is missing manifest/path/media evidence",
                            spine.idref
                        ),
                    );
                }
            }
            StoredSpineParseStatus::MissingManifest => {
                coverage.package_spine.spine_items_missing_manifest += 1;
                push_degradation(
                    findings,
                    "spine",
                    "spine_missing_manifest_item",
                    format!("spine idref {:?} does not resolve in the manifest", spine.idref),
                );
            }
            StoredSpineParseStatus::UnsupportedMedia => {
                coverage.package_spine.spine_items_unsupported_media += 1;
                push_degradation(
                    findings,
                    "spine",
                    "spine_unsupported_media",
                    format!("spine idref {:?} uses unsupported top-level media", spine.idref),
                );
            }
        }
    }

    if map.linear_spine_items != coverage.package_spine.linear_spine_items
        || map.non_linear_spine_items != coverage.package_spine.non_linear_spine_items
    {
        push_error(
            findings,
            "spine",
            "spine_linearity_summary_mismatch",
            "structure-map linear/non-linear summary disagrees with spine rows",
        );
    }

    check_summary_count(
        document,
        "epub_spine_items_total",
        coverage.package_spine.spine_items_total,
        "spine",
        findings,
    );
    check_summary_count(
        document,
        "epub_spine_items",
        coverage.package_spine.spine_items_parsed,
        "spine",
        findings,
    );
    check_summary_count(
        document,
        "epub_linear_spine_items",
        coverage.package_spine.linear_spine_items,
        "spine",
        findings,
    );
    check_summary_count(
        document,
        "epub_non_linear_spine_items",
        coverage.package_spine.non_linear_spine_items,
        "spine",
        findings,
    );

    coverage.structure.sections_total = map.sections.len();
    coverage.structure.applied_navigation_nodes = map.applied_navigation_nodes;
    coverage.structure.diagnostics = map.diagnostics.len();

    if map.sections.len() != canonical_sections.len() {
        push_error(
            findings,
            "structure",
            "structure_section_count_mismatch",
            format!(
                "structure map has {} Sections, canonical Document has {}",
                map.sections.len(),
                canonical_sections.len()
            ),
        );
    }
    if map.navigation_nodes != coverage.navigation.nodes_total {
        push_error(
            findings,
            "structure",
            "structure_navigation_count_mismatch",
            format!(
                "structure map records {} navigation nodes, navigation map has {}",
                map.navigation_nodes, coverage.navigation.nodes_total
            ),
        );
    }
    if let Some(navigation) = navigation
        && map.navigation_provenance != navigation.provenance
    {
        push_error(
            findings,
            "structure",
            "structure_navigation_provenance_mismatch",
            "structure map navigation provenance disagrees with navigation map",
        );
    }

    let navigation_by_order = navigation
        .map(|map| {
            let mut flat = Vec::new();
            flatten_navigation_for_lookup(&map.nodes, &mut flat);
            flat.into_iter()
                .map(|node| (node.source_order, node))
                .collect::<HashMap<_, _>>()
        })
        .unwrap_or_default();

    let mut fact_by_id = HashMap::new();
    let mut previous_spine = 0usize;
    for (expected_order, fact) in map.sections.iter().enumerate() {
        if fact.source_order != expected_order {
            push_error(
                findings,
                "structure",
                "structure_source_order_gap",
                format!(
                    "Section fact {:?} has source_order {}, expected {}",
                    fact.section_id, fact.source_order, expected_order
                ),
            );
        }
        if fact.spine_index < previous_spine {
            push_error(
                findings,
                "structure",
                "structure_spine_order_regression",
                format!(
                    "Section fact {:?} regresses from spine {} to {}",
                    fact.section_id, previous_spine, fact.spine_index
                ),
            );
        }
        previous_spine = fact.spine_index;
        if fact_by_id.insert(fact.section_id.clone(), fact).is_some() {
            push_error(
                findings,
                "structure",
                "duplicate_structure_section_fact",
                format!("structure map repeats Section fact {:?}", fact.section_id),
            );
        }

        let Some(section) = canonical_sections.get(&SectionId(fact.section_id.clone())) else {
            push_error(
                findings,
                "structure",
                "structure_fact_unknown_section",
                format!("structure map references missing Section {:?}", fact.section_id),
            );
            continue;
        };

        if fact.canonical_title != section.title
            || fact.canonical_level != section.level
            || fact.canonical_parent_id.as_deref()
                != section.parent_id.as_ref().map(|value| value.0.as_str())
        {
            push_error(
                findings,
                "structure",
                "structure_fact_canonical_mismatch",
                format!(
                    "structure fact for {:?} disagrees with canonical title/level/parent",
                    fact.section_id
                ),
            );
        }

        if section.chapter.as_deref() != Some(format!("spine-{}", fact.spine_index).as_str()) {
            push_error(
                findings,
                "structure",
                "section_spine_provenance_mismatch",
                format!(
                    "Section {:?} chapter provenance does not match spine index {}",
                    fact.section_id, fact.spine_index
                ),
            );
        }
        if !section
            .native_location
            .as_deref()
            .is_some_and(|value| value.starts_with(&format!("epub:{}", fact.entry_path)))
        {
            push_error(
                findings,
                "structure",
                "section_native_location_mismatch",
                format!(
                    "Section {:?} native location does not match entry {:?}",
                    fact.section_id, fact.entry_path
                ),
            );
        }

        match fact.provenance {
            StoredStructureProvenance::EpubNav => coverage.structure.sections_epub_nav += 1,
            StoredStructureProvenance::EpubNcx => coverage.structure.sections_epub_ncx += 1,
            StoredStructureProvenance::XhtmlHeading => {
                coverage.structure.sections_xhtml_heading += 1
            }
            StoredStructureProvenance::SpineItem => coverage.structure.sections_spine_item += 1,
        }

        match fact.provenance {
            StoredStructureProvenance::EpubNav | StoredStructureProvenance::EpubNcx => {
                let Some(order) = fact.navigation_source_order else {
                    push_error(
                        findings,
                        "structure",
                        "navigation_provenance_without_source_order",
                        format!(
                            "Section {:?} has navigation provenance without navigation source_order",
                            fact.section_id
                        ),
                    );
                    continue;
                };
                let Some(node) = navigation_by_order.get(&order) else {
                    push_error(
                        findings,
                        "structure",
                        "navigation_section_fact_missing_node",
                        format!(
                            "Section {:?} references missing navigation node {}",
                            fact.section_id, order
                        ),
                    );
                    continue;
                };
                let expected_provenance = match fact.provenance {
                    StoredStructureProvenance::EpubNav => EpubNavigationProvenance::EpubNav,
                    StoredStructureProvenance::EpubNcx => EpubNavigationProvenance::EpubNcx,
                    _ => unreachable!(),
                };
                if node.provenance != expected_provenance
                    || fact.navigation_resolution_status != Some(node.resolution_status)
                {
                    push_error(
                        findings,
                        "structure",
                        "navigation_section_fact_mismatch",
                        format!(
                            "Section {:?} navigation provenance/status disagrees with node {}",
                            fact.section_id, order
                        ),
                    );
                }
            }
            StoredStructureProvenance::XhtmlHeading | StoredStructureProvenance::SpineItem => {
                if fact.navigation_source_order.is_some() {
                    push_error(
                        findings,
                        "structure",
                        "fallback_section_has_navigation_owner",
                        format!(
                            "fallback Section {:?} unexpectedly claims navigation source_order",
                            fact.section_id
                        ),
                    );
                }
            }
        }
    }

    let applied_from_sections = coverage.structure.sections_epub_nav + coverage.structure.sections_epub_ncx;
    if applied_from_sections != map.applied_navigation_nodes {
        push_error(
            findings,
            "structure",
            "applied_navigation_summary_mismatch",
            format!(
                "structure map says {} applied navigation nodes but {} Sections carry navigation provenance",
                map.applied_navigation_nodes, applied_from_sections
            ),
        );
    }

    validate_canonical_sibling_source_order(
        &document.root_sections,
        &fact_by_id,
        findings,
    );

    for diagnostic in &map.diagnostics {
        push_degradation(
            findings,
            "structure",
            diagnostic.code.clone(),
            diagnostic.message.clone(),
        );
    }

    check_summary_count(
        document,
        "epub_structure_sections",
        coverage.structure.sections_total,
        "structure",
        findings,
    );
    check_summary_count(
        document,
        "epub_structure_applied_navigation_nodes",
        coverage.structure.applied_navigation_nodes,
        "structure",
        findings,
    );
    check_summary_count(
        document,
        "epub_structure_diagnostics",
        coverage.structure.diagnostics,
        "structure",
        findings,
    );
}

fn flatten_navigation_for_lookup<'a>(
    nodes: &'a [EpubNavigationNode],
    output: &mut Vec<&'a EpubNavigationNode>,
) {
    for node in nodes {
        output.push(node);
        flatten_navigation_for_lookup(&node.children, output);
    }
}

fn validate_canonical_sibling_source_order(
    siblings: &[Section],
    fact_by_id: &HashMap<String, &StoredStructureSectionFact>,
    findings: &mut Vec<EpubValidationFinding>,
) {
    let mut previous = None;
    for section in siblings {
        if let Some(fact) = fact_by_id.get(&section.id.0) {
            if let Some(previous_order) = previous
                && fact.source_order <= previous_order
            {
                push_error(
                    findings,
                    "structure",
                    "canonical_sibling_source_order_regression",
                    format!(
                        "canonical sibling Section {:?} has source_order {} after {}",
                        section.id.0, fact.source_order, previous_order
                    ),
                );
            }
            if let Some(parent_id) = section.parent_id.as_ref()
                && let Some(parent_fact) = fact_by_id.get(&parent_id.0)
                && parent_fact.source_order >= fact.source_order
            {
                push_error(
                    findings,
                    "structure",
                    "canonical_parent_not_before_child",
                    format!(
                        "canonical parent {:?} source_order {} is not before child {:?} at {}",
                        parent_id.0, parent_fact.source_order, section.id.0, fact.source_order
                    ),
                );
            }
            previous = Some(fact.source_order);
        }
        validate_canonical_sibling_source_order(&section.children, fact_by_id, findings);
    }
}

fn validate_blocks_and_text_units(
    document: &Document,
    coverage: &mut EpubValidationCoverage,
    findings: &mut Vec<EpubValidationFinding>,
) {
    let blocks = match document.normalized_block_map() {
        Ok(Some(map)) => map,
        Ok(None) => {
            push_error(
                findings,
                "blocks",
                "missing_normalized_block_map",
                "persisted EPUB Document has no normalized_block_map fact",
            );
            return;
        }
        Err(error) => {
            push_error(
                findings,
                "blocks",
                "invalid_normalized_block_map",
                error.to_string(),
            );
            return;
        }
    };

    check_summary_count(
        document,
        "normalized_blocks",
        blocks.blocks.len(),
        "blocks",
        findings,
    );

    let mut blocks_per_section = HashMap::<SectionId, usize>::new();
    for block in &blocks.blocks {
        coverage.blocks.blocks_total += 1;
        coverage.blocks.block_chars += block.normalized_range.len();
        *blocks_per_section
            .entry(block.owner_section_id.clone())
            .or_default() += 1;
        match block.kind {
            NormalizedBlockKind::Paragraph => coverage.blocks.paragraph_blocks += 1,
            NormalizedBlockKind::BlockQuote => coverage.blocks.blockquote_blocks += 1,
            NormalizedBlockKind::ListItem => coverage.blocks.list_item_blocks += 1,
            NormalizedBlockKind::Preformatted => coverage.blocks.preformatted_blocks += 1,
            NormalizedBlockKind::Table => coverage.blocks.table_blocks += 1,
        }
        if !block
            .native_location
            .as_deref()
            .is_some_and(|value| value.starts_with("epub:"))
        {
            push_error(
                findings,
                "blocks",
                "epub_block_native_location_mismatch",
                format!(
                    "normalized block {} in Section {:?} lacks EPUB-native location provenance",
                    block.block_index, block.owner_section_id.0
                ),
            );
        }
    }

    for section in all_sections(document) {
        let chars = section.content.chars().count();
        coverage.blocks.section_content_chars += chars;
        if chars > 0 {
            coverage.blocks.sections_with_nonempty_content += 1;
            if blocks_per_section.contains_key(&section.id) {
                coverage.blocks.sections_with_blocks += 1;
            } else {
                coverage.blocks.nonempty_sections_without_blocks += 1;
                push_degradation(
                    findings,
                    "blocks",
                    "nonempty_section_without_normalized_blocks",
                    format!(
                        "Section {:?} has {} normalized characters but no preserved native body block",
                        section.id.0, chars
                    ),
                );
            }
        }
    }
    coverage.blocks.separator_or_unmodeled_chars = coverage
        .blocks
        .section_content_chars
        .saturating_sub(coverage.blocks.block_chars);

    let paragraph_set = document.paragraph_text_units();
    coverage.text_units.paragraph_units = paragraph_set.units.len();
    let mut paragraph_by_id = HashMap::new();
    let mut paragraph_ranges = HashSet::new();
    let mut expected_paragraph_index = HashMap::<SectionId, usize>::new();
    for (expected_source_order, paragraph) in paragraph_set.units.iter().enumerate() {
        if paragraph.source_order != expected_source_order {
            push_error(
                findings,
                "text_units",
                "paragraph_source_order_gap",
                format!(
                    "Paragraph {} has source_order {}, expected {}",
                    paragraph.id.0, paragraph.source_order, expected_source_order
                ),
            );
        }
        let Some(owner) = document.find_section(&paragraph.owner_section_id) else {
            push_error(
                findings,
                "text_units",
                "paragraph_unknown_owner",
                format!("Paragraph {} has missing owner Section", paragraph.id.0),
            );
            continue;
        };
        match owner.normalized_text_slice(paragraph.normalized_range) {
            Ok(slice) if slice == paragraph.text => {}
            _ => push_error(
                findings,
                "text_units",
                "paragraph_exact_slice_mismatch",
                format!("Paragraph {} is not the exact persisted owner slice", paragraph.id.0),
            ),
        }
        let expected = expected_paragraph_index
            .entry(paragraph.owner_section_id.clone())
            .or_insert(1);
        if paragraph.paragraph_index != *expected {
            push_error(
                findings,
                "text_units",
                "paragraph_index_gap",
                format!(
                    "Paragraph {} has index {}, expected {} in owner {:?}",
                    paragraph.id.0,
                    paragraph.paragraph_index,
                    *expected,
                    paragraph.owner_section_id.0
                ),
            );
        }
        *expected += 1;
        coverage.text_units.paragraph_chars += paragraph.normalized_range.len();
        paragraph_ranges.insert((
            paragraph.owner_section_id.clone(),
            paragraph.normalized_range.start(),
            paragraph.normalized_range.end(),
        ));
        paragraph_by_id.insert(paragraph.id.clone(), paragraph);
    }

    for section_coverage in &paragraph_set.coverage {
        let Some(section) = document.find_section(&section_coverage.owner_section_id) else {
            push_error(
                findings,
                "text_units",
                "paragraph_coverage_unknown_owner",
                format!(
                    "Paragraph coverage references missing Section {:?}",
                    section_coverage.owner_section_id.0
                ),
            );
            continue;
        };
        if section_coverage.owner_chars != section.normalized_text_len()
            || section_coverage.paragraph_chars + section_coverage.separator_chars
                != section_coverage.owner_chars
        {
            push_error(
                findings,
                "text_units",
                "paragraph_coverage_invariant_failed",
                format!(
                    "Paragraph coverage for Section {:?} does not partition owner text",
                    section_coverage.owner_section_id.0
                ),
            );
        }
        coverage.text_units.paragraph_separator_chars += section_coverage.separator_chars;
    }

    for block in &blocks.blocks {
        if paragraph_ranges.contains(&(
            block.owner_section_id.clone(),
            block.normalized_range.start(),
            block.normalized_range.end(),
        )) {
            coverage.blocks.blocks_with_exact_paragraph_match += 1;
        } else {
            coverage.blocks.blocks_without_exact_paragraph_match += 1;
        }
    }
    if coverage.blocks.blocks_without_exact_paragraph_match > 0 {
        push_degradation(
            findings,
            "text_units",
            "native_blocks_not_exact_current_paragraphs",
            format!(
                "{} normalized native blocks do not equal one current text-segmentation/v1 Paragraph range",
                coverage.blocks.blocks_without_exact_paragraph_match
            ),
        );
    }

    let sentence_set = document.sentence_text_units();
    coverage.text_units.sentence_units = sentence_set.units.len();
    let mut expected_sentence_index = HashMap::new();
    for (expected_source_order, sentence) in sentence_set.units.iter().enumerate() {
        if sentence.source_order != expected_source_order {
            push_error(
                findings,
                "text_units",
                "sentence_source_order_gap",
                format!(
                    "Sentence {} has source_order {}, expected {}",
                    sentence.id.0, sentence.source_order, expected_source_order
                ),
            );
        }
        let Some(owner) = document.find_section(&sentence.owner_section_id) else {
            push_error(
                findings,
                "text_units",
                "sentence_unknown_owner",
                format!("Sentence {} has missing owner Section", sentence.id.0),
            );
            continue;
        };
        match owner.normalized_text_slice(sentence.normalized_range) {
            Ok(slice) if slice == sentence.text => {}
            _ => push_error(
                findings,
                "text_units",
                "sentence_exact_slice_mismatch",
                format!("Sentence {} is not the exact persisted owner slice", sentence.id.0),
            ),
        }
        let Some(paragraph) = paragraph_by_id.get(&sentence.parent_paragraph_id) else {
            push_error(
                findings,
                "text_units",
                "sentence_missing_parent_paragraph",
                format!("Sentence {} parent Paragraph is missing", sentence.id.0),
            );
            continue;
        };
        if paragraph.owner_section_id != sentence.owner_section_id
            || sentence.normalized_range.start() < paragraph.normalized_range.start()
            || sentence.normalized_range.end() > paragraph.normalized_range.end()
        {
            push_error(
                findings,
                "text_units",
                "sentence_outside_parent_paragraph",
                format!("Sentence {} is not contained by its parent Paragraph", sentence.id.0),
            );
        }
        let expected = expected_sentence_index
            .entry(sentence.parent_paragraph_id.clone())
            .or_insert(1usize);
        if sentence.sentence_index != *expected {
            push_error(
                findings,
                "text_units",
                "sentence_index_gap",
                format!(
                    "Sentence {} has index {}, expected {}",
                    sentence.id.0, sentence.sentence_index, *expected
                ),
            );
        }
        *expected += 1;
        coverage.text_units.sentence_chars += sentence.normalized_range.len();
    }

    for sentence_coverage in &sentence_set.coverage {
        if sentence_coverage.sentence_chars
            + sentence_coverage.separator_chars
            + sentence_coverage.coarse_only_chars
            != sentence_coverage.paragraph_chars
        {
            push_error(
                findings,
                "text_units",
                "sentence_coverage_invariant_failed",
                format!(
                    "Sentence coverage for Paragraph {} does not partition its current Paragraph text",
                    sentence_coverage.paragraph_id.0
                ),
            );
        }
        coverage.text_units.sentence_separator_chars += sentence_coverage.separator_chars;
        coverage.text_units.sentence_coarse_only_chars += sentence_coverage.coarse_only_chars;
        if sentence_coverage.eligibility == SentenceEligibility::CoarseParagraphOnly {
            coverage.text_units.coarse_paragraphs += 1;
        }
    }

    for block in blocks
        .blocks
        .iter()
        .filter(|block| matches!(block.kind, NormalizedBlockKind::Preformatted | NormalizedBlockKind::Table))
    {
        if sentence_set.units.iter().any(|sentence| {
            sentence.owner_section_id == block.owner_section_id
                && ranges_overlap(
                    block.normalized_range.start(),
                    block.normalized_range.end(),
                    sentence.normalized_range.start(),
                    sentence.normalized_range.end(),
                )
        }) {
            coverage.blocks.native_non_prose_blocks_with_sentence_units += 1;
        }
    }
    if coverage.blocks.native_non_prose_blocks_with_sentence_units > 0 {
        push_degradation(
            findings,
            "text_units",
            "current_sentences_overlap_native_non_prose_blocks",
            format!(
                "{} native pre/table blocks currently contain text-segmentation/v1 Sentence units; block-aware eligibility is not yet identity-bearing",
                coverage.blocks.native_non_prose_blocks_with_sentence_units
            ),
        );
    }
}

fn all_sections(document: &Document) -> Vec<&Section> {
    fn collect<'a>(section: &'a Section, output: &mut Vec<&'a Section>) {
        output.push(section);
        for child in &section.children {
            collect(child, output);
        }
    }
    let mut sections = Vec::new();
    for section in &document.root_sections {
        collect(section, &mut sections);
    }
    sections
}

fn ranges_overlap(first_start: usize, first_end: usize, second_start: usize, second_end: usize) -> bool {
    first_start < second_end && second_start < first_end
}

fn metadata_usize(
    document: &Document,
    key: &str,
    plane: &str,
    findings: &mut Vec<EpubValidationFinding>,
) -> Option<usize> {
    let Some(value) = document.metadata.get(key) else {
        push_error(
            findings,
            plane,
            "missing_summary_metadata",
            format!("missing required EPUB summary metadata key {key:?}"),
        );
        return None;
    };
    match value.parse::<usize>() {
        Ok(value) => Some(value),
        Err(error) => {
            push_error(
                findings,
                plane,
                "invalid_summary_metadata",
                format!("EPUB summary metadata {key:?} is not an integer: {error}"),
            );
            None
        }
    }
}

fn check_summary_count(
    document: &Document,
    key: &str,
    expected: usize,
    plane: &str,
    findings: &mut Vec<EpubValidationFinding>,
) {
    if let Some(actual) = metadata_usize(document, key, plane, findings)
        && actual != expected
    {
        push_error(
            findings,
            plane,
            "summary_count_mismatch",
            format!(
                "EPUB summary metadata {key:?} is {actual}, expected {expected} from persisted facts"
            ),
        );
    }
}

fn push_error(
    findings: &mut Vec<EpubValidationFinding>,
    plane: impl Into<String>,
    code: impl Into<String>,
    message: impl Into<String>,
) {
    findings.push(EpubValidationFinding {
        severity: EpubValidationSeverity::Error,
        plane: plane.into(),
        code: code.into(),
        message: message.into(),
    });
}

fn push_degradation(
    findings: &mut Vec<EpubValidationFinding>,
    plane: impl Into<String>,
    code: impl Into<String>,
    message: impl Into<String>,
) {
    findings.push(EpubValidationFinding {
        severity: EpubValidationSeverity::Degradation,
        plane: plane.into(),
        code: code.into(),
        message: message.into(),
    });
}
