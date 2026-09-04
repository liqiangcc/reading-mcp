use async_trait::async_trait;
use lopdf::Document as LopdfDocument;

use crate::application::ports::{ApplicationError, Parser, RetrievedResource};
use crate::domain::{Document, OriginalSourceBindingMap, Section};

use super::pdf_front_matter::{
    PDF_FRONT_MATTER_ABSTRACT_COUNT_METADATA_KEY, PDF_FRONT_MATTER_INFERENCE_VERSION,
    PDF_FRONT_MATTER_INFERENCE_VERSION_METADATA_KEY, split_reliable_abstract_from_preamble,
};
use super::pdf_layout::{abstract_heading_inference_status, extract_text_fragment_evidence};

const MAX_PAGE_DECOMPRESSED_BYTES: usize = 16 * 1024 * 1024;
const PDF_LAYOUT_EXTRACTION_ERRORS_METADATA_KEY: &str = "pdf_layout_extraction_errors";
const PDF_FRONT_MATTER_INFERENCE_STATUS_METADATA_KEY: &str = "pdf_front_matter_inference_status";

#[derive(Default)]
pub struct PdfParser;

#[async_trait]
impl Parser for PdfParser {
    async fn parse(&self, resource: RetrievedResource) -> Result<Document, ApplicationError> {
        let layout_resource = resource.clone();
        let mut document = super::pdf::PdfParser.parse(resource).await?;

        if document
            .metadata
            .get("pdf_structure_provenance")
            .map(String::as_str)
            != Some("inferred_numbered_headings")
        {
            return Ok(document);
        }

        let Some((first_section_page, first_section_title)) =
            first_numbered_top_level_section(&document.root_sections)
        else {
            return Ok(document);
        };

        let pdf = LopdfDocument::load_mem(&layout_resource.bytes).map_err(|error| {
            ApplicationError::ParseFailed(format!(
                "cannot reopen PDF for layout evidence extraction: {error}"
            ))
        })?;
        let page_numbers = pdf.get_pages().keys().copied().collect::<Vec<_>>();
        let (evidence, layout_errors) =
            extract_text_fragment_evidence(&pdf, &page_numbers, MAX_PAGE_DECOMPRESSED_BYTES);
        if !layout_errors.is_empty() {
            document.metadata.insert(
                PDF_LAYOUT_EXTRACTION_ERRORS_METADATA_KEY.into(),
                layout_errors.len().to_string(),
            );
        }
        document.metadata.insert(
            PDF_FRONT_MATTER_INFERENCE_VERSION_METADATA_KEY.into(),
            PDF_FRONT_MATTER_INFERENCE_VERSION.into(),
        );

        // Proceedings PDFs may contain cover/front pages before the paper itself.  Front-matter
        // inference is intentionally scoped to the page that owns the first numbered section,
        // while the canonical Preamble keeps its original multi-page bindings.
        let front_matter_evidence = evidence
            .iter()
            .filter(|item| item.page == first_section_page)
            .cloned()
            .collect::<Vec<_>>();
        document.metadata.insert(
            PDF_FRONT_MATTER_INFERENCE_STATUS_METADATA_KEY.into(),
            abstract_heading_inference_status(
                &front_matter_evidence,
                first_section_page,
                &first_section_title,
            )
            .into(),
        );

        let mut bindings = document
            .original_source_binding_map()
            .map_err(|error| {
                ApplicationError::ParseFailed(format!(
                    "invalid original PDF source binding evidence before front-matter inference: {error}"
                ))
            })?
            .map(|map| map.bindings)
            .unwrap_or_default();

        let abstract_count = if split_reliable_abstract_from_preamble(
            &mut document.root_sections,
            &mut bindings,
            &front_matter_evidence,
            first_section_page,
            &first_section_title,
        ) {
            document
                .set_original_source_binding_map(OriginalSourceBindingMap::new(bindings))
                .map_err(|error| {
                    ApplicationError::ParseFailed(format!(
                        "invalid original PDF source binding evidence after front-matter inference: {error}"
                    ))
                })?;
            1
        } else {
            0
        };
        document.metadata.insert(
            PDF_FRONT_MATTER_ABSTRACT_COUNT_METADATA_KEY.into(),
            abstract_count.to_string(),
        );

        Ok(document)
    }
}

fn first_numbered_top_level_section(sections: &[Section]) -> Option<(u32, String)> {
    sections.iter().find_map(|section| {
        if top_level_number(&section.title) != Some(1) {
            return None;
        }
        Some((section.location.page?, section.title.clone()))
    })
}

fn top_level_number(title: &str) -> Option<u32> {
    let title = title.trim_start();
    let digit_count = title
        .bytes()
        .take_while(|byte| byte.is_ascii_digit())
        .count();
    if digit_count == 0 {
        return None;
    }
    let rest = &title[digit_count..];
    if !rest
        .chars()
        .next()
        .is_some_and(|character| character.is_whitespace() || matches!(character, '.' | ':' | '-'))
    {
        return None;
    }
    title[..digit_count].parse().ok()
}

#[cfg(test)]
mod tests {
    use super::top_level_number;

    #[test]
    fn top_level_number_requires_heading_separator() {
        assert_eq!(top_level_number("1 Introduction"), Some(1));
        assert_eq!(top_level_number("2. Replication"), Some(2));
        assert_eq!(top_level_number("12 monkeys"), Some(12));
        assert_eq!(top_level_number("1Introduction"), None);
        assert_eq!(top_level_number("Introduction"), None);
    }
}
