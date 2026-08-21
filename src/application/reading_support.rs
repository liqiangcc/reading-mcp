use crate::domain::Section;

const DEFAULT_CONTENT_RESPONSE_CHARS: usize = 32_000;
const MAX_CONTENT_RESPONSE_CHARS: usize = 64_000;
pub(crate) const SECTION_TREE_READ_MODE: &str = "section_tree";
pub(crate) const SECTION_TREE_RENDERING_VERSION: &str = "section-tree-markdown/v1";
pub(crate) const SECTION_TREE_STREAM_COORDINATE_SPACE: &str =
    "section-tree-rendered-unicode-scalar/v1";

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RenderedStreamSlice {
    pub content: String,
    pub start_char: usize,
    pub end_char: usize,
    pub total_chars: usize,
    pub complete: bool,
}

pub(crate) fn render_section_tree(section: &Section) -> String {
    let mut output = String::new();
    render_tree_into(section, &mut output);
    output.trim().to_string()
}

pub(crate) fn render_section_shallow(section: &Section) -> String {
    let mut output = String::new();
    let heading_level = usize::from(section.level.clamp(1, 6));
    output.push_str(&"#".repeat(heading_level));
    output.push(' ');
    output.push_str(&section.title);

    if !section.content.trim().is_empty() {
        output.push_str("\n\n");
        output.push_str(section.content.trim());
    }

    output
}

pub(crate) fn flatten_sections<'a>(sections: &'a [Section], output: &mut Vec<&'a Section>) {
    for section in sections {
        output.push(section);
        flatten_sections(&section.children, output);
    }
}

pub(crate) fn content_response_limit(requested_max_chars: Option<usize>) -> usize {
    requested_max_chars
        .unwrap_or(DEFAULT_CONTENT_RESPONSE_CHARS)
        .min(MAX_CONTENT_RESPONSE_CHARS)
}

pub(crate) fn slice_rendered_stream(
    content: &str,
    start_char: usize,
    limit: usize,
) -> RenderedStreamSlice {
    let total_chars = content.chars().count();
    let end_char = start_char.saturating_add(limit).min(total_chars);
    let returned = content
        .chars()
        .skip(start_char)
        .take(end_char.saturating_sub(start_char))
        .collect();

    RenderedStreamSlice {
        content: returned,
        start_char,
        end_char,
        total_chars,
        complete: end_char == total_chars,
    }
}

pub(crate) fn truncate_chars(
    content: String,
    requested_max_chars: Option<usize>,
) -> (String, bool) {
    let slice = slice_rendered_stream(&content, 0, content_response_limit(requested_max_chars));
    (slice.content, !slice.complete)
}

fn render_tree_into(section: &Section, output: &mut String) {
    output.push_str(&render_section_shallow(section));
    output.push('\n');

    for child in &section.children {
        output.push('\n');
        render_tree_into(child, output);
    }
}

#[cfg(test)]
mod tests {
    use super::slice_rendered_stream;

    #[test]
    fn rendered_stream_slice_uses_unicode_scalar_coordinates() {
        let slice = slice_rendered_stream("A中🙂Z", 1, 2);
        assert_eq!(slice.content, "中🙂");
        assert_eq!(slice.start_char, 1);
        assert_eq!(slice.end_char, 3);
        assert_eq!(slice.total_chars, 4);
        assert!(!slice.complete);
    }
}
