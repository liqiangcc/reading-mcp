use std::collections::HashMap;
use std::sync::Arc;

use crate::application::body_order::{BODY_ORDER_VERSION, section_body_order};
use crate::application::ports::{ApplicationError, DocumentRepository};
use crate::application::structure_cursor::{
    STRUCTURE_TRAVERSAL_VERSION, StructureCursorClaims, decode_structure_cursor,
    encode_structure_cursor,
};
use crate::domain::{
    ContentHash, Document, DocumentId, Location, NORMALIZATION_VERSION,
    NORMALIZED_DOCUMENT_HASH_VERSION, NormalizedDocumentHash, Section, SectionId,
    TEXT_SEGMENTATION_VERSION, TextLocator,
};

pub const DEFAULT_STRUCTURE_MAX_NODES: usize = 1_000;
pub const MAX_STRUCTURE_RESPONSE_NODES: usize = 1_000;
pub const NAMED_SECTION_RESOLUTION_VERSION: &str = "named-section-resolution/v1";
pub const NAMED_SECTION_BOUNDARY_VERSION: &str = "named-section-boundary/v1";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GetDocumentStructureCommand {
    pub document_id: DocumentId,
    pub root_section_id: Option<SectionId>,
    pub max_depth: Option<u8>,
    pub max_nodes: Option<usize>,
    pub cursor: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SectionOutline {
    pub section_id: SectionId,
    pub parent_id: Option<SectionId>,
    pub title: String,
    pub level: u8,
    pub location: Location,
    pub body_order: usize,
    pub children_complete: bool,
    pub children: Vec<SectionOutline>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NamedSectionResolutionStatus {
    Resolved,
    Ambiguous,
    NotFound,
    Unavailable,
    BoundaryUnavailable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NamedSectionMatchKind {
    ExactTitle,
    SectionPrefixedTitle,
    TitleOnly,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NamedSectionCandidate {
    pub section_id: SectionId,
    pub parent_id: Option<SectionId>,
    pub title: String,
    pub level: u8,
    pub location: Location,
    pub body_order: usize,
    pub start_locator: TextLocator,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BodyOrderInterval {
    pub start: usize,
    pub end: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NamedSectionBoundary {
    pub version: String,
    pub body_order_version: String,
    pub intervals: Vec<BodyOrderInterval>,
    pub end_exclusive: Option<NamedSectionCandidate>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NamedSectionResolution {
    pub version: String,
    pub status: NamedSectionResolutionStatus,
    pub query: String,
    pub match_kind: Option<NamedSectionMatchKind>,
    pub matched: Option<NamedSectionCandidate>,
    pub candidates: Vec<NamedSectionCandidate>,
    pub boundary: Option<NamedSectionBoundary>,
    pub degradation: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolveNamedSectionCommand {
    pub document_id: DocumentId,
    pub query: String,
    pub expected_content_hash: String,
    pub expected_normalized_document_hash: String,
    pub expected_structure_resolution_version: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolveNamedSectionResult {
    pub document_id: DocumentId,
    pub content_hash: ContentHash,
    pub normalized_document_hash: NormalizedDocumentHash,
    pub normalized_document_hash_version: String,
    pub normalization_version: String,
    pub segmentation_version: String,
    pub resolution: NamedSectionResolution,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StructureStreamSegment {
    pub traversal_version: String,
    pub body_order_version: String,
    pub root_section_id: Option<SectionId>,
    pub max_depth: Option<u8>,
    pub start_index: usize,
    pub end_index: usize,
    pub total_nodes: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DocumentStructureResult {
    pub document_id: DocumentId,
    pub title: String,
    pub content_hash: ContentHash,
    pub normalized_document_hash: NormalizedDocumentHash,
    pub normalized_document_hash_version: String,
    pub normalization_version: String,
    pub segmentation_version: String,
    pub sections: Vec<SectionOutline>,
    pub truncated: bool,
    pub complete: bool,
    pub next_cursor: Option<String>,
    pub stream: StructureStreamSegment,
}

pub struct GetDocumentStructureUseCase {
    repository: Arc<dyn DocumentRepository>,
}

impl GetDocumentStructureUseCase {
    pub fn new(repository: Arc<dyn DocumentRepository>) -> Self {
        Self { repository }
    }

    /// Backward-compatible application entrypoint for callers that only need the historical
    /// whole-document/max-depth first page.
    pub async fn execute(
        &self,
        document_id: DocumentId,
        max_depth: Option<u8>,
    ) -> Result<DocumentStructureResult, ApplicationError> {
        self.execute_command(GetDocumentStructureCommand {
            document_id,
            root_section_id: None,
            max_depth,
            max_nodes: None,
            cursor: None,
        })
        .await
    }

    pub async fn resolve_named_section(
        &self,
        command: ResolveNamedSectionCommand,
    ) -> Result<ResolveNamedSectionResult, ApplicationError> {
        let document = self
            .repository
            .get(&command.document_id)
            .await?
            .ok_or(ApplicationError::DocumentNotFound)?;
        let normalized_hash = document.normalized_document_hash();
        let body_order = section_body_order(&document)?;
        let resolution = resolve_named_section(&document, &body_order, &command, &normalized_hash)?;
        Ok(ResolveNamedSectionResult {
            document_id: document.id.clone(),
            content_hash: document.content_hash.clone(),
            normalized_document_hash: normalized_hash,
            normalized_document_hash_version: NORMALIZED_DOCUMENT_HASH_VERSION.into(),
            normalization_version: NORMALIZATION_VERSION.into(),
            segmentation_version: TEXT_SEGMENTATION_VERSION.into(),
            resolution,
        })
    }

    pub async fn execute_command(
        &self,
        command: GetDocumentStructureCommand,
    ) -> Result<DocumentStructureResult, ApplicationError> {
        let max_nodes = effective_max_nodes(command.max_nodes)?;
        let cursor_claims = command
            .cursor
            .as_deref()
            .map(decode_structure_cursor)
            .transpose()?;
        let scope = resolve_scope(&command, cursor_claims.as_ref())?;

        let document = self
            .repository
            .get(&command.document_id)
            .await?
            .ok_or(ApplicationError::DocumentNotFound)?;
        let normalized_hash = document.normalized_document_hash();
        let body_order = section_body_order(&document)?;

        if let Some(claims) = &cursor_claims {
            if claims.content_hash != document.content_hash.0 {
                return Err(ApplicationError::StaleCursor(format!(
                    "raw content hash changed from {} to {}",
                    claims.content_hash, document.content_hash.0
                )));
            }
            if claims.normalized_document_hash != normalized_hash.0 {
                return Err(ApplicationError::StaleCursor(format!(
                    "normalized document hash changed from {} to {}",
                    claims.normalized_document_hash, normalized_hash.0
                )));
            }
        }

        let entries = flatten_requested_scope(
            &document,
            scope.root_section_id.as_ref(),
            scope.effective_max_depth,
            cursor_claims.is_some(),
            &body_order,
        )?;
        let total_nodes = entries.len();

        let start_index = if let Some(claims) = &cursor_claims {
            if claims.total_nodes != total_nodes {
                return Err(ApplicationError::InvalidCursor(format!(
                    "structure cursor expected {} nodes but matching canonical identity produced {total_nodes}",
                    claims.total_nodes
                )));
            }
            if claims.next_index >= total_nodes {
                return Err(ApplicationError::InvalidCursor(format!(
                    "structure cursor position {} is not resumable for {total_nodes} nodes",
                    claims.next_index
                )));
            }
            claims.next_index
        } else {
            0
        };
        let end_index = start_index.saturating_add(max_nodes).min(total_nodes);
        let complete = end_index == total_nodes;
        let sections = project_page_forest(&entries, start_index, end_index);
        let next_cursor = if complete {
            None
        } else {
            Some(encode_structure_cursor(StructureCursorClaims::new(
                document.id.0.clone(),
                document.content_hash.0.clone(),
                normalized_hash.0.clone(),
                scope.root_section_id.as_ref().map(|id| id.0.clone()),
                scope.effective_max_depth,
                end_index,
                total_nodes,
            ))?)
        };

        Ok(DocumentStructureResult {
            document_id: document.id.clone(),
            title: document.title,
            content_hash: document.content_hash.clone(),
            normalized_document_hash: normalized_hash,
            normalized_document_hash_version: NORMALIZED_DOCUMENT_HASH_VERSION.into(),
            normalization_version: NORMALIZATION_VERSION.into(),
            segmentation_version: TEXT_SEGMENTATION_VERSION.into(),
            sections,
            truncated: !complete,
            complete,
            next_cursor,
            stream: StructureStreamSegment {
                traversal_version: STRUCTURE_TRAVERSAL_VERSION.into(),
                body_order_version: BODY_ORDER_VERSION.into(),
                root_section_id: scope.root_section_id,
                max_depth: scope.effective_max_depth,
                start_index,
                end_index,
                total_nodes,
            },
        })
    }
}

fn resolve_named_section(
    document: &Document,
    body_order: &HashMap<SectionId, usize>,
    command: &ResolveNamedSectionCommand,
    normalized_hash: &NormalizedDocumentHash,
) -> Result<NamedSectionResolution, ApplicationError> {
    let query = command.query.trim();
    if query.is_empty() {
        return Err(ApplicationError::InvalidRequest(
            "named_section_query must not be empty".into(),
        ));
    }
    let expected_content_hash = command.expected_content_hash.as_str();
    let expected_normalized_hash = command.expected_normalized_document_hash.as_str();
    if expected_content_hash != document.content_hash.0 {
        return Err(ApplicationError::StaleStructure(format!(
            "raw content hash changed from {expected_content_hash} to {}",
            document.content_hash.0
        )));
    }
    if expected_normalized_hash != normalized_hash.0 {
        return Err(ApplicationError::StaleStructure(format!(
            "normalized document hash changed from {expected_normalized_hash} to {}",
            normalized_hash.0
        )));
    }
    if let Some(expected_version) = command.expected_structure_resolution_version.as_deref()
        && expected_version != NAMED_SECTION_RESOLUTION_VERSION
    {
        return Err(ApplicationError::StaleStructure(format!(
            "structure resolution version {expected_version} is unsupported; expected {NAMED_SECTION_RESOLUTION_VERSION}"
        )));
    }

    if is_page_only_pdf_fallback(document) {
        return Ok(NamedSectionResolution {
            version: NAMED_SECTION_RESOLUTION_VERSION.into(),
            status: NamedSectionResolutionStatus::Unavailable,
            query: query.into(),
            match_kind: None,
            matched: None,
            candidates: Vec::new(),
            boundary: None,
            degradation: Some(
                "canonical PDF structure is page-only; named structural headings are unavailable"
                    .into(),
            ),
        });
    }

    let refs = all_section_refs(document);
    let normalized_query = normalize_heading_key(query);
    let without_section_prefix = strip_section_prefix(&normalized_query);
    let query_has_section_prefix = without_section_prefix != normalized_query;
    let query_has_number = first_token_is_numeric_designator(&without_section_prefix);

    let exact = refs
        .iter()
        .copied()
        .filter(|section| normalize_heading_key(&section.title) == normalized_query)
        .collect::<Vec<_>>();
    let prefixed = if query_has_section_prefix {
        refs.iter()
            .copied()
            .filter(|section| normalize_heading_key(&section.title) == without_section_prefix)
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    let title_only = if !query_has_number {
        refs.iter()
            .copied()
            .filter(|section| {
                strip_numeric_designator(&normalize_heading_key(&section.title)) == normalized_query
            })
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };

    let (matches, match_kind) = if !exact.is_empty() {
        (exact, NamedSectionMatchKind::ExactTitle)
    } else if !prefixed.is_empty() {
        (prefixed, NamedSectionMatchKind::SectionPrefixedTitle)
    } else if !title_only.is_empty() {
        (title_only, NamedSectionMatchKind::TitleOnly)
    } else {
        return Ok(NamedSectionResolution {
            version: NAMED_SECTION_RESOLUTION_VERSION.into(),
            status: NamedSectionResolutionStatus::NotFound,
            query: query.into(),
            match_kind: None,
            matched: None,
            candidates: Vec::new(),
            boundary: None,
            degradation: None,
        });
    };

    let candidates = matches
        .iter()
        .map(|section| candidate_metadata(document, section, body_order))
        .collect::<Vec<_>>();
    if matches.len() != 1 {
        return Ok(NamedSectionResolution {
            version: NAMED_SECTION_RESOLUTION_VERSION.into(),
            status: NamedSectionResolutionStatus::Ambiguous,
            query: query.into(),
            match_kind: Some(match_kind),
            matched: None,
            candidates,
            boundary: None,
            degradation: None,
        });
    }

    let matched_section = matches[0];
    let matched = candidate_metadata(document, matched_section, body_order);
    let boundary = build_named_boundary(document, matched_section, body_order);
    let status = if boundary.is_some() {
        NamedSectionResolutionStatus::Resolved
    } else {
        NamedSectionResolutionStatus::BoundaryUnavailable
    };
    Ok(NamedSectionResolution {
        version: NAMED_SECTION_RESOLUTION_VERSION.into(),
        status,
        query: query.into(),
        match_kind: Some(match_kind),
        matched: Some(matched),
        candidates: Vec::new(),
        boundary,
        degradation: (status == NamedSectionResolutionStatus::BoundaryUnavailable).then(|| {
            "named Section resolved, but its executable body-order boundary exceeds the response budget"
                .into()
        }),
    })
}

fn normalize_heading_key(value: &str) -> String {
    let normalized = value
        .chars()
        .map(|character| match character {
            '-' | '–' | '—' | ':' => ' ',
            _ => character,
        })
        .collect::<String>();
    normalized
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

fn strip_section_prefix(value: &str) -> String {
    value.strip_prefix("section ").unwrap_or(value).to_string()
}

fn strip_numeric_designator(value: &str) -> String {
    let mut parts = value.split_whitespace();
    let Some(first) = parts.next() else {
        return String::new();
    };
    if is_numeric_designator(first) {
        parts.collect::<Vec<_>>().join(" ")
    } else {
        value.to_string()
    }
}

fn first_token_is_numeric_designator(value: &str) -> bool {
    value
        .split_whitespace()
        .next()
        .is_some_and(is_numeric_designator)
}

fn is_numeric_designator(value: &str) -> bool {
    let value = value.trim_end_matches('.');
    !value.is_empty()
        && value
            .split('.')
            .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
}

fn all_section_refs(document: &Document) -> Vec<&Section> {
    fn collect<'a>(section: &'a Section, output: &mut Vec<&'a Section>) {
        output.push(section);
        for child in &section.children {
            collect(child, output);
        }
    }
    let mut output = Vec::new();
    for section in &document.root_sections {
        collect(section, &mut output);
    }
    output
}

fn is_page_only_pdf_fallback(document: &Document) -> bool {
    document
        .media_type
        .0
        .eq_ignore_ascii_case("application/pdf")
        && document
            .metadata
            .get("pdf_structure_provenance")
            .is_some_and(|value| value == "page_fallback")
}

fn candidate_metadata(
    document: &Document,
    section: &Section,
    body_order: &HashMap<SectionId, usize>,
) -> NamedSectionCandidate {
    NamedSectionCandidate {
        section_id: section.id.clone(),
        parent_id: section.parent_id.clone(),
        title: section.title.clone(),
        level: section.level,
        location: section.location.clone(),
        body_order: body_order[&section.id],
        start_locator: TextLocator::for_section(document, section),
    }
}

fn build_named_boundary(
    document: &Document,
    section: &Section,
    body_order: &HashMap<SectionId, usize>,
) -> Option<NamedSectionBoundary> {
    let mut orders = Vec::new();
    collect_scope_body_orders(section, body_order, &mut orders);
    orders.sort_unstable();
    orders.dedup();
    let intervals = compress_body_order_intervals(&orders);
    if intervals.len() > MAX_STRUCTURE_RESPONSE_NODES {
        return None;
    }

    let end_exclusive = if intervals.len() == 1 {
        let next_order = intervals[0].end;
        section_at_body_order(document, body_order, next_order)
            .map(|next| candidate_metadata(document, next, body_order))
    } else {
        None
    };
    Some(NamedSectionBoundary {
        version: NAMED_SECTION_BOUNDARY_VERSION.into(),
        body_order_version: BODY_ORDER_VERSION.into(),
        intervals,
        end_exclusive,
    })
}

fn collect_scope_body_orders(
    section: &Section,
    body_order: &HashMap<SectionId, usize>,
    output: &mut Vec<usize>,
) {
    output.push(body_order[&section.id]);
    for child in &section.children {
        collect_scope_body_orders(child, body_order, output);
    }
}

fn compress_body_order_intervals(orders: &[usize]) -> Vec<BodyOrderInterval> {
    let Some(first) = orders.first().copied() else {
        return Vec::new();
    };
    let mut intervals = Vec::new();
    let mut start = first;
    let mut end = first.saturating_add(1);
    for order in orders.iter().copied().skip(1) {
        if order == end {
            end = end.saturating_add(1);
        } else {
            intervals.push(BodyOrderInterval { start, end });
            start = order;
            end = order.saturating_add(1);
        }
    }
    intervals.push(BodyOrderInterval { start, end });
    intervals
}

fn section_at_body_order<'a>(
    document: &'a Document,
    body_order: &HashMap<SectionId, usize>,
    target: usize,
) -> Option<&'a Section> {
    all_section_refs(document)
        .into_iter()
        .find(|section| body_order.get(&section.id).copied() == Some(target))
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct StructureScope {
    root_section_id: Option<SectionId>,
    effective_max_depth: Option<u8>,
}

fn resolve_scope(
    command: &GetDocumentStructureCommand,
    cursor: Option<&StructureCursorClaims>,
) -> Result<StructureScope, ApplicationError> {
    let requested_max_depth = command.max_depth.map(normalize_max_depth);
    let Some(cursor) = cursor else {
        return Ok(StructureScope {
            root_section_id: command.root_section_id.clone(),
            effective_max_depth: requested_max_depth,
        });
    };

    if cursor.document_id != command.document_id.0 {
        return Err(ApplicationError::CursorTargetMismatch(format!(
            "structure cursor document {} does not match requested document {}",
            cursor.document_id, command.document_id.0
        )));
    }

    let cursor_root = cursor
        .root_section_id
        .as_ref()
        .map(|id| SectionId(id.clone()));
    if let Some(requested_root) = command.root_section_id.as_ref()
        && cursor_root.as_ref() != Some(requested_root)
    {
        return Err(ApplicationError::CursorTargetMismatch(format!(
            "structure cursor root {:?} does not match requested root {}",
            cursor.root_section_id, requested_root.0
        )));
    }
    if command.max_depth.is_some() && requested_max_depth != cursor.effective_max_depth {
        return Err(ApplicationError::CursorTargetMismatch(format!(
            "structure cursor max_depth {:?} does not match requested effective max_depth {:?}",
            cursor.effective_max_depth, requested_max_depth
        )));
    }

    Ok(StructureScope {
        root_section_id: cursor_root,
        effective_max_depth: cursor.effective_max_depth,
    })
}

fn normalize_max_depth(value: u8) -> u8 {
    value.max(1)
}

fn effective_max_nodes(requested: Option<usize>) -> Result<usize, ApplicationError> {
    let requested = requested.unwrap_or(DEFAULT_STRUCTURE_MAX_NODES);
    if requested == 0 {
        return Err(ApplicationError::InvalidRequest(
            "get_document_structure max_nodes must be greater than zero".into(),
        ));
    }
    Ok(requested.min(MAX_STRUCTURE_RESPONSE_NODES))
}

#[derive(Clone, Debug)]
struct FlatStructureEntry {
    section_id: SectionId,
    parent_id: Option<SectionId>,
    title: String,
    level: u8,
    location: Location,
    body_order: usize,
    in_scope_child_ids: Vec<SectionId>,
}

fn flatten_requested_scope(
    document: &Document,
    root_section_id: Option<&SectionId>,
    max_depth: Option<u8>,
    from_cursor: bool,
    body_order: &std::collections::HashMap<SectionId, usize>,
) -> Result<Vec<FlatStructureEntry>, ApplicationError> {
    let mut output = Vec::new();
    match root_section_id {
        Some(root_section_id) => {
            let root = document.find_section(root_section_id).ok_or_else(|| {
                if from_cursor {
                    ApplicationError::InvalidCursor(format!(
                        "structure cursor root {} is absent despite matching document identity",
                        root_section_id.0
                    ))
                } else {
                    ApplicationError::SectionNotFound
                }
            })?;
            flatten_section(root, 1, max_depth, &mut output, body_order);
        }
        None => {
            for section in &document.root_sections {
                flatten_section(section, 1, max_depth, &mut output, body_order);
            }
        }
    }
    Ok(output)
}

fn flatten_section(
    section: &Section,
    depth: u8,
    max_depth: Option<u8>,
    output: &mut Vec<FlatStructureEntry>,
    body_order: &std::collections::HashMap<SectionId, usize>,
) {
    let include_children = max_depth.is_none_or(|limit| depth < limit);
    output.push(FlatStructureEntry {
        section_id: section.id.clone(),
        parent_id: section.parent_id.clone(),
        title: section.title.clone(),
        level: section.level,
        location: section.location.clone(),
        body_order: *body_order
            .get(&section.id)
            .expect("validated body order must contain every canonical Section"),
        in_scope_child_ids: if include_children {
            section
                .children
                .iter()
                .map(|child| child.id.clone())
                .collect()
        } else {
            Vec::new()
        },
    });

    if include_children {
        for child in &section.children {
            flatten_section(
                child,
                depth.saturating_add(1),
                max_depth,
                output,
                body_order,
            );
        }
    }
}

fn project_page_forest(
    entries: &[FlatStructureEntry],
    start_index: usize,
    end_index: usize,
) -> Vec<SectionOutline> {
    let page = &entries[start_index..end_index];
    let by_id = page
        .iter()
        .enumerate()
        .map(|(index, entry)| (entry.section_id.clone(), index))
        .collect::<HashMap<_, _>>();
    let mut child_indexes = vec![Vec::<usize>::new(); page.len()];
    let mut roots = Vec::new();

    for (index, entry) in page.iter().enumerate() {
        match entry
            .parent_id
            .as_ref()
            .and_then(|parent| by_id.get(parent).copied())
        {
            Some(parent_index) => child_indexes[parent_index].push(index),
            None => roots.push(index),
        }
    }

    roots
        .into_iter()
        .map(|index| build_page_outline(index, page, &child_indexes, &by_id))
        .collect()
}

fn build_page_outline(
    index: usize,
    page: &[FlatStructureEntry],
    child_indexes: &[Vec<usize>],
    page_by_id: &HashMap<SectionId, usize>,
) -> SectionOutline {
    let entry = &page[index];
    SectionOutline {
        section_id: entry.section_id.clone(),
        parent_id: entry.parent_id.clone(),
        title: entry.title.clone(),
        level: entry.level,
        location: entry.location.clone(),
        body_order: entry.body_order,
        children_complete: entry
            .in_scope_child_ids
            .iter()
            .all(|child| page_by_id.contains_key(child)),
        children: child_indexes[index]
            .iter()
            .map(|child| build_page_outline(*child, page, child_indexes, page_by_id))
            .collect(),
    }
}
