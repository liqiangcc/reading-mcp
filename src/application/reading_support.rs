use crate::domain::Section;

const DEFAULT_CONTENT_RESPONSE_CHARS: usize = 32_000;
const MAX_CONTENT_RESPONSE_CHARS: usize = 64_000;

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

pub(crate) fn truncate_chars(
    content: String,
    requested_max_chars: Option<usize>,
) -> (String, bool) {
    let limit = requested_max_chars
        .unwrap_or(DEFAULT_CONTENT_RESPONSE_CHARS)
        .min(MAX_CONTENT_RESPONSE_CHARS);

    if content.chars().count() <= limit {
        return (content, false);
    }

    (content.chars().take(limit).collect(), true)
}

fn render_tree_into(section: &Section, output: &mut String) {
    output.push_str(&render_section_shallow(section));
    output.push('\n');

    for child in &section.children {
        output.push('\n');
        render_tree_into(child, output);
    }
}
