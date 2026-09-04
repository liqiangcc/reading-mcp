use std::collections::{BTreeMap, HashMap};

use async_trait::async_trait;
use lopdf::{Document as LopdfDocument, Object, TocType, decode_text_string};

use crate::application::ports::{ApplicationError, Parser, RetrievedResource};
use crate::domain::{
    Document, Location, NormalizedTextRange, OriginalSourceBinding, OriginalSourceBindingMap,
    OriginalSourceTarget, Section, SectionId,
};

use super::common::{content_hash, document_id, slugify, title_from_metadata};

const MAX_PAGE_DECOMPRESSED_BYTES: usize = 16 * 1024 * 1024;
const PDF_STRUCTURE_PROVENANCE_METADATA_KEY: &str = "pdf_structure_provenance";
const PDF_STRUCTURE_NATIVE_TOC: &str = "native_toc";
const PDF_STRUCTURE_INFERRED_NUMBERED_HEADINGS: &str = "inferred_numbered_headings";
const PDF_STRUCTURE_PAGE_FALLBACK: &str = "page_fallback";
const PDF_HEADING_INFERENCE_VERSION_METADATA_KEY: &str = "pdf_heading_inference_version";
const PDF_HEADING_INFERENCE_COUNT_METADATA_KEY: &str = "pdf_heading_inference_count";
const PDF_HEADING_INFERENCE_VERSION: &str = "pdf-numbered-heading-inference/v1";
const MAX_INFERRED_HEADING_CHARS: usize = 160;
const MAX_INFERRED_HEADING_WORDS: usize = 24;

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

#[derive(Clone, Debug, PartialEq, Eq)]
struct PdfHeadingCandidate {
    number_path: Vec<u32>,
    title: String,
    page: u32,
    global_line: usize,
}

#[derive(Clone, Debug)]
struct PdfSourceLine {
    page: u32,
    global_line: usize,
    text: String,
}

#[derive(Clone, Debug, Default)]
struct PageFragments(Vec<(u32, String)>);

impl PageFragments {
    fn push_line(&mut self, page: u32, text: &str) {
        match self.0.last_mut() {
            Some((last_page, fragment)) if *last_page == page => {
                if !fragment.is_empty() {
                    fragment.push('\n');
                }
                fragment.push_str(text);
            }
            _ => self.0.push((page, text.to_string())),
        }
    }
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

        let (root_sections, source_bindings) = match toc {
            Some(entries) => {
                metadata.insert(
                    PDF_STRUCTURE_PROVENANCE_METADATA_KEY.into(),
                    PDF_STRUCTURE_NATIVE_TOC.into(),
                );
                build_toc_sections(&entries, &pages, max_page)
            }
            None => match infer_numbered_headings(&pages) {
                Some(headings) => {
                    metadata.insert(
                        PDF_STRUCTURE_PROVENANCE_METADATA_KEY.into(),
                        PDF_STRUCTURE_INFERRED_NUMBERED_HEADINGS.into(),
                    );
                    metadata.insert(
                        PDF_HEADING_INFERENCE_VERSION_METADATA_KEY.into(),
                        PDF_HEADING_INFERENCE_VERSION.into(),
                    );
                    metadata.insert(
                        PDF_HEADING_INFERENCE_COUNT_METADATA_KEY.into(),
                        headings.len().to_string(),
                    );
                    build_inferred_heading_sections(&headings, &pages)
                }
                None => {
                    metadata.insert(
                        PDF_STRUCTURE_PROVENANCE_METADATA_KEY.into(),
                        PDF_STRUCTURE_PAGE_FALLBACK.into(),
                    );
                    build_page_sections(&pages)
                }
            },
        };

        let fallback_title = title_from_metadata(&metadata, &resource.final_source);
        let title = pdf_title.unwrap_or(fallback_title);
        let mut document = Document {
            id,
            source: resource.final_source,
            title,
            media_type: resource.media_type,
            content_hash: hash,
            metadata,
            root_sections,
        };
        document
            .set_original_source_binding_map(OriginalSourceBindingMap::new(source_bindings))
            .map_err(|error| {
                ApplicationError::ParseFailed(format!(
                    "invalid original PDF source binding evidence: {error}"
                ))
            })?;
        Ok(document)
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

fn infer_numbered_headings(pages: &[PageText]) -> Option<Vec<PdfHeadingCandidate>> {
    let lines = source_lines(pages);
    let parsed = lines
        .iter()
        .filter_map(|line| {
            parse_numbered_heading(&line.text).map(|(number_path, title)| PdfHeadingCandidate {
                number_path,
                title,
                page: line.page,
                global_line: line.global_line,
            })
        })
        .collect::<Vec<_>>();

    let mut accepted = Vec::new();
    let mut expected_top = 1_u32;
    let mut current_top = None;
    let mut started = false;

    for candidate in parsed {
        let top = candidate.number_path[0];
        if candidate.number_path.len() == 1 {
            if top == expected_top {
                started = true;
                current_top = Some(top);
                expected_top = expected_top.saturating_add(1);
                accepted.push(candidate);
            } else if started && top > expected_top {
                break;
            }
            continue;
        }

        if started
            && current_top == Some(top)
            && has_parent_heading(&accepted, &candidate.number_path)
        {
            accepted.push(candidate);
        }
    }

    let top_level_count = accepted
        .iter()
        .filter(|candidate| candidate.number_path.len() == 1)
        .count();
    (top_level_count >= 2).then_some(accepted)
}

fn source_lines(pages: &[PageText]) -> Vec<PdfSourceLine> {
    let mut lines = Vec::new();
    let mut global_line = 0_usize;
    for page in pages {
        for text in page.text.lines() {
            lines.push(PdfSourceLine {
                page: page.number,
                global_line,
                text: text.to_string(),
            });
            global_line = global_line.saturating_add(1);
        }
    }
    lines
}

fn parse_numbered_heading(line: &str) -> Option<(Vec<u32>, String)> {
    let line = line.trim();
    if line.is_empty() || line.chars().count() > MAX_INFERRED_HEADING_CHARS {
        return None;
    }
    let mut parts = line.split_whitespace();
    let raw_number = parts.next()?;
    let title = parts.collect::<Vec<_>>().join(" ");
    if title.is_empty()
        || title.split_whitespace().count() > MAX_INFERRED_HEADING_WORDS
        || !title.chars().any(char::is_alphabetic)
    {
        return None;
    }

    let number = raw_number.trim_end_matches('.');
    if number.is_empty() {
        return None;
    }
    let number_path = number
        .split('.')
        .map(|part| {
            if part.is_empty() || part.len() > 3 || !part.bytes().all(|byte| byte.is_ascii_digit()) {
                return None;
            }
            let value = part.parse::<u32>().ok()?;
            (value > 0).then_some(value)
        })
        .collect::<Option<Vec<_>>>()?;
    if number_path.is_empty() || number_path.len() > 6 {
        return None;
    }
    Some((number_path, format!("{} {}", number, title)))
}

fn has_parent_heading(accepted: &[PdfHeadingCandidate], path: &[u32]) -> bool {
    if path.len() <= 1 {
        return true;
    }
    let parent = &path[..path.len() - 1];
    accepted
        .iter()
        .rev()
        .any(|candidate| candidate.number_path == parent)
}

fn build_inferred_heading_sections(
    headings: &[PdfHeadingCandidate],
    pages: &[PageText],
) -> (Vec<Section>, Vec<OriginalSourceBinding>) {
    let lines = source_lines(pages);
    let heading_by_line = headings
        .iter()
        .enumerate()
        .map(|(index, heading)| (heading.global_line, index))
        .collect::<HashMap<_, _>>();

    let mut fragments = vec![PageFragments::default(); headings.len()];
    let mut preamble = PageFragments::default();
    let mut current_heading = None;
    for line in &lines {
        if let Some(index) = heading_by_line.get(&line.global_line).copied() {
            current_heading = Some(index);
            continue;
        }
        match current_heading {
            Some(index) => fragments[index].push_line(line.page, &line.text),
            None => preamble.push_line(line.page, &line.text),
        }
    }

    let mut nodes = Vec::<SectionNode>::with_capacity(headings.len());
    let mut id_counts = HashMap::<String, usize>::new();
    for (index, heading) in headings.iter().enumerate() {
        let parent = if heading.number_path.len() == 1 {
            None
        } else {
            let parent_path = &heading.number_path[..heading.number_path.len() - 1];
            headings[..index]
                .iter()
                .rposition(|candidate| candidate.number_path == parent_path)
        };
        let mut path = parent
            .map(|parent_index| nodes[parent_index].path.clone())
            .unwrap_or_default();
        path.push(heading.title.clone());
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
            title: heading.title.clone(),
            level: heading.number_path.len() as u8,
            start_page: heading.page,
            content: String::new(),
            location: Location::default(),
            path,
        });
    }

    let mut bindings = Vec::new();
    for (index, section_fragments) in fragments.iter().enumerate() {
        let (content, section_bindings, end_page) =
            content_and_bindings_from_fragments(section_fragments, &nodes[index].id);
        bindings.extend(section_bindings);
        nodes[index].content = content;
        nodes[index].location = Location {
            page: Some(nodes[index].start_page),
            chapter: nodes[index].path.first().cloned(),
            section_path: nodes[index].path.clone(),
            native_location: Some(pdf_page_range_location(nodes[index].start_page, end_page)),
            ..Location::default()
        };
    }

    let mut sections = Vec::new();
    let (preamble_content, preamble_bindings, preamble_end_page) =
        content_and_bindings_from_fragments(&preamble, &SectionId("section://preamble".into()));
    if !preamble_content.trim().is_empty() {
        let start_page = preamble.0.first().map(|(page, _)| *page).unwrap_or(1);
        sections.push(Section {
            id: SectionId("section://preamble".into()),
            parent_id: None,
            title: "Preamble".into(),
            level: 1,
            content: preamble_content,
            location: Location {
                page: Some(start_page),
                section_path: vec!["Preamble".into()],
                native_location: Some(pdf_page_range_location(start_page, preamble_end_page)),
                ..Location::default()
            },
            children: Vec::new(),
        });
        bindings.extend(preamble_bindings);
    }
    sections.extend(
        nodes
            .iter()
            .enumerate()
            .filter(|(_, node)| node.parent.is_none())
            .map(|(index, _)| build_section(index, &nodes)),
    );
    (sections, bindings)
}

fn content_and_bindings_from_fragments(
    fragments: &PageFragments,
    owner_section_id: &SectionId,
) -> (String, Vec<OriginalSourceBinding>, u32) {
    let mut content = String::new();
    let mut bindings = Vec::new();
    let mut end_page = fragments.0.first().map(|(page, _)| *page).unwrap_or(1);
    for (page, fragment) in &fragments.0 {
        let fragment = fragment.trim();
        if fragment.is_empty() {
            continue;
        }
        if !content.is_empty() {
            content.push_str("\n\n");
        }
        let start = content.chars().count();
        content.push_str(fragment);
        let end = content.chars().count();
        bindings.push(OriginalSourceBinding {
            owner_section_id: owner_section_id.clone(),
            normalized_range: NormalizedTextRange::new(start, end)
                .expect("inferred PDF fragment range must be ordered"),
            target: OriginalSourceTarget::Page { page_number: *page },
        });
        end_page = *page;
    }
    (content, bindings, end_page)
}

fn pdf_page_range_location(start_page: u32, end_page: u32) -> String {
    if end_page > start_page {
        format!("pdf:pages:{start_page}-{end_page}")
    } else {
        format!("pdf:page:{start_page}")
    }
}

fn build_page_sections(pages: &[PageText]) -> (Vec<Section>, Vec<OriginalSourceBinding>) {
    let mut bindings = Vec::new();
    let sections = pages
        .iter()
        .map(|page| {
            let title = format!("Page {}", page.number);
            let id = SectionId(format!("section://page-{}", page.number));
            let content_len = page.text.chars().count();
            if content_len > 0 {
                bindings.push(OriginalSourceBinding {
                    owner_section_id: id.clone(),
                    normalized_range: NormalizedTextRange::new(0, content_len)
                        .expect("page content range must be ordered"),
                    target: OriginalSourceTarget::Page {
                        page_number: page.number,
                    },
                });
            }
            Section {
                id,
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
        .collect();
    (sections, bindings)
}

fn build_toc_sections(
    entries: &[PdfTocEntry],
    pages: &[PageText],
    max_page: u32,
) -> (Vec<Section>, Vec<OriginalSourceBinding>) {
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

    let mut bindings = Vec::new();
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

        let (content, section_bindings) = end_page
            .map(|end| {
                source_text_and_bindings_for_page_range(
                    &page_map,
                    &nodes[index].id,
                    start_page,
                    end,
                )
            })
            .unwrap_or_default();
        bindings.extend(section_bindings);
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

    let sections = nodes
        .iter()
        .enumerate()
        .filter(|(_, node)| node.parent.is_none())
        .map(|(index, _)| build_section(index, &nodes))
        .collect();
    (sections, bindings)
}

fn source_text_and_bindings_for_page_range(
    pages: &BTreeMap<u32, &str>,
    owner_section_id: &SectionId,
    start_page: u32,
    end_page: u32,
) -> (String, Vec<OriginalSourceBinding>) {
    let mut content = String::new();
    let mut bindings = Vec::new();

    for page_number in start_page..=end_page {
        let Some(text) = pages.get(&page_number).copied() else {
            continue;
        };
        if text.trim().is_empty() {
            continue;
        }
        if !content.is_empty() {
            content.push_str("\n\n");
        }
        let start = content.chars().count();
        content.push_str(text);
        let end = content.chars().count();
        bindings.push(OriginalSourceBinding {
            owner_section_id: owner_section_id.clone(),
            normalized_range: NormalizedTextRange::new(start, end)
                .expect("page binding range must be ordered"),
            target: OriginalSourceTarget::Page { page_number },
        });
    }

    (content, bindings)
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
