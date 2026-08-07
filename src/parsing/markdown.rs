use std::collections::HashMap;

use async_trait::async_trait;

use crate::application::ports::{ApplicationError, Parser, RetrievedResource};
use crate::domain::{Document, Location, Section, SectionId};

use super::common::{content_hash, document_id, slugify, title_from_metadata};

#[derive(Default)]
pub struct MarkdownParser;

#[derive(Clone, Debug)]
struct HeadingEvent {
    byte_start: usize,
    body_start: usize,
    line_number: usize,
    level: u8,
    title: String,
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
}

#[async_trait]
impl Parser for MarkdownParser {
    async fn parse(&self, resource: RetrievedResource) -> Result<Document, ApplicationError> {
        let text = String::from_utf8(resource.bytes.clone()).map_err(|error| {
            ApplicationError::ParseFailed(format!("invalid UTF-8 markdown: {error}"))
        })?;
        let hash = content_hash(&resource.bytes);
        let id = document_id(&resource.final_source, &hash);
        let events = collect_headings(&text);
        let fallback_title = title_from_metadata(&resource.metadata, &resource.final_source);
        let title = events
            .iter()
            .find(|event| event.level == 1)
            .map(|event| event.title.clone())
            .unwrap_or_else(|| fallback_title.clone());

        let root_sections = if events.is_empty() {
            vec![Section {
                id: SectionId("section://document".into()),
                parent_id: None,
                title: fallback_title,
                level: 1,
                content: text.clone(),
                location: Location {
                    section_path: vec!["document".into()],
                    char_start: Some(0),
                    char_end: Some(text.chars().count()),
                    native_location: Some("markdown:0".into()),
                    ..Location::default()
                },
                children: vec![],
            }]
        } else {
            build_markdown_sections(&text, &events)
        };

        Ok(Document {
            id,
            source: resource.final_source,
            title,
            media_type: resource.media_type,
            content_hash: hash,
            metadata: resource.metadata,
            root_sections,
        })
    }
}

fn collect_headings(text: &str) -> Vec<HeadingEvent> {
    let mut events = Vec::new();
    let mut byte_offset = 0usize;
    let mut fence: Option<char> = None;

    for (line_index, line) in text.split_inclusive('\n').enumerate() {
        let trimmed = line.trim_start();
        if let Some(marker) = fence_marker(trimmed) {
            match fence {
                Some(active) if active == marker => fence = None,
                None => fence = Some(marker),
                _ => {}
            }
            byte_offset += line.len();
            continue;
        }

        if fence.is_none() {
            if let Some((level, title)) = parse_atx_heading(line) {
                events.push(HeadingEvent {
                    byte_start: byte_offset,
                    body_start: byte_offset + line.len(),
                    line_number: line_index + 1,
                    level,
                    title,
                });
            }
        }

        byte_offset += line.len();
    }

    events
}

fn fence_marker(line: &str) -> Option<char> {
    let trimmed = line.trim_start();
    if trimmed.starts_with("```") {
        Some('`')
    } else if trimmed.starts_with("~~~") {
        Some('~')
    } else {
        None
    }
}

fn parse_atx_heading(line: &str) -> Option<(u8, String)> {
    let trimmed = line.trim_start().trim_end_matches(['\r', '\n']);
    let level = trimmed.chars().take_while(|ch| *ch == '#').count();
    if !(1..=6).contains(&level) {
        return None;
    }

    let remainder = &trimmed[level..];
    if !remainder.is_empty() && !remainder.starts_with(char::is_whitespace) {
        return None;
    }

    let title = remainder.trim().trim_end_matches('#').trim();
    if title.is_empty() {
        return None;
    }

    Some((level as u8, title.to_string()))
}

fn build_markdown_sections(text: &str, events: &[HeadingEvent]) -> Vec<Section> {
    let mut nodes: Vec<SectionNode> = Vec::with_capacity(events.len() + 1);
    let mut last_at_level: [Option<usize>; 6] = [None; 6];
    let mut id_counts: HashMap<String, usize> = HashMap::new();

    let preamble = text[..events[0].byte_start].trim();
    if !preamble.is_empty() {
        nodes.push(SectionNode {
            id: SectionId("section://preamble".into()),
            parent: None,
            title: "Preamble".into(),
            level: 1,
            content: preamble.into(),
            location: Location {
                section_path: vec!["Preamble".into()],
                char_start: Some(0),
                char_end: Some(text[..events[0].byte_start].chars().count()),
                native_location: Some("markdown:preamble".into()),
                ..Location::default()
            },
            path: vec!["Preamble".into()],
        });
    }

    let heading_base = nodes.len();
    for (event_index, event) in events.iter().enumerate() {
        let level_index = usize::from(event.level - 1);
        let parent_event_index = (0..level_index).rev().find_map(|idx| last_at_level[idx]);
        let parent = parent_event_index.map(|idx| heading_base + idx);

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
        let section_id = if *count == 1 {
            base_id
        } else {
            format!("{base_id}-{}", *count)
        };

        let body_end = events
            .get(event_index + 1)
            .map(|next| next.byte_start)
            .unwrap_or(text.len());
        let body = text[event.body_start..body_end].trim().to_string();

        nodes.push(SectionNode {
            id: SectionId(section_id),
            parent,
            title: event.title.clone(),
            level: event.level,
            content: body,
            location: Location {
                section_path: path.clone(),
                anchor: Some(slugify(&event.title)),
                char_start: Some(text[..event.body_start].chars().count()),
                char_end: Some(text[..body_end].chars().count()),
                native_location: Some(format!("markdown:line:{}", event.line_number)),
                ..Location::default()
            },
            path,
        });
    }

    nodes
        .iter()
        .enumerate()
        .filter(|(_, node)| node.parent.is_none())
        .map(|(index, _)| build_section(index, &nodes))
        .collect()
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
