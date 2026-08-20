use crate::application::ports::ApplicationError;
use crate::domain::Section;

pub(crate) const DEFAULT_READ_MAX_CHARS: usize = 40_000;
pub(crate) const MAX_READ_MAX_CHARS: usize = 80_000;
pub(crate) const DEFAULT_CONTEXT_MAX_CHARS: usize = 24_000;
pub(crate) const MAX_CONTEXT_MAX_CHARS: usize = 48_000;

pub(crate) fn resolve_response_limit(
    requested: Option<usize>,
    default_limit: usize,
    hard_limit: usize,
) -> Result<usize, ApplicationError> {
    let requested = requested.unwrap_or(default_limit);
    if requested == 0 {
        return Err(ApplicationError::InvalidRequest(
            "max_chars must be greater than zero".into(),
        ));
    }
    Ok(requested.min(hard_limit))
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

pub(crate) fn truncate_chars(content: String, limit: usize) -> (String, bool) {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn response_limit_defaults_and_clamps_to_server_hard_limit() {
        assert_eq!(
            resolve_response_limit(None, DEFAULT_READ_MAX_CHARS, MAX_READ_MAX_CHARS).unwrap(),
            DEFAULT_READ_MAX_CHARS
        );
        assert_eq!(
            resolve_response_limit(
                Some(MAX_READ_MAX_CHARS * 2),
                DEFAULT_READ_MAX_CHARS,
                MAX_READ_MAX_CHARS,
            )
            .unwrap(),
            MAX_READ_MAX_CHARS
        );
        assert!(resolve_response_limit(Some(0), 100, 200).is_err());
    }
}
