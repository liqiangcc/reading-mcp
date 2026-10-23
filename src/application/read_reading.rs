//! The v2 `read` tool: one item per call by default, or a char-budgeted batch, over the whole book.
//!
//! Reading order is the v1 one: sections follow `body_order`, empty sections are skipped, and every
//! section contributes its v1 `PreserveSource` sentence (or paragraph) stream. The walk only uses the
//! O(page) `TextUnitIndex` stream methods, so a call touches one page per section it crosses.

use std::collections::HashMap;
use std::sync::Arc;

use crate::application::get_text_units::{
    EffectiveTextUnitKind, RequestedTextUnitKind, TextUnitCoveragePolicy, TextUnitDirection,
};
use crate::application::ports::{
    DocumentRepository, OwnedStreamUnit, StoredStreamItem, TextUnitIndex, TextUnitStreamKey,
};
use crate::application::reading_error::ReadingError;
use crate::application::section_view::DocumentView;
use crate::application::short_handle::{
    CursorUnit, DocumentHandle, ReadingCursor, ReadingFrom, ReadingHandle, SectionHandle,
    handle_checksum, paragraph_handle, parse_from, sentence_handle,
};
use crate::application::text_unit_stream::item_kind;
use crate::domain::{DocumentId, SectionId};

/// The largest `max_chars` a batch read accepts.
pub const MAX_BATCH_CHARS: usize = 16_000;
/// The largest `back` a read accepts.
pub const MAX_BACK: usize = 50;
/// How many stream rows one page fetch asks for. Only the last fetch of a call is partly used.
const FETCH_CHUNK: usize = 64;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReadReadingCommand {
    /// A document, section, paragraph or sentence handle, or a backward `b:` cursor.
    pub from: String,
    /// `sentence` (default) or `paragraph`.
    pub unit: RequestedTextUnitKind,
    /// `None` returns exactly one item; `Some` fills a char budget.
    pub max_chars: Option<usize>,
    /// Only meaningful for handles: a cursor carries its own direction.
    pub direction: Option<TextUnitDirection>,
    pub back: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReadReadingItem {
    pub handle: String,
    pub text: String,
    /// The v1 content class of a coarse or non-prose item, in lowercase snake case.
    pub content_class: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReadReadingSection {
    /// The index of this section's first stream item within `items`.
    pub at: usize,
    pub handle: String,
    pub title: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReadReadingResult {
    pub items: Vec<ReadReadingItem>,
    pub sections: Vec<ReadReadingSection>,
    /// The next item's handle when reading forward, or a `b:` cursor when reading backward.
    pub next: Option<String>,
    /// The index of the `from` item within `items`, when `back` > 0 and it was returned.
    pub anchor: Option<usize>,
}

pub struct ReadReadingUseCase {
    repository: Arc<dyn DocumentRepository>,
    text_unit_index: Arc<dyn TextUnitIndex>,
}

impl ReadReadingUseCase {
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
        command: ReadReadingCommand,
    ) -> Result<ReadReadingResult, ReadingError> {
        if command.back > MAX_BACK {
            return Err(ReadingError::invalid(
                "back",
                format!("back 必须是 0 到 {MAX_BACK} 之间的整数"),
            ));
        }
        if let Some(max_chars) = command.max_chars
            && (max_chars == 0 || max_chars > MAX_BATCH_CHARS)
        {
            return Err(ReadingError::invalid(
                "max_chars",
                format!("max_chars 必须在 1 到 {MAX_BATCH_CHARS} 之间"),
            ));
        }

        let from = parse_from(&command.from).map_err(|error| {
            ReadingError::invalid(
                "from",
                format!(
                    "from 必须是句柄（d<key>~<chk>[.s<key>[.p<n>[.<n>]]]）或后向游标（b:<section>.<S|P>.<pos>）：{}",
                    error.message
                ),
            )
        })?;

        let (document_handle, direction, cursor_position) = match &from {
            ReadingFrom::Handle(handle) => (
                handle.document().clone(),
                command.direction.unwrap_or(TextUnitDirection::Forward),
                None,
            ),
            ReadingFrom::Cursor(cursor) => {
                // The `b:` prefix carries the direction, so it must agree with `direction`.
                if command.direction == Some(TextUnitDirection::Forward) {
                    return Err(ReadingError::invalid(
                        "direction",
                        "direction 必须与游标前缀一致：b: 是 backward".to_owned(),
                    ));
                }
                (
                    cursor.section.document.clone(),
                    TextUnitDirection::Backward,
                    Some(cursor.position),
                )
            }
        };

        let document_id = self
            .text_unit_index
            .document_id_for_key(document_handle.doc_key)
            .await?
            .ok_or_else(|| {
                ReadingError::unknown_handle(format!(
                    "doc_key {} is not stored in this server",
                    document_handle.doc_key
                ))
            })?;
        // The header carries the identity the handle is bound to; the body order is read by
        // position, never materialized as a `Vec<SectionId>`.
        let (view, normalized_hash) =
            DocumentView::load_with_hash(Arc::clone(&self.repository), &document_id)
                .await?
                .ok_or_else(ReadingError::not_opened)?;
        if handle_checksum(view.id(), &normalized_hash) != document_handle.checksum {
            return Err(ReadingError::stale_handle(format!(
                "handle checksum {} does not match the current document identity",
                document_handle.checksum
            )));
        }

        // The body order's length. A stored body order that is readable holds every section
        // exactly once (`body_order_ids` rejects anything else), so the header's section count is
        // that length, and reading it is one row instead of a `COUNT(*)` over every order row.
        // A document whose stored order could not be computed still reports that error, unchanged,
        // from the first `body_order_at`/`body_order_window` this call makes.
        let order_len = view.header().section_count;
        if order_len == 0 {
            return Ok(ReadReadingResult {
                items: Vec::new(),
                sections: Vec::new(),
                next: None,
                anchor: None,
            });
        }
        let mut walk = Walk::new(
            self.text_unit_index.as_ref(),
            self.repository.as_ref(),
            view.id(),
            order_len,
            command.unit,
        );

        let anchor = self
            .anchor(&mut walk, &view, &from, cursor_position, command.unit)
            .await?;

        // `back` first steps backwards in reading order, clamped at the start of the book.
        let mut start = anchor;
        for _ in 0..command.back {
            match walk.step_back(start).await? {
                Some(previous) => start = previous,
                None => break,
            }
        }

        let count_limit = match (command.max_chars, command.back) {
            (None, 0) => Some(1),
            (None, back) => Some(back + 1),
            (Some(_), _) => None,
        };

        let mut fill = Fill::new(&document_handle, command.unit, anchor, command.back > 0);
        match direction {
            TextUnitDirection::Forward => {
                fill.forward(&mut walk, start, count_limit, command.max_chars)
                    .await?;
            }
            TextUnitDirection::Backward => {
                // A cursor already consumed the item at its position; a handle includes it.
                let exclusive = if command.back > 0 {
                    start.1 + 1
                } else {
                    match (&from, cursor_position) {
                        (ReadingFrom::Cursor(_), Some(position)) => position,
                        _ => anchor.1 + 1,
                    }
                };
                fill.backward(
                    &mut walk,
                    (start.0, exclusive),
                    count_limit,
                    command.max_chars,
                )
                .await?;
            }
        }

        fill.finish(&mut walk).await
    }

    /// The section index and stream position the `from` names.
    async fn anchor(
        &self,
        walk: &mut Walk<'_>,
        view: &DocumentView,
        from: &ReadingFrom,
        cursor_position: Option<usize>,
        unit: RequestedTextUnitKind,
    ) -> Result<(usize, usize), ReadingError> {
        match from {
            ReadingFrom::Cursor(cursor) => {
                let index = self.section_index(view, walk, &cursor.section).await?;
                Ok((index, cursor_position.unwrap_or(0)))
            }
            ReadingFrom::Handle(handle) => match handle {
                ReadingHandle::Document(_) => match walk.next_non_empty(0).await? {
                    Some(index) => Ok((index, 0)),
                    None => Ok((0, 0)),
                },
                ReadingHandle::Section(section) => {
                    let index = self.section_index(view, walk, section).await?;
                    Ok((index, 0))
                }
                ReadingHandle::Paragraph {
                    section,
                    paragraph_index,
                } => {
                    let index = self.section_index(view, walk, section).await?;
                    let position = match unit {
                        RequestedTextUnitKind::Paragraph => {
                            walk.position(
                                index,
                                EffectiveTextUnitKind::Paragraph,
                                *paragraph_index,
                                None,
                            )
                            .await?
                        }
                        // A coarse paragraph is one sentence-stream item; a prose paragraph starts at
                        // its first sentence, whose index is always 1.
                        RequestedTextUnitKind::Sentence => {
                            match walk
                                .position(
                                    index,
                                    EffectiveTextUnitKind::Paragraph,
                                    *paragraph_index,
                                    None,
                                )
                                .await?
                            {
                                Some(position) => Some(position),
                                None => {
                                    walk.position(
                                        index,
                                        EffectiveTextUnitKind::Sentence,
                                        *paragraph_index,
                                        Some(1),
                                    )
                                    .await?
                                }
                            }
                        }
                    };
                    Ok((
                        index,
                        position.ok_or_else(|| {
                            ReadingError::unknown_handle(format!(
                                "paragraph {paragraph_index} of section {} is not readable",
                                section.section_key
                            ))
                        })?,
                    ))
                }
                ReadingHandle::Sentence {
                    section,
                    paragraph_index,
                    sentence_index,
                } => {
                    let index = self.section_index(view, walk, section).await?;
                    let position = match unit {
                        RequestedTextUnitKind::Sentence => {
                            walk.position(
                                index,
                                EffectiveTextUnitKind::Sentence,
                                *paragraph_index,
                                Some(*sentence_index),
                            )
                            .await?
                        }
                        RequestedTextUnitKind::Paragraph => {
                            walk.position(
                                index,
                                EffectiveTextUnitKind::Paragraph,
                                *paragraph_index,
                                None,
                            )
                            .await?
                        }
                    };
                    Ok((
                        index,
                        position.ok_or_else(|| {
                            ReadingError::unknown_handle(format!(
                                "sentence {paragraph_index}.{sentence_index} of section {} is not readable",
                                section.section_key
                            ))
                        })?,
                    ))
                }
            },
        }
    }

    /// The body-order position of one section handle: the stored section id proves the section
    /// belongs to the document, and the stored body order gives its position.
    async fn section_index(
        &self,
        view: &DocumentView,
        walk: &mut Walk<'_>,
        section: &SectionHandle,
    ) -> Result<usize, ReadingError> {
        let section_id = self
            .text_unit_index
            .section_id_for_key(view.id(), section.section_key)
            .await?
            .ok_or_else(|| {
                ReadingError::unknown_handle(format!(
                    "section key {} is not stored for this document",
                    section.section_key
                ))
            })?;
        if view.section_by_id(&section_id).await?.is_none() {
            return Err(ReadingError::unknown_handle(format!(
                "section {} is not part of this document",
                section.section_key
            )));
        }
        walk.ord_of(&section_id).await?.ok_or_else(|| {
            ReadingError::unknown_handle(format!(
                "section {} is not in the reading order",
                section.section_key
            ))
        })
    }
}

/// The per-call view of a document's section streams, with the neighbour search every walk needs.
///
/// A position is a body-order position, never an index into a materialized `Vec<SectionId>`:
/// `body_order_at` turns a position into a section key, `section_id_for_key` into the section id a
/// stream key needs, and `body_order_window` turns a section id back into its position. The two
/// maps are this call's own memos, so a section the walk crosses is resolved once per call.
struct Walk<'a> {
    index: &'a dyn TextUnitIndex,
    repository: &'a dyn DocumentRepository,
    document_id: &'a DocumentId,
    unit: RequestedTextUnitKind,
    len: usize,
    totals: HashMap<usize, usize>,
    sections: HashMap<usize, (i64, SectionId)>,
}

impl<'a> Walk<'a> {
    fn new(
        index: &'a dyn TextUnitIndex,
        repository: &'a dyn DocumentRepository,
        document_id: &'a DocumentId,
        len: usize,
        unit: RequestedTextUnitKind,
    ) -> Self {
        Self {
            index,
            repository,
            document_id,
            unit,
            len,
            totals: HashMap::new(),
            sections: HashMap::new(),
        }
    }

    /// The section key and id at one body-order position.
    async fn position_section(&mut self, index: usize) -> Result<(i64, SectionId), ReadingError> {
        if let Some(section) = self.sections.get(&index) {
            return Ok(section.clone());
        }
        let Some(section_key) = self
            .repository
            .body_order_at(self.document_id, index)
            .await?
        else {
            return Err(ReadingError::stale_handle(format!(
                "body order position {index} of document {} has no section",
                self.document_id.0
            )));
        };
        let section_id = self
            .index
            .section_id_for_key(self.document_id, section_key)
            .await?
            .ok_or_else(|| {
                ReadingError::stale_handle(format!(
                    "section key {section_key} of document {} has no stored section id",
                    self.document_id.0
                ))
            })?;
        let section = (section_key, section_id);
        self.sections.insert(index, section.clone());
        Ok(section)
    }

    /// The section key at one body-order position.
    async fn section_key(&mut self, index: usize) -> Result<i64, ReadingError> {
        Ok(self.position_section(index).await?.0)
    }

    /// The body-order position of one section id, or `None` when it is not in the body order.
    async fn ord_of(&mut self, section_id: &SectionId) -> Result<Option<usize>, ReadingError> {
        let Some(section_key) = self.index.section_key(self.document_id, section_id).await? else {
            return Ok(None);
        };
        Ok(self
            .repository
            .body_order_window(self.document_id, section_key, 0, 0)
            .await?
            .map(|window| window.ord))
    }

    fn stream_key<'s>(&'s self, section_id: &'s SectionId) -> TextUnitStreamKey<'s> {
        TextUnitStreamKey {
            document_id: self.document_id,
            section_id,
            kind: self.unit,
            policy: TextUnitCoveragePolicy::PreserveSource,
        }
    }

    /// The number of stream items of one section; a section whose units are not stored counts as 0.
    async fn total(&mut self, index: usize) -> Result<usize, ReadingError> {
        if let Some(total) = self.totals.get(&index) {
            return Ok(*total);
        }
        let (_, section_id) = self.position_section(index).await?;
        let total = self
            .index
            .section_stream_summary(self.stream_key(&section_id))
            .await?
            .map_or(0, |summary| summary.total_items);
        self.totals.insert(index, total);
        Ok(total)
    }

    /// The section key and title of one body-order position, for a section banner.
    async fn banner(&mut self, index: usize) -> Result<(i64, String), ReadingError> {
        let section_key = self.section_key(index).await?;
        let title = self
            .repository
            .section_by_key(self.document_id, section_key)
            .await?
            .map(|row| row.title)
            .unwrap_or_default();
        Ok((section_key, title))
    }

    async fn items(
        &mut self,
        index: usize,
        direction: TextUnitDirection,
        position: usize,
        limit: usize,
    ) -> Result<Vec<StoredStreamItem>, ReadingError> {
        let (_, section_id) = self.position_section(index).await?;
        let rows = self
            .index
            .section_stream_items(self.stream_key(&section_id), direction, position, limit)
            .await?;
        Ok(rows)
    }

    async fn position(
        &mut self,
        index: usize,
        kind: EffectiveTextUnitKind,
        paragraph_index: usize,
        sentence_index: Option<usize>,
    ) -> Result<Option<usize>, ReadingError> {
        let (_, section_id) = self.position_section(index).await?;
        let position = self
            .index
            .section_stream_position(
                self.stream_key(&section_id),
                kind,
                paragraph_index,
                sentence_index,
            )
            .await?;
        Ok(position)
    }

    async fn next_non_empty(&mut self, from: usize) -> Result<Option<usize>, ReadingError> {
        for index in from..self.len {
            if self.total(index).await? > 0 {
                return Ok(Some(index));
            }
        }
        Ok(None)
    }

    async fn prev_non_empty(&mut self, from: usize) -> Result<Option<usize>, ReadingError> {
        let mut index = from;
        loop {
            if self.total(index).await? > 0 {
                return Ok(Some(index));
            }
            if index == 0 {
                return Ok(None);
            }
            index -= 1;
        }
    }

    /// The item one step before `position` in reading order, across section boundaries.
    async fn step_back(
        &mut self,
        (section, position): (usize, usize),
    ) -> Result<Option<(usize, usize)>, ReadingError> {
        if position > 0 {
            return Ok(Some((section, position - 1)));
        }
        if section == 0 {
            return Ok(None);
        }
        match self.prev_non_empty(section - 1).await? {
            Some(previous) => {
                let total = self.total(previous).await?;
                Ok(Some((previous, total - 1)))
            }
            None => Ok(None),
        }
    }

    /// Where a forward read resumes from `(section, position)`.
    async fn forward_next(
        &mut self,
        (section, position): (usize, usize),
    ) -> Result<Option<(usize, usize)>, ReadingError> {
        if position < self.total(section).await? {
            return Ok(Some((section, position)));
        }
        Ok(self
            .next_non_empty(section + 1)
            .await?
            .map(|next| (next, 0)))
    }

    /// Where a backward read resumes from `(section, exclusive end)`.
    async fn backward_next(
        &mut self,
        (section, end): (usize, usize),
    ) -> Result<Option<(usize, usize)>, ReadingError> {
        if end > 0 {
            return Ok(Some((section, end)));
        }
        if section == 0 {
            return Ok(None);
        }
        match self.prev_non_empty(section - 1).await? {
            Some(previous) => {
                let total = self.total(previous).await?;
                Ok((total > 0).then_some((previous, total)))
            }
            None => Ok(None),
        }
    }
}

/// Accumulates the answer while walking.
struct Fill<'a> {
    handle: &'a DocumentHandle,
    unit: RequestedTextUnitKind,
    items: Vec<ReadReadingItem>,
    sections: Vec<ReadReadingSection>,
    anchor_at: Option<(usize, usize)>,
    anchor: Option<usize>,
    next: Option<((usize, usize), TextUnitDirection)>,
}

impl<'a> Fill<'a> {
    fn new(
        handle: &'a DocumentHandle,
        unit: RequestedTextUnitKind,
        anchor_at: (usize, usize),
        track_anchor: bool,
    ) -> Self {
        Self {
            handle,
            unit,
            items: Vec::new(),
            sections: Vec::new(),
            anchor_at: track_anchor.then_some(anchor_at),
            anchor: None,
            next: None,
        }
    }

    async fn forward(
        &mut self,
        walk: &mut Walk<'_>,
        start: (usize, usize),
        count_limit: Option<usize>,
        max_chars: Option<usize>,
    ) -> Result<(), ReadingError> {
        let mut position = start;
        let mut chars = 0usize;
        loop {
            if count_limit.is_some_and(|limit| self.items.len() >= limit) {
                self.next = walk
                    .forward_next(position)
                    .await?
                    .map(|next| (next, TextUnitDirection::Forward));
                return Ok(());
            }
            let total = walk.total(position.0).await?;
            if position.1 >= total {
                match walk.next_non_empty(position.0 + 1).await? {
                    Some(next) => {
                        position = (next, 0);
                        continue;
                    }
                    None => return Ok(()),
                }
            }
            let want = count_limit
                .map(|limit| (limit - self.items.len()).min(FETCH_CHUNK))
                .unwrap_or(FETCH_CHUNK);
            let rows = walk
                .items(position.0, TextUnitDirection::Forward, position.1, want)
                .await?;
            if rows.is_empty() {
                match walk.next_non_empty(position.0 + 1).await? {
                    Some(next) => {
                        position = (next, 0);
                        continue;
                    }
                    None => return Ok(()),
                }
            }
            for row in rows {
                if count_limit.is_some_and(|limit| self.items.len() >= limit) {
                    self.next = walk
                        .forward_next(position)
                        .await?
                        .map(|next| (next, TextUnitDirection::Forward));
                    return Ok(());
                }
                if let Some(max_chars) = max_chars {
                    if self.items.is_empty() && row.char_count > max_chars {
                        return Err(ReadingError::too_long(row.char_count));
                    }
                    if chars + row.char_count > max_chars {
                        self.next = walk
                            .forward_next(position)
                            .await?
                            .map(|next| (next, TextUnitDirection::Forward));
                        return Ok(());
                    }
                }
                chars += row.char_count;
                self.push(walk, position.0, position.1, row).await?;
                position.1 += 1;
            }
        }
    }

    async fn backward(
        &mut self,
        walk: &mut Walk<'_>,
        start: (usize, usize),
        count_limit: Option<usize>,
        max_chars: Option<usize>,
    ) -> Result<(), ReadingError> {
        let mut end = start;
        let mut chars = 0usize;
        loop {
            if count_limit.is_some_and(|limit| self.items.len() >= limit) {
                self.next = walk
                    .backward_next(end)
                    .await?
                    .map(|next| (next, TextUnitDirection::Backward));
                return Ok(());
            }
            if end.1 == 0 {
                match walk.backward_next(end).await? {
                    Some(previous) => {
                        end = previous;
                        continue;
                    }
                    None => return Ok(()),
                }
            }
            let total = walk.total(end.0).await?;
            if end.1 > total {
                // A cursor may name a position past the end of its section; clamp it so every
                // returned item keeps its true stream position.
                end.1 = total;
                if end.1 == 0 {
                    continue;
                }
            }
            let want = count_limit
                .map(|limit| (limit - self.items.len()).min(FETCH_CHUNK))
                .unwrap_or(FETCH_CHUNK);
            let rows = walk
                .items(end.0, TextUnitDirection::Backward, end.1, want)
                .await?;
            if rows.is_empty() {
                match walk.backward_next((end.0, 0)).await? {
                    Some(previous) => {
                        end = previous;
                        continue;
                    }
                    None => return Ok(()),
                }
            }
            for row in rows.into_iter().rev() {
                if count_limit.is_some_and(|limit| self.items.len() >= limit) {
                    self.next = walk
                        .backward_next(end)
                        .await?
                        .map(|next| (next, TextUnitDirection::Backward));
                    return Ok(());
                }
                if let Some(max_chars) = max_chars {
                    if self.items.is_empty() && row.char_count > max_chars {
                        return Err(ReadingError::too_long(row.char_count));
                    }
                    if chars + row.char_count > max_chars {
                        self.next = walk
                            .backward_next(end)
                            .await?
                            .map(|next| (next, TextUnitDirection::Backward));
                        return Ok(());
                    }
                }
                chars += row.char_count;
                end.1 -= 1;
                self.unshift(walk, end.0, end.1, row).await?;
            }
        }
    }

    async fn push(
        &mut self,
        walk: &mut Walk<'_>,
        section: usize,
        position: usize,
        row: StoredStreamItem,
    ) -> Result<(), ReadingError> {
        let at = self.items.len();
        self.record(walk, section, position, at).await?;
        let item = render(walk, section, &row, self.unit, self.handle).await?;
        self.items.push(item);
        Ok(())
    }

    async fn unshift(
        &mut self,
        walk: &mut Walk<'_>,
        section: usize,
        position: usize,
        row: StoredStreamItem,
    ) -> Result<(), ReadingError> {
        // Backward fills from the end, so every new item goes before the ones already collected.
        for entry in &mut self.sections {
            entry.at += 1;
        }
        if let Some(anchor) = self.anchor.as_mut() {
            *anchor += 1;
        }
        self.record(walk, section, position, 0).await?;
        let item = render(walk, section, &row, self.unit, self.handle).await?;
        self.items.insert(0, item);
        Ok(())
    }

    async fn record(
        &mut self,
        walk: &mut Walk<'_>,
        section: usize,
        position: usize,
        at: usize,
    ) -> Result<(), ReadingError> {
        if self.anchor_at == Some((section, position)) {
            self.anchor = Some(at);
        }
        if position == 0 {
            let (section_key, title) = walk.banner(section).await?;
            self.sections.push(ReadReadingSection {
                at,
                handle: SectionHandle {
                    document: self.handle.clone(),
                    section_key,
                }
                .to_string(),
                title,
            });
        }
        Ok(())
    }

    async fn finish(mut self, walk: &mut Walk<'_>) -> Result<ReadReadingResult, ReadingError> {
        let next = match self.next {
            Some(((section, position), TextUnitDirection::Forward)) => {
                // Forward `next` is the next item's own handle, so a caller passes it back as `from`.
                let section_handle = SectionHandle {
                    document: self.handle.clone(),
                    section_key: walk.section_key(section).await?,
                };
                let row = walk
                    .items(section, TextUnitDirection::Forward, position, 1)
                    .await?
                    .into_iter()
                    .next();
                row.map(|row| item_handle(&section_handle, &row))
            }
            Some(((section, position), TextUnitDirection::Backward)) => {
                let section_key = walk.section_key(section).await?;
                Some(
                    ReadingCursor {
                        section: SectionHandle {
                            document: self.handle.clone(),
                            section_key,
                        },
                        unit: match self.unit {
                            RequestedTextUnitKind::Sentence => CursorUnit::Sentence,
                            RequestedTextUnitKind::Paragraph => CursorUnit::Paragraph,
                        },
                        position,
                    }
                    .to_string(),
                )
            }
            None => None,
        };
        Ok(ReadReadingResult {
            items: std::mem::take(&mut self.items),
            sections: std::mem::take(&mut self.sections),
            next,
            anchor: self.anchor,
        })
    }
}

async fn render(
    walk: &mut Walk<'_>,
    section: usize,
    row: &StoredStreamItem,
    unit: RequestedTextUnitKind,
    document: &DocumentHandle,
) -> Result<ReadReadingItem, ReadingError> {
    let section_key = walk.section_key(section).await?;
    let handle = SectionHandle {
        document: document.clone(),
        section_key,
    };
    Ok(ReadReadingItem {
        handle: item_handle(&handle, row),
        // Only the sentence stream returns a whole paragraph; then the item carries the v1 content
        // class when it is coarse or non-prose, so the caller can tell a table from prose.
        content_class: match (unit, &row.unit) {
            (RequestedTextUnitKind::Sentence, OwnedStreamUnit::Paragraph(_)) => {
                item_kind(row.content_class).map(str::to_owned)
            }
            _ => None,
        },
        text: row_text(row),
    })
}

/// The handle of one stored stream item: a sentence handle, or a paragraph handle when the item is a
/// coarse paragraph or the stream itself is the paragraph stream.
fn item_handle(section: &SectionHandle, row: &StoredStreamItem) -> String {
    match &row.unit {
        OwnedStreamUnit::Sentence(sentence) => {
            sentence_handle(section, sentence.paragraph_index, sentence.sentence_index)
        }
        OwnedStreamUnit::Paragraph(paragraph) => {
            paragraph_handle(section, paragraph.paragraph_index)
        }
    }
}

fn row_text(row: &StoredStreamItem) -> String {
    match &row.unit {
        OwnedStreamUnit::Sentence(sentence) => sentence.text.clone(),
        OwnedStreamUnit::Paragraph(paragraph) => paragraph.text.clone(),
    }
}
