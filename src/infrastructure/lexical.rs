use std::collections::HashSet;

use crate::application::ports::{ApplicationError, SearchHitKind};
use crate::domain::{Document, Location, Section, SentenceTextUnit, TextLocator, TextUnit};

pub(crate) const LEXICAL_SEARCH_INDEX_VERSION: &str = "lexical-search-index/v3";
const MAX_SNIPPET_CHARS: usize = 320;

#[derive(Clone, Debug)]
pub(crate) struct LexicalCandidate {
    pub(crate) candidate_kind: SearchHitKind,
    pub(crate) section_id: crate::domain::SectionId,
    pub(crate) title: String,
    pub(crate) source: crate::domain::DocumentSource,
    pub(crate) snippet: String,
    pub(crate) location: Location,
    pub(crate) text_locator: TextLocator,
    pub(crate) searchable_text: String,
    pub(crate) tokens: Vec<String>,
    pub(crate) source_order: usize,
}

pub(crate) fn build_lexical_candidates(
    document: &Document,
) -> Result<Vec<LexicalCandidate>, ApplicationError> {
    let paragraphs = document.try_paragraph_text_units().map_err(|error| {
        ApplicationError::IndexFailed(format!(
            "cannot build lexical Paragraph candidates from persisted block evidence: {error}"
        ))
    })?;
    let sentences = document.try_sentence_text_units().map_err(|error| {
        ApplicationError::IndexFailed(format!(
            "cannot build lexical Sentence candidates from persisted block evidence: {error}"
        ))
    })?;
    let mut candidates = Vec::new();
    let mut source_order = 0usize;

    for section in &document.root_sections {
        collect_section_candidates(
            document,
            section,
            &paragraphs.units,
            &sentences.units,
            &mut candidates,
            &mut source_order,
        );
    }

    Ok(candidates)
}

fn collect_section_candidates(
    document: &Document,
    section: &Section,
    paragraphs: &[TextUnit],
    sentences: &[SentenceTextUnit],
    output: &mut Vec<LexicalCandidate>,
    source_order: &mut usize,
) {
    push_candidate(
        document,
        section,
        SearchHitKind::Section,
        section.title.clone(),
        section.title.clone(),
        section.location.clone(),
        TextLocator::for_section(document, section),
        output,
        source_order,
    );

    for paragraph in paragraphs
        .iter()
        .filter(|unit| unit.owner_section_id == section.id)
    {
        push_candidate(
            document,
            section,
            SearchHitKind::Paragraph,
            paragraph.text.clone(),
            paragraph.text.clone(),
            text_unit_location(section, paragraph.paragraph_index, None),
            TextLocator::for_paragraph(document, section, paragraph),
            output,
            source_order,
        );
    }

    for sentence in sentences
        .iter()
        .filter(|unit| unit.owner_section_id == section.id)
    {
        push_candidate(
            document,
            section,
            SearchHitKind::Sentence,
            sentence.text.clone(),
            sentence.text.clone(),
            text_unit_location(
                section,
                sentence.paragraph_index,
                Some(sentence.sentence_index),
            ),
            TextLocator::for_sentence(document, section, sentence),
            output,
            source_order,
        );
    }

    for child in &section.children {
        collect_section_candidates(document, child, paragraphs, sentences, output, source_order);
    }
}

fn text_unit_location(
    section: &Section,
    paragraph_index: usize,
    sentence_index: Option<usize>,
) -> Location {
    let mut location = section.location.clone();
    location.paragraph = u32::try_from(paragraph_index).ok();
    let suffix = match sentence_index {
        Some(sentence_index) => {
            format!("search-unit:{paragraph_index}#sentence:{sentence_index}")
        }
        None => format!("search-unit:{paragraph_index}"),
    };
    location.native_location = Some(match &section.location.native_location {
        Some(native) => format!("{native}#{suffix}"),
        None => suffix,
    });
    location
}

#[allow(clippy::too_many_arguments)]
fn push_candidate(
    document: &Document,
    section: &Section,
    candidate_kind: SearchHitKind,
    searchable_text: String,
    snippet_source: String,
    location: Location,
    text_locator: TextLocator,
    output: &mut Vec<LexicalCandidate>,
    source_order: &mut usize,
) {
    let tokens = tokenize(&searchable_text);
    if tokens.is_empty() {
        return;
    }
    output.push(LexicalCandidate {
        candidate_kind,
        section_id: section.id.clone(),
        title: section.title.clone(),
        source: document.source.clone(),
        snippet: truncate(&snippet_source, MAX_SNIPPET_CHARS),
        location,
        text_locator,
        searchable_text,
        tokens,
        source_order: *source_order,
    });
    *source_order += 1;
}

pub(crate) fn tokenize(value: &str) -> Vec<String> {
    let normalized = value.to_lowercase();
    let mut output = Vec::new();
    let mut seen = HashSet::new();

    for segment in normalized.split_whitespace() {
        let trimmed = segment.trim_matches(|ch: char| {
            !(ch.is_alphanumeric() || is_cjk(ch) || is_technical_punctuation(ch))
        });
        if !trimmed.is_empty()
            && !trimmed.chars().any(is_cjk)
            && trimmed.chars().any(char::is_alphanumeric)
        {
            push_unique(&mut output, &mut seen, trimmed.to_string());
        }
        tokenize_segment(trimmed, &mut output, &mut seen);
    }

    output
}

fn tokenize_segment(segment: &str, output: &mut Vec<String>, seen: &mut HashSet<String>) {
    let chars = segment.chars().collect::<Vec<_>>();
    let mut index = 0usize;
    while index < chars.len() {
        if is_cjk(chars[index]) {
            let start = index;
            while index < chars.len() && is_cjk(chars[index]) {
                index += 1;
            }
            add_cjk_run(&chars[start..index], output, seen);
            continue;
        }
        if chars[index].is_alphanumeric() || chars[index] == '_' {
            let start = index;
            while index < chars.len()
                && !is_cjk(chars[index])
                && (chars[index].is_alphanumeric() || chars[index] == '_')
            {
                index += 1;
            }
            push_unique(output, seen, chars[start..index].iter().collect::<String>());
            continue;
        }
        index += 1;
    }
}

fn add_cjk_run(chars: &[char], output: &mut Vec<String>, seen: &mut HashSet<String>) {
    for ch in chars {
        push_unique(output, seen, ch.to_string());
    }
    for pair in chars.windows(2) {
        push_unique(output, seen, pair.iter().collect::<String>());
    }
}

fn push_unique(output: &mut Vec<String>, seen: &mut HashSet<String>, token: String) {
    if !token.is_empty() && seen.insert(token.clone()) {
        output.push(token);
    }
}

fn is_technical_punctuation(ch: char) -> bool {
    matches!(ch, '-' | '.' | '/' | ':' | '+' | '#' | '$' | '@')
}

fn is_cjk(ch: char) -> bool {
    matches!(
        ch as u32,
        0x3400..=0x4DBF
            | 0x4E00..=0x9FFF
            | 0xF900..=0xFAFF
            | 0x3040..=0x309F
            | 0x30A0..=0x30FF
            | 0xAC00..=0xD7AF
    )
}

pub(crate) fn encoded_lexemes(tokens: &[String]) -> String {
    tokens
        .iter()
        .map(|token| encode_token(token))
        .collect::<Vec<_>>()
        .join(" ")
}

pub(crate) fn encoded_query(value: &str) -> Option<String> {
    let tokens = tokenize(value);
    if tokens.is_empty() {
        return None;
    }
    Some(
        tokens
            .iter()
            .map(|token| format!("\"{}\"", encode_token(token)))
            .collect::<Vec<_>>()
            .join(" AND "),
    )
}

fn encode_token(token: &str) -> String {
    let mut output = String::with_capacity(1 + token.len() * 2);
    output.push('x');
    for byte in token.as_bytes() {
        use std::fmt::Write as _;
        write!(&mut output, "{byte:02x}").expect("writing to String cannot fail");
    }
    output
}

pub(crate) fn score_candidate(candidate: &LexicalCandidate, query: &str) -> Option<f32> {
    let query_tokens = tokenize(query);
    if query_tokens.is_empty() {
        return None;
    }
    let candidate_tokens = candidate.tokens.iter().collect::<HashSet<_>>();
    if !query_tokens
        .iter()
        .all(|token| candidate_tokens.contains(token))
    {
        return None;
    }

    let normalized_query = query.to_lowercase();
    let normalized_text = candidate.searchable_text.to_lowercase();
    let phrase_bonus = if normalized_text.contains(&normalized_query) {
        4.0
    } else {
        0.0
    };
    let kind_bonus = match candidate.candidate_kind {
        SearchHitKind::Sentence => 1.0,
        SearchHitKind::Paragraph => 0.5,
        SearchHitKind::Section => 0.25,
    };
    Some(phrase_bonus + query_tokens.len() as f32 + kind_bonus)
}

fn truncate(value: &str, max_chars: usize) -> String {
    value.chars().take(max_chars).collect()
}

#[cfg(test)]
mod tests {
    use super::{encoded_query, tokenize};

    #[test]
    fn cjk_substrings_are_represented_by_unigrams_and_bigrams() {
        let tokens = tokenize("虚拟内存机制");
        for expected in ["虚", "拟", "内", "存", "虚拟", "拟内", "内存"] {
            assert!(tokens.iter().any(|token| token == expected), "{expected}");
        }
        let query = encoded_query("拟内存").expect("query");
        assert!(query.contains(" AND "));
    }

    #[test]
    fn technical_identifiers_keep_full_and_component_tokens() {
        let tokens = tokenize("read-cursor/v2 std::sync::Arc x86_64");
        for expected in [
            "read-cursor/v2",
            "read",
            "cursor",
            "v2",
            "std::sync::arc",
            "std",
            "sync",
            "arc",
            "x86_64",
        ] {
            assert!(tokens.iter().any(|token| token == expected), "{expected}");
        }
    }
}
