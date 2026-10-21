use std::sync::Arc;

use crate::application::derived_text_units::DerivedTextUnits;
use crate::application::get_text_units::{
    EffectiveTextUnitKind, RequestedTextUnitKind, TextUnitCoveragePolicy, TextUnitDirection,
};
use crate::application::light_sections::SectionTexts;
use crate::application::locator_resolution::{ResolvedLocatorKind, resolve_text_locator};
use crate::application::ports::{
    ApplicationError, DocumentRepository, OwnedStreamUnit, StoredStreamItem, TextUnitIndex,
    TextUnitStreamKey,
};
use crate::application::reading_support::{
    content_response_limit, flatten_sections, render_section_shallow, truncate_chars,
};
use crate::domain::{
    Document, DocumentId, DocumentSource, Location, NormalizedDocumentHash, ParagraphContentClass,
    Section, SectionId, TEXT_SEGMENTATION_VERSION, TextLocator,
};

const MAX_CONTEXT_WINDOW: usize = 20;
const MAX_STRUCTURAL_CONTEXT_ITEMS: usize = 100;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GetContextCommand {
    pub document_id: DocumentId,
    pub section_id: SectionId,
    pub before: usize,
    pub after: usize,
    pub max_chars: Option<usize>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GetStructuredContextCommand {
    pub document_id: DocumentId,
    pub target: ContextTarget,
    pub relation: ContextRelation,
    pub max_chars: Option<usize>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ContextTarget {
    Section(SectionId),
    Locator(TextLocator),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContextUnit {
    Section,
    Paragraph,
    Sentence,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContextContainerKind {
    Paragraph,
    Section,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StructuralContextKind {
    OwnerSection,
    Ancestors,
    Siblings,
    Children,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ContextRelation {
    Neighbor {
        unit: ContextUnit,
        before: usize,
        after: usize,
    },
    Container {
        kind: ContextContainerKind,
    },
    Structural {
        kind: StructuralContextKind,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContextItemRole {
    Before,
    Anchor,
    After,
    Container,
    Structural,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContextItemKind {
    Section,
    Paragraph,
    Sentence,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContextItem {
    pub title: Option<String>,
    pub content: Option<String>,
    pub locator: TextLocator,
    pub role: ContextItemRole,
    pub effective_kind: ContextItemKind,
    pub content_class: Option<String>,
    pub degradation: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GetContextResult {
    pub document_id: DocumentId,
    pub source: DocumentSource,
    pub owner_section_id: SectionId,
    pub content: String,
    pub location: Location,
    pub truncated: bool,
    pub complete: bool,
    pub anchor_locator: TextLocator,
    pub relation: ContextRelation,
    pub items: Vec<ContextItem>,
}

pub struct GetContextUseCase {
    repository: Arc<dyn DocumentRepository>,
    text_unit_index: Option<Arc<dyn TextUnitIndex>>,
}

impl GetContextUseCase {
    pub fn new(repository: Arc<dyn DocumentRepository>) -> Self {
        Self {
            repository,
            text_unit_index: None,
        }
    }

    pub fn with_text_unit_index(mut self, index: Arc<dyn TextUnitIndex>) -> Self {
        self.text_unit_index = Some(index);
        self
    }

    pub async fn execute(
        &self,
        command: GetContextCommand,
    ) -> Result<GetContextResult, ApplicationError> {
        validate_window(command.before, command.after)?;
        let (document, normalized_document_hash) = self.load_document(&command.document_id).await?;
        let mut texts = self.section_texts(&document);
        section_neighbor_result(
            &document,
            &command.section_id,
            command.before,
            command.after,
            command.max_chars,
            &normalized_document_hash,
            &mut texts,
        )
        .await
    }

    pub async fn execute_structured(
        &self,
        command: GetStructuredContextCommand,
    ) -> Result<GetContextResult, ApplicationError> {
        if let ContextRelation::Neighbor { before, after, .. } = &command.relation {
            validate_window(*before, *after)?;
        }

        let (document, normalized_document_hash) = self.load_document(&command.document_id).await?;
        let units = self.text_units(&document);
        let mut texts = self.section_texts(&document);
        let resolved = resolve_target(
            &document,
            command.target,
            &normalized_document_hash,
            units.as_ref(),
            &mut texts,
        )
        .await?;

        match command.relation {
            ContextRelation::Neighbor {
                unit: ContextUnit::Section,
                before,
                after,
            } => {
                require_anchor_kind(&resolved, ContextItemKind::Section)?;
                section_neighbor_result(
                    &document,
                    &resolved.locator.owner_section_id,
                    before,
                    after,
                    command.max_chars,
                    &normalized_document_hash,
                    &mut texts,
                )
                .await
            }
            ContextRelation::Neighbor {
                unit: ContextUnit::Paragraph,
                before,
                after,
            } => {
                require_anchor_kind(&resolved, ContextItemKind::Paragraph)?;
                text_unit_neighbor_result(
                    &document,
                    resolved,
                    ContextUnit::Paragraph,
                    before,
                    after,
                    command.max_chars,
                    units.as_ref(),
                )
                .await
            }
            ContextRelation::Neighbor {
                unit: ContextUnit::Sentence,
                before,
                after,
            } => {
                require_anchor_kind(&resolved, ContextItemKind::Sentence)?;
                text_unit_neighbor_result(
                    &document,
                    resolved,
                    ContextUnit::Sentence,
                    before,
                    after,
                    command.max_chars,
                    units.as_ref(),
                )
                .await
            }
            ContextRelation::Container {
                kind: ContextContainerKind::Paragraph,
            } => {
                paragraph_container_result(&document, resolved, command.max_chars, units.as_ref())
                    .await
            }
            ContextRelation::Container {
                kind: ContextContainerKind::Section,
            } => {
                section_container_result(
                    &document,
                    resolved,
                    command.max_chars,
                    &normalized_document_hash,
                    &mut texts,
                )
                .await
            }
            ContextRelation::Structural { kind } => structural_result(
                &document,
                resolved,
                kind,
                command.max_chars,
                &normalized_document_hash,
            ),
        }
    }

    /// The light document when a stored text-unit index serves the units — every reader then asks
    /// the repository for the text of exactly the sections it renders — and the full document when
    /// the units come from `DerivedTextUnits`, which segments the document's own text.
    async fn load_document(
        &self,
        id: &DocumentId,
    ) -> Result<(Arc<Document>, NormalizedDocumentHash), ApplicationError> {
        let loaded = match self.text_unit_index {
            Some(_) => self.repository.get_light_with_hash(id).await?,
            None => self.repository.get_with_hash(id).await?,
        };
        loaded.ok_or(ApplicationError::DocumentNotFound)
    }

    fn section_texts<'a>(&self, document: &'a Arc<Document>) -> SectionTexts<'a> {
        match self.text_unit_index {
            Some(_) => SectionTexts::from_repository(document, self.repository.as_ref()),
            None => SectionTexts::from_document(document),
        }
    }

    fn text_units(&self, document: &Arc<Document>) -> Arc<dyn TextUnitIndex> {
        match &self.text_unit_index {
            Some(index) => Arc::clone(index),
            None => Arc::new(DerivedTextUnits::new(Arc::clone(document))),
        }
    }
}

#[derive(Clone, Debug)]
struct ResolvedTarget {
    locator: TextLocator,
    kind: ContextItemKind,
}

async fn resolve_target(
    document: &Document,
    target: ContextTarget,
    normalized_document_hash: &NormalizedDocumentHash,
    units: &dyn TextUnitIndex,
    texts: &mut SectionTexts<'_>,
) -> Result<ResolvedTarget, ApplicationError> {
    match target {
        ContextTarget::Section(section_id) => {
            let section = document
                .find_section(&section_id)
                .ok_or(ApplicationError::SectionNotFound)?;
            Ok(ResolvedTarget {
                locator: TextLocator::for_section_with_hash(
                    document,
                    section,
                    normalized_document_hash,
                ),
                kind: ContextItemKind::Section,
            })
        }
        ContextTarget::Locator(locator) => {
            let resolved =
                resolve_text_locator(document, &locator, normalized_document_hash, units, texts)
                    .await?;
            let kind = match resolved.kind {
                ResolvedLocatorKind::Section => ContextItemKind::Section,
                ResolvedLocatorKind::Paragraph => ContextItemKind::Paragraph,
                ResolvedLocatorKind::Sentence => ContextItemKind::Sentence,
                ResolvedLocatorKind::CharacterRange => {
                    return Err(ApplicationError::InvalidRequest(
                        "get_context does not accept CharacterRange locator anchors".into(),
                    ));
                }
            };
            Ok(ResolvedTarget {
                locator: resolved.locator,
                kind,
            })
        }
    }
}

fn validate_window(before: usize, after: usize) -> Result<(), ApplicationError> {
    if before > MAX_CONTEXT_WINDOW || after > MAX_CONTEXT_WINDOW {
        return Err(ApplicationError::InvalidRequest(format!(
            "context window must not exceed {MAX_CONTEXT_WINDOW} items on either side"
        )));
    }
    Ok(())
}

fn require_anchor_kind(
    target: &ResolvedTarget,
    expected: ContextItemKind,
) -> Result<(), ApplicationError> {
    if target.kind != expected {
        return Err(ApplicationError::InvalidRequest(format!(
            "neighbor relation requires a {expected:?} locator anchor, got {:?}",
            target.kind
        )));
    }
    Ok(())
}

async fn section_neighbor_result(
    document: &Document,
    section_id: &SectionId,
    before: usize,
    after: usize,
    max_chars: Option<usize>,
    normalized_document_hash: &NormalizedDocumentHash,
    texts: &mut SectionTexts<'_>,
) -> Result<GetContextResult, ApplicationError> {
    let mut sections = Vec::new();
    flatten_sections(&document.root_sections, &mut sections);
    let owner_index = sections
        .iter()
        .position(|section| section.id == *section_id)
        .ok_or(ApplicationError::SectionNotFound)?;
    let owner = sections[owner_index];

    let start = owner_index.saturating_sub(before);
    let end = (owner_index + after + 1).min(sections.len());
    // Each rendered neighbour's own text, and nothing else: `before + after + 1` sections.
    let mut rendered = Vec::with_capacity(end - start);
    for section in &sections[start..end] {
        let content = texts.text(&section.id).await?;
        rendered.push(render_section_shallow(section, &content));
    }
    let rendered = rendered.join("\n\n");
    let (content, truncated) = truncate_chars(rendered, max_chars);
    let anchor_locator =
        TextLocator::for_section_with_hash(document, owner, normalized_document_hash);
    let items = sections[start..end]
        .iter()
        .enumerate()
        .map(|(offset, section)| {
            let absolute = start + offset;
            ContextItem {
                title: Some(section.title.clone()),
                content: None,
                locator: TextLocator::for_section_with_hash(
                    document,
                    section,
                    normalized_document_hash,
                ),
                role: relative_role(absolute, owner_index),
                effective_kind: ContextItemKind::Section,
                content_class: None,
                degradation: None,
            }
        })
        .collect();

    Ok(GetContextResult {
        document_id: document.id.clone(),
        source: document.source.clone(),
        owner_section_id: owner.id.clone(),
        content,
        location: owner.location.clone(),
        truncated,
        complete: !truncated,
        anchor_locator,
        relation: ContextRelation::Neighbor {
            unit: ContextUnit::Section,
            before,
            after,
        },
        items,
    })
}

async fn text_unit_neighbor_result(
    document: &Document,
    target: ResolvedTarget,
    unit: ContextUnit,
    before: usize,
    after: usize,
    max_chars: Option<usize>,
    units: &dyn TextUnitIndex,
) -> Result<GetContextResult, ApplicationError> {
    let section = document
        .find_section(&target.locator.owner_section_id)
        .ok_or(ApplicationError::SectionNotFound)?;

    let (requested_kind, unit_kind) = match unit {
        ContextUnit::Paragraph => (
            RequestedTextUnitKind::Paragraph,
            EffectiveTextUnitKind::Paragraph,
        ),
        ContextUnit::Sentence => (
            RequestedTextUnitKind::Sentence,
            EffectiveTextUnitKind::Sentence,
        ),
        ContextUnit::Section => unreachable!("Section neighbors use section_neighbor_result"),
    };
    let key = TextUnitStreamKey {
        document_id: &document.id,
        section_id: &section.id,
        kind: requested_kind,
        policy: TextUnitCoveragePolicy::PreserveSource,
    };
    let summary = units
        .section_stream_summary(key)
        .await?
        .ok_or_else(|| stale_text_units(document))?;
    let paragraph_index = target
        .locator
        .paragraph_index
        .ok_or_else(|| not_in_stream(unit))?;
    let anchor_position = units
        .section_stream_position(
            key,
            unit_kind,
            paragraph_index,
            target.locator.sentence_index,
        )
        .await?
        .ok_or_else(|| not_in_stream(unit))?;
    let start = anchor_position.saturating_sub(before);
    let end = (anchor_position + after + 1).min(summary.total_items);
    let rows = units
        .section_stream_items(key, TextUnitDirection::Forward, start, end - start)
        .await?;

    let mut anchor_found = false;
    let mut items = Vec::with_capacity(rows.len());
    for row in rows {
        let position = row.position;
        let mut item = context_item_from_row(document, section, row, unit);
        if position == anchor_position {
            if item.locator != target.locator {
                return Err(not_in_stream(unit));
            }
            anchor_found = true;
        }
        item.role = relative_role(position, anchor_position);
        items.push(item);
    }
    if !anchor_found {
        return Err(not_in_stream(unit));
    }
    ensure_precise_item_budget(&items, max_chars)?;

    Ok(GetContextResult {
        document_id: document.id.clone(),
        source: document.source.clone(),
        owner_section_id: section.id.clone(),
        content: String::new(),
        location: section.location.clone(),
        truncated: false,
        complete: true,
        anchor_locator: target.locator,
        relation: ContextRelation::Neighbor {
            unit,
            before,
            after,
        },
        items,
    })
}

async fn paragraph_container_result(
    document: &Document,
    target: ResolvedTarget,
    max_chars: Option<usize>,
    units: &dyn TextUnitIndex,
) -> Result<GetContextResult, ApplicationError> {
    if target.kind == ContextItemKind::Section {
        return Err(ApplicationError::InvalidRequest(
            "container(kind=paragraph) requires a Paragraph or Sentence locator".into(),
        ));
    }

    let section = document
        .find_section(&target.locator.owner_section_id)
        .ok_or(ApplicationError::SectionNotFound)?;
    let paragraph_index = target.locator.paragraph_index.ok_or_else(|| {
        ApplicationError::InvalidLocator("text locator has no paragraph ownership".into())
    })?;

    let key = TextUnitStreamKey {
        document_id: &document.id,
        section_id: &section.id,
        kind: RequestedTextUnitKind::Paragraph,
        policy: TextUnitCoveragePolicy::PreserveSource,
    };
    let missing_paragraph = || {
        ApplicationError::StaleLocator(format!(
            "paragraph {paragraph_index} no longer exists in section {}",
            section.id.0
        ))
    };
    let position = units
        .section_stream_position(key, EffectiveTextUnitKind::Paragraph, paragraph_index, None)
        .await?
        .ok_or_else(missing_paragraph)?;
    let content_class = units
        .section_stream_items(key, TextUnitDirection::Forward, position, 1)
        .await?
        .into_iter()
        .next()
        .map(|row| row.content_class)
        .ok_or_else(missing_paragraph)?;
    let paragraph = match units
        .section_unit(
            &document.id,
            &section.id,
            EffectiveTextUnitKind::Paragraph,
            paragraph_index,
            None,
        )
        .await?
    {
        Some(OwnedStreamUnit::Paragraph(paragraph)) => paragraph,
        _ => return Err(missing_paragraph()),
    };

    let item = ContextItem {
        title: None,
        content: Some(paragraph.text.clone()),
        locator: TextLocator::for_paragraph(document, section, &paragraph),
        role: ContextItemRole::Container,
        effective_kind: ContextItemKind::Paragraph,
        content_class: Some(content_class.as_str().into()),
        degradation: None,
    };
    let items = vec![item];
    ensure_precise_item_budget(&items, max_chars)?;

    Ok(GetContextResult {
        document_id: document.id.clone(),
        source: document.source.clone(),
        owner_section_id: section.id.clone(),
        content: String::new(),
        location: section.location.clone(),
        truncated: false,
        complete: true,
        anchor_locator: target.locator,
        relation: ContextRelation::Container {
            kind: ContextContainerKind::Paragraph,
        },
        items,
    })
}

fn context_item_from_row(
    document: &Document,
    section: &Section,
    row: StoredStreamItem,
    stream_unit: ContextUnit,
) -> ContextItem {
    let degradation = match (&row.unit, stream_unit) {
        (OwnedStreamUnit::Paragraph(_), ContextUnit::Sentence) => {
            Some(coarse_sentence_context_degradation(row.content_class).into())
        }
        _ => None,
    };
    let (content, locator, effective_kind) = match &row.unit {
        OwnedStreamUnit::Paragraph(unit) => (
            unit.text.clone(),
            TextLocator::for_paragraph(document, section, unit),
            ContextItemKind::Paragraph,
        ),
        OwnedStreamUnit::Sentence(unit) => (
            unit.text.clone(),
            TextLocator::for_sentence(document, section, unit),
            ContextItemKind::Sentence,
        ),
    };
    ContextItem {
        title: None,
        content: Some(content),
        locator,
        role: ContextItemRole::Anchor,
        effective_kind,
        content_class: Some(row.content_class.as_str().into()),
        degradation,
    }
}

fn not_in_stream(unit: ContextUnit) -> ApplicationError {
    ApplicationError::StaleLocator(format!(
        "locator is not part of the current {} context stream",
        match unit {
            ContextUnit::Paragraph => "paragraph",
            ContextUnit::Sentence => "sentence",
            ContextUnit::Section => "section",
        }
    ))
}

fn stale_text_units(document: &Document) -> ApplicationError {
    ApplicationError::StaleDocument(format!(
        "text units of document {} are not stored for {TEXT_SEGMENTATION_VERSION}; open the document again",
        document.id.0
    ))
}

async fn section_container_result(
    document: &Document,
    target: ResolvedTarget,
    max_chars: Option<usize>,
    normalized_document_hash: &NormalizedDocumentHash,
    texts: &mut SectionTexts<'_>,
) -> Result<GetContextResult, ApplicationError> {
    let section = document
        .find_section(&target.locator.owner_section_id)
        .ok_or(ApplicationError::SectionNotFound)?;
    let section_text = texts.text(&section.id).await?;
    let (content, truncated) =
        truncate_chars(render_section_shallow(section, &section_text), max_chars);

    Ok(GetContextResult {
        document_id: document.id.clone(),
        source: document.source.clone(),
        owner_section_id: section.id.clone(),
        content,
        location: section.location.clone(),
        truncated,
        complete: !truncated,
        anchor_locator: target.locator,
        relation: ContextRelation::Container {
            kind: ContextContainerKind::Section,
        },
        items: vec![section_context_item(
            document,
            section,
            ContextItemRole::Container,
            normalized_document_hash,
        )],
    })
}

fn structural_result(
    document: &Document,
    target: ResolvedTarget,
    kind: StructuralContextKind,
    max_chars: Option<usize>,
    normalized_document_hash: &NormalizedDocumentHash,
) -> Result<GetContextResult, ApplicationError> {
    let owner = document
        .find_section(&target.locator.owner_section_id)
        .ok_or(ApplicationError::SectionNotFound)?;

    let sections = match kind {
        StructuralContextKind::OwnerSection => vec![owner],
        StructuralContextKind::Ancestors => ancestors(document, owner)?,
        StructuralContextKind::Siblings => siblings(document, owner)?,
        StructuralContextKind::Children => owner.children.iter().collect(),
    };
    if sections.len() > MAX_STRUCTURAL_CONTEXT_ITEMS {
        return Err(ApplicationError::ResourceLimitExceeded(format!(
            "structural context contains {} items, exceeding limit {MAX_STRUCTURAL_CONTEXT_ITEMS}",
            sections.len()
        )));
    }

    let metadata_projection = sections
        .iter()
        .map(|section| section.title.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    ensure_string_budget(&metadata_projection, max_chars)?;
    let items = sections
        .iter()
        .map(|section| {
            section_context_item(
                document,
                section,
                ContextItemRole::Structural,
                normalized_document_hash,
            )
        })
        .collect();

    Ok(GetContextResult {
        document_id: document.id.clone(),
        source: document.source.clone(),
        owner_section_id: owner.id.clone(),
        content: String::new(),
        location: owner.location.clone(),
        truncated: false,
        complete: true,
        anchor_locator: target.locator,
        relation: ContextRelation::Structural { kind },
        items,
    })
}

fn ancestors<'a>(
    document: &'a Document,
    owner: &'a Section,
) -> Result<Vec<&'a Section>, ApplicationError> {
    let mut result = Vec::new();
    let mut current = owner;
    while let Some(parent_id) = &current.parent_id {
        let current_id = current.id.0.clone();
        let parent = document.find_section(parent_id).ok_or_else(|| {
            ApplicationError::InvalidRequest(format!(
                "section {current_id} references missing parent {}",
                parent_id.0
            ))
        })?;
        result.push(parent);
        current = parent;
    }
    result.reverse();
    Ok(result)
}

fn siblings<'a>(
    document: &'a Document,
    owner: &'a Section,
) -> Result<Vec<&'a Section>, ApplicationError> {
    let candidates: Vec<&Section> = match &owner.parent_id {
        Some(parent_id) => {
            let parent = document.find_section(parent_id).ok_or_else(|| {
                ApplicationError::InvalidRequest(format!(
                    "section {} references missing parent {}",
                    owner.id.0, parent_id.0
                ))
            })?;
            parent.children.iter().collect()
        }
        None => document.root_sections.iter().collect(),
    };

    Ok(candidates
        .into_iter()
        .filter(|section| section.id != owner.id)
        .collect())
}

fn coarse_sentence_context_degradation(content_class: ParagraphContentClass) -> &'static str {
    match content_class {
        ParagraphContentClass::BlockQuote | ParagraphContentClass::ListItem => {
            "flat_native_container_no_nested_textunit_evidence"
        }
        ParagraphContentClass::CodeBlock
        | ParagraphContentClass::Table
        | ParagraphContentClass::Preformatted => {
            "requested_sentence_context_but_non_prose_is_paragraph_only"
        }
        ParagraphContentClass::ProseOrUnknown | ParagraphContentClass::NativeParagraph => {
            "requested_sentence_context_but_paragraph_is_coarse_only"
        }
    }
}

fn section_context_item(
    document: &Document,
    section: &Section,
    role: ContextItemRole,
    normalized_document_hash: &NormalizedDocumentHash,
) -> ContextItem {
    ContextItem {
        title: Some(section.title.clone()),
        content: None,
        locator: TextLocator::for_section_with_hash(document, section, normalized_document_hash),
        role,
        effective_kind: ContextItemKind::Section,
        content_class: None,
        degradation: None,
    }
}

fn relative_role(index: usize, anchor_index: usize) -> ContextItemRole {
    if index < anchor_index {
        ContextItemRole::Before
    } else if index == anchor_index {
        ContextItemRole::Anchor
    } else {
        ContextItemRole::After
    }
}

fn ensure_precise_item_budget(
    items: &[ContextItem],
    max_chars: Option<usize>,
) -> Result<(), ApplicationError> {
    let limit = content_response_limit(max_chars);
    let mut total = 0usize;
    let mut content_items = 0usize;
    for item in items {
        if let Some(content) = &item.content {
            let item_chars = content.chars().count();
            if item_chars > limit {
                return Err(ApplicationError::ResourceLimitExceeded(format!(
                    "context item contains {item_chars} characters, exceeding max_chars {limit}"
                )));
            }
            if content_items > 0 {
                total = total.saturating_add(2);
            }
            total = total.saturating_add(item_chars);
            content_items += 1;
        }
    }
    if total > limit {
        return Err(ApplicationError::ResourceLimitExceeded(format!(
            "requested precise context contains {total} characters, exceeding max_chars {limit}"
        )));
    }
    Ok(())
}

fn ensure_string_budget(content: &str, max_chars: Option<usize>) -> Result<(), ApplicationError> {
    let limit = content_response_limit(max_chars);
    let chars = content.chars().count();
    if chars > limit {
        return Err(ApplicationError::ResourceLimitExceeded(format!(
            "context metadata contains {chars} characters, exceeding max_chars {limit}"
        )));
    }
    Ok(())
}
