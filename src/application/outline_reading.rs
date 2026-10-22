//! The v2 `outline` tool: the section tree in preorder, one short handle per node.
//!
//! `at` names a document (its root sections) or a section (its subsections). `depth` bounds how
//! many levels below `at` are listed, `limit` how many nodes one answer carries, and `cursor`
//! continues a truncated listing. A cursor is `o:<document handle>.<position>~<check>`; the check
//! binds the document identity, the scope and the depth, so a cursor from another scope fails as
//! `stale_handle` instead of silently listing a different subtree.

use std::sync::Arc;

use sha2::{Digest, Sha256};

use crate::application::handle_reading::{resolve_document, resolve_section};
use crate::application::ports::{DocumentRepository, TextUnitIndex};
use crate::application::reading_error::ReadingError;
use crate::application::short_handle::{ReadingHandle, SectionHandle, parse_handle};
use crate::domain::Section;

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
        let resolved = resolve_document(
            self.text_unit_index.as_ref(),
            self.repository.as_ref(),
            handle.document(),
        )
        .await?;
        let section_scope = match &handle {
            ReadingHandle::Section(section) => {
                Some(resolve_section(self.text_unit_index.as_ref(), &resolved, section).await?)
            }
            _ => None,
        };
        let (scope_key, scope) = match &handle {
            ReadingHandle::Document(_) => (
                resolved.handle.to_string(),
                preorder(&resolved.document.root_sections, command.depth),
            ),
            ReadingHandle::Section(section) => (
                section.to_string(),
                preorder(
                    &section_scope.as_ref().expect("resolved above").children,
                    command.depth,
                ),
            ),
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
            Some(cursor) => {
                let position = decode_cursor(cursor, &resolved.handle.to_string(), &check)?;
                if position > scope.len() {
                    return Err(ReadingError::stale_handle(
                        "outline cursor points past the end of its scope",
                    ));
                }
                position
            }
        };
        let end = (start + command.limit).min(scope.len());
        let mut items = Vec::with_capacity(end - start);
        for section in &scope[start..end] {
            let section_key = self
                .text_unit_index
                .section_key(&resolved.document.id, &section.id)
                .await?
                .ok_or_else(|| {
                    ReadingError::stale_handle(format!(
                        "section {} has no stored section key",
                        section.id.0
                    ))
                })?;
            items.push(OutlineReadingItem {
                handle: SectionHandle {
                    document: resolved.handle.clone(),
                    section_key,
                }
                .to_string(),
                title: section.title.clone(),
                level: section.level,
                // The stored length of the section's normalized text, which is exactly what
                // `section.content.chars().count()` counted on the full document.
                chars: self
                    .repository
                    .load_section_char_len(&resolved.document.id, &section.id)
                    .await?
                    .ok_or_else(|| {
                        ReadingError::stale_handle(format!(
                            "section {} has no stored text",
                            section.id.0
                        ))
                    })?,
            });
        }
        let next = (end < scope.len()).then(|| format!("o:{}.{}~{}", resolved.handle, end, check));
        Ok(OutlineReadingResult { items, next })
    }
}

/// The preorder listing of `roots`, at most `depth` levels below the scope.
fn preorder(roots: &[Section], depth: usize) -> Vec<&Section> {
    let mut output = Vec::new();
    for root in roots {
        collect(root, 1, depth, &mut output);
    }
    output
}

fn collect<'a>(section: &'a Section, level: usize, depth: usize, output: &mut Vec<&'a Section>) {
    if level > depth {
        return;
    }
    output.push(section);
    for child in &section.children {
        collect(child, level + 1, depth, output);
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
