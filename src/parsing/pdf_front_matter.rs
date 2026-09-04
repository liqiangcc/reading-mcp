use crate::domain::{
    Location, NormalizedTextRange, OriginalSourceBinding, OriginalSourceTarget, Section, SectionId,
};

use super::pdf_layout::{PdfTextFragmentEvidence, infer_abstract_heading};

pub(super) const PDF_FRONT_MATTER_INFERENCE_VERSION: &str = "pdf-front-matter-inference/v1";
pub(super) const PDF_FRONT_MATTER_INFERENCE_VERSION_METADATA_KEY: &str =
    "pdf_front_matter_inference_version";
pub(super) const PDF_FRONT_MATTER_ABSTRACT_COUNT_METADATA_KEY: &str =
    "pdf_front_matter_abstract_count";

pub(super) fn split_reliable_abstract_from_preamble(
    sections: &mut Vec<Section>,
    bindings: &mut Vec<OriginalSourceBinding>,
    evidence: &[PdfTextFragmentEvidence],
    first_section_page: u32,
    first_section_title: &str,
) -> bool {
    let Some(candidate) = infer_abstract_heading(
        evidence,
        first_section_page,
        first_section_title,
    ) else {
        return false;
    };

    let Some(preamble_index) = sections
        .iter()
        .position(|section| section.id.0 == "section://preamble")
    else {
        return false;
    };
    let preamble_content = sections[preamble_index].content.clone();
    let Some((before, after)) = split_abstract_line(&preamble_content) else {
        return false;
    };
    if after.is_empty() {
        return false;
    }

    let preamble_id = SectionId("section://preamble".into());
    let abstract_id = SectionId("section://abstract".into());
    let preamble_page = sections[preamble_index]
        .location
        .page
        .unwrap_or(candidate.page);

    bindings.retain(|binding| binding.owner_section_id != preamble_id);
    if before.is_empty() {
        sections.remove(preamble_index);
    } else {
        sections[preamble_index].content = before.clone();
        sections[preamble_index].location.native_location =
            Some(format!("pdf:page:{preamble_page}"));
        bindings.push(page_binding(&preamble_id, &before, preamble_page));
    }

    let insert_index = if before.is_empty() {
        preamble_index
    } else {
        preamble_index + 1
    };
    sections.insert(
        insert_index,
        Section {
            id: abstract_id.clone(),
            parent_id: None,
            title: "Abstract".into(),
            level: 1,
            content: after.clone(),
            location: Location {
                page: Some(candidate.page),
                section_path: vec!["Abstract".into()],
                native_location: Some(format!("pdf:page:{}", candidate.page)),
                ..Location::default()
            },
            children: Vec::new(),
        },
    );
    bindings.push(page_binding(&abstract_id, &after, candidate.page));
    true
}

fn split_abstract_line(content: &str) -> Option<(String, String)> {
    let mut offset = 0_usize;
    let mut match_range = None;
    for segment in content.split_inclusive('\n') {
        if segment.trim().eq_ignore_ascii_case("Abstract") {
            if match_range.is_some() {
                return None;
            }
            match_range = Some((offset, offset + segment.len()));
        }
        offset += segment.len();
    }
    if offset < content.len() {
        let segment = &content[offset..];
        if segment.trim().eq_ignore_ascii_case("Abstract") {
            if match_range.is_some() {
                return None;
            }
            match_range = Some((offset, content.len()));
        }
    }

    let (start, end) = match_range?;
    let before = content[..start].trim().to_string();
    let after = content[end..].trim().to_string();
    Some((before, after))
}

fn page_binding(
    owner_section_id: &SectionId,
    content: &str,
    page_number: u32,
) -> OriginalSourceBinding {
    OriginalSourceBinding {
        owner_section_id: owner_section_id.clone(),
        normalized_range: NormalizedTextRange::new(0, content.chars().count())
            .expect("front-matter split range must be ordered"),
        target: OriginalSourceTarget::Page { page_number },
    }
}

#[cfg(test)]
mod tests {
    use super::split_abstract_line;

    #[test]
    fn exact_standalone_abstract_line_splits_preamble() {
        let (before, after) = split_abstract_line(
            "Conference\nPaper title\nAuthors\nAbstract\nAbstract body sentence.",
        )
        .expect("standalone Abstract should split");
        assert_eq!(before, "Conference\nPaper title\nAuthors");
        assert_eq!(after, "Abstract body sentence.");
    }

    #[test]
    fn body_occurrence_does_not_split() {
        assert!(
            split_abstract_line("Title\nThis abstract discusses replication.").is_none()
        );
    }

    #[test]
    fn ambiguous_multiple_abstract_lines_fail_closed() {
        assert!(split_abstract_line("Abstract\nBody\nAbstract\nMore").is_none());
    }
}
