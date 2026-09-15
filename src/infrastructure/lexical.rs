use std::collections::HashSet;

use crate::application::ports::{ApplicationError, SearchHitKind};
use crate::domain::{Document, Location, Section, SentenceTextUnit, TextLocator, TextUnit};

pub(crate) const LEXICAL_SEARCH_INDEX_VERSION: &str = "lexical-search-index/v4";
pub(crate) const MAX_SNIPPET_CHARS: usize = 320;

#[derive(Clone, Debug)]
pub(crate) struct LexicalCandidate {
    pub(crate) candidate_kind: SearchHitKind,
    pub(crate) section_id: crate::domain::SectionId,
    pub(crate) title: String,
    pub(crate) source: crate::domain::DocumentSource,
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
    Some(join_encoded(&tokens, " AND "))
}

/// Relaxed OR query used only after the strict AND query returned no rows.
/// Callers must gate this on a multi-token query; single-token queries are
/// never relaxed.
pub(crate) fn encoded_relaxed_query(tokens: &[String]) -> String {
    join_encoded(tokens, " OR ")
}

fn join_encoded(tokens: &[String], operator: &str) -> String {
    tokens
        .iter()
        .map(|token| format!("\"{}\"", encode_token(token)))
        .collect::<Vec<_>>()
        .join(operator)
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
    Some(scored(candidate, query_tokens.len(), query))
}

/// Relaxed counterpart of [`score_candidate`]: ranks candidates matching any
/// query token by matched-token count. Used only when the strict all-token
/// query produced zero hits on a multi-token query.
pub(crate) fn relaxed_score_candidate(
    candidate: &LexicalCandidate,
    query_tokens: &[String],
    query: &str,
) -> Option<f32> {
    let candidate_tokens = candidate.tokens.iter().collect::<HashSet<_>>();
    let matched = query_tokens
        .iter()
        .filter(|token| candidate_tokens.contains(*token))
        .count();
    if matched == 0 {
        return None;
    }
    Some(scored(candidate, matched, query))
}

fn scored(candidate: &LexicalCandidate, matched: usize, query: &str) -> f32 {
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
    phrase_bonus + matched as f32 + kind_bonus
}

/// Bounded snippet centered on the earliest query-token match. Snippets are
/// generated at query time (not index time) so the window follows the match;
/// clipped sides get a single-char ellipsis and the result never exceeds
/// `max_chars` Unicode scalars.
pub(crate) fn match_snippet(text: &str, query: &str, max_chars: usize) -> String {
    let total = text.chars().count();
    if total <= max_chars {
        return text.into();
    }

    let lowered = text.to_lowercase();
    let anchor = tokenize(query)
        .iter()
        .filter_map(|token| lowered.find(token.as_str()))
        .min()
        .map(|byte| lowered[..byte].chars().count())
        .unwrap_or(0);

    // Center a full-size window on the anchor to learn which sides clip, then
    // re-window within the reduced budget so one scalar per clipped side is
    // reserved for the ellipsis marker.
    let probe_start = anchor.saturating_sub(max_chars / 2);
    let probe_end = (probe_start + max_chars).min(total);
    let probe_start = probe_end.saturating_sub(max_chars);
    let inner = max_chars - usize::from(probe_start > 0) - usize::from(probe_end < total);
    let start = anchor.saturating_sub(inner / 2);
    let end = (start + inner).min(total);
    let start = end.saturating_sub(inner);

    // Re-derive clipping from the final window: a side that clipped only after
    // re-windowing must still fit its marker inside the budget.
    let clipped_left = start > 0;
    let clipped_right = end < total;
    let take =
        (end - start).min(max_chars - usize::from(clipped_left) - usize::from(clipped_right));
    let body: String = text.chars().skip(start).take(take).collect();
    let prefix = if clipped_left { "…" } else { "" };
    let suffix = if clipped_right { "…" } else { "" };
    format!("{prefix}{body}{suffix}")
}

#[cfg(test)]
mod tests {
    use super::{MAX_SNIPPET_CHARS, encoded_query, match_snippet, tokenize};

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

    #[test]
    fn match_snippet_centers_window_on_late_match() {
        let text = format!("{}{}", "padding ".repeat(80), "needle term appears late");
        assert!(text.chars().count() > MAX_SNIPPET_CHARS);
        let snippet = match_snippet(&text, "needle", MAX_SNIPPET_CHARS);
        assert!(snippet.contains("needle"));
        assert!(snippet.starts_with('…'));
        assert!(!snippet.ends_with('…'));
        assert!(snippet.chars().count() <= MAX_SNIPPET_CHARS);
    }

    #[test]
    fn match_snippet_marks_both_sides_for_middle_match() {
        let text = format!("{} needle {}", "left ".repeat(60), "right ".repeat(60));
        assert!(text.chars().count() > MAX_SNIPPET_CHARS);
        let snippet = match_snippet(&text, "needle", MAX_SNIPPET_CHARS);
        assert!(snippet.contains("needle"));
        assert!(snippet.starts_with('…'));
        assert!(snippet.ends_with('…'));
        assert!(snippet.chars().count() <= MAX_SNIPPET_CHARS);
    }

    #[test]
    fn match_snippet_returns_short_text_unchanged() {
        let snippet = match_snippet("short needle text", "needle", MAX_SNIPPET_CHARS);
        assert_eq!(snippet, "short needle text");
    }

    #[test]
    fn match_snippet_respects_cjk_scalar_boundaries() {
        let text = format!("{}物理帧命中{}", "字".repeat(400), "尾".repeat(200));
        let snippet = match_snippet(&text, "物理帧", MAX_SNIPPET_CHARS);
        assert!(snippet.contains("物理帧"));
        assert!(snippet.starts_with('…'));
        assert!(snippet.ends_with('…'));
        assert!(snippet.chars().count() <= MAX_SNIPPET_CHARS);
    }
}
