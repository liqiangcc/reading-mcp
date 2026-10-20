use std::borrow::Cow;
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};

use crate::application::ports::{ApplicationError, LexicalSearchHit, SearchHitKind};
use crate::domain::{
    Document, Location, NormalizedDocumentHash, Section, SectionId, SentenceTextUnit, TextLocator,
    TextUnit,
};

pub(crate) const LEXICAL_SEARCH_INDEX_VERSION: &str = "lexical-scan/v1";
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
    Ok(lexical_candidates_from_units(
        document,
        &paragraphs.units,
        &sentences.units,
    ))
}

/// Calls `sink` with each lexical candidate of `document`, in index order, built from units the caller already has.
pub(crate) fn for_each_lexical_candidate(
    document: &Document,
    paragraphs: &[TextUnit],
    sentences: &[SentenceTextUnit],
    sink: &mut dyn FnMut(LexicalCandidate) -> Result<(), ApplicationError>,
) -> Result<(), ApplicationError> {
    let mut paragraphs_by_section: HashMap<&SectionId, Vec<&TextUnit>> = HashMap::new();
    for paragraph in paragraphs {
        paragraphs_by_section
            .entry(&paragraph.owner_section_id)
            .or_default()
            .push(paragraph);
    }
    let mut sentences_by_section: HashMap<&SectionId, Vec<&SentenceTextUnit>> = HashMap::new();
    for sentence in sentences {
        sentences_by_section
            .entry(&sentence.owner_section_id)
            .or_default()
            .push(sentence);
    }

    let normalized_document_hash = document.normalized_document_hash();

    for section in &document.root_sections {
        collect_section_candidates(
            document,
            section,
            &normalized_document_hash,
            &paragraphs_by_section,
            &sentences_by_section,
            sink,
        )?;
    }

    Ok(())
}

pub(crate) fn lexical_candidates_from_units(
    document: &Document,
    paragraphs: &[TextUnit],
    sentences: &[SentenceTextUnit],
) -> Vec<LexicalCandidate> {
    let mut candidates =
        Vec::with_capacity(document.section_count() + paragraphs.len() + sentences.len());
    for_each_lexical_candidate(document, paragraphs, sentences, &mut |candidate| {
        candidates.push(candidate);
        Ok(())
    })
    .expect("pushing into a Vec cannot fail");
    candidates
}

fn collect_section_candidates(
    document: &Document,
    section: &Section,
    normalized_document_hash: &NormalizedDocumentHash,
    paragraphs: &HashMap<&SectionId, Vec<&TextUnit>>,
    sentences: &HashMap<&SectionId, Vec<&SentenceTextUnit>>,
    sink: &mut dyn FnMut(LexicalCandidate) -> Result<(), ApplicationError>,
) -> Result<(), ApplicationError> {
    push_candidate(
        document,
        section,
        SearchHitKind::Section,
        section.title.clone(),
        section.location.clone(),
        TextLocator::for_section_with_hash(document, section, normalized_document_hash),
        sink,
    )?;

    for paragraph in paragraphs.get(&section.id).into_iter().flatten().copied() {
        push_candidate(
            document,
            section,
            SearchHitKind::Paragraph,
            paragraph.text.clone(),
            text_unit_location(section, paragraph.paragraph_index, None),
            TextLocator::for_paragraph(document, section, paragraph),
            sink,
        )?;
    }

    for sentence in sentences.get(&section.id).into_iter().flatten().copied() {
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
            sink,
        )?;
    }

    for child in &section.children {
        collect_section_candidates(
            document,
            child,
            normalized_document_hash,
            paragraphs,
            sentences,
            sink,
        )?;
    }

    Ok(())
}

pub(crate) fn text_unit_location(
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
    sink: &mut dyn FnMut(LexicalCandidate) -> Result<(), ApplicationError>,
) -> Result<(), ApplicationError> {
    let tokens = tokenize(&searchable_text);
    if tokens.is_empty() {
        return Ok(());
    }
    sink(LexicalCandidate {
        candidate_kind,
        section_id: section.id.clone(),
        title: section.title.clone(),
        source: document.source.clone(),
        location,
        text_locator,
        searchable_text,
        tokens,
    })?;
    Ok(())
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

/// `text.to_lowercase()` without allocating when the text cannot change under lowercasing.
/// Every branch returns exactly `text.to_lowercase()`.
pub(crate) fn lowercase_fast(text: &str) -> Cow<'_, str> {
    if text
        .chars()
        .all(|ch| (ch.is_ascii() && !ch.is_ascii_uppercase()) || is_caseless(ch))
    {
        return Cow::Borrowed(text);
    }
    if text.chars().all(|ch| ch.is_ascii() || is_caseless(ch)) {
        let mut lowered = String::with_capacity(text.len());
        for ch in text.chars() {
            if ch.is_ascii() {
                lowered.push(ch.to_ascii_lowercase());
            } else {
                lowered.push(ch);
            }
        }
        return Cow::Owned(lowered);
    }
    Cow::Owned(text.to_lowercase())
}

/// Characters that `to_lowercase` always returns unchanged: CJK scripts and the punctuation ranges
/// around them. Fullwidth Latin letters are deliberately absent, since they lowercase to ASCII.
fn is_caseless(ch: char) -> bool {
    is_cjk(ch)
        || matches!(
            ch as u32,
            0x2000..=0x206F | 0x3000..=0x303F | 0xFF00..=0xFF20 | 0xFF3B..=0xFF40 | 0xFF5B..=0xFF65
        )
}

/// One parsed search query: the tokens that must be found in a candidate's own `tokenize` output,
/// plus the lowercase forms the prefilter and the phrase bonus compare against.
pub(crate) struct LexicalQuery {
    /// `query.to_lowercase()`; the phrase bonus compares candidates against it.
    pub(crate) raw_lowered: String,
    /// `tokenize(query)`, in its order.
    pub(crate) tokens: Vec<String>,
    /// Per token: a CJK token (one or two CJK chars) is a `tokenize` member exactly when the lowered
    /// text contains it, because `tokenize` emits every CJK unigram and every adjacent CJK pair.
    cjk: Vec<bool>,
    /// Set when a token holds a sigma: `to_lowercase` maps `Σ` by position, so a slice can lowercase
    /// differently at its edge than the text it was cut from.
    fold: bool,
    /// The tokens in prefilter order: longer first, sigma-folded when `fold` is set.
    prefilter: Vec<String>,
    /// The index of the token that equals `raw_lowered`, when one does. A `strict_match` then already
    /// answered the phrase check `score_lowered` would repeat, because it contains-tests every token.
    phrase_is_token: Option<usize>,
}

impl LexicalQuery {
    /// `None` when the query has no searchable token.
    pub(crate) fn new(query: &str) -> Option<Self> {
        let tokens = tokenize(query);
        if tokens.is_empty() {
            return None;
        }
        let raw_lowered = query.to_lowercase();
        let phrase_is_token = tokens.iter().position(|token| *token == raw_lowered);
        let fold = tokens
            .iter()
            .any(|token| token.contains('σ') || token.contains('ς'));
        let cjk = tokens
            .iter()
            .map(|token| {
                let count = token.chars().count();
                (count == 1 || count == 2) && token.chars().all(is_cjk)
            })
            .collect();
        let mut prefilter = tokens
            .iter()
            .map(|token| {
                if fold {
                    token.replace('ς', "σ")
                } else {
                    token.clone()
                }
            })
            .collect::<Vec<_>>();
        // Longer tokens are rarer, so a failing prefilter is found sooner.
        prefilter.sort_by_key(|token| std::cmp::Reverse(token.chars().count()));
        Some(Self {
            raw_lowered,
            tokens,
            cjk,
            fold,
            prefilter,
            phrase_is_token,
        })
    }

    pub(crate) fn token_count(&self) -> usize {
        self.tokens.len()
    }

    /// Every token is a CJK unigram or bigram, so `may_contain_all` on a lowered text is exactly
    /// `strict_match` on it: `tokenize` emits every CJK char and every adjacent CJK pair of a run.
    pub(crate) fn all_cjk(&self) -> bool {
        self.cjk.iter().all(|&cjk| cjk)
    }

    /// The index of the token equal to `raw_lowered`, if any: `strict_match` contains-tests that very
    /// token, so the phrase bonus of a strict match is known without searching again.
    pub(crate) fn phrase_is_token(&self) -> Option<usize> {
        self.phrase_is_token
    }

    /// Every query token is in `tokenize(text)`. `lowered` must be `lowercase_fast(text)`.
    pub(crate) fn strict_match(&self, text: &str, lowered: &str) -> bool {
        if !self
            .tokens
            .iter()
            .all(|token| lowered.contains(token.as_str()))
        {
            return false;
        }
        if self.cjk.iter().all(|&cjk| cjk) {
            return true;
        }
        let tokens = tokenize(text).into_iter().collect::<HashSet<_>>();
        self.tokens
            .iter()
            .zip(&self.cjk)
            .all(|(token, &cjk)| cjk || tokens.contains(token))
    }

    /// The number of query tokens in `tokenize(text)`. `lowered` must be `lowercase_fast(text)`.
    pub(crate) fn relaxed_count(&self, text: &str, lowered: &str) -> usize {
        let mut matched = 0usize;
        let mut tokens: Option<HashSet<String>> = None;
        for (token, &cjk) in self.tokens.iter().zip(&self.cjk) {
            if !lowered.contains(token.as_str()) {
                continue;
            }
            let member = if cjk {
                true
            } else {
                tokens
                    .get_or_insert_with(|| tokenize(text).into_iter().collect())
                    .contains(token)
            };
            if member {
                matched += 1;
            }
        }
        matched
    }

    /// Prefilter on a larger lowered text that contains the candidates: could every token be in it?
    pub(crate) fn may_contain_all(&self, lowered_container: &str) -> bool {
        let folded;
        let container = if self.fold {
            folded = lowered_container.replace('ς', "σ");
            folded.as_str()
        } else {
            lowered_container
        };
        self.prefilter
            .iter()
            .all(|token| container.contains(token.as_str()))
    }

    /// Prefilter on a larger lowered text that contains the candidates: could any token be in it?
    pub(crate) fn may_contain_any(&self, lowered_container: &str) -> bool {
        let folded;
        let container = if self.fold {
            folded = lowered_container.replace('ς', "σ");
            folded.as_str()
        } else {
            lowered_container
        };
        self.prefilter
            .iter()
            .any(|token| container.contains(token.as_str()))
    }
}

/// The one lexical score formula: an exact phrase match, the matched token count and the kind bonus.
/// `lowered_text` must be `lowercase_fast(text)` and `raw_lowered_query` `query.to_lowercase()`.
pub(crate) fn score_lowered(
    lowered_text: &str,
    raw_lowered_query: &str,
    kind: SearchHitKind,
    matched: usize,
) -> f32 {
    score_lowered_with_phrase(lowered_text, raw_lowered_query, kind, matched, None)
}

/// [`score_lowered`] with the phrase check possibly already answered by the match that produced the
/// hit: `Some(true)` when a `strict_match` contains-tested `raw_lowered_query` itself.
pub(crate) fn score_lowered_with_phrase(
    lowered_text: &str,
    raw_lowered_query: &str,
    kind: SearchHitKind,
    matched: usize,
    phrase_known: Option<bool>,
) -> f32 {
    let phrase_matched = match phrase_known {
        Some(known) => known,
        None => lowered_text.contains(raw_lowered_query),
    };
    let phrase_bonus = if phrase_matched { 4.0 } else { 0.0 };
    let kind_bonus = match kind {
        SearchHitKind::Sentence => 1.0,
        SearchHitKind::Paragraph => 0.5,
        SearchHitKind::Section => 0.25,
    };
    phrase_bonus + matched as f32 + kind_bonus
}

/// The paragraph prefilters of one section, keyed by the paragraph range a sentence belongs to.
///
/// Sentences arrive grouped by their parent paragraph, so a one-entry memo answers nearly every
/// lookup. The rest are answered by a binary search over the paragraph results in paragraph order,
/// which is only valid when those ranges strictly increase; a section that does not keep a
/// `HashMap` instead. A parent that is no paragraph at all is computed from its slice by the caller
/// and remembered, as it was before.
#[derive(Debug, Default)]
pub(crate) struct ParagraphPrefilters {
    /// Whether `ordered` is keyed in increasing `(start, end)` order.
    increasing: bool,
    memo: Option<((usize, usize), bool)>,
    ordered: Vec<(usize, usize, bool)>,
    by_range: HashMap<(usize, usize), bool>,
}

impl ParagraphPrefilters {
    /// `increasing` must be true only when the paragraph ranges of the section strictly increase.
    pub(crate) fn new(increasing: bool) -> Self {
        Self {
            increasing,
            ..Self::default()
        }
    }

    /// Whether the paragraph ranges of one section are strictly increasing, in layout order.
    pub(crate) fn ranges_increase(ranges: impl IntoIterator<Item = (usize, usize)>) -> bool {
        let mut previous: Option<(usize, usize)> = None;
        for range in ranges {
            if let Some(previous) = previous
                && previous >= range
            {
                return false;
            }
            previous = Some(range);
        }
        true
    }

    /// Records the prefilter of the paragraph `start..end`.
    pub(crate) fn insert(&mut self, start: usize, end: usize, prefilter: bool) {
        if self.increasing {
            self.ordered.push((start, end, prefilter));
        } else {
            self.by_range.insert((start, end), prefilter);
        }
    }

    /// The prefilter of the paragraph `start..end`, when this section has a paragraph with that range.
    pub(crate) fn get(&mut self, start: usize, end: usize) -> Option<bool> {
        if let Some(((memo_start, memo_end), prefilter)) = self.memo
            && (memo_start, memo_end) == (start, end)
        {
            return Some(prefilter);
        }
        if self.increasing {
            let index = self
                .ordered
                .binary_search_by_key(&(start, end), |&(start, end, _)| (start, end))
                .ok()?;
            let (_, _, prefilter) = self.ordered[index];
            self.memo = Some(((start, end), prefilter));
            return Some(prefilter);
        }
        let prefilter = *self.by_range.get(&(start, end))?;
        self.memo = Some(((start, end), prefilter));
        Some(prefilter)
    }

    /// Records the prefilter of a parent range this section has no paragraph for.
    pub(crate) fn remember(&mut self, start: usize, end: usize, prefilter: bool) {
        self.memo = Some(((start, end), prefilter));
    }
}

/// The total order of lexical hits: score, then kind, then section, then the start of the unit.
pub(crate) fn compare_hits(left: &LexicalSearchHit, right: &LexicalSearchHit) -> Ordering {
    right
        .score
        .total_cmp(&left.score)
        .then_with(|| {
            candidate_kind_order(left.candidate_kind)
                .cmp(&candidate_kind_order(right.candidate_kind))
        })
        .then_with(|| left.section_id.0.cmp(&right.section_id.0))
        .then_with(|| {
            left.text_locator
                .normalized_range
                .map(|range| range.start())
                .unwrap_or_default()
                .cmp(
                    &right
                        .text_locator
                        .normalized_range
                        .map(|range| range.start())
                        .unwrap_or_default(),
                )
        })
}

pub(crate) const fn candidate_kind_order(kind: SearchHitKind) -> u8 {
    match kind {
        SearchHitKind::Sentence => 0,
        SearchHitKind::Paragraph => 1,
        SearchHitKind::Section => 2,
    }
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
    score_lowered(
        &lowercase_fast(&candidate.searchable_text),
        &query.to_lowercase(),
        candidate.candidate_kind,
        matched,
    )
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
    use std::collections::HashSet;

    use super::{
        LexicalQuery, MAX_SNIPPET_CHARS, ParagraphPrefilters, is_caseless, lowercase_fast,
        match_snippet, score_lowered, score_lowered_with_phrase, tokenize,
    };
    use crate::application::ports::SearchHitKind;
    use crate::domain::{NormalizedTextIndex, NormalizedTextRange};

    #[test]
    fn cjk_substrings_are_represented_by_unigrams_and_bigrams() {
        let tokens = tokenize("虚拟内存机制");
        for expected in ["虚", "拟", "内", "存", "虚拟", "拟内", "内存"] {
            assert!(tokens.iter().any(|token| token == expected), "{expected}");
        }
    }

    #[test]
    fn caseless_ranges_lowercase_to_themselves() {
        let ranges: [(u32, u32); 11] = [
            (0x2000, 0x206F),
            (0x3000, 0x303F),
            (0xFF00, 0xFF20),
            (0xFF3B, 0xFF40),
            (0xFF5B, 0xFF65),
            (0x3400, 0x4DBF),
            (0x4E00, 0x9FFF),
            (0xF900, 0xFAFF),
            (0x3040, 0x309F),
            (0x30A0, 0x30FF),
            (0xAC00, 0xD7AF),
        ];
        let mut checked = 0usize;
        for (first, last) in ranges {
            for code in first..=last {
                let ch = char::from_u32(code).expect("range holds only scalar values");
                assert!(is_caseless(ch), "U+{code:04X} must be caseless");
                assert_eq!(
                    ch.to_lowercase().collect::<String>(),
                    ch.to_string(),
                    "U+{code:04X} is not caseless"
                );
                checked += 1;
            }
        }
        assert!(checked > 39_000, "only {checked} chars checked");
    }

    #[test]
    fn lowercase_fast_matches_to_lowercase() {
        for sample in [
            "ASCII Mixed Case",
            "虚拟内存机制，页表。「引用」",
            "ＡＢＣ",
            "İstanbul",
            "ΟΔΟΣ ΟΔΟΣ.",
            "x86_64 Read-Cursor/V2",
            "",
            "已经小写的文本 123",
        ] {
            let expected = sample.to_lowercase();
            assert_eq!(
                lowercase_fast(sample).as_ref(),
                expected.as_str(),
                "sample {sample:?}"
            );
        }
    }

    #[test]
    fn strict_and_relaxed_matching_follow_the_tokenize_definition() {
        let samples = [
            ("虚拟内存机制，页表。「引用」", "虚拟内存"),
            ("虚拟内存机制，页表。「引用」", "虚拟 页表"),
            ("x86_64 Read-Cursor/V2 std::sync::Arc", "read-cursor/v2"),
            ("x86_64 Read-Cursor/V2 std::sync::Arc", "x86_64 arc"),
            ("x86_64 Read-Cursor/V2 std::sync::Arc", "x86 arc missing"),
            ("İstanbul is a city", "istanbul"),
            ("ＡＢＣ fullwidth", "abc"),
            ("ΟΔΟΣ.ΟΔΟΣ", "οδος"),
            ("plain english sentence", "sentence"),
        ];
        for (text, query) in samples {
            let query = LexicalQuery::new(query).expect("query tokens");
            let lowered = lowercase_fast(text);
            let tokens = tokenize(text).into_iter().collect::<HashSet<_>>();
            let expected_strict = query.tokens.iter().all(|token| tokens.contains(token));
            let expected_matched = query
                .tokens
                .iter()
                .filter(|token| tokens.contains(*token))
                .count();
            assert_eq!(
                query.strict_match(text, &lowered),
                expected_strict,
                "strict {text:?}"
            );
            assert_eq!(
                query.relaxed_count(text, &lowered),
                expected_matched,
                "relaxed {text:?}"
            );
        }
    }

    #[test]
    fn prefilters_never_reject_a_containing_slice() {
        let container = "ΟΔΟΣ.ΟΔΟΣ rest of the paragraph";
        let lowered = lowercase_fast(container);
        let query = LexicalQuery::new("οδος").expect("query");
        assert!(query.may_contain_all(&lowered));
        assert!(query.may_contain_any(&lowered));
        let missing = LexicalQuery::new("οδος quasar").expect("query");
        assert!(!missing.may_contain_all(&lowered));
        assert!(missing.may_contain_any(&lowered));
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

    /// Ten samples of every query shape: CJK chars, CJK pairs, CJK phrases, Latin words and a sigma.
    const SHORTCUT_SAMPLES: [(&str, &str); 10] = [
        ("皇帝在朝堂上讨论虚拟内存机制与页表映射。", "皇帝"),
        ("皇帝 内存 与页表", "皇帝 内存"),
        ("節度使與太祖論兵於朝。", "節度使"),
        ("虛擬內存機制，頁表。「引用」", "内存"),
        ("宋史卷一百 皇帝", "宋史"),
        ("皇帝", "皇"),
        ("没有匹配的一行", "皇帝"),
        ("Pierre Bezukhov met Andrew.", "pierre"),
        ("ΟΔΟΣ.ΟΔΟΣ", "οδος"),
        ("Memory Layout of the tables", "memory"),
    ];

    #[test]
    fn all_cjk_prefilter_is_strict_match() {
        let mut cjk_samples = 0usize;
        for (text, query) in SHORTCUT_SAMPLES {
            let query = LexicalQuery::new(query).expect("query tokens");
            if !query.all_cjk() {
                continue;
            }
            cjk_samples += 1;
            let lowered = lowercase_fast(text);
            let prefilter = query.may_contain_all(&lowered);
            let general = if query.strict_match(text, &lowered) {
                Some(score_lowered(
                    &lowered,
                    &query.raw_lowered,
                    SearchHitKind::Paragraph,
                    query.token_count(),
                ))
            } else {
                None
            };
            let shortcut = if prefilter {
                Some(score_lowered_with_phrase(
                    &lowered,
                    &query.raw_lowered,
                    SearchHitKind::Paragraph,
                    query.token_count(),
                    query.phrase_is_token().map(|_| true),
                ))
            } else {
                None
            };
            assert_eq!(
                general,
                shortcut,
                "text {text:?} query {raw:?}",
                raw = query.raw_lowered
            );
        }
        assert!(cjk_samples >= 5, "only {cjk_samples} all-CJK samples");
    }

    #[test]
    fn phrase_is_token_matches_the_phrase_check() {
        let mut checked = 0usize;
        for (text, raw_query) in SHORTCUT_SAMPLES {
            let query = LexicalQuery::new(raw_query).expect("query tokens");
            if query.phrase_is_token().is_none() {
                continue;
            }
            let lowered = lowercase_fast(text);
            if !query.strict_match(text, &lowered) {
                continue;
            }
            checked += 1;
            // A strict match contains-tests every token, so a token equal to the whole query proves
            // the phrase: the known answer and the general formula must agree.
            assert!(
                lowered.contains(query.raw_lowered.as_str()),
                "phrase of {raw_query:?} must match in {text:?}"
            );
            assert_eq!(
                score_lowered(
                    &lowered,
                    &query.raw_lowered,
                    SearchHitKind::Sentence,
                    query.token_count()
                ),
                score_lowered_with_phrase(
                    &lowered,
                    &query.raw_lowered,
                    SearchHitKind::Sentence,
                    query.token_count(),
                    query.phrase_is_token().map(|_| true),
                ),
                "score of {raw_query:?} in {text:?}"
            );
        }
        assert!(checked >= 3, "only {checked} phrase-equals-token samples");
    }

    fn slice_lowered_of<'a>(
        index: &NormalizedTextIndex<'a>,
        start: usize,
        end: usize,
    ) -> std::borrow::Cow<'a, str> {
        let range = NormalizedTextRange::new(start, end).expect("range");
        lowercase_fast(index.slice(range).expect("slice"))
    }

    #[test]
    fn paragraph_prefilters_answer_like_the_slice() {
        let text = "皇帝在朝堂。群臣议事。節度使入朝。太祖問兵。";
        let index = NormalizedTextIndex::new(text);
        let ranges = [(0usize, 6usize), (6, 11), (11, 17), (17, 22)];
        let query = LexicalQuery::new("朝").expect("query tokens");
        let expected =
            |start: usize, end: usize| query.may_contain_all(&slice_lowered_of(&index, start, end));

        let mut increasing = ParagraphPrefilters::new(ParagraphPrefilters::ranges_increase(ranges));
        for (start, end) in ranges {
            increasing.insert(start, end, expected(start, end));
        }
        for (start, end) in ranges {
            assert_eq!(
                increasing.get(start, end),
                Some(expected(start, end)),
                "paragraph {start}..{end}"
            );
        }
        // The memo answers the repeated parent without another lookup.
        let repeated = ranges[1];
        assert_eq!(
            increasing.get(repeated.0, repeated.1),
            Some(expected(repeated.0, repeated.1))
        );
        // A parent that is no paragraph is absent, then remembered by the caller.
        let parent = (0usize, 11usize);
        assert_eq!(increasing.get(parent.0, parent.1), None);
        increasing.remember(parent.0, parent.1, expected(parent.0, parent.1));
        assert_eq!(
            increasing.get(parent.0, parent.1),
            Some(expected(parent.0, parent.1))
        );

        // Ranges that do not increase keep the HashMap, and still answer like the slice.
        let unordered_ranges = [(11usize, 17usize), (0usize, 6usize)];
        assert!(!ParagraphPrefilters::ranges_increase(unordered_ranges));
        let mut unordered = ParagraphPrefilters::new(false);
        for (start, end) in unordered_ranges {
            unordered.insert(start, end, expected(start, end));
        }
        for (start, end) in unordered_ranges {
            assert_eq!(
                unordered.get(start, end),
                Some(expected(start, end)),
                "unordered paragraph {start}..{end}"
            );
        }
        assert_eq!(unordered.get(2, 3), None);
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
