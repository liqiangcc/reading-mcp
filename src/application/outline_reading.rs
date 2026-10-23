//! The v2 `outline` tool: the section tree in preorder, one short handle per node.
//!
//! `at` names a document (its root sections) or a section (its subsections). `depth` bounds how
//! many levels below `at` are listed, `limit` how many nodes one answer carries, and `cursor`
//! continues a truncated listing. A cursor is `o:<document handle>.<position>~<check>`; the check
//! binds the document identity, the scope and the depth, so a cursor from another scope fails as
//! `stale_handle` instead of silently listing a different subtree.

use std::sync::Arc;

use sha2::{Digest, Sha256};

use crate::application::handle_reading::{ResolvedHeader, resolve_header, resolve_section};
use crate::application::ports::{ApplicationError, DocumentRepository, SectionRow, TextUnitIndex};
use crate::application::reading_error::ReadingError;
use crate::application::section_view::DocumentView;
use crate::application::short_handle::{ReadingHandle, SectionHandle, parse_handle};

/// How many nodes one `outline` answer lists when the caller does not say.
pub const DEFAULT_OUTLINE_LIMIT: usize = 100;
/// The largest `limit` an outline accepts.
pub const MAX_OUTLINE_LIMIT: usize = 1_000;
/// How many levels below `at` an outline lists when the caller does not say.
pub const DEFAULT_OUTLINE_DEPTH: usize = 2;
/// The largest `depth` an outline accepts.
pub const MAX_OUTLINE_DEPTH: usize = 32;
/// The number of lowercase hex digits of the scope check.
pub const OUTLINE_CHECK_DIGITS: usize = 4;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutlineReadingCommand {
    /// A document or section handle.
    pub at: String,
    /// Levels below `at`, 1-based.
    pub depth: usize,
    pub limit: usize,
    pub cursor: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutlineReadingItem {
    pub handle: String,
    pub title: String,
    pub level: u8,
    pub chars: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutlineReadingResult {
    pub items: Vec<OutlineReadingItem>,
    pub next: Option<String>,
}

pub struct OutlineReadingUseCase {
    repository: Arc<dyn DocumentRepository>,
    text_unit_index: Arc<dyn TextUnitIndex>,
}

impl OutlineReadingUseCase {
    pub fn new(
        repository: Arc<dyn DocumentRepository>,
        text_unit_index: Arc<dyn TextUnitIndex>,
    ) -> Self {
        Self {
            repository,
            text_unit_index,
        }
    }

    pub async fn execute(
        &self,
        command: OutlineReadingCommand,
    ) -> Result<OutlineReadingResult, ReadingError> {
        if command.depth == 0 || command.depth > MAX_OUTLINE_DEPTH {
            return Err(ReadingError::invalid(
                "depth",
                format!("depth must be between 1 and {MAX_OUTLINE_DEPTH}"),
            ));
        }
        if command.limit == 0 || command.limit > MAX_OUTLINE_LIMIT {
            return Err(ReadingError::invalid(
                "limit",
                format!("limit must be between 1 and {MAX_OUTLINE_LIMIT}"),
            ));
        }
        let handle = parse_handle(&command.at).map_err(|error| {
            ReadingError::invalid(
                "at",
                format!("at must be a document or section handle: {}", error.message),
            )
        })?;
        let resolved = resolve_header(
            self.text_unit_index.as_ref(),
            &self.repository,
            handle.document(),
        )
        .await?;
        let (scope_key, mut preorder) = match &handle {
            ReadingHandle::Document(_) => (
                resolved.handle.to_string(),
                Preorder::roots(&resolved.view, command.depth),
            ),
            ReadingHandle::Section(section) => {
                let row =
                    resolve_section(self.text_unit_index.as_ref(), &resolved, section).await?;
                (
                    section.to_string(),
                    Preorder::below(&resolved.view, &row, command.depth),
                )
            }
            ReadingHandle::Paragraph { .. } | ReadingHandle::Sentence { .. } => {
                return Err(ReadingError::invalid(
                    "at",
                    "at must be a document or section handle, not a paragraph or sentence",
                ));
            }
        };
        let check = outline_check(&scope_key, command.depth);

        let start = match command.cursor.as_deref() {
            None => 0,
            Some(cursor) => decode_cursor(cursor, &resolved.handle.to_string(), &check)?,
        };
        // Skip to the cursor position one row at a time; a cursor past the end of the scope is the
        // same `stale_handle` it always was.
        for _ in 0..start {
            if preorder.next().await?.is_none() {
                return Err(ReadingError::stale_handle(
                    "outline cursor points past the end of its scope",
                ));
            }
        }
        let mut items = Vec::with_capacity(command.limit);
        while items.len() < command.limit {
            let Some(row) = preorder.next().await? else {
                break;
            };
            items.push(self.item(&resolved, &row).await?);
        }
        // One row past the page decides `has_more`, without listing the rest of the tree.
        let next = preorder
            .next()
            .await?
            .map(|_| format!("o:{}.{}~{}", resolved.handle, start + items.len(), check));
        Ok(OutlineReadingResult { items, next })
    }

    async fn item(
        &self,
        resolved: &ResolvedHeader,
        row: &SectionRow,
    ) -> Result<OutlineReadingItem, ReadingError> {
        let section_key = self
            .text_unit_index
            .section_key(resolved.id(), &row.id)
            .await?
            .ok_or_else(|| {
                ReadingError::stale_handle(format!(
                    "section {} has no stored section key",
                    row.id.0
                ))
            })?;
        Ok(OutlineReadingItem {
            handle: SectionHandle {
                document: resolved.handle.clone(),
                section_key,
            }
            .to_string(),
            title: row.title.clone(),
            level: row.level,
            // The stored length of the section's normalized text, which is exactly what
            // `section.content.chars().count()` counted on the full document.
            chars: self
                .repository
                .load_section_char_len(resolved.id(), &row.id)
                .await?
                .ok_or_else(|| {
                    ReadingError::stale_handle(format!("section {} has no stored text", row.id.0))
                })?,
        })
    }
}

/// The preorder listing of a scope, one `section_children` read per listed node: the scope's roots
/// (or one section's children) first, then their children, at most `depth` levels below the scope.
struct Preorder<'a> {
    view: &'a DocumentView,
    depth: usize,
    stack: Vec<(SectionRow, usize)>,
    /// The parent whose children seed the walk, until the first `next` loads them.
    seed: Option<Option<i64>>,
}

impl<'a> Preorder<'a> {
    fn roots(view: &'a DocumentView, depth: usize) -> Self {
        Self {
            view,
            depth,
            stack: Vec::new(),
            seed: Some(None),
        }
    }

    fn below(view: &'a DocumentView, section: &SectionRow, depth: usize) -> Self {
        Self {
            view,
            depth,
            stack: Vec::new(),
            seed: Some(Some(section.section_key)),
        }
    }

    /// The next row in preorder, or `None` at the end of the scope.
    async fn next(&mut self) -> Result<Option<SectionRow>, ApplicationError> {
        if let Some(parent_key) = self.seed.take() {
            self.push_children(parent_key, 1).await?;
        }
        let Some((row, level)) = self.stack.pop() else {
            return Ok(None);
        };
        if level < self.depth {
            self.push_children(Some(row.section_key), level + 1).await?;
        }
        Ok(Some(row))
    }

    /// Pushes `parent_key`'s children in reverse so that popping yields ascending key order, which
    /// is the order the document holds them in.
    async fn push_children(
        &mut self,
        parent_key: Option<i64>,
        level: usize,
    ) -> Result<(), ApplicationError> {
        let children = self.view.children(parent_key, usize::MAX).await?;
        for child in children.into_iter().rev() {
            self.stack.push((child, level));
        }
        Ok(())
    }
}

/// The four lowercase hex digits that bind a cursor to one scope and depth.
fn outline_check(scope_key: &str, depth: usize) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"reading-mcp/outline-cursor/v1\0");
    hasher.update(scope_key.as_bytes());
    hasher.update(b"|");
    hasher.update(depth.to_string().as_bytes());
    let hex = format!("{:x}", hasher.finalize());
    hex[..OUTLINE_CHECK_DIGITS].to_owned()
}

/// Decodes `o:<document handle>.<position>~<check>` and verifies the check.
fn decode_cursor(
    cursor: &str,
    document: &str,
    expected_check: &str,
) -> Result<usize, ReadingError> {
    let rest = cursor
        .strip_prefix("o:")
        .ok_or_else(|| ReadingError::invalid("cursor", "cursor must start with o:"))?;
    let (left, check) = rest
        .rsplit_once('~')
        .ok_or_else(|| ReadingError::invalid("cursor", "outline cursor is missing its check"))?;
    let (cursor_document, position) = left
        .rsplit_once('.')
        .ok_or_else(|| ReadingError::invalid("cursor", "outline cursor is missing its position"))?;
    if cursor_document != document || check != expected_check {
        return Err(ReadingError::stale_handle(
            "outline cursor was issued for a different document, scope or depth",
        ));
    }
    position
        .parse::<usize>()
        .map_err(|_| ReadingError::invalid("cursor", "outline cursor position must be an integer"))
}
