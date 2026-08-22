use std::collections::HashMap;
use std::sync::Arc;

use crate::application::ports::{ApplicationError, DocumentRepository};
use crate::application::reading_support::{
    content_response_limit, flatten_sections, render_section_shallow, truncate_chars,
};
use crate::domain::{
    Document, DocumentId, DocumentSource, Location, ParagraphContentClass, Section, SectionId,
    SentenceEligibility, SentenceParagraphCoverage, SentenceTextUnit, TextLocator, TextUnit,
    TEXT_SEGMENTATION_VERSION,
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
}

impl GetContextUseCase {
    pub fn new(repository: Arc<dyn DocumentRepository>) -> Self {
        Self { repository }
    }

    pub async fn execute(
        &self,
        command: GetContextCommand,
    ) -> Result<GetContextResult, ApplicationError> {
        validate_window(command.before, command.after)?;
        let document = self.load_document(&command.document_id).await?;
        section_neighbor_result(
            &document,
            &command.section_id,
            command.before,
            command.after,
            command.max_chars,
        )
    }

    pub async fn execute_structured(
        &self,
        command: GetStructuredContextCommand,
    ) -> Result<GetContextResult, ApplicationError> {
        if let ContextRelation::Neighbor { before, after, .. } = &command.relation {
            validate_window(*before, *after)?;
        }

        let document = self.load_document(&command.document_id).await?;
        let resolved = resolve_target(&document, command.target)?;

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
                )
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
                )
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
                )
            }
            ContextRelation::Container {
                kind: ContextContainerKind::Paragraph,
            } => paragraph_container_result(&document, resolved, command.max_chars),
            ContextRelation::Container {
                kind: ContextContainerKind::Section,
            } => section_container_result(&document, resolved, command.max_chars),
            ContextRelation::Structural { kind } => {
                structural_result(&document, resolved, kind, command.max_chars)
            }
        }
    }

    async fn load_document(&self, id: &DocumentId) -> Result<Document, ApplicationError> {
        self.repository
            .get(id)
            .await?
            .ok_or(ApplicationError::DocumentNotFound)
    }
}

#[derive(Clone, Debug)]
struct ResolvedTarget {
    locator: TextLocator,
    kind: ContextItemKind,
}

fn resolve_target(
    document: &Document,
    target: ContextTarget,
) -> Result<ResolvedTarget, ApplicationError> {
    match target {
        ContextTarget::Section(section_id) => {
            let section = document
                .find_section(&section_id)
                .ok_or(ApplicationError::SectionNotFound)?;
            Ok(ResolvedTarget {
                locator: TextLocator::for_section(document, section),
                kind: ContextItemKind::Section,
            })
        }
        ContextTarget::Locator(locator) => validate_locator(document, locator),
    }
}

fn validate_locator(
    document: &Document,
    locator: TextLocator,
) -> Result<ResolvedTarget, ApplicationError> {
    if locator.document_id != document.id {
        return Err(ApplicationError::InvalidLocator(format!(
            "locator document {} does not match requested document {}",
            locator.document_id.0, document.id.0
        )));
    }
    if locator.content_hash != document.content_hash {
        return Err(ApplicationError::StaleLocator(format!(
            "raw content hash changed from {} to {}",
            locator.content_hash.0, document.content_hash.0
        )));
    }

    let normalized_hash = document.normalized_document_hash();
    if locator.normalized_document_hash != normalized_hash {
        return Err(ApplicationError::StaleLocator(format!(
            "normalized document hash changed from {} to {}",
            locator.normalized_document_hash.0, normalized_hash.0
        )));
    }

    let section = document
        .find_section(&locator.owner_section_id)
        .ok_or_else(|| {
            ApplicationError::InvalidLocator(format!(
                "locator owner section {} does not exist",
                locator.owner_section_id.0
            ))
        })?;

    match (
        locator.paragraph_index,
        locator.sentence_index,
        locator.normalized_range,
        locator.segmentation_version.as_deref(),
    ) {
        (None, None, None, None) => Ok(ResolvedTarget {
            locator: TextLocator::for_section(document, section),
            kind: ContextItemKind::Section,
        }),
        (Some(paragraph_index), None, Some(range), Some(segmentation_version)) => {
            validate_segmentation_version(segmentation_version)?;
            if paragraph_index == 0 {
                return Err(ApplicationError::InvalidLocator(
                    "paragraph_index must be 1-based".into(),
                ));
            }
            let paragraph = document
                .paragraph_text_units()
                .units
                .into_iter()
                .find(|unit| {
                    unit.owner_section_id == locator.owner_section_id
                        && unit.paragraph_index == paragraph_index
                })
                .ok_or_else(|| {
                    ApplicationError::StaleLocator(format!(
                        "paragraph {paragraph_index} no longer exists in section {}",
                        locator.owner_section_id.0
                    ))
                })?;
            if paragraph.normalized_range != range {
                return Err(ApplicationError::StaleLocator(format!(
                    "paragraph {paragraph_index} normalized range changed"
                )));
            }
            Ok(ResolvedTarget {
                locator: TextLocator::for_paragraph(document, section, &paragraph),
                kind: ContextItemKind::Paragraph,
            })
        }
        (
            Some(paragraph_index),
            Some(sentence_index),
            Some(range),
            Some(segmentation_version),
        ) => {
            validate_segmentation_version(segmentation_version)?;
            if paragraph_index == 0 || sentence_index == 0 {
                return Err(ApplicationError::InvalidLocator(
                    "paragraph_index and sentence_index must be 1-based".into(),
                ));
            }
            let sentence = document
                .sentence_text_units()
                .units
                .into_iter()
                .find(|unit| {
                    unit.owner_section_id == locator.owner_section_id
                        && unit.paragraph_index == paragraph_index
                        && unit.sentence_index == sentence_index
                })
                .ok_or_else(|| {
                    ApplicationError::StaleLocator(format!(
                        "sentence {paragraph_index}.{sentence_index} no longer exists in section {}",
                        locator.owner_section_id.0
                    ))
                })?;
            if sentence.normalized_range != range {
                return Err(ApplicationError::StaleLocator(format!(
                    "sentence {paragraph_index}.{sentence_index} normalized range changed"
                )));
            }
            Ok(ResolvedTarget {
                locator: TextLocator::for_sentence(document, section, &sentence),
                kind: ContextItemKind::Sentence,
            })
        }
        _ => Err(ApplicationError::InvalidLocator(
            "locator Paragraph/Sentence/range/segmentation fields form an invalid shape".into(),
        )),
    }
}

fn validate_segmentation_version(version: &str) -> Result<(), ApplicationError> {
    if version != TEXT_SEGMENTATION_VERSION {
        return Err(ApplicationError::StaleLocator(format!(
            "locator segmentation version {version} is incompatible with {TEXT_SEGMENTATION_VERSION}"
        )));
    }
    Ok(())
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

fn section_neighbor_result(
    document: &Document,
    section_id: &SectionId,
    before: usize,
    after: usize,
    max_chars: Option<usize>,
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
    let rendered = sections[start..end]
        .iter()
        .map(|section| render_section_shallow(section))
        .collect::<Vec<_>>()
        .join("\n\n");
    let (content, truncated) = truncate_chars(rendered, max_chars);
    let anchor_locator = TextLocator::for_section(document, owner);
    let items = sections[start..end]
        .iter()
        .enumerate()
        .map(|(offset, section)| {
            let absolute = start + offset;
            ContextItem {
                title: Some(section.title.clone()),
                content: None,
                locator: TextLocator::for_section(document, section),
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

fn text_unit_neighbor_result(
    document: &Document,
    target: ResolvedTarget,
    unit: ContextUnit,
    before: usize,
    after: usize,
    max_chars: Option<usize>,
) -> Result<GetContextResult, ApplicationError> {
    let section = document
        .find_section(&target.locator.owner_section_id)
        .ok_or(ApplicationError::SectionNotFound)?;

    let stream = match unit {
        ContextUnit::Paragraph => paragraph_context_stream(document, section)?,
        ContextUnit::Sentence => sentence_context_stream(document, section)?,
        ContextUnit::Section => unreachable!("Section neighbors use section_neighbor_result"),
    };

    let anchor_index = stream
        .iter()
        .position(|item| item.locator == target.locator)
        .ok_or_else(|| {
            ApplicationError::StaleLocator(format!(
                "locator is not part of the current {} context stream",
                match unit {
                    ContextUnit::Paragraph => "paragraph",
                    ContextUnit::Sentence => "sentence",
                    ContextUnit::Section => "section",
                }
            ))
        })?;
    let start = anchor_index.saturating_sub(before);
    let end = (anchor_index + after + 1).min(stream.len());
    let mut items = stream[start..end].to_vec();
    for (offset, item) in items.iter_mut().enumerate() {
        item.role = relative_role(start + offset, anchor_index);
    }
    ensure_precise_item_budget(&items, max_chars)?;
    let content = join_item_content(&items);

    Ok(GetContextResult {
        document_id: document.id.clone(),
        source: document.source.clone(),
        owner_section_id: section.id.clone(),
        content,
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

fn paragraph_container_result(
    document: &Document,
    target: ResolvedTarget,
    max_chars: Option<usize>,
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

    let paragraph_set = document.paragraph_text_units();
    let paragraph = paragraph_set
        .units
        .iter()
        .find(|unit| {
            unit.owner_section_id == section.id && unit.paragraph_index == paragraph_index
        })
        .ok_or_else(|| {
            ApplicationError::StaleLocator(format!(
                "paragraph {paragraph_index} no longer exists in section {}",
                section.id.0
            ))
        })?;
    let sentence_set = document.sentence_text_units();
    let coverage = sentence_set
        .coverage
        .iter()
        .find(|coverage| {
            coverage.owner_section_id == section.id
                && coverage.paragraph_index == paragraph_index
        })
        .ok_or_else(|| {
            ApplicationError::InvalidRequest(format!(
                "sentence coverage is unavailable for paragraph {paragraph_index}"
            ))
        })?;

    let mut item = paragraph_context_item(document, section, paragraph, coverage);
    item.role = ContextItemRole::Container;
    let items = vec![item];
    ensure_precise_item_budget(&items, max_chars)?;

    Ok(GetContextResult {
        document_id: document.id.clone(),
        source: document.source.clone(),
        owner_section_id: section.id.clone(),
        content: paragraph.text.clone(),
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

fn section_container_result(
    document: &Document,
    target: ResolvedTarget,
    max_chars: Option<usize>,
) -> Result<GetContextResult, ApplicationError> {
    let section = document
        .find_section(&target.locator.owner_section_id)
        .ok_or(ApplicationError::SectionNotFound)?;
    let (content, truncated) = truncate_chars(render_section_shallow(section), max_chars);

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
        )],
    })
}

fn structural_result(
    document: &Document,
    target: ResolvedTarget,
    kind: StructuralContextKind,
    max_chars: Option<usize>,
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

    let content = sections
        .iter()
        .map(|section| section.title.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    ensure_string_budget(&content, max_chars)?;
    let items = sections
        .iter()
        .map(|section| section_context_item(document, section, ContextItemRole::Structural))
        .collect();

    Ok(GetContextResult {
        document_id: document.id.clone(),
        source: document.source.clone(),
        owner_section_id: owner.id.clone(),
        content,
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

fn paragraph_context_stream(
    document: &Document,
    section: &Section,
) -> Result<Vec<ContextItem>, ApplicationError> {
    let paragraph_set = document.paragraph_text_units();
    let sentence_set = document.sentence_text_units();
    let coverage = sentence_set
        .coverage
        .iter()
        .filter(|coverage| coverage.owner_section_id == section.id)
        .map(|coverage| (coverage.paragraph_index, coverage))
        .collect::<HashMap<_, _>>();

    paragraph_set
        .units
        .iter()
        .filter(|paragraph| paragraph.owner_section_id == section.id)
        .map(|paragraph| {
            let paragraph_coverage = coverage
                .get(&paragraph.paragraph_index)
                .copied()
                .ok_or_else(|| {
                    ApplicationError::InvalidRequest(format!(
                        "sentence coverage is unavailable for paragraph {} in section {}",
                        paragraph.paragraph_index, section.id.0
                    ))
                })?;
            Ok(paragraph_context_item(
                document,
                section,
                paragraph,
                paragraph_coverage,
            ))
        })
        .collect()
}

fn sentence_context_stream(
    document: &Document,
    section: &Section,
) -> Result<Vec<ContextItem>, ApplicationError> {
    let paragraph_set = document.paragraph_text_units();
    let sentence_set = document.sentence_text_units();
    let coverage_by_paragraph = sentence_set
        .coverage
        .iter()
        .filter(|coverage| coverage.owner_section_id == section.id)
        .map(|coverage| (coverage.paragraph_index, coverage))
        .collect::<HashMap<_, _>>();
    let mut sentences_by_paragraph: HashMap<usize, Vec<&SentenceTextUnit>> = HashMap::new();
    for sentence in sentence_set
        .units
        .iter()
        .filter(|sentence| sentence.owner_section_id == section.id)
    {
        sentences_by_paragraph
            .entry(sentence.paragraph_index)
            .or_default()
            .push(sentence);
    }

    let mut result = Vec::new();
    for paragraph in paragraph_set
        .units
        .iter()
        .filter(|paragraph| paragraph.owner_section_id == section.id)
    {
        let coverage = coverage_by_paragraph
            .get(&paragraph.paragraph_index)
            .copied()
            .ok_or_else(|| {
                ApplicationError::InvalidRequest(format!(
                    "sentence coverage is unavailable for paragraph {} in section {}",
                    paragraph.paragraph_index, section.id.0
                ))
            })?;

        if coverage.eligibility == SentenceEligibility::CoarseParagraphOnly {
            let mut item = paragraph_context_item(document, section, paragraph, coverage);
            item.degradation =
                Some("requested_sentence_context_but_non_prose_is_paragraph_only".into());
            result.push(item);
            continue;
        }

        for sentence in sentences_by_paragraph
            .get(&paragraph.paragraph_index)
            .into_iter()
            .flatten()
        {
            result.push(ContextItem {
                title: None,
                content: Some(sentence.text.clone()),
                locator: TextLocator::for_sentence(document, section, sentence),
                role: ContextItemRole::Anchor,
                effective_kind: ContextItemKind::Sentence,
                content_class: Some(ParagraphContentClass::ProseOrUnknown.as_str().into()),
                degradation: None,
            });
        }
    }

    Ok(result)
}

fn paragraph_context_item(
    document: &Document,
    section: &Section,
    paragraph: &TextUnit,
    coverage: &SentenceParagraphCoverage,
) -> ContextItem {
    ContextItem {
        title: None,
        content: Some(paragraph.text.clone()),
        locator: TextLocator::for_paragraph(document, section, paragraph),
        role: ContextItemRole::Anchor,
        effective_kind: ContextItemKind::Paragraph,
        content_class: Some(coverage.content_class.as_str().into()),
        degradation: None,
    }
}

fn section_context_item(
    document: &Document,
    section: &Section,
    role: ContextItemRole,
) -> ContextItem {
    ContextItem {
        title: Some(section.title.clone()),
        content: None,
        locator: TextLocator::for_section(document, section),
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

fn ensure_string_budget(
    content: &str,
    max_chars: Option<usize>,
) -> Result<(), ApplicationError> {
    let limit = content_response_limit(max_chars);
    let chars = content.chars().count();
    if chars > limit {
        return Err(ApplicationError::ResourceLimitExceeded(format!(
            "context metadata contains {chars} characters, exceeding max_chars {limit}"
        )));
    }
    Ok(())
}

fn join_item_content(items: &[ContextItem]) -> String {
    items
        .iter()
        .filter_map(|item| item.content.as_deref())
        .collect::<Vec<_>>()
        .join("\n\n")
}
