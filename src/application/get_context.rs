use std::sync::Arc;

use crate::application::derived_text_units::DerivedTextUnits;
use crate::application::get_text_units::{
    EffectiveTextUnitKind, RequestedTextUnitKind, TextUnitCoveragePolicy, TextUnitDirection,
};
use crate::application::light_sections::SectionTexts;
use crate::application::locator_resolution::{ResolvedLocatorKind, resolve_text_locator_at};
use crate::application::ports::{
    ApplicationError, DocumentRepository, OwnedStreamUnit, StoredStreamItem, TextUnitIndex,
    TextUnitStreamKey,
};
use crate::application::reading_support::{
    content_response_limit, render_section_shallow, truncate_chars,
};
use crate::application::section_view::{DocumentView, carrier_section};
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
        let context = self.load_context(&command.document_id).await?;
        let mut texts = self.section_texts(&context);
        section_neighbor_result(
            &context,
            &command.section_id,
            command.before,
            command.after,
            command.max_chars,
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

        let context = self.load_context(&command.document_id).await?;
        let units = self.text_units(&context);
        let mut texts = self.section_texts(&context);
        let resolved = resolve_target(&context, command.target, units.as_ref(), &mut texts).await?;

        match command.relation {
            ContextRelation::Neighbor {
                unit: ContextUnit::Section,
                before,
                after,
            } => {
                require_anchor_kind(&resolved, ContextItemKind::Section)?;
                section_neighbor_result(
                    &context,
                    &resolved.locator.owner_section_id,
                    before,
                    after,
                    command.max_chars,
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
                    &context,
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
                    &context,
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
                paragraph_container_result(&context, resolved, command.max_chars, units.as_ref())
                    .await
            }
            ContextRelation::Container {
                kind: ContextContainerKind::Section,
            } => section_container_result(&context, resolved, command.max_chars, &mut texts).await,
            ContextRelation::Structural { kind } => {
                structural_result(&context, resolved, kind, command.max_chars).await
            }
        }
    }

    /// The document one context call works on: the header and one row per section question, and —
    /// only when the units come from `DerivedTextUnits` — the full document, which that fallback
    /// segments itself.
    async fn load_context(&self, id: &DocumentId) -> Result<ContextDocument, ApplicationError> {
        let (view, full, normalized_document_hash) = match self.text_unit_index {
            Some(_) => {
                let (view, hash) = DocumentView::load_with_hash(Arc::clone(&self.repository), id)
                    .await?
                    .ok_or(ApplicationError::DocumentNotFound)?;
                (view, None, hash)
            }
            None => {
                let (document, hash) = self
                    .repository
                    .get_with_hash(id)
                    .await?
                    .ok_or(ApplicationError::DocumentNotFound)?;
                let view = DocumentView::from_document(
                    Arc::clone(&self.repository),
                    Arc::clone(&document),
                    &hash,
                );
                (view, Some(document), hash)
            }
        };
        let carrier = view.document();
        Ok(ContextDocument {
            view,
            carrier,
            full,
            normalized_document_hash,
        })
    }

    fn section_texts<'a>(&self, context: &'a ContextDocument) -> SectionTexts<'a> {
        match &context.full {
            Some(document) => SectionTexts::from_document(document),
            None => SectionTexts::from_repository(&context.carrier, Arc::clone(&self.repository)),
        }
    }

    fn text_units(&self, context: &ContextDocument) -> Arc<dyn TextUnitIndex> {
        match &self.text_unit_index {
            Some(index) => Arc::clone(index),
            None => Arc::new(DerivedTextUnits::new(Arc::clone(
                context
                    .full
                    .as_ref()
                    .expect("the derived path loads the full document"),
            ))),
        }
    }
}

/// The document one context call works on.
struct ContextDocument {
    view: DocumentView,
    /// The identity-only document the locator constructors read: `id`, `source`, `content_hash`
    /// and no tree. Never rendered.
    carrier: Document,
    /// The full document, only when the units are derived from it.
    full: Option<Arc<Document>>,
    normalized_document_hash: NormalizedDocumentHash,
}

impl ContextDocument {
    fn document(&self) -> &Document {
        &self.carrier
    }

    fn normalized_hash(&self) -> &NormalizedDocumentHash {
        &self.normalized_document_hash
    }
}

#[derive(Clone, Debug)]
struct ResolvedTarget {
    locator: TextLocator,
    kind: ContextItemKind,
}

async fn resolve_target(
    context: &ContextDocument,
    target: ContextTarget,
    units: &dyn TextUnitIndex,
    texts: &mut SectionTexts<'_>,
) -> Result<ResolvedTarget, ApplicationError> {
    match target {
        ContextTarget::Section(section_id) => {
            let row = context
                .view
                .section_by_id(&section_id)
                .await?
                .ok_or(ApplicationError::SectionNotFound)?;
            Ok(ResolvedTarget {
                locator: TextLocator::for_section_with_hash(
                    context.document(),
                    &carrier_section(&row),
                    context.normalized_hash(),
                ),
                kind: ContextItemKind::Section,
            })
        }
        ContextTarget::Locator(locator) => {
            let resolved = resolve_text_locator_at(
                context.document(),
                &context.view,
                &locator,
                context.normalized_hash(),
                units,
                texts,
            )
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

/// Section neighbours are depth-first document order, which is exactly `section_key` order, so the
/// window is one key-range read: `before` rows below the owner's key and `after` rows above it.
async fn section_neighbor_result(
    context: &ContextDocument,
    section_id: &SectionId,
    before: usize,
    after: usize,
    max_chars: Option<usize>,
    texts: &mut SectionTexts<'_>,
) -> Result<GetContextResult, ApplicationError> {
    let owner = context
        .view
        .section_by_id(section_id)
        .await?
        .ok_or(ApplicationError::SectionNotFound)?;
    let before = i64::try_from(before).unwrap_or(i64::MAX);
    let after = i64::try_from(after).unwrap_or(i64::MAX);
    let sections = context
        .view
        .key_range(
            owner.section_key.saturating_sub(before),
            owner.section_key.saturating_add(after),
        )
        .await?;
    // Each rendered neighbour's own text, and nothing else: `before + after + 1` sections.
    let mut rendered = Vec::with_capacity(sections.len());
    for row in &sections {
        let content = texts.text(&row.id).await?;
        rendered.push(render_section_shallow(&carrier_section(row), &content));
    }
    let rendered = rendered.join("\n\n");
    let (content, truncated) = truncate_chars(rendered, max_chars);
    let owner_section = carrier_section(&owner);
    let anchor_locator = TextLocator::for_section_with_hash(
        context.document(),
        &owner_section,
        context.normalized_hash(),
    );
    let items = sections
        .iter()
        .map(|row| {
            let section = carrier_section(row);
            ContextItem {
                title: Some(section.title.clone()),
                content: None,
                locator: TextLocator::for_section_with_hash(
                    context.document(),
                    &section,
                    context.normalized_hash(),
                ),
                role: relative_role(row.section_key, owner.section_key),
                effective_kind: ContextItemKind::Section,
                content_class: None,
                degradation: None,
            }
        })
        .collect();

    Ok(GetContextResult {
        document_id: context.document().id.clone(),
        source: context.document().source.clone(),
        owner_section_id: owner.id.clone(),
        content,
        location: owner.location.clone(),
        truncated,
        complete: !truncated,
        anchor_locator,
        relation: ContextRelation::Neighbor {
            unit: ContextUnit::Section,
            before: usize::try_from(before).unwrap_or(usize::MAX),
            after: usize::try_from(after).unwrap_or(usize::MAX),
        },
        items,
    })
}

async fn text_unit_neighbor_result(
    context: &ContextDocument,
    target: ResolvedTarget,
    unit: ContextUnit,
    before: usize,
    after: usize,
    max_chars: Option<usize>,
    units: &dyn TextUnitIndex,
) -> Result<GetContextResult, ApplicationError> {
    let row = context
        .view
        .section_by_id(&target.locator.owner_section_id)
        .await?
        .ok_or(ApplicationError::SectionNotFound)?;
    let section = carrier_section(&row);
    let document = context.document();

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
        let mut item = context_item_from_row(document, &section, row, unit);
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
    context: &ContextDocument,
    target: ResolvedTarget,
    max_chars: Option<usize>,
    units: &dyn TextUnitIndex,
) -> Result<GetContextResult, ApplicationError> {
    if target.kind == ContextItemKind::Section {
        return Err(ApplicationError::InvalidRequest(
            "container(kind=paragraph) requires a Paragraph or Sentence locator".into(),
        ));
    }

    let row = context
        .view
        .section_by_id(&target.locator.owner_section_id)
        .await?
        .ok_or(ApplicationError::SectionNotFound)?;
    let section = carrier_section(&row);
    let document = context.document();
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
        locator: TextLocator::for_paragraph(document, &section, &paragraph),
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
    context: &ContextDocument,
    target: ResolvedTarget,
    max_chars: Option<usize>,
    texts: &mut SectionTexts<'_>,
) -> Result<GetContextResult, ApplicationError> {
    let row = context
        .view
        .section_by_id(&target.locator.owner_section_id)
        .await?
        .ok_or(ApplicationError::SectionNotFound)?;
    let section = carrier_section(&row);
    let section_text = texts.text(&section.id).await?;
    let (content, truncated) =
        truncate_chars(render_section_shallow(&section, &section_text), max_chars);

    Ok(GetContextResult {
        document_id: context.document().id.clone(),
        source: context.document().source.clone(),
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
            context.document(),
            &section,
            ContextItemRole::Container,
            context.normalized_hash(),
        )],
    })
}

/// The structural relations, each one point query: `section_ancestors` for the parent chain,
/// `section_children` for siblings and for children.
async fn structural_result(
    context: &ContextDocument,
    target: ResolvedTarget,
    kind: StructuralContextKind,
    max_chars: Option<usize>,
) -> Result<GetContextResult, ApplicationError> {
    let owner_row = context
        .view
        .section_by_id(&target.locator.owner_section_id)
        .await?
        .ok_or(ApplicationError::SectionNotFound)?;
    let owner = carrier_section(&owner_row);
    // No limit: the over-limit error names the exact item count, and a section with more children
    // than the limit costs exactly the walk the tree version did.
    let limit = usize::MAX;

    let rows = match kind {
        StructuralContextKind::OwnerSection => vec![owner_row.clone()],
        StructuralContextKind::Ancestors => context.view.ancestors(owner_row.section_key).await?,
        StructuralContextKind::Siblings => {
            let candidates = context.view.children(owner_row.parent_key, limit).await?;
            candidates
                .into_iter()
                .filter(|row| row.id != owner.id)
                .collect()
        }
        StructuralContextKind::Children => {
            context
                .view
                .children(Some(owner_row.section_key), limit)
                .await?
        }
    };
    if rows.len() > MAX_STRUCTURAL_CONTEXT_ITEMS {
        return Err(ApplicationError::ResourceLimitExceeded(format!(
            "structural context contains {} items, exceeding limit {MAX_STRUCTURAL_CONTEXT_ITEMS}",
            rows.len()
        )));
    }

    let sections = rows.iter().map(carrier_section).collect::<Vec<Section>>();
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
                context.document(),
                section,
                ContextItemRole::Structural,
                context.normalized_hash(),
            )
        })
        .collect();

    Ok(GetContextResult {
        document_id: context.document().id.clone(),
        source: context.document().source.clone(),
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

fn relative_role<T: Ord>(index: T, anchor_index: T) -> ContextItemRole {
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
