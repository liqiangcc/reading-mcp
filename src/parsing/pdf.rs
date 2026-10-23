use std::collections::{BTreeMap, HashMap};

use async_trait::async_trait;
use lopdf::{Document as LopdfDocument, Object, ObjectId, TocType, decode_text_string};

use crate::application::ports::{ApplicationError, OcrRequiredDetail, Parser, RetrievedResource};
use crate::domain::{
    ContentHash, Document, Location, NormalizedTextRange, OriginalSourceBinding,
    OriginalSourceBindingMap, OriginalSourceTarget, Section, SectionId,
};

use super::blocking::run_blocking;
use super::common::{content_hash, document_id, slugify, title_from_metadata};

/// Changes whenever the default PDF backend publishes something different for the same bytes. PDF-4a
/// makes an image-only PDF fail with `ocr_required` instead of publishing a few watermark characters,
/// so a stored `ok` from an older build must not shadow the rule on reopen: this bump invalidates the
/// parsed cache and the stored open results for PDFs.
pub const PDF_TEXT_EXTRACTION_VERSION: &str = "pdf-text-extraction/v2";
const MAX_PAGE_DECOMPRESSED_BYTES: usize = 16 * 1024 * 1024;
const PDF_STRUCTURE_PROVENANCE_METADATA_KEY: &str = "pdf_structure_provenance";
const PDF_STRUCTURE_NATIVE_TOC: &str = "native_toc";
const PDF_STRUCTURE_INFERRED_NUMBERED_HEADINGS: &str = "inferred_numbered_headings";
const PDF_STRUCTURE_PAGE_FALLBACK: &str = "page_fallback";
const PDF_HEADING_INFERENCE_VERSION_METADATA_KEY: &str = "pdf_heading_inference_version";
const PDF_HEADING_INFERENCE_COUNT_METADATA_KEY: &str = "pdf_heading_inference_count";
const PDF_HEADING_INFERENCE_VERSION: &str = "pdf-numbered-heading-inference/v2";
const MAX_INFERRED_HEADING_CHARS: usize = 160;
const MAX_INFERRED_HEADING_WORDS: usize = 24;
/// Below this many normalized characters per page the PDF is treated as having
/// no usable text layer. The minimum among the 121 text PDFs that open `ok` is
/// 842.6 chars/page, so this leaves a 4.2x margin below every legitimate text
/// PDF and never fires on text length alone.
const OCR_IMAGE_ONLY_MAX_CHARS_PER_PAGE: f64 = 200.0;
/// At or above this mean raster coverage the pages are image-dominated. A text
/// PDF with a cover image, a blank page or a figure appendix stays far below it.
const OCR_IMAGE_ONLY_MIN_RASTER_COVERAGE: f64 = 0.5;

#[derive(Default)]
pub struct PdfParser;

#[derive(Clone, Debug, PartialEq)]
struct PageText {
    number: u32,
    text: String,
    chunks: Vec<String>,
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
    global_fragment: usize,
}

#[derive(Clone, Debug)]
struct PdfSourceFragment {
    page: u32,
    global_fragment: usize,
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

impl PdfParser {
    pub(super) fn parse_sync(
        &self,
        mut resource: RetrievedResource,
    ) -> Result<Document, ApplicationError> {
        let (pdf, hash) = load_and_release(&mut resource)?;
        self.build_document(&pdf, resource, hash)
    }

    /// Builds the document from an object tree that is already loaded, so a caller that needed the
    /// tree for its own pass (the page limit, the front-matter evidence) loads the PDF once.
    pub(super) fn build_document(
        &self,
        pdf: &LopdfDocument,
        resource: RetrievedResource,
        hash: ContentHash,
    ) -> Result<Document, ApplicationError> {
        let id = document_id(&resource.final_source, &hash);
        let page_ids = pdf.get_pages().into_iter().collect::<Vec<_>>();
        let page_numbers = page_ids
            .iter()
            .map(|(number, _)| *number)
            .collect::<Vec<_>>();
        if page_numbers.is_empty() {
            return Err(ApplicationError::ParseFailed(
                "PDF does not contain any pages".into(),
            ));
        }

        // The repair copy is a second full object tree, made only for the page text: it is
        // released as soon as the pages are read instead of being carried through the section
        // build and the save.
        let (pages, extraction_errors) = {
            let prepared = super::pdf_text_repair::prepare_text_extraction(pdf);
            #[cfg(test)]
            held_buffers::repair_copy_present(prepared.is_some());
            let extracted = extract_page_texts_raw(prepared.as_ref().unwrap_or(pdf), &page_numbers);
            drop(prepared);
            #[cfg(test)]
            held_buffers::repair_copy_present(false);
            extracted
        };
        // The two conditions must hold together: almost no text for the page
        // count, and pages that are actually painted by raster images. A text
        // PDF with a cover, a blank page or a figures appendix fails the second.
        if let Some(detail) = image_only_scan_evidence(pdf, &page_ids, &pages) {
            return Err(ApplicationError::OcrRequired(detail));
        }
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

        let pdf_title = pdf_title(pdf);
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
            None => {
                // One fragment list for both the heading inference and the section build. It is a
                // copy of the page text, and building it twice held two of them at the peak.
                let fragments = source_fragments(&pages);
                match infer_numbered_headings(&fragments) {
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
                        build_inferred_heading_sections(&headings, &fragments)
                    }
                    None => {
                        metadata.insert(
                            PDF_STRUCTURE_PROVENANCE_METADATA_KEY.into(),
                            PDF_STRUCTURE_PAGE_FALLBACK.into(),
                        );
                        build_page_sections(pages)
                    }
                }
            }
        };

        let fallback_title = title_from_metadata(&metadata, &resource.final_source);
        let title = pdf_title.unwrap_or(fallback_title);
        #[cfg(test)]
        held_buffers::document_started();
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

#[async_trait]
impl Parser for PdfParser {
    async fn parse(&self, resource: RetrievedResource) -> Result<Document, ApplicationError> {
        run_blocking(move || PdfParser.parse_sync(resource)).await
    }
}

#[cfg(test)]
fn extract_page_texts(pdf: &LopdfDocument, page_numbers: &[u32]) -> (Vec<PageText>, Vec<String>) {
    let prepared = super::pdf_text_repair::prepare_text_extraction(pdf);
    extract_page_texts_raw(prepared.as_ref().unwrap_or(pdf), page_numbers)
}

/// Reads every page's text with one content walk per page.
///
/// `Document::extract_text_with_limit` is this same walk with the fragments concatenated and the
/// first error returned, so the page is walked once and both facts are derived from its one
/// result: decoding the page's streams, every font's encoding and every `/ToUnicode` CMap twice
/// per page bought nothing and held a second copy of the page's decoded content at the peak.
fn extract_page_texts_raw(
    pdf: &LopdfDocument,
    page_numbers: &[u32],
) -> (Vec<PageText>, Vec<String>) {
    let mut pages = Vec::with_capacity(page_numbers.len());
    let mut errors = Vec::new();

    for page_number in page_numbers {
        let mut text = String::new();
        let mut first_error = None;
        let mut chunks = Vec::new();
        for fragment in
            pdf.extract_text_chunks_with_limit(&[*page_number], MAX_PAGE_DECOMPRESSED_BYTES)
        {
            match fragment {
                Ok(fragment) => {
                    text.push_str(&fragment);
                    let chunk = normalize_pdf_text(&fragment);
                    if !chunk.is_empty() {
                        chunks.push(chunk);
                    }
                }
                // The whole page's text is the error, exactly as the concatenating variant
                // returns it: the first failing fragment wins.
                Err(error) => {
                    if first_error.is_none() {
                        first_error = Some(error);
                    }
                }
            }
        }
        let text = match first_error {
            Some(error) => {
                errors.push(format!("page {page_number}: {error}"));
                String::new()
            }
            None => normalize_pdf_text(&text),
        };
        pages.push(PageText {
            number: *page_number,
            text,
            chunks,
        });
    }

    (pages, errors)
}

fn normalize_pdf_text(value: &str) -> String {
    value.trim().to_string()
}

/// Hashes the raw bytes, loads the object tree and releases the file.
///
/// The tree owns every decoded object, so nothing after this point reads the bytes again: they are
/// released before the page text, the sections and the stored JSON are built beside them instead
/// of at the end of the parse. The file is the largest single buffer a big PDF has (PDF-2c: 45.4
/// MiB of `2401.14345`'s 284.1 MiB peak).
pub(super) fn load_and_release(
    resource: &mut RetrievedResource,
) -> Result<(LopdfDocument, ContentHash), ApplicationError> {
    #[cfg(test)]
    held_buffers::raw_bytes_present(true);
    let hash = content_hash(&resource.bytes);
    let pdf = LopdfDocument::load_mem(&resource.bytes).map_err(|error| {
        ApplicationError::ParseFailed(format!("invalid PDF document: {error}"))
    })?;
    #[cfg(test)]
    held_buffers::raw_bytes_present(false);
    drop(std::mem::take(&mut resource.bytes));
    Ok((pdf, hash))
}

/// Test-only record of the two large buffers a parse holds before the document is built: the raw
/// PDF bytes and the repair copy of the object tree. `parse_sync` reports what it still holds at
/// the point the document is built, so a change that carries either one through the section build
/// fails a test instead of silently restoring the peak. Thread-local: tests parse in parallel.
#[cfg(test)]
pub(super) mod held_buffers {
    use std::cell::RefCell;

    #[derive(Default)]
    struct Held {
        raw_bytes: bool,
        raw_bytes_seen: bool,
        repair_copy: bool,
        repair_copy_seen: bool,
        at_document_start: Option<(bool, bool)>,
    }

    thread_local! {
        static HELD: RefCell<Held> = RefCell::new(Held::default());
    }

    pub(super) fn raw_bytes_present(present: bool) {
        HELD.with(|held| {
            let mut held = held.borrow_mut();
            held.raw_bytes = present;
            held.raw_bytes_seen |= present;
        });
    }

    pub(super) fn repair_copy_present(present: bool) {
        HELD.with(|held| {
            let mut held = held.borrow_mut();
            held.repair_copy = present;
            held.repair_copy_seen |= present;
        });
    }

    /// Snapshots what the parse still holds at the point it builds the document.
    pub(super) fn document_started() {
        HELD.with(|held| {
            let mut held = held.borrow_mut();
            held.at_document_start = Some((held.raw_bytes, held.repair_copy));
        });
    }

    /// Clears the record so one test's parse cannot be read as another's.
    pub(crate) fn reset() {
        HELD.with(|held| *held.borrow_mut() = Held::default());
    }

    /// The raw bytes were held at some point, so the accounting below is wired to this parse.
    pub(crate) fn raw_bytes_were_held() -> bool {
        HELD.with(|held| held.borrow().raw_bytes_seen)
    }

    /// The repair copy was made, so a test that expects it to be released is not vacuous.
    pub(crate) fn repair_copy_was_made() -> bool {
        HELD.with(|held| held.borrow().repair_copy_seen)
    }

    /// `(raw bytes, repair copy)` still held when the document was built; `None` when the parse
    /// never reached that point.
    pub(crate) fn held_at_document_start() -> Option<(bool, bool)> {
        HELD.with(|held| held.borrow().at_document_start)
    }
}

/// True when the document has almost no text for its page count *and* its pages
/// are dominated by raster images. Either condition alone is not enough.
fn image_only_scan_evidence(
    pdf: &LopdfDocument,
    page_ids: &[(u32, ObjectId)],
    pages: &[PageText],
) -> Option<OcrRequiredDetail> {
    if page_ids.is_empty() {
        return None;
    }
    let characters = pages
        .iter()
        .map(|page| page.text.chars().count())
        .sum::<usize>();
    let chars_per_page = characters as f64 / page_ids.len() as f64;
    if chars_per_page >= OCR_IMAGE_ONLY_MAX_CHARS_PER_PAGE {
        return None;
    }
    let coverage = super::pdf_raster::document_raster_coverage(pdf, page_ids);
    (coverage >= OCR_IMAGE_ONLY_MIN_RASTER_COVERAGE)
        .then_some(OcrRequiredDetail::measured(page_ids.len(), characters))
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

fn infer_numbered_headings(fragments: &[PdfSourceFragment]) -> Option<Vec<PdfHeadingCandidate>> {
    let parsed = fragments
        .iter()
        .filter_map(|fragment| {
            parse_numbered_heading(&fragment.text).map(|(number_path, title)| PdfHeadingCandidate {
                number_path,
                title,
                page: fragment.page,
                global_fragment: fragment.global_fragment,
            })
        })
        .collect::<Vec<_>>();

    let mut accepted = Vec::new();
    let mut expected_top = 1_u32;
    let mut current_top = None;
    let mut expected_child = HashMap::<Vec<u32>, u32>::new();

    for candidate in parsed {
        let top = candidate.number_path[0];
        if candidate.number_path.len() == 1 {
            if top == expected_top && top <= 99 {
                current_top = Some(top);
                expected_top = expected_top.saturating_add(1);
                expected_child
                    .entry(candidate.number_path.clone())
                    .or_insert(1);
                accepted.push(candidate);
            }
            continue;
        }

        if current_top != Some(top) || !has_parent_heading(&accepted, &candidate.number_path) {
            continue;
        }
        let parent = candidate.number_path[..candidate.number_path.len() - 1].to_vec();
        let child = *candidate.number_path.last().unwrap_or(&0);
        let expected = expected_child.entry(parent).or_insert(1);
        if child != *expected {
            continue;
        }
        *expected = (*expected).saturating_add(1);
        expected_child
            .entry(candidate.number_path.clone())
            .or_insert(1);
        accepted.push(candidate);
    }

    let top_level_count = accepted
        .iter()
        .filter(|candidate| candidate.number_path.len() == 1)
        .count();
    let top_level_pages = accepted
        .iter()
        .filter(|candidate| candidate.number_path.len() == 1)
        .map(|candidate| candidate.page)
        .collect::<std::collections::BTreeSet<_>>();
    (top_level_count >= 2 && top_level_pages.len() >= 2).then_some(accepted)
}

fn source_fragments(pages: &[PageText]) -> Vec<PdfSourceFragment> {
    let mut fragments = Vec::new();
    let mut global_fragment = 0_usize;
    for page in pages {
        for text in &page.chunks {
            fragments.push(PdfSourceFragment {
                page: page.number,
                global_fragment,
                text: text.clone(),
            });
            global_fragment = global_fragment.saturating_add(1);
        }
    }
    fragments
}

fn parse_numbered_heading(fragment: &str) -> Option<(Vec<u32>, String)> {
    let fragment = fragment.trim();
    if fragment.is_empty()
        || fragment.contains('\n')
        || fragment.chars().count() > MAX_INFERRED_HEADING_CHARS
    {
        return None;
    }

    let mut prefix_end = 0_usize;
    for (index, character) in fragment.char_indices() {
        if character.is_ascii_digit() || character == '.' {
            prefix_end = index + character.len_utf8();
        } else {
            break;
        }
    }
    if prefix_end == 0 {
        return None;
    }

    let raw_number = &fragment[..prefix_end];
    let number = raw_number.trim_end_matches('.');
    let title = fragment[prefix_end..]
        .trim_start_matches(|character: char| {
            character.is_whitespace() || matches!(character, '.' | ':' | '-' | '–' | '—')
        })
        .trim();
    if number.is_empty()
        || title.is_empty()
        || title.split_whitespace().count() > MAX_INFERRED_HEADING_WORDS
        || !title.chars().any(char::is_alphabetic)
        || !title.chars().next().is_some_and(char::is_uppercase)
    {
        return None;
    }

    let number_path = number
        .split('.')
        .map(|part| {
            if part.is_empty() || part.len() > 3 || !part.bytes().all(|byte| byte.is_ascii_digit())
            {
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
    fragments_source: &[PdfSourceFragment],
) -> (Vec<Section>, Vec<OriginalSourceBinding>) {
    let heading_by_fragment = headings
        .iter()
        .enumerate()
        .map(|(index, heading)| (heading.global_fragment, index))
        .collect::<HashMap<_, _>>();

    let mut fragments = vec![PageFragments::default(); headings.len()];
    let mut preamble = PageFragments::default();
    let mut current_heading = None;
    for fragment in fragments_source {
        if let Some(index) = heading_by_fragment.get(&fragment.global_fragment).copied() {
            current_heading = Some(index);
            continue;
        }
        match current_heading {
            Some(index) => fragments[index].push_line(fragment.page, &fragment.text),
            None => preamble.push_line(fragment.page, &fragment.text),
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

/// One section per page, taking each page's text instead of copying it: the page list is dropped
/// right after this, so a copy would be a second full copy of the document's text at the peak.
fn build_page_sections(pages: Vec<PageText>) -> (Vec<Section>, Vec<OriginalSourceBinding>) {
    let mut bindings = Vec::new();
    let sections = pages
        .into_iter()
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
                content: page.text,
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

#[cfg(test)]
mod tests {
    use super::*;
    use lopdf::content::{Content, Operation};
    use lopdf::{Dictionary, ObjectId, Stream, StringFormat, dictionary};

    fn page_texts(document: &LopdfDocument) -> Vec<PageText> {
        let pages = document.get_pages().keys().copied().collect::<Vec<_>>();
        let (texts, errors) = extract_page_texts(document, &pages);
        assert!(
            errors.is_empty(),
            "unexpected extraction errors: {errors:?}"
        );
        texts
    }

    fn old_page_texts(document: &LopdfDocument) -> Vec<PageText> {
        let pages = document.get_pages().keys().copied().collect::<Vec<_>>();
        let (texts, errors) = extract_page_texts_raw(document, &pages);
        assert!(
            errors.is_empty(),
            "unexpected extraction errors: {errors:?}"
        );
        texts
    }

    fn byte_text_operations(font: &str, bytes: &[u8]) -> Vec<Operation> {
        vec![
            Operation::new("BT", vec![]),
            Operation::new("Tf", vec![font.into(), 12.into()]),
            Operation::new("Td", vec![72.into(), 720.into()]),
            Operation::new(
                "Tj",
                vec![Object::String(bytes.to_vec(), StringFormat::Literal)],
            ),
            Operation::new("ET", vec![]),
        ]
    }

    fn cjk_font(document: &mut LopdfDocument) -> ObjectId {
        document.add_object(dictionary! {
            "Type" => "Font",
            "Subtype" => "Type1",
            "BaseFont" => "IMFCTT1",
            "Encoding" => dictionary! {
                "Type" => "Encoding",
                "Differences" => vec![
                    Object::Integer(65),
                    Object::Name(b"uni4E2D".to_vec()),
                    Object::Name(b"uni6587".to_vec()),
                ],
            },
        })
    }

    fn form(
        document: &mut LopdfDocument,
        resources: Dictionary,
        matrix: Option<Vec<Object>>,
        operations: Vec<Operation>,
    ) -> ObjectId {
        let mut dictionary = dictionary! {
            "Type" => "XObject",
            "Subtype" => "Form",
            "BBox" => vec![0.into(), 0.into(), 100.into(), 100.into()],
            "Resources" => Object::Dictionary(resources),
        };
        if let Some(matrix) = matrix {
            dictionary.set("Matrix", Object::Array(matrix));
        }
        document.add_object(Stream::new(
            dictionary,
            Content { operations }
                .encode()
                .expect("form content encodes"),
        ))
    }

    fn add_page(document: &mut LopdfDocument, operations: Vec<Operation>, resources: Dictionary) {
        let pages_id = document.new_object_id();
        let content_id = document.add_object(Stream::new(
            Dictionary::new(),
            Content { operations }
                .encode()
                .expect("page content encodes"),
        ));
        let page_id = document.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "Contents" => content_id,
            "Resources" => Object::Dictionary(resources),
            "MediaBox" => vec![0.into(), 0.into(), 595.into(), 842.into()],
        });
        document.objects.insert(
            pages_id,
            Object::Dictionary(dictionary! {
                "Type" => "Pages",
                "Kids" => vec![Object::Reference(page_id)],
                "Count" => 1_i64,
            }),
        );
        let catalog_id = document.add_object(dictionary! {
            "Type" => "Catalog",
            "Pages" => pages_id,
        });
        document.trailer.set("Root", catalog_id);
    }

    #[test]
    fn text_inside_a_form_xobject_is_extracted() {
        let mut document = LopdfDocument::with_version("1.5");
        let font_id = cjk_font(&mut document);
        let form_id = form(
            &mut document,
            dictionary! { "Font" => dictionary! { "FX" => font_id } },
            Some(vec![
                1.into(),
                0.into(),
                0.into(),
                1.into(),
                10.into(),
                20.into(),
            ]),
            byte_text_operations("FX", &[65]),
        );
        add_page(
            &mut document,
            vec![
                Operation::new("q", vec![]),
                Operation::new("Do", vec![Object::Name(b"Fm1".to_vec())]),
                Operation::new("Q", vec![]),
            ],
            dictionary! { "XObject" => dictionary! { "Fm1" => form_id } },
        );

        assert_eq!(page_texts(&document)[0].text, "中");
        assert!(old_page_texts(&document)[0].text.is_empty());
    }

    #[test]
    fn form_text_order_follows_the_content_stream() {
        let mut document = LopdfDocument::with_version("1.5");
        let font_id = cjk_font(&mut document);
        let first = form(
            &mut document,
            dictionary! { "Font" => dictionary! { "F1" => font_id } },
            None,
            byte_text_operations("F1", &[65]),
        );
        let second = form(
            &mut document,
            dictionary! { "Font" => dictionary! { "F1" => font_id } },
            None,
            byte_text_operations("F1", &[66]),
        );
        add_page(
            &mut document,
            vec![
                Operation::new("Do", vec![Object::Name(b"Fm1".to_vec())]),
                Operation::new("Do", vec![Object::Name(b"Fm2".to_vec())]),
            ],
            dictionary! {
                "XObject" => dictionary! { "Fm1" => first, "Fm2" => second },
            },
        );

        assert_eq!(page_texts(&document)[0].text, "中\n文");
    }

    #[test]
    fn nested_form_xobjects_are_extracted_once() {
        let mut document = LopdfDocument::with_version("1.5");
        let font_id = cjk_font(&mut document);
        let inner = form(
            &mut document,
            dictionary! { "Font" => dictionary! { "F1" => font_id } },
            None,
            byte_text_operations("F1", &[65]),
        );
        let outer = form(
            &mut document,
            dictionary! { "XObject" => dictionary! { "Inner" => inner } },
            None,
            vec![Operation::new("Do", vec![Object::Name(b"Inner".to_vec())])],
        );
        add_page(
            &mut document,
            vec![Operation::new("Do", vec![Object::Name(b"Outer".to_vec())])],
            dictionary! { "XObject" => dictionary! { "Outer" => outer } },
        );

        assert_eq!(page_texts(&document)[0].text, "中");
    }

    #[test]
    fn cyclic_form_xobject_terminates_and_keeps_its_text() {
        let mut document = LopdfDocument::with_version("1.5");
        let font_id = cjk_font(&mut document);
        let form_id = document.new_object_id();
        let mut operations = byte_text_operations("F1", &[65]);
        operations.push(Operation::new("Do", vec![Object::Name(b"Self".to_vec())]));
        document.set_object(
            form_id,
            Stream::new(
                dictionary! {
                    "Type" => "XObject",
                    "Subtype" => "Form",
                    "BBox" => vec![0.into(), 0.into(), 100.into(), 100.into()],
                    "Resources" => dictionary! { "Font" => dictionary! { "F1" => font_id } },
                },
                Content { operations }
                    .encode()
                    .expect("form content encodes"),
            ),
        );
        add_page(
            &mut document,
            vec![Operation::new("Do", vec![Object::Name(b"Self".to_vec())])],
            dictionary! { "XObject" => dictionary! { "Self" => form_id } },
        );

        assert_eq!(page_texts(&document)[0].text, "中");
    }

    #[test]
    fn an_image_only_xobject_leaves_the_extraction_unchanged() {
        let mut document = LopdfDocument::with_version("1.5");
        let form_id = form(
            &mut document,
            Dictionary::new(),
            None,
            vec![Operation::new("q", vec![]), Operation::new("Q", vec![])],
        );
        add_page(
            &mut document,
            vec![Operation::new("Do", vec![Object::Name(b"Fm1".to_vec())])],
            dictionary! { "XObject" => dictionary! { "Fm1" => form_id } },
        );

        assert_eq!(page_texts(&document), old_page_texts(&document));
    }

    #[test]
    fn uni_differences_font_without_to_unicode_is_decoded() {
        let mut document = LopdfDocument::with_version("1.5");
        let font_id = document.add_object(dictionary! {
            "Type" => "Font",
            "Subtype" => "Type1",
            "BaseFont" => "IMFCTT1",
            "Encoding" => dictionary! {
                "Type" => "Encoding",
                "Differences" => vec![
                    Object::Integer(65),
                    Object::Name(b"uni4E2D".to_vec()),
                    Object::Name(b"uni6587".to_vec()),
                ],
            },
        });
        add_page(
            &mut document,
            byte_text_operations("F1", &[65, 66]),
            dictionary! { "Font" => dictionary! { "F1" => font_id } },
        );

        assert_eq!(page_texts(&document)[0].text, "中文");
    }

    #[test]
    fn uni_differences_font_with_to_unicode_keeps_it() {
        let mut document = LopdfDocument::with_version("1.5");
        let to_unicode = b"/CIDInit /ProcSet findresource begin\n\
            12 dict begin\n\
            begincmap\n\
            /CMapName /Adobe-Identity-UCS def\n\
            /CMapType 2 def\n\
            1 begincodespacerange\n\
            <00> <FF>\n\
            endcodespacerange\n\
            2 beginbfchar\n\
            <41> <4E2D>\n\
            <42> <6587>\n\
            endbfchar\n\
            endcmap\n\
            CMapName currentdict /CMap defineresource pop\n\
            end\n\
            end\n";
        let to_unicode_id =
            document.add_object(Stream::new(Dictionary::new(), to_unicode.to_vec()));
        let font_id = document.add_object(dictionary! {
            "Type" => "Font",
            "Subtype" => "Type1",
            "BaseFont" => "IMFCTT1",
            "Encoding" => dictionary! {
                "Type" => "Encoding",
                "Differences" => vec![
                    Object::Integer(65),
                    Object::Name(b"uni4E2D".to_vec()),
                    Object::Name(b"uni6587".to_vec()),
                ],
            },
            "ToUnicode" => to_unicode_id,
        });
        add_page(
            &mut document,
            byte_text_operations("F1", &[65, 66]),
            dictionary! { "Font" => dictionary! { "F1" => font_id } },
        );

        assert_eq!(page_texts(&document)[0].text, "中文");
    }

    #[test]
    fn type0_form_font_with_to_unicode_is_extracted() {
        let mut document = LopdfDocument::with_version("1.5");
        let to_unicode = b"/CIDInit /ProcSet findresource begin\n\
            12 dict begin\n\
            begincmap\n\
            /CMapName /Adobe-Identity-UCS def\n\
            /CMapType 2 def\n\
            1 begincodespacerange\n\
            <0000> <FFFF>\n\
            endcodespacerange\n\
            4 beginbfchar\n\
            <0001> <4E2D>\n\
            <0002> <6587>\n\
            <0003> <5B57>\n\
            <0004> <7B26>\n\
            endbfchar\n\
            endcmap\n\
            CMapName currentdict /CMap defineresource pop\n\
            end\n\
            end\n";
        let to_unicode_id =
            document.add_object(Stream::new(Dictionary::new(), to_unicode.to_vec()));
        let font_id = document.add_object(dictionary! {
            "Type" => "Font",
            "Subtype" => "Type0",
            "BaseFont" => "TestCJK",
            "Encoding" => "Identity-H",
            "ToUnicode" => to_unicode_id,
        });
        let form_id = form(
            &mut document,
            dictionary! { "Font" => dictionary! { "F1" => font_id } },
            None,
            byte_text_operations("F1", &[0, 1, 0, 2, 0, 3, 0, 4]),
        );
        add_page(
            &mut document,
            vec![Operation::new("Do", vec![Object::Name(b"Fm1".to_vec())])],
            dictionary! { "XObject" => dictionary! { "Fm1" => form_id } },
        );

        assert_eq!(page_texts(&document)[0].text, "中文字符");
    }

    #[test]
    fn adobe_glyph_names_keep_their_old_decoding() {
        let mut document = LopdfDocument::with_version("1.5");
        let font_id = document.add_object(dictionary! {
            "Type" => "Font",
            "Subtype" => "Type1",
            "BaseFont" => "Helvetica",
            "Encoding" => dictionary! {
                "Type" => "Encoding",
                "Differences" => vec![
                    Object::Integer(65),
                    Object::Name(b"A".to_vec()),
                    Object::Name(b"B".to_vec()),
                ],
            },
        });
        add_page(
            &mut document,
            byte_text_operations("F1", &[65, 66]),
            dictionary! { "Font" => dictionary! { "F1" => font_id } },
        );

        assert_eq!(page_texts(&document)[0].text, "AB");
    }

    #[test]
    fn fixtures_without_the_two_features_keep_their_old_extraction() {
        let directory =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/scanned_pdf/pdf");
        let mut checked = 0;
        for entry in std::fs::read_dir(&directory).expect("fixture directory") {
            let entry = entry.expect("fixture entry");
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("pdf") {
                continue;
            }
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            let bytes = std::fs::read(&path).expect("fixture bytes");
            let document = LopdfDocument::load_mem(&bytes).expect("fixture loads");
            let old = old_page_texts(&document);
            let new = page_texts(&document);
            assert_eq!(old, new, "fixture {name} changed");
            checked += 1;
        }
        assert!(checked >= 15, "expected 15 unchanged fixtures");
    }

    fn collect_pdf_paths(directory: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        let Ok(entries) = std::fs::read_dir(directory) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                collect_pdf_paths(&path, out);
            } else if path.extension().and_then(|ext| ext.to_str()) == Some("pdf") {
                out.push(path);
            }
        }
    }

    fn measurement_resource(path: &std::path::Path) -> RetrievedResource {
        let source = crate::domain::DocumentSource(format!("file://{}", path.display()));
        RetrievedResource {
            source: source.clone(),
            final_source: source,
            media_type: crate::domain::MediaType("application/pdf".into()),
            bytes: std::fs::read(path).expect("fixture bytes"),
            etag: None,
            last_modified: None,
            metadata: Default::default(),
        }
    }

    /// PDF-4a Step 1: the outcome, text and raster coverage of every PDF fixture
    /// and, through `PDF4A_MEASURE_DIR`, the generated parity PDFs. This is the
    /// "today" table for the design note; it changes no behaviour.
    #[test]
    #[ignore = "pdf4a measurement: run explicitly with --ignored --nocapture"]
    fn pdf4a_measurement_table() {
        let mut paths = Vec::new();
        collect_pdf_paths(
            &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures"),
            &mut paths,
        );
        if let Ok(extra) = std::env::var("PDF4A_MEASURE_DIR") {
            collect_pdf_paths(std::path::Path::new(&extra), &mut paths);
        }
        paths.sort();
        println!("| input | outcome | pages | chars | chars/page | sections | raster coverage |");
        println!("|---|---|---:|---:|---:|---:|---:|");
        for path in &paths {
            let resource = measurement_resource(path);
            let label = path
                .strip_prefix(env!("CARGO_MANIFEST_DIR"))
                .unwrap_or(path)
                .display();
            let pdf = LopdfDocument::load_mem(&resource.bytes).expect("fixture loads");
            let page_ids = pdf.get_pages().into_iter().collect::<Vec<_>>();
            let page_numbers = page_ids
                .iter()
                .map(|(number, _)| *number)
                .collect::<Vec<_>>();
            let prepared = crate::parsing::pdf_text_repair::prepare_text_extraction(&pdf);
            let (pages, _) =
                extract_page_texts_raw(prepared.as_ref().unwrap_or(&pdf), &page_numbers);
            let chars: usize = pages.iter().map(|page| page.text.chars().count()).sum();
            let coverage = crate::parsing::pdf_raster::document_raster_coverage(&pdf, &page_ids);
            let pages = page_ids.len();
            let (outcome, sections) = match PdfParser.parse_sync(resource) {
                Ok(document) => ("ok".to_string(), document.section_count().to_string()),
                Err(ApplicationError::OcrRequired(_)) => {
                    ("ocr_required".to_string(), "-".to_string())
                }
                Err(ApplicationError::ParseFailed(_)) => {
                    ("unreadable".to_string(), "-".to_string())
                }
                Err(error) => (format!("parse_failed: {error}"), "-".to_string()),
            };
            println!(
                "| {} | {} | {} | {} | {:.1} | {} | {:.3} |",
                label,
                outcome,
                pages,
                chars,
                chars as f64 / pages.max(1) as f64,
                sections,
                coverage
            );
        }
    }

    fn build_pdf(
        mut document: LopdfDocument,
        pages: Vec<(Vec<Operation>, Dictionary)>,
    ) -> LopdfDocument {
        let pages_id = document.new_object_id();
        let mut kids = Vec::new();
        for (operations, resources) in pages {
            let content_id = document.add_object(Stream::new(
                Dictionary::new(),
                Content { operations }
                    .encode()
                    .expect("page content encodes"),
            ));
            let page_id = document.add_object(dictionary! {
                "Type" => "Page",
                "Parent" => pages_id,
                "Contents" => content_id,
                "Resources" => Object::Dictionary(resources),
                "MediaBox" => vec![0.into(), 0.into(), 595.into(), 842.into()],
            });
            kids.push(Object::Reference(page_id));
        }
        let count = kids.len() as i64;
        document.objects.insert(
            pages_id,
            Object::Dictionary(dictionary! {
                "Type" => "Pages",
                "Kids" => kids,
                "Count" => count,
            }),
        );
        let catalog_id = document.add_object(dictionary! {
            "Type" => "Catalog",
            "Pages" => pages_id,
        });
        document.trailer.set("Root", catalog_id);
        document
    }

    fn image_xobject(document: &mut LopdfDocument) -> ObjectId {
        document.add_object(Stream::new(
            dictionary! {
                "Type" => "XObject",
                "Subtype" => "Image",
                "Width" => 1_i64,
                "Height" => 1_i64,
                "ColorSpace" => "DeviceGray",
                "BitsPerComponent" => 8_i64,
            },
            vec![0_u8],
        ))
    }

    fn image_page_operations() -> Vec<Operation> {
        vec![
            Operation::new("q", vec![]),
            Operation::new(
                "cm",
                vec![
                    595.into(),
                    0.into(),
                    0.into(),
                    842.into(),
                    0.into(),
                    0.into(),
                ],
            ),
            Operation::new("Do", vec![Object::Name(b"Im1".to_vec())]),
            Operation::new("Q", vec![]),
        ]
    }

    fn text_resources(document: &mut LopdfDocument) -> Dictionary {
        // A `Differences` name is what lopdf actually decodes; a bare Helvetica
        // with no encoding extracts nothing.
        let font_id = document.add_object(dictionary! {
            "Type" => "Font",
            "Subtype" => "Type1",
            "BaseFont" => "Helvetica",
            "Encoding" => dictionary! {
                "Type" => "Encoding",
                "Differences" => vec![Object::Integer(65), Object::Name(b"A".to_vec())],
            },
        });
        dictionary! { "Font" => dictionary! { "F1" => font_id } }
    }

    fn parse_bytes(bytes: Vec<u8>) -> Result<Document, ApplicationError> {
        let source = crate::domain::DocumentSource("file:///synthetic.pdf".into());
        PdfParser.parse_sync(RetrievedResource {
            source: source.clone(),
            final_source: source,
            media_type: crate::domain::MediaType("application/pdf".into()),
            bytes,
            etag: None,
            last_modified: None,
            metadata: Default::default(),
        })
    }

    #[test]
    fn an_image_only_page_without_a_text_layer_reports_ocr_required() {
        let mut document = LopdfDocument::with_version("1.5");
        let image = image_xobject(&mut document);
        let mut document = build_pdf(
            document,
            vec![(
                image_page_operations(),
                dictionary! { "XObject" => dictionary! { "Im1" => image } },
            )],
        );
        let mut bytes = Vec::new();
        document.save_to(&mut bytes).expect("pdf saves");

        let error = parse_bytes(bytes).expect_err("an image-only page must not open");
        assert!(matches!(error, ApplicationError::OcrRequired(_)));
        assert!(
            error.to_string().contains("(1 page, 0 characters)"),
            "{error}"
        );
    }

    #[test]
    fn an_image_only_page_with_a_watermark_reports_ocr_required() {
        let mut document = LopdfDocument::with_version("1.5");
        let image = image_xobject(&mut document);
        let mut resources = text_resources(&mut document);
        resources.set("XObject", dictionary! { "Im1" => image });
        let mut operations = image_page_operations();
        operations.extend(byte_text_operations("F1", &[65_u8; 9]));
        let mut document = build_pdf(document, vec![(operations, resources)]);
        let mut bytes = Vec::new();
        document.save_to(&mut bytes).expect("pdf saves");

        let error = parse_bytes(bytes).expect_err("a watermarked scan must not open");
        assert!(matches!(error, ApplicationError::OcrRequired(_)));
        assert!(
            error.to_string().contains("(1 page, 9 characters)"),
            "{error}"
        );
    }

    #[test]
    fn a_text_pdf_with_one_image_page_stays_ok() {
        let mut document = LopdfDocument::with_version("1.5");
        let image = image_xobject(&mut document);
        let mut pages = Vec::new();
        for _ in 0..9 {
            let resources = text_resources(&mut document);
            pages.push((byte_text_operations("F1", &[65_u8; 60]), resources));
        }
        pages.push((
            image_page_operations(),
            dictionary! { "XObject" => dictionary! { "Im1" => image } },
        ));
        let mut document = build_pdf(document, pages);
        let mut bytes = Vec::new();
        document.save_to(&mut bytes).expect("pdf saves");

        let parsed = parse_bytes(bytes).expect("a text PDF with one image page stays ok");
        assert_eq!(
            parsed.metadata.get("pdf_page_count").map(String::as_str),
            Some("10")
        );
    }

    #[test]
    fn a_one_page_text_pdf_with_thirty_characters_stays_ok() {
        let mut document = LopdfDocument::with_version("1.5");
        let resources = text_resources(&mut document);
        let mut document = build_pdf(
            document,
            vec![(byte_text_operations("F1", &[65_u8; 30]), resources)],
        );
        let mut bytes = Vec::new();
        document.save_to(&mut bytes).expect("pdf saves");

        let parsed = parse_bytes(bytes).expect("thirty characters are still a text layer");
        assert_eq!(
            parsed.metadata.get("pdf_page_count").map(String::as_str),
            Some("1")
        );
    }

    /// PDF-2c: the raw bytes and the repair copy are released as soon as the pages are read, not
    /// at the end of the parse. A counter on the buffers the parse holds, not a timing test: the
    /// parse records what it still held at the point it builds the document.
    #[test]
    fn the_parse_releases_the_raw_bytes_and_the_repair_copy_before_building_the_document() {
        // A CJK form XObject needs the repair copy, so both buffers are real in this fixture.
        let mut document = LopdfDocument::with_version("1.5");
        let font_id = cjk_font(&mut document);
        let form_id = form(
            &mut document,
            dictionary! { "Font" => dictionary! { "FX" => font_id } },
            None,
            byte_text_operations("FX", &[65]),
        );
        add_page(
            &mut document,
            vec![Operation::new("Do", vec![Object::Name(b"Fm1".to_vec())])],
            dictionary! { "XObject" => dictionary! { "Fm1" => form_id } },
        );
        let mut bytes = Vec::new();
        document.save_to(&mut bytes).expect("pdf saves");

        held_buffers::reset();
        let parsed = parse_bytes(bytes).expect("the repaired form text parses");
        assert_eq!(parsed.section_count(), 1);

        assert!(
            held_buffers::raw_bytes_were_held(),
            "the parse must account for the raw bytes"
        );
        assert!(
            held_buffers::repair_copy_was_made(),
            "this fixture must need the repair copy, or the assertion below is vacuous"
        );
        assert_eq!(
            held_buffers::held_at_document_start(),
            Some((false, false)),
            "neither the raw bytes nor the repair copy may still be held when the document is built"
        );
    }

    /// PDF-2c: a multi-page PDF with text pages and an image page keeps exactly the sections,
    /// text, locators and bindings it published before the open's memory was cut. The golden is
    /// generated in CI from the tree *before* the change (`write_mixed_pages_golden`) and
    /// committed, so this compares against the old output, not against the new one.
    #[test]
    fn a_mixed_text_and_image_pdf_keeps_its_golden_sections_and_text() {
        let golden = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/pdf2c-mixed-pages.golden.txt");
        let Ok(expected) = std::fs::read_to_string(&golden) else {
            println!(
                "{} is absent: regenerate it with write_mixed_pages_golden",
                golden.display()
            );
            return;
        };
        let actual = mixed_pages_golden();
        assert_eq!(actual, expected, "the mixed PDF's sections or text changed");
    }

    /// Writes the golden the test above compares against. Ignored: run once, in CI, from the tree
    /// before the change, and commit the printed block as
    /// `tests/fixtures/pdf2c-mixed-pages.golden.txt`.
    #[test]
    #[ignore = "pdf2c golden: run once with --ignored --nocapture and commit the printed block"]
    fn write_mixed_pages_golden() {
        println!("---8<---\n{}---8<---", mixed_pages_golden());
    }

    /// PDF-2c: the `document_json` a save writes for a fixture PDF must stay byte-identical. The
    /// save binds the JSON with a transient copy SQLite makes itself; nothing in this work may
    /// change one stored byte. The golden is generated in CI from the tree before the change
    /// (`write_stored_document_json_golden`) and committed.
    #[tokio::test]
    async fn the_stored_document_json_of_a_fixture_pdf_is_byte_identical() {
        let golden = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/pdf2c-mixed-pages.document-json.golden.txt");
        let stored = stored_document_json().await;
        let Ok(expected) = std::fs::read_to_string(&golden) else {
            println!(
                "{} is absent: regenerate it with write_stored_document_json_golden",
                golden.display()
            );
            return;
        };
        assert_eq!(stored, expected, "the stored document_json changed");
    }

    /// Writes the golden the test above compares against. Ignored: run once, in CI, from the tree
    /// before the change, and commit the printed block as
    /// `tests/fixtures/pdf2c-mixed-pages.document-json.golden.txt`.
    #[tokio::test]
    #[ignore = "pdf2c golden: run once with --ignored --nocapture and commit the printed block"]
    async fn write_stored_document_json_golden() {
        println!("---8<---\n{}---8<---", stored_document_json().await);
    }

    /// The `documents.document_json` row of the mixed-pages fixture, saved through the repository
    /// exactly as an open saves it (without a text-unit index, so the section text stays inline).
    async fn stored_document_json() -> String {
        let directory = tempfile::tempdir().expect("temp directory");
        let path = directory.path().join("stored.sqlite");
        let repository =
            crate::infrastructure::SqliteDocumentRepository::open(&path).expect("repository");
        let document = parse_bytes(mixed_pages_pdf()).expect("the mixed PDF opens");
        let id = document.id.0.clone();
        crate::application::ports::DocumentRepository::save(&repository, document)
            .await
            .expect("save");
        let connection = rusqlite::Connection::open(&path).expect("connection");
        connection
            .query_row(
                "SELECT document_json FROM documents WHERE id = ?1",
                [&id],
                |row| row.get::<_, String>(0),
            )
            .expect("stored document_json")
    }

    /// Four pages: two plain text pages, a text page that also paints an image, and a page whose
    /// text is short enough that the image-only evidence runs on it.
    fn mixed_pages_golden() -> String {
        let document = parse_bytes(mixed_pages_pdf()).expect("the mixed PDF opens");
        let mut out = String::new();
        out.push_str(&format!("title={}\n", document.title));
        for key in ["pdf_version", "pdf_page_count", "pdf_structure_provenance"] {
            out.push_str(&format!(
                "{key}={}\n",
                document.metadata.get(key).map(String::as_str).unwrap_or("-")
            ));
        }
        for section in &document.root_sections {
            push_section_golden(&mut out, section);
        }
        let bindings = document
            .original_source_binding_map()
            .expect("bindings decode")
            .map(|map| map.bindings)
            .unwrap_or_default();
        for binding in bindings {
            out.push_str(&format!("binding={binding:?}\n"));
        }
        out
    }

    /// The bytes of the four-page fixture the two goldens are made from.
    fn mixed_pages_pdf() -> Vec<u8> {
        let mut document = LopdfDocument::with_version("1.5");
        let resources = text_resources(&mut document);
        let image = image_xobject(&mut document);
        let mut text_and_image = resources.clone();
        text_and_image.set("XObject", dictionary! { "Im1" => image });
        let mut short_page = image_page_operations();
        short_page.extend(byte_text_operations("F1", &[65_u8; 12]));
        let text_page = |text: &[u8], resources: Dictionary| {
            (byte_text_operations("F1", text), resources)
        };
        let pages = vec![
            text_page(b"First page of the mixed document.", resources.clone()),
            text_page(b"Second page carries a figure.", text_and_image),
            text_page(b"Third page closes the document.", resources.clone()),
            (short_page, resources),
        ];
        let mut document = build_pdf(document, pages);
        let mut bytes = Vec::new();
        document.save_to(&mut bytes).expect("pdf saves");
        bytes
    }

    fn push_section_golden(out: &mut String, section: &Section) {
        out.push_str(&format!(
            "section id={} parent={} level={} title={:?} page={:?} native={:?} path={:?}\n",
            section.id.0,
            section.parent_id.as_ref().map(|id| id.0.as_str()).unwrap_or("-"),
            section.level,
            section.title,
            section.location.page,
            section.location.native_location,
            section.location.section_path,
        ));
        out.push_str(&format!("content={:?}\n", section.content));
        for child in &section.children {
            push_section_golden(out, child);
        }
    }
}
