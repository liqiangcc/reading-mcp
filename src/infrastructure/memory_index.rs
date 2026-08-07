use std::collections::HashMap;
use std::sync::RwLock;

use async_trait::async_trait;

use crate::application::ports::{ApplicationError, SearchHit, SearchIndex};
use crate::domain::{Document, DocumentId, Location, Section, SectionId};

const MAX_SNIPPET_CHARS: usize = 320;

#[derive(Default)]
pub struct InMemorySearchIndex {
    documents: RwLock<HashMap<DocumentId, Vec<SearchUnit>>>,
}

#[derive(Clone, Debug)]
struct SearchUnit {
    section_id: SectionId,
    search_text: String,
    snippet: String,
    location: Location,
}

#[async_trait]
impl SearchIndex for InMemorySearchIndex {
    async fn index(&self, document: &Document) -> Result<(), ApplicationError> {
        let mut units = Vec::new();
        for section in &document.root_sections {
            collect_units(section, &mut units);
        }

        self.documents
            .write()
            .map_err(|_| ApplicationError::IndexFailed("search index lock poisoned".into()))?
            .insert(document.id.clone(), units);

        Ok(())
    }

    async fn search(
        &self,
        document_id: &DocumentId,
        query: &str,
        limit: usize,
    ) -> Result<Vec<SearchHit>, ApplicationError> {
        let normalized_query = normalize(query);
        if normalized_query.is_empty() {
            return Err(ApplicationError::InvalidRequest(
                "search query must not be empty".into(),
            ));
        }
        if limit == 0 {
            return Ok(vec![]);
        }

        let documents = self
            .documents
            .read()
            .map_err(|_| ApplicationError::IndexFailed("search index lock poisoned".into()))?;
        let units = documents
            .get(document_id)
            .ok_or(ApplicationError::DocumentNotFound)?;
        let terms = query_terms(&normalized_query);

        let mut scored = units
            .iter()
            .filter_map(|unit| {
                let haystack = normalize(&unit.search_text);
                let score = score(&haystack, &normalized_query, &terms);
                (score > 0.0).then(|| SearchHit {
                    section_id: unit.section_id.clone(),
                    snippet: unit.snippet.clone(),
                    score,
                    location: unit.location.clone(),
                })
            })
            .collect::<Vec<_>>();

        scored.sort_by(|left, right| {
            right
                .score
                .total_cmp(&left.score)
                .then_with(|| left.section_id.0.cmp(&right.section_id.0))
                .then_with(|| left.snippet.cmp(&right.snippet))
        });
        scored.truncate(limit);

        Ok(scored)
    }
}

fn collect_units(section: &Section, output: &mut Vec<SearchUnit>) {
    let title = section.title.trim();
    if section.content.trim().is_empty() {
        output.push(SearchUnit {
            section_id: section.id.clone(),
            search_text: title.to_string(),
            snippet: truncate(title, MAX_SNIPPET_CHARS),
            location: section.location.clone(),
        });
    } else {
        for (unit_index, (start, end)) in paragraph_ranges(&section.content).into_iter().enumerate() {
            let paragraph = &section.content[start..end];
            let mut location = section.location.clone();
            if let Some(base) = section.location.char_start {
                location.char_start = Some(base + section.content[..start].chars().count());
                location.char_end = Some(base + section.content[..end].chars().count());
            }
            location.native_location = Some(match &section.location.native_location {
                Some(native) => format!("{native}#search-unit:{}", unit_index + 1),
                None => format!("search-unit:{}", unit_index + 1),
            });

            output.push(SearchUnit {
                section_id: section.id.clone(),
                search_text: format!("{title}\n{paragraph}"),
                snippet: truncate(paragraph, MAX_SNIPPET_CHARS),
                location,
            });
        }
    }

    for child in &section.children {
        collect_units(child, output);
    }
}

fn paragraph_ranges(content: &str) -> Vec<(usize, usize)> {
    let mut ranges = Vec::new();
    let mut paragraph_start: Option<usize> = None;
    let mut offset = 0usize;

    for line in content.split_inclusive('\n') {
        let line_start = offset;
        offset += line.len();

        if line.trim().is_empty() {
            if let Some(start) = paragraph_start.take()
                && let Some(range) = trim_range(content, start, line_start)
            {
                ranges.push(range);
            }
        } else if paragraph_start.is_none() {
            paragraph_start = Some(line_start);
        }
    }

    if let Some(start) = paragraph_start
        && let Some(range) = trim_range(content, start, content.len())
    {
        ranges.push(range);
    }

    if ranges.is_empty() && !content.trim().is_empty() {
        if let Some(range) = trim_range(content, 0, content.len()) {
            ranges.push(range);
        }
    }

    ranges
}

fn trim_range(content: &str, mut start: usize, mut end: usize) -> Option<(usize, usize)> {
    while start < end {
        let Some(ch) = content[start..end].chars().next() else {
            break;
        };
        if !ch.is_whitespace() {
            break;
        }
        start += ch.len_utf8();
    }

    while start < end {
        let Some(ch) = content[start..end].chars().next_back() else {
            break;
        };
        if !ch.is_whitespace() {
            break;
        }
        end -= ch.len_utf8();
    }

    (start < end).then_some((start, end))
}

fn normalize(value: &str) -> String {
    value.to_lowercase()
}

fn query_terms(query: &str) -> Vec<&str> {
    query
        .split(|ch: char| ch.is_whitespace() || ch.is_ascii_punctuation())
        .filter(|term| !term.is_empty())
        .collect()
}

fn score(haystack: &str, query: &str, terms: &[&str]) -> f32 {
    let phrase_hits = haystack.matches(query).count();
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

    phrase_hits as f32 * 3.0 + matched_terms as f32 + term_hits as f32 * 0.25
}

fn truncate(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_string();
    }

    value.chars().take(max_chars).collect()
}
