use std::collections::{BTreeMap, HashMap};

use async_trait::async_trait;
use lopdf::{Document as LopdfDocument, Object, TocType, decode_text_string};

use crate::application::ports::{ApplicationError, Parser, RetrievedResource};
use crate::domain::{Document, Location, Section, SectionId};

use super::common::{content_hash, document_id, slugify, title_from_metadata};

const MAX_PAGE_DECOMPRESSED_BYTES: usize = 16 * 1024 * 1024;

#[derive(Default)]
pub struct PdfParser;

#[derive(Clone, Debug)]
struct PageText {
    number: u32,
    text: String,
}

#[derive(Clone, Debug)]
struct PdfTocEntry {
    level: u8,
    title: String,
    page: u32,
}

#[derive(Clone, Debug)]
struct SectionNode {
    id: SectionId,
    parent: Option<usize>,
    title: String,
    level: u8,
    start_page: u32,
    content: String,
    location: Location,
    path: Vec<String>,
}

#[async_trait]
impl Parser for PdfParser {
    async fn parse(&self, resource: RetrievedResource) -> Result<Document, ApplicationError> {
        let hash = content_hash(&resource.bytes);
        let id = document_id(&resource.final_source, &hash);
        let pdf = LopdfDocument::load_mem(&resource.bytes).map_err(|error| {
            ApplicationError::ParseFailed(format!("invalid PDF document: {error}"))
        })?;

        let page_numbers = pdf.get_pages().keys().copied().collect::<Vec<_>>();
        if page_numbers.is_empty() {
            return Err(ApplicationError::ParseFailed(
                "PDF does not contain any pages".into(),
            ));
        }

        let (pages, extraction_errors) = extract_page_texts(&pdf, &page_numbers);
        if pages.iter().all(|page| page.text.trim().is_empty()) {
            return Err(ApplicationError::ParseFailed(
                "PDF contains no extractable text; scanned PDFs require OCR, which is not supported in this phase"
                    .into(),
            ));
        }

        let mut metadata = resource.metadata;
        metadata.insert("pdf_version".into(), pdf.version.clone());
        metadata.insert("pdf_page_count".into(), page_numbers.len().to_string());
        if !extraction_errors.is_empty() {
            metadata.insert(
                "pdf_text_extraction_errors".into(),
                extraction_errors.len().to_string(),
            );
        }

        let pdf_title = pdf_title(&pdf);
        if let Some(title) = &pdf_title {
            metadata.insert("pdf_title".into(), title.clone());
        }

        let max_page = *page_numbers.last().unwrap_or(&1);
        let toc = pdf
            .get_toc()
            .ok()
            .map(|toc| normalize_toc(&toc.toc, max_page))
            .filter(|entries| !entries.is_empty());

        if let Some(entries) = &toc {
            metadata.insert("pdf_toc_entries".into(), entries.len().to_string());
        }

        let root_sections = match toc {
            Some(entries) => build_toc_sections(&entries, &pages, max_page),
            None => build_page_sections(&pages),
        };

        let fallback_title = title_from_metadata(&metadata, &resource.final_source);
        let title = pdf_title.unwrap_or(fallback_title);

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

fn extract_page_texts(pdf: &LopdfDocument, page_numbers: &[u32]) -> (Vec<PageText>, Vec<String>) {
    let mut pages = Vec::with_capacity(page_numbers.len());
    let mut errors = Vec::new();

    for page_number in page_numbers {
        match pdf.extract_text_with_limit(&[*page_number], MAX_PAGE_DECOMPRESSED_BYTES) {
            Ok(text) => pages.push(PageText {
                number: *page_number,
                text: normalize_pdf_text(&text),
            }),
            Err(error) => {
                errors.push(format!("page {page_number}: {error}"));
                pages.push(PageText {
                    number: *page_number,
                    text: String::new(),
                });
            }
        }
    }

    (pages, errors)
}

fn normalize_pdf_text(value: &str) -> String {
    value.trim().to_string()
}

fn pdf_title(pdf: &LopdfDocument) -> Option<String> {
    let info = pdf.trailer.get(b"Info").ok()?;
    let title = match info {
        Object::Reference(id) => pdf.get_dictionary(*id).ok()?.get(b"Title").ok()?,
        Object::Dictionary(dictionary) => dictionary.get(b"Title").ok()?,
        _ => return None,
    };

    decode_text_string(title)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn normalize_toc(entries: &[TocType], max_page: u32) -> Vec<PdfTocEntry> {
    entries
        .iter()
        .filter_map(|entry| {
            let page = u32::try_from(entry.page).ok()?;
            let title = entry.title.trim();
            if page == 0 || page > max_page || title.is_empty() {
                return None;
            }

            Some(PdfTocEntry {
                level: entry.level.clamp(1, 6) as u8,
                title: title.to_string(),
                page,
            })
        })
        .collect()
}

fn build_page_sections(pages: &[PageText]) -> Vec<Section> {
    pages
        .iter()
        .map(|page| {
            let title = format!("Page {}", page.number);
            Section {
                id: SectionId(format!("section://page-{}", page.number)),
                parent_id: None,
                title: title.clone(),
                level: 1,
                content: page.text.clone(),
                location: Location {
                    page: Some(page.number),
                    section_path: vec![title],
                    native_location: Some(format!("pdf:page:{}", page.number)),
                    ..Location::default()
                },
                children: vec![],
            }
        })
        .collect()
}

fn build_toc_sections(entries: &[PdfTocEntry], pages: &[PageText], max_page: u32) -> Vec<Section> {
    let page_map = pages
        .iter()
        .map(|page| (page.number, page.text.as_str()))
        .collect::<BTreeMap<_, _>>();
    let mut nodes = Vec::<SectionNode>::with_capacity(entries.len());
    let mut last_at_level: [Option<usize>; 6] = [None; 6];
    let mut id_counts = HashMap::<String, usize>::new();

    for (index, entry) in entries.iter().enumerate() {
        let level_index = usize::from(entry.level - 1);
        let parent = (0..level_index)
            .rev()
            .find_map(|candidate| last_at_level[candidate]);

        for slot in last_at_level.iter_mut().skip(level_index) {
            *slot = None;
        }
        last_at_level[level_index] = Some(index);

        let mut path = parent
            .map(|parent_index| nodes[parent_index].path.clone())
            .unwrap_or_default();
        path.push(entry.title.clone());

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
            title: entry.title.clone(),
            level: entry.level,
            start_page: entry.page,
            content: String::new(),
            location: Location::default(),
            path,
        });
    }

    for index in 0..nodes.len() {
        let start_page = nodes[index].start_page;
        let first_child_page = nodes
            .iter()
            .filter(|candidate| candidate.parent == Some(index))
            .map(|candidate| candidate.start_page)
            .min();

        let end_page = if let Some(child_page) = first_child_page {
            child_page.checked_sub(1).filter(|page| *page >= start_page)
        } else {
            let next_boundary = nodes
                .iter()
                .skip(index + 1)
                .find(|candidate| candidate.level <= nodes[index].level)
                .map(|candidate| candidate.start_page);

            Some(match next_boundary {
                Some(page) if page > start_page => page - 1,
                Some(_) => start_page,
                None => max_page,
            })
        };

        let content = end_page
            .map(|end| text_for_page_range(&page_map, start_page, end))
            .unwrap_or_default();
        let native_location = match end_page {
            Some(end) if end > start_page => format!("pdf:pages:{start_page}-{end}"),
            _ => format!("pdf:page:{start_page}"),
        };

        nodes[index].content = content;
        nodes[index].location = Location {
            page: Some(start_page),
            chapter: nodes[index].path.first().cloned(),
            section_path: nodes[index].path.clone(),
            native_location: Some(native_location),
            ..Location::default()
        };
    }

    nodes
        .iter()
        .enumerate()
        .filter(|(_, node)| node.parent.is_none())
        .map(|(index, _)| build_section(index, &nodes))
        .collect()
}

fn text_for_page_range(
    pages: &BTreeMap<u32, &str>,
    start_page: u32,
    end_page: u32,
) -> String {
    (start_page..=end_page)
        .filter_map(|page| pages.get(&page).copied())
        .filter(|text| !text.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n\n")
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
