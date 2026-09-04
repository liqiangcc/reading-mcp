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
    let Some(candidate) = infer_abstract_heading(evidence, first_section_page, first_section_title)
    else {
        return false;
    };

    let Some(preamble_index) = sections
        .iter()
        .position(|section| section.id.0 == "section://preamble")
    else {
        return false;
    };
    let preamble_content = sections[preamble_index].content.clone();
    let Some(split) = split_abstract_line_ranges(&preamble_content) else {
        return false;
    };
    if split.after.is_empty() {
        return false;
    }

    let preamble_id = SectionId("section://preamble".into());
    let abstract_id = SectionId("section://abstract".into());
    let old_bindings = bindings
        .iter()
        .filter(|binding| binding.owner_section_id == preamble_id)
        .cloned()
        .collect::<Vec<_>>();
    if old_bindings.is_empty() {
        return false;
    }

    let before_bindings = project_bindings(
        &old_bindings,
        &preamble_id,
        split.before_range,
        split.before_range.start(),
    );
    let abstract_bindings = project_bindings(
        &old_bindings,
        &abstract_id,
        split.after_range,
        split.after_range.start(),
    );
    if abstract_bindings.is_empty()
        || !abstract_bindings.iter().all(|binding| {
            matches!(
                binding.target,
                OriginalSourceTarget::Page { page_number } if page_number == candidate.page
            )
        })
    {
        return false;
    }

    bindings.retain(|binding| binding.owner_section_id != preamble_id);
    bindings.extend(before_bindings);
    bindings.extend(abstract_bindings);

    if split.before.is_empty() {
        sections.remove(preamble_index);
    } else {
        sections[preamble_index].content = split.before.clone();
        let start_page = old_bindings
            .iter()
            .filter_map(binding_page)
            .next()
            .unwrap_or(candidate.page);
        let end_page = old_bindings
            .iter()
            .filter(|binding| binding.normalized_range.start() < split.before_range.end())
            .filter_map(binding_page)
            .last()
            .unwrap_or(start_page);
        sections[preamble_index].location.native_location = Some(if end_page > start_page {
            format!("pdf:pages:{start_page}-{end_page}")
        } else {
            format!("pdf:page:{start_page}")
        });
    }

    let insert_index = if split.before.is_empty() {
        preamble_index
    } else {
        preamble_index + 1
    };
    sections.insert(
        insert_index,
        Section {
            id: abstract_id,
            parent_id: None,
            title: "Abstract".into(),
            level: 1,
            content: split.after,
            location: Location {
                page: Some(candidate.page),
                section_path: vec!["Abstract".into()],
                native_location: Some(format!("pdf:page:{}", candidate.page)),
                ..Location::default()
            },
            children: Vec::new(),
        },
    );
    true
}

#[derive(Debug)]
struct AbstractSplit {
    before: String,
    after: String,
    before_range: NormalizedTextRange,
    after_range: NormalizedTextRange,
}

fn split_abstract_line_ranges(content: &str) -> Option<AbstractSplit> {
    let mut offset = 0_usize;
    let mut match_range = None;
    for segment in content.split_inclusive('\n') {
        let segment_len = segment.chars().count();
        if segment.trim().eq_ignore_ascii_case("Abstract") {
            if match_range.is_some() {
                return None;
            }
            match_range = Some((offset, offset + segment_len));
        }
        offset += segment_len;
    }

    let (heading_start, heading_end) = match_range?;
    let before_raw = NormalizedTextRange::new(0, heading_start).ok()?;
    let after_raw = NormalizedTextRange::new(heading_end, content.chars().count()).ok()?;
    let before_text = before_raw.slice(content).ok()?;
    let after_text = after_raw.slice(content).ok()?;

    let before_trailing = before_text
        .chars()
        .rev()
        .take_while(|character| character.is_whitespace())
        .count();
    let after_leading = after_text
        .chars()
        .take_while(|character| character.is_whitespace())
        .count();
    let before_end = heading_start.saturating_sub(before_trailing);
    let after_start = heading_end.saturating_add(after_leading);
    let before_range = NormalizedTextRange::new(0, before_end).ok()?;
    let after_range = NormalizedTextRange::new(after_start, content.chars().count()).ok()?;

    Some(AbstractSplit {
        before: before_range.slice(content).ok()?.to_string(),
        after: after_range.slice(content).ok()?.to_string(),
        before_range,
        after_range,
    })
}

fn project_bindings(
    source: &[OriginalSourceBinding],
    new_owner: &SectionId,
    selected: NormalizedTextRange,
    shift: usize,
) -> Vec<OriginalSourceBinding> {
    source
        .iter()
        .filter_map(|binding| {
            let start = binding.normalized_range.start().max(selected.start());
            let end = binding.normalized_range.end().min(selected.end());
            (start < end).then(|| OriginalSourceBinding {
                owner_section_id: new_owner.clone(),
                normalized_range: NormalizedTextRange::new(start - shift, end - shift)
                    .expect("projected binding must remain ordered"),
                target: binding.target.clone(),
            })
        })
        .collect()
}

fn binding_page(binding: &OriginalSourceBinding) -> Option<u32> {
    match binding.target {
        OriginalSourceTarget::Page { page_number } => Some(page_number),
    }
}

fn split_abstract_line(content: &str) -> Option<(String, String)> {
    let split = split_abstract_line_ranges(content)?;
    Some((split.before, split.after))
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
        assert!(split_abstract_line("Title\nThis abstract discusses replication.").is_none());
    }

    #[test]
    fn ambiguous_multiple_abstract_lines_fail_closed() {
        assert!(split_abstract_line("Abstract\nBody\nAbstract\nMore").is_none());
    }
}
