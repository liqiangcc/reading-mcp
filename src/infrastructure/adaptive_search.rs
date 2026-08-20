use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;

use crate::application::ports::{ApplicationError, DocumentRepository, SearchHit, SearchIndex};
use crate::domain::{Document, DocumentId, Location, Section};

const MAX_SNIPPET_CHARS: usize = 320;

pub struct AdaptiveSearchIndex {
    inner: Arc<dyn SearchIndex>,
    repository: Arc<dyn DocumentRepository>,
}

impl AdaptiveSearchIndex {
    pub fn new(inner: Arc<dyn SearchIndex>, repository: Arc<dyn DocumentRepository>) -> Self {
        Self { inner, repository }
    }
}

#[async_trait]
impl SearchIndex for AdaptiveSearchIndex {
    async fn index(&self, document: &Document) -> Result<(), ApplicationError> {
        self.inner.index(document).await
    }

    async fn search(
        &self,
        document_id: &DocumentId,
        query: &str,
        limit: usize,
    ) -> Result<Vec<SearchHit>, ApplicationError> {
        if query.trim().is_empty() {
            return Err(ApplicationError::InvalidRequest(
                "search query must not be empty".into(),
            ));
        }
        if limit == 0 {
            return Ok(vec![]);
        }

        let mut indexed_hits = self.inner.search(document_id, query, limit).await?;
        let document = self
            .repository
            .get(document_id)
            .await?
            .ok_or(ApplicationError::DocumentNotFound)?;
        let normalized_query = normalize(query);
        let terms = query_terms(&normalized_query);
        let should_fallback =
            normalized_query.chars().any(is_compact_script) || indexed_hits.len() < limit;

        for hit in &mut indexed_hits {
            if let Some(section) = document.find_section(&hit.section_id) {
                let source_text = paragraph_for_location(section, &hit.location)
                    .unwrap_or(section.content.as_str());
                hit.snippet = centered_snippet(source_text, &normalized_query, &terms);
            }
        }

        let mut merged = HashMap::<String, SearchHit>::new();
        for hit in indexed_hits {
            merge_hit(&mut merged, hit);
        }

        // FTS5's default unicode tokenizer is excellent for whitespace-delimited
        // text, but CJK text and strict multi-term matching can under-recall.
        // The canonical document is already bounded by ResourceBudget, so a
        // deterministic paragraph scan is a safe fallback without adding a
        // vector database or changing the SearchIndex contract.
        if should_fallback {
            let mut fallback = Vec::new();
            for section in &document.root_sections {
                collect_fallback_hits(section, &document, &normalized_query, &terms, &mut fallback);
            }
            fallback.sort_by(|left, right| {
                right
                    .score
                    .total_cmp(&left.score)
                    .then_with(|| left.section_id.0.cmp(&right.section_id.0))
                    .then_with(|| left.snippet.cmp(&right.snippet))
            });
            fallback.truncate(limit.saturating_mul(3));
            for hit in fallback {
                merge_hit(&mut merged, hit);
            }
        }

        let mut hits = merged.into_values().collect::<Vec<_>>();
        hits.sort_by(|left, right| {
            right
                .score
                .total_cmp(&left.score)
                .then_with(|| left.section_id.0.cmp(&right.section_id.0))
                .then_with(|| left.snippet.cmp(&right.snippet))
        });
        hits.truncate(limit);
        Ok(hits)
    }
}

fn merge_hit(output: &mut HashMap<String, SearchHit>, hit: SearchHit) {
    let key = format!(
        "{}|{}|{}",
        hit.section_id.0,
        hit.location.paragraph.unwrap_or_default(),
        hit.location.native_location.as_deref().unwrap_or_default()
    );
    match output.get_mut(&key) {
        Some(existing) if hit.score > existing.score => *existing = hit,
        Some(_) => {}
        None => {
            output.insert(key, hit);
        }
    }
}

fn collect_fallback_hits(
    section: &Section,
    document: &Document,
    query: &str,
    terms: &[String],
    output: &mut Vec<SearchHit>,
) {
    let title = section.title.trim();
    let paragraphs = section
        .content
        .split("\n\n")
        .map(str::trim)
        .filter(|paragraph| !paragraph.is_empty())
        .collect::<Vec<_>>();

    if paragraphs.is_empty() {
        let haystack = normalize(title);
        let score = score_text(&haystack, query, terms);
        if score > 0.0 {
            output.push(SearchHit {
                section_id: section.id.clone(),
                title: section.title.clone(),
                source: document.source.clone(),
                snippet: centered_snippet(title, query, terms),
                score,
                location: section.location.clone(),
            });
        }
    } else {
        for (index, paragraph) in paragraphs.into_iter().enumerate() {
            let haystack = normalize(&format!("{title}\n{paragraph}"));
            let score = score_text(&haystack, query, terms);
            if score <= 0.0 {
                continue;
            }

            let mut location = section.location.clone();
            location.paragraph = Some((index + 1) as u32);
            location.native_location = Some(match &section.location.native_location {
                Some(native) => format!("{native}#search-unit:{}", index + 1),
                None => format!("search-unit:{}", index + 1),
            });
            output.push(SearchHit {
                section_id: section.id.clone(),
                title: section.title.clone(),
                source: document.source.clone(),
                snippet: centered_snippet(paragraph, query, terms),
                score,
                location,
            });
        }
    }

    for child in &section.children {
        collect_fallback_hits(child, document, query, terms, output);
    }
}

fn paragraph_for_location<'a>(section: &'a Section, location: &Location) -> Option<&'a str> {
    let paragraph = usize::try_from(location.paragraph?).ok()?;
    section
        .content
        .split("\n\n")
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .nth(paragraph.saturating_sub(1))
}

fn normalize(value: &str) -> String {
    value.to_lowercase()
}

fn query_terms(query: &str) -> Vec<String> {
    let mut terms = Vec::new();
    let mut word = String::new();
    let mut compact = String::new();

    let flush_word = |word: &mut String, terms: &mut Vec<String>| {
        if !word.is_empty() {
            terms.push(std::mem::take(word));
        }
    };
    let flush_compact = |compact: &mut String, terms: &mut Vec<String>| {
        if compact.is_empty() {
            return;
        }
        let chars = compact.chars().collect::<Vec<_>>();
        if chars.len() == 1 {
            terms.push(chars[0].to_string());
        } else {
            for pair in chars.windows(2) {
                terms.push(pair.iter().collect());
            }
        }
        compact.clear();
    };

    for ch in query.chars() {
        if is_compact_script(ch) {
            flush_word(&mut word, &mut terms);
            compact.push(ch);
        } else if ch.is_alphanumeric() {
            flush_compact(&mut compact, &mut terms);
            word.push(ch);
        } else {
            flush_word(&mut word, &mut terms);
            flush_compact(&mut compact, &mut terms);
        }
    }
    flush_word(&mut word, &mut terms);
    flush_compact(&mut compact, &mut terms);

    terms.sort();
    terms.dedup();
    terms
}

fn is_compact_script(ch: char) -> bool {
    matches!(
        ch,
        '\u{3400}'..='\u{4dbf}'
            | '\u{4e00}'..='\u{9fff}'
            | '\u{f900}'..='\u{faff}'
            | '\u{3040}'..='\u{309f}'
            | '\u{30a0}'..='\u{30ff}'
            | '\u{ac00}'..='\u{d7af}'
    )
}

fn score_text(haystack: &str, query: &str, terms: &[String]) -> f32 {
    if haystack.is_empty() {
        return 0.0;
    }
    let phrase_hits = if query.is_empty() {
        0
    } else {
        haystack.matches(query).count()
    };
    let mut matched_terms = 0usize;
    let mut term_hits = 0usize;
    for term in terms {
        let hits = haystack.matches(term).count();
        if hits > 0 {
            matched_terms += 1;
            term_hits += hits;
        }
    }
    if phrase_hits == 0 && matched_terms == 0 {
        return 0.0;
    }

    let coverage = if terms.is_empty() {
        0.0
    } else {
        matched_terms as f32 / terms.len() as f32
    };
    let phrase_bonus = if phrase_hits > 0 { 0.35 } else { 0.0 };
    (0.2 + phrase_bonus + coverage * 0.4 + (term_hits.min(8) as f32 * 0.02)).min(0.99)
}

fn centered_snippet(text: &str, query: &str, terms: &[String]) -> String {
    let trimmed = text.trim();
    if trimmed.chars().count() <= MAX_SNIPPET_CHARS {
        return trimmed.to_string();
    }

    let normalized = normalize(trimmed);
    let byte_match = if !query.is_empty() {
        normalized.find(query)
    } else {
        None
    }
    .or_else(|| {
        terms
            .iter()
            .filter(|term| !term.is_empty())
            .find_map(|term| normalized.find(term))
    });

    let match_char = byte_match
        .map(|byte| normalized[..byte].chars().count())
        .unwrap_or_default();
    let chars = trimmed.chars().collect::<Vec<_>>();
    let before = MAX_SNIPPET_CHARS / 3;
    let mut start = match_char.saturating_sub(before);
    if start + MAX_SNIPPET_CHARS > chars.len() {
        start = chars.len().saturating_sub(MAX_SNIPPET_CHARS);
    }
    let end = (start + MAX_SNIPPET_CHARS).min(chars.len());
    let mut snippet = chars[start..end].iter().collect::<String>();
    if start > 0 {
        snippet.insert(0, '…');
    }
    if end < chars.len() {
        snippet.push('…');
    }
    snippet
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compact_script_queries_generate_overlapping_terms() {
        let terms = query_terms("什么是虚拟内存页面置换");
        assert!(terms.contains(&"虚拟".to_string()));
        assert!(terms.contains(&"内存".to_string()));
        assert!(terms.contains(&"置换".to_string()));
    }

    #[test]
    fn snippet_is_centered_around_a_late_match() {
        let text = format!("{}needle{}", "a".repeat(500), "b".repeat(500));
        let snippet = centered_snippet(&text, "needle", &["needle".into()]);
        assert!(snippet.contains("needle"));
        assert!(snippet.chars().count() <= MAX_SNIPPET_CHARS + 2);
    }
}
