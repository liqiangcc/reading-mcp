use std::collections::HashMap;
use std::sync::Arc;

use crate::application::ports::{ApplicationError, DocumentRepository};
use crate::application::structure_cursor::{
    STRUCTURE_TRAVERSAL_VERSION, StructureCursorClaims, decode_structure_cursor,
    encode_structure_cursor,
};
use crate::domain::{Document, DocumentId, Location, Section, SectionId};

pub const DEFAULT_STRUCTURE_MAX_NODES: usize = 1_000;
pub const MAX_STRUCTURE_RESPONSE_NODES: usize = 1_000;

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
    pub children_complete: bool,
    pub children: Vec<SectionOutline>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StructureStreamSegment {
    pub traversal_version: String,
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
                normalized_hash.0,
                scope.root_section_id.as_ref().map(|id| id.0.clone()),
                scope.effective_max_depth,
                end_index,
                total_nodes,
            ))?)
        };

        Ok(DocumentStructureResult {
            document_id: document.id.clone(),
            title: document.title,
            sections,
            truncated: !complete,
            complete,
            next_cursor,
            stream: StructureStreamSegment {
                traversal_version: STRUCTURE_TRAVERSAL_VERSION.into(),
                root_section_id: scope.root_section_id,
                max_depth: scope.effective_max_depth,
                start_index,
                end_index,
                total_nodes,
            },
        })
    }
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

    let cursor_root = cursor.root_section_id.as_ref().map(|id| SectionId(id.clone()));
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
    in_scope_child_ids: Vec<SectionId>,
}

fn flatten_requested_scope(
    document: &Document,
    root_section_id: Option<&SectionId>,
    max_depth: Option<u8>,
    from_cursor: bool,
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
            flatten_section(root, 1, max_depth, &mut output);
        }
        None => {
            for section in &document.root_sections {
                flatten_section(section, 1, max_depth, &mut output);
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
) {
    let include_children = max_depth.is_none_or(|limit| depth < limit);
    output.push(FlatStructureEntry {
        section_id: section.id.clone(),
        parent_id: section.parent_id.clone(),
        title: section.title.clone(),
        level: section.level,
        location: section.location.clone(),
        in_scope_child_ids: if include_children {
            section.children.iter().map(|child| child.id.clone()).collect()
        } else {
            Vec::new()
        },
    });

    if include_children {
        for child in &section.children {
            flatten_section(child, depth.saturating_add(1), max_depth, output);
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
