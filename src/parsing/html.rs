use std::collections::{BTreeMap, HashMap};

use async_trait::async_trait;
use markup5ever::interface::tree_builder::TreeSink;
use scraper::{ElementRef, Html, HtmlTreeSink, Selector};

use crate::application::ports::{ApplicationError, Parser, RetrievedResource};
use crate::domain::{
    Document, Location, NormalizedBlock, NormalizedBlockKind, NormalizedBlockMap,
    NormalizedBlockProvenance, NormalizedTextRange, Section, SectionId,
};

use super::common::{content_hash, document_id, slugify, title_from_metadata};

#[derive(Default)]
pub struct HtmlParser;

#[derive(Clone, Debug)]
struct NativeBodyBlock {
    kind: NormalizedBlockKind,
    text: String,
    anchor: Option<String>,
    source_ordinal: usize,
}

#[derive(Clone, Debug)]
struct HeadingEvent {
    level: u8,
    title: String,
    anchor: Option<String>,
    body: Vec<NativeBodyBlock>,
    ordinal: usize,
}

#[derive(Clone, Debug)]
struct SectionNode {
    id: SectionId,
    parent: Option<usize>,
    title: String,
    level: u8,
    content: String,
    location: Location,
    path: Vec<String>,
    blocks: Vec<NormalizedBlock>,
}

#[async_trait]
impl Parser for HtmlParser {
    async fn parse(&self, resource: RetrievedResource) -> Result<Document, ApplicationError> {
        let html = String::from_utf8(resource.bytes.clone()).map_err(|error| {
            ApplicationError::ParseFailed(format!("invalid UTF-8 HTML: {error}"))
        })?;
        let hash = content_hash(&resource.bytes);
        let id = document_id(&resource.final_source, &hash);

        let document = remove_noise(Html::parse_document(&html))?;
        let root = content_root(&document)?;
        let (events, preamble) = collect_content(root)?;
        let mut metadata = resource.metadata;
        capture_html_metadata(&document, &mut metadata)?;

        let fallback_title = title_from_metadata(&metadata, &resource.final_source);
        let title = events
            .iter()
            .find(|event| event.level == 1)
            .map(|event| event.title.clone())
            .or_else(|| metadata.get("html_title").cloned())
            .unwrap_or_else(|| fallback_title.clone());

        let (root_sections, blocks) = if events.is_empty() {
            let section_id = SectionId("section://document".into());
            let (content, blocks) = if preamble.is_empty() {
                (
                    collapse_whitespace(&root.text().collect::<Vec<_>>().join("")),
                    Vec::new(),
                )
            } else {
                render_blocks(&section_id, &preamble)
            };

            (
                vec![Section {
                    id: section_id,
                    parent_id: None,
                    title: title.clone(),
                    level: 1,
                    content,
                    location: Location {
                        section_path: vec!["document".into()],
                        native_location: Some("html:document".into()),
                        ..Location::default()
                    },
                    children: vec![],
                }],
                blocks,
            )
        } else {
            build_html_sections(&events, &preamble)
        };

        let mut normalized = Document {
            id,
            source: resource.final_source,
            title,
            media_type: resource.media_type,
            content_hash: hash,
            metadata,
            root_sections,
        };
        normalized
            .set_normalized_block_map(NormalizedBlockMap::new(blocks))
            .map_err(|error| ApplicationError::ParseFailed(error.to_string()))?;
        Ok(normalized)
    }
}

fn remove_noise(document: Html) -> Result<Html, ApplicationError> {
    let selector = selector("script, style, nav, footer, aside, noscript, template, svg")?;
    let node_ids = document
        .select(&selector)
        .map(|element| element.id())
        .collect::<Vec<_>>();
    let tree = HtmlTreeSink::new(document);
    for id in node_ids {
        tree.remove_from_parent(&id);
    }
    Ok(tree.finish())
}

fn content_root(document: &Html) -> Result<ElementRef<'_>, ApplicationError> {
    for query in ["main", "article", "body"] {
        let selector = selector(query)?;
        if let Some(element) = document.select(&selector).next() {
            return Ok(element);
        }
    }

    Ok(document.root_element())
}

fn capture_html_metadata(
    document: &Html,
    metadata: &mut BTreeMap<String, String>,
) -> Result<(), ApplicationError> {
    let title_selector = selector("title")?;
    if let Some(title) = document.select(&title_selector).next() {
        let value = normalized_element_text(title);
        if !value.is_empty() {
            metadata.insert("html_title".into(), value);
        }
    }

    let link_selector = selector("link[rel]")?;
    if let Some(canonical) = document.select(&link_selector).find(|element| {
        element.value().attr("rel").is_some_and(|rel| {
            rel.split_ascii_whitespace()
                .any(|item| item.eq_ignore_ascii_case("canonical"))
        })
    }) && let Some(href) = canonical.value().attr("href")
    {
        metadata.insert("canonical_href".into(), href.to_string());
    }

    Ok(())
}

fn collect_content(
    root: ElementRef<'_>,
) -> Result<(Vec<HeadingEvent>, Vec<NativeBodyBlock>), ApplicationError> {
    let block_selector = selector("h1, h2, h3, h4, h5, h6, p, pre, blockquote, li, table")?;
    let mut events: Vec<HeadingEvent> = Vec::new();
    let mut preamble = Vec::new();
    let mut body_ordinal = 0usize;

    for element in root.select(&block_selector) {
        let tag = element.value().name();
        if let Some(level) = heading_level(tag) {
            let title = normalized_element_text(element);
            if title.is_empty() {
                continue;
            }
            events.push(HeadingEvent {
                level,
                title,
                anchor: element.value().attr("id").map(str::to_string),
                body: Vec::new(),
                ordinal: events.len() + 1,
            });
            continue;
        }

        let Some(kind) = normalized_block_kind(tag) else {
            continue;
        };
        if has_native_body_block_ancestor(element) {
            continue;
        }
        let text = match tag {
            "pre" => element
                .text()
                .collect::<Vec<_>>()
                .join("")
                .trim()
                .to_string(),
            "table" => normalized_table_text(element)?,
            _ => normalized_element_text(element),
        };
        if text.is_empty() {
            continue;
        }

        body_ordinal += 1;
        let block = NativeBodyBlock {
            kind,
            text,
            anchor: element.value().attr("id").map(str::to_string),
            source_ordinal: body_ordinal,
        };
        if let Some(current) = events.last_mut() {
            current.body.push(block);
        } else {
            preamble.push(block);
        }
    }

    Ok((events, preamble))
}

fn has_native_body_block_ancestor(element: ElementRef<'_>) -> bool {
    let element_id = element.id();
    element
        .ancestors()
        .filter(|node| node.id() != element_id)
        .filter_map(ElementRef::wrap)
        .any(|ancestor| normalized_block_kind(ancestor.value().name()).is_some())
}

fn build_html_sections(
    events: &[HeadingEvent],
    preamble: &[NativeBodyBlock],
) -> (Vec<Section>, Vec<NormalizedBlock>) {
    let mut nodes: Vec<SectionNode> =
        Vec::with_capacity(events.len() + usize::from(!preamble.is_empty()));
    let mut last_at_level: [Option<usize>; 6] = [None; 6];
    let mut id_counts: HashMap<String, usize> = HashMap::new();

    if !preamble.is_empty() {
        let id = SectionId("section://preamble".into());
        let (content, blocks) = render_blocks(&id, preamble);
        nodes.push(SectionNode {
            id,
            parent: None,
            title: "Preamble".into(),
            level: 1,
            content,
            location: Location {
                section_path: vec!["Preamble".into()],
                native_location: Some("html:preamble".into()),
                ..Location::default()
            },
            path: vec!["Preamble".into()],
            blocks,
        });
    }

    let heading_base = nodes.len();
    for (event_index, event) in events.iter().enumerate() {
        let level_index = usize::from(event.level - 1);
        let parent_event_index = (0..level_index)
            .rev()
            .find_map(|index| last_at_level[index]);
        let parent = parent_event_index.map(|index| heading_base + index);

        for slot in last_at_level.iter_mut().skip(level_index) {
            *slot = None;
        }
        last_at_level[level_index] = Some(event_index);

        let mut path = parent
            .map(|parent_index| nodes[parent_index].path.clone())
            .unwrap_or_default();
        path.push(event.title.clone());

        let base_id = format!(
            "section://{}",
            path.iter()
                .map(|segment| slugify(segment))
                .collect::<Vec<_>>()
                .join("/")
        );
        let count = id_counts.entry(base_id.clone()).or_insert(0);
        *count += 1;
        let section_id = SectionId(if *count == 1 {
            base_id
        } else {
            format!("{base_id}-{}", *count)
        });

        let native_location = event
            .anchor
            .as_ref()
            .map(|anchor| format!("html:#{anchor}"))
            .unwrap_or_else(|| format!("html:heading:{}", event.ordinal));
        let (content, blocks) = render_blocks(&section_id, &event.body);

        nodes.push(SectionNode {
            id: section_id,
            parent,
            title: event.title.clone(),
            level: event.level,
            content,
            location: Location {
                section_path: path.clone(),
                anchor: event.anchor.clone(),
                native_location: Some(native_location),
                ..Location::default()
            },
            path,
            blocks,
        });
    }

    let root_sections = nodes
        .iter()
        .enumerate()
        .filter(|(_, node)| node.parent.is_none())
        .map(|(index, _)| build_section(index, &nodes))
        .collect();
    let mut blocks = Vec::new();
    for node in &nodes {
        for block in &node.blocks {
            let mut block = block.clone();
            block.source_order = blocks.len();
            blocks.push(block);
        }
    }
    (root_sections, blocks)
}

fn render_blocks(
    owner_section_id: &SectionId,
    blocks: &[NativeBodyBlock],
) -> (String, Vec<NormalizedBlock>) {
    let mut content = String::new();
    let mut normalized_blocks = Vec::with_capacity(blocks.len());

    for (offset, block) in blocks.iter().enumerate() {
        if !content.is_empty() {
            content.push_str("\n\n");
        }
        let start = content.chars().count();
        content.push_str(&block.text);
        let end = content.chars().count();
        let normalized_range = NormalizedTextRange::new(start, end)
            .expect("rendered normalized block boundaries must be ordered");
        let native_location = block
            .anchor
            .as_ref()
            .map(|anchor| format!("html:#{anchor}"))
            .unwrap_or_else(|| {
                format!(
                    "html:block:{}:{}",
                    block.kind.as_str(),
                    block.source_ordinal
                )
            });
        normalized_blocks.push(NormalizedBlock {
            owner_section_id: owner_section_id.clone(),
            block_index: offset + 1,
            source_order: offset,
            kind: block.kind,
            normalized_range,
            native_anchor: block.anchor.clone(),
            native_location: Some(native_location),
            provenance: NormalizedBlockProvenance::XhtmlNativeBlock,
        });
    }

    (content, normalized_blocks)
}

fn build_section(index: usize, nodes: &[SectionNode]) -> Section {
    let node = &nodes[index];
    let children = nodes
        .iter()
        .enumerate()
        .filter(|(_, candidate)| candidate.parent == Some(index))
        .map(|(child_index, _)| build_section(child_index, nodes))
        .collect();

    Section {
        id: node.id.clone(),
        parent_id: node.parent.map(|parent| nodes[parent].id.clone()),
        title: node.title.clone(),
        level: node.level,
        content: node.content.clone(),
        location: node.location.clone(),
        children,
    }
}

fn heading_level(tag: &str) -> Option<u8> {
    match tag {
        "h1" => Some(1),
        "h2" => Some(2),
        "h3" => Some(3),
        "h4" => Some(4),
        "h5" => Some(5),
        "h6" => Some(6),
        _ => None,
    }
}

fn normalized_block_kind(tag: &str) -> Option<NormalizedBlockKind> {
    match tag {
        "p" => Some(NormalizedBlockKind::Paragraph),
        "blockquote" => Some(NormalizedBlockKind::BlockQuote),
        "li" => Some(NormalizedBlockKind::ListItem),
        "pre" => Some(NormalizedBlockKind::Preformatted),
        "table" => Some(NormalizedBlockKind::Table),
        _ => None,
    }
}

fn normalized_element_text(element: ElementRef<'_>) -> String {
    collapse_whitespace(&element.text().collect::<Vec<_>>().join(""))
}

fn normalized_table_text(element: ElementRef<'_>) -> Result<String, ApplicationError> {
    let cells = selector("th, td")?;
    let values = element
        .select(&cells)
        .map(normalized_element_text)
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>();
    if values.is_empty() {
        Ok(normalized_element_text(element))
    } else {
        Ok(values.join(" "))
    }
}

fn collapse_whitespace(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn selector(value: &str) -> Result<Selector, ApplicationError> {
    Selector::parse(value).map_err(|error| {
        ApplicationError::ParseFailed(format!("invalid internal HTML selector `{value}`: {error}"))
    })
}
