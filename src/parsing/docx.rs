use std::collections::HashMap;
use std::io::Cursor;

use async_trait::async_trait;
use roxmltree::Document as XmlDocument;
use zip::ZipArchive;

use crate::application::ports::{ApplicationError, Parser, RetrievedResource};
use crate::domain::{Document, Location, Section, SectionId};

use super::archive::{
    ArchiveLimits, read_entry, read_optional_entry, utf8_entry, validate_archive_entries,
};
use super::common::{content_hash, document_id, slugify, title_from_metadata};

pub struct DocxParser {
    limits: ArchiveLimits,
}

impl DocxParser {
    pub fn new(limits: ArchiveLimits) -> Self {
        Self { limits }
    }
}

#[derive(Clone, Debug)]
struct SectionNode {
    id: SectionId,
    parent: Option<usize>,
    title: String,
    level: u8,
    content: Vec<String>,
    location: Location,
    path: Vec<String>,
}

#[async_trait]
impl Parser for DocxParser {
    async fn parse(&self, resource: RetrievedResource) -> Result<Document, ApplicationError> {
        let hash = content_hash(&resource.bytes);
        let id = document_id(&resource.final_source, &hash);
        let mut archive =
            ZipArchive::new(Cursor::new(resource.bytes.as_slice())).map_err(|error| {
                ApplicationError::ParseFailed(format!("invalid DOCX ZIP archive: {error}"))
            })?;
        validate_archive_entries(&archive, &self.limits)?;
        let mut total_read = 0usize;
        let document_xml = utf8_entry(
            read_entry(
                &mut archive,
                "word/document.xml",
                &self.limits,
                &mut total_read,
            )?,
            "word/document.xml",
        )?;
        let xml = XmlDocument::parse(&document_xml).map_err(|error| {
            ApplicationError::ParseFailed(format!("invalid DOCX document.xml: {error}"))
        })?;

        let mut nodes = Vec::<SectionNode>::new();
        let mut last_at_level: [Option<usize>; 6] = [None; 6];
        let mut id_counts = HashMap::<String, usize>::new();
        let mut preamble = Vec::<String>::new();
        let mut current_section: Option<usize> = None;
        let mut paragraph_number = 0u32;
        let mut first_heading = None::<String>;

        for paragraph in xml
            .descendants()
            .filter(|node| node.is_element() && node.tag_name().name() == "p")
        {
            paragraph_number += 1;
            let text = paragraph
                .descendants()
                .filter(|node| node.is_element() && node.tag_name().name() == "t")
                .filter_map(|node| node.text())
                .collect::<String>()
                .trim()
                .to_string();
            if text.is_empty() {
                continue;
            }
            let style = paragraph
                .descendants()
                .find(|node| node.is_element() && node.tag_name().name() == "pStyle")
                .and_then(|node| {
                    node.attributes()
                        .find(|attribute| attribute.name() == "val")
                        .map(|attribute| attribute.value())
                });

            if let Some(level) = style.and_then(heading_level) {
                first_heading.get_or_insert_with(|| text.clone());
                let level_index = usize::from(level - 1);
                let parent = (0..level_index).rev().find_map(|idx| last_at_level[idx]);
                for slot in last_at_level.iter_mut().skip(level_index) {
                    *slot = None;
                }

                let mut path = parent
                    .map(|parent_index| nodes[parent_index].path.clone())
                    .unwrap_or_default();
                path.push(text.clone());
                let base_id = format!(
                    "section://{}",
                    path.iter()
                        .map(|segment| slugify(segment))
                        .collect::<Vec<_>>()
                        .join("/")
                );
                let count = id_counts.entry(base_id.clone()).or_insert(0);
                *count += 1;
                let id = if *count == 1 {
                    base_id
                } else {
                    format!("{base_id}-{}", *count)
                };

                nodes.push(SectionNode {
                    id: SectionId(id),
                    parent,
                    title: text,
                    level,
                    content: vec![],
                    location: Location {
                        section_path: path.clone(),
                        paragraph: Some(paragraph_number),
                        native_location: Some(format!("docx:paragraph:{paragraph_number}")),
                        ..Location::default()
                    },
                    path,
                });
                let index = nodes.len() - 1;
                last_at_level[level_index] = Some(index);
                current_section = Some(index);
            } else if let Some(index) = current_section {
                nodes[index].content.push(text);
            } else {
                preamble.push(text);
            }
        }

        let mut root_sections = if nodes.is_empty() {
            vec![Section {
                id: SectionId("section://document".into()),
                parent_id: None,
                title: title_from_metadata(&resource.metadata, &resource.final_source),
                level: 1,
                content: preamble.join("\n\n"),
                location: Location {
                    section_path: vec!["document".into()],
                    native_location: Some("docx:document".into()),
                    ..Location::default()
                },
                children: vec![],
            }]
        } else {
            let mut roots = nodes
                .iter()
                .enumerate()
                .filter(|(_, node)| node.parent.is_none())
                .map(|(index, _)| build_section(index, &nodes))
                .collect::<Vec<_>>();
            if !preamble.is_empty() {
                roots.insert(
                    0,
                    Section {
                        id: SectionId("section://preamble".into()),
                        parent_id: None,
                        title: "Preamble".into(),
                        level: 1,
                        content: preamble.join("\n\n"),
                        location: Location {
                            section_path: vec!["Preamble".into()],
                            native_location: Some("docx:preamble".into()),
                            ..Location::default()
                        },
                        children: vec![],
                    },
                );
            }
            roots
        };

        if root_sections.is_empty() {
            return Err(ApplicationError::ParseFailed(
                "DOCX contains no readable paragraphs".into(),
            ));
        }

        let core_title = read_optional_entry(
            &mut archive,
            "docProps/core.xml",
            &self.limits,
            &mut total_read,
        )?
        .map(|bytes| utf8_entry(bytes, "docProps/core.xml"))
        .transpose()?
        .and_then(|xml| extract_core_title(&xml));
        let mut metadata = resource.metadata;
        metadata.insert("docx_paragraph_count".into(), paragraph_number.to_string());
        let title = core_title
            .or(first_heading)
            .unwrap_or_else(|| title_from_metadata(&metadata, &resource.final_source));

        normalize_root_levels(&mut root_sections);
        Ok(Document {
            id,
            source: resource.final_source,
            title,
            media_type: resource.media_type,
            content_hash: hash,
            metadata,
            root_sections,
        })
    }
}

fn heading_level(style: &str) -> Option<u8> {
    let normalized = style
        .chars()
        .filter(|ch| !ch.is_whitespace())
        .collect::<String>()
        .to_ascii_lowercase();
    let suffix = normalized.strip_prefix("heading")?;
    suffix
        .parse::<u8>()
        .ok()
        .filter(|level| (1..=6).contains(level))
}

fn extract_core_title(xml: &str) -> Option<String> {
    let document = XmlDocument::parse(xml).ok()?;
    document
        .descendants()
        .find(|node| node.is_element() && node.tag_name().name() == "title")
        .and_then(|node| node.text())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn build_section(index: usize, nodes: &[SectionNode]) -> Section {
    let node = &nodes[index];
    Section {
        id: node.id.clone(),
        parent_id: node.parent.map(|parent| nodes[parent].id.clone()),
        title: node.title.clone(),
        level: node.level,
        content: node.content.join("\n\n"),
        location: node.location.clone(),
        children: nodes
            .iter()
            .enumerate()
            .filter(|(_, candidate)| candidate.parent == Some(index))
            .map(|(child_index, _)| build_section(child_index, nodes))
            .collect(),
    }
}

fn normalize_root_levels(sections: &mut [Section]) {
    for section in sections {
        if section.level == 0 {
            section.level = 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::heading_level;

    #[test]
    fn docx_heading_styles_are_normalized() {
        assert_eq!(heading_level("Heading1"), Some(1));
        assert_eq!(heading_level("heading 3"), Some(3));
        assert_eq!(heading_level("Normal"), None);
    }
}
