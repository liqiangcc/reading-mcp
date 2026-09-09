use std::collections::BTreeMap;
use std::env;
use std::fs;

use lopdf::Document as LopdfDocument;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

#[path = "../../../src/parsing/pdf_layout.rs"]
#[allow(dead_code)]
mod pdf_layout;

const MAX_PAGE_BYTES: usize = 16 * 1024 * 1024;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = env::args().skip(1);
    let engine = args.next().unwrap_or_else(|| "lopdf-layout".into());
    let path = args.next().ok_or("usage: issue89-pdf-probe <lopdf-layout|pdf-extract> <pdf>")?;
    let bytes = fs::read(&path)?;
    let result = match engine.as_str() {
        "lopdf-layout" => probe_lopdf(&bytes)?,
        "pdf-extract" => probe_pdf_extract(&bytes)?,
        other => return Err(format!("unknown engine: {other}").into()),
    };
    println!("{}", serde_json::to_string(&result)?);
    Ok(())
}

fn probe_lopdf(bytes: &[u8]) -> Result<Value, Box<dyn std::error::Error>> {
    let pdf = LopdfDocument::load_mem(bytes)?;
    let page_numbers = pdf.get_pages().keys().copied().collect::<Vec<_>>();
    let (fragments, errors) = pdf_layout::extract_text_fragment_evidence(
        &pdf,
        &page_numbers,
        MAX_PAGE_BYTES,
    );
    let lines = reconstruct_lines(&fragments);
    let ordered = reading_order(&lines);
    let abstract_index = ordered.iter().position(|line| is_abstract(&line.text));
    let first_section_index = ordered
        .iter()
        .position(|line| is_first_section(&line.text));
    let abstract_body = match (abstract_index, first_section_index) {
        (Some(start), Some(end)) if start < end => ordered[start + 1..end]
            .iter()
            .filter(|line| line.page == ordered[start].page)
            .map(|line| line.text.as_str())
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    };
    let ranges = sentence_ranges(&abstract_body);
    Ok(json!({
        "engine": "lopdf-layout-candidate-v1",
        "pages": page_numbers.len(),
        "fragment_count": fragments.len(),
        "line_count": ordered.len(),
        "layout_errors": errors.len(),
        "abstract_heading_found": abstract_index.is_some(),
        "first_section_found": first_section_index.is_some(),
        "abstract_body_chars": abstract_body.chars().count(),
        "abstract_sentence_count": ranges.len(),
        "abstract_sentence_ranges": ranges.iter().map(|(start, end)| json!({
            "start": start,
            "end": end,
            "chars": end - start,
            "sha256": sha256_slice(&abstract_body, *start, *end),
        })).collect::<Vec<_>>(),
        "deterministic_input_hash": sha256_bytes(bytes),
    }))
}

fn probe_pdf_extract(bytes: &[u8]) -> Result<Value, Box<dyn std::error::Error>> {
    let pages = pdf_extract::extract_text_from_mem_by_pages(bytes)?;
    let lines = pages
        .iter()
        .enumerate()
        .flat_map(|(page, text)| text.lines().map(move |line| (page + 1, line)))
        .collect::<Vec<_>>();
    let candidates = lines
        .iter()
        .filter_map(|(page, line)| parse_numbered(line).map(|number| json!({
            "page": page,
            "designator": number,
            "chars": line.chars().count(),
            "words": line.split_whitespace().count(),
        })))
        .collect::<Vec<_>>();
    let top_pages = candidates.iter().fold(BTreeMap::<String, Vec<usize>>::new(), |mut acc, value| {
        let number = value["designator"].as_str().unwrap_or_default();
        if number.matches('.').count() == 0 && (1..=10).any(|n| number == n.to_string()) {
            acc.entry(number.into())
                .or_default()
                .push(value["page"].as_u64().unwrap_or_default() as usize);
        }
        acc
    });
    Ok(json!({
        "engine": "pdf-extract-0.12.0",
        "pages": pages.len(),
        "line_count": lines.len(),
        "numbered_candidate_count": candidates.len(),
        "numbered_candidates": candidates.iter().take(80).collect::<Vec<_>>(),
        "top_level_pages": top_pages,
        "deterministic_input_hash": sha256_bytes(bytes),
    }))
}

#[derive(Clone, Debug)]
struct Line {
    page: u32,
    text: String,
    x: f32,
    y: f32,
}

fn reconstruct_lines(fragments: &[pdf_layout::PdfTextFragmentEvidence]) -> Vec<Line> {
    let mut ordered = fragments.to_vec();
    ordered.sort_by(|left, right| {
        left.page
            .cmp(&right.page)
            .then_with(|| right.y.unwrap_or(0.0).total_cmp(&left.y.unwrap_or(0.0)))
            .then_with(|| left.x.unwrap_or(0.0).total_cmp(&right.x.unwrap_or(0.0)))
            .then_with(|| left.sequence_index.cmp(&right.sequence_index))
    });
    let mut lines = Vec::new();
    for fragment in ordered {
        let x = fragment.x.unwrap_or(0.0);
        let y = fragment.y.unwrap_or(0.0);
        let join = lines.last().is_some_and(|line: &Line| {
            line.page == fragment.page && (line.y - y).abs() <= 1.0 && (x - line.x).abs() < 180.0
        });
        if join {
            let line = lines.last_mut().expect("joinable line");
            if !line.text.is_empty() && !fragment.text.starts_with(char::is_whitespace) {
                line.text.push(' ');
            }
            line.text.push_str(fragment.text.trim());
        } else {
            lines.push(Line {
                page: fragment.page,
                text: fragment.text.trim().into(),
                x,
                y,
            });
        }
    }
    lines
}

fn reading_order(lines: &[Line]) -> Vec<Line> {
    let mut by_page = BTreeMap::<u32, Vec<Line>>::new();
    for line in lines {
        by_page.entry(line.page).or_default().push(line.clone());
    }
    let mut output = Vec::new();
    for (_page, mut page_lines) in by_page {
        page_lines.sort_by(|left, right| {
            right.y.total_cmp(&left.y).then_with(|| left.x.total_cmp(&right.x))
        });
        let mut starts = page_lines.iter().map(|line| line.x).collect::<Vec<_>>();
        starts.sort_by(|left, right| left.total_cmp(right));
        let split = starts.windows(2).enumerate().max_by(|left, right| {
            let left_gap = left.1[1] - left.1[0];
            let right_gap = right.1[1] - right.1[0];
            left_gap.total_cmp(&right_gap)
        });
        if let Some((_index, gap_values)) = split {
            let gap = gap_values[1] - gap_values[0];
            if gap > 120.0 {
                let cut = (gap_values[0] + gap_values[1]) / 2.0;
                page_lines.sort_by(|left, right| {
                    let left_column = left.x >= cut;
                    let right_column = right.x >= cut;
                    left_column.cmp(&right_column).then_with(|| right.y.total_cmp(&left.y))
                });
            }
        }
        output.extend(page_lines);
    }
    output
}

fn is_abstract(text: &str) -> bool {
    text.chars().filter(|character| character.is_alphanumeric()).flat_map(char::to_lowercase).collect::<String>() == "abstract"
}

fn is_first_section(text: &str) -> bool {
    let trimmed = text.trim_start();
    let rest = trimmed.strip_prefix('1');
    rest.is_some_and(|value| value.chars().next().is_some_and(|character| character.is_whitespace() || matches!(character, '.' | ':' | '-')))
}

fn parse_numbered(line: &str) -> Option<String> {
    let consumed = line.chars().take_while(|character| character.is_ascii_digit() || *character == '.').collect::<String>();
    if consumed.is_empty() || consumed.starts_with('.') || consumed.ends_with('.') || consumed.split('.').any(|part| part.is_empty() || part == "0") || !(1..=6).contains(&consumed.matches('.').count().saturating_add(1)) {
        return None;
    }
    line[consumed.len()..].chars().any(char::is_alphabetic).then_some(consumed)
}

fn sentence_ranges(text: &str) -> Vec<(usize, usize)> {
    let chars = text.chars().collect::<Vec<_>>();
    let mut ranges = Vec::new();
    let mut start = 0;
    for index in 0..chars.len() {
        if !matches!(chars[index], '.' | '!' | '?' | '。' | '！' | '？') {
            continue;
        }
        let next = chars.get(index + 1).copied();
        let prev = chars.get(index.saturating_sub(1)).copied();
        let decimal = prev.is_some_and(|character| character.is_ascii_digit()) && next.is_some_and(|character| character.is_ascii_digit());
        let identifier = prev.is_some_and(|character| character.is_alphanumeric()) && next.is_some_and(|character| character.is_alphanumeric());
        let abbreviation = is_abbreviation(&chars, index, next);
        if decimal || identifier || abbreviation || !next.is_none_or(char::is_whitespace) {
            continue;
        }
        let end = index + 1;
        if start < end {
            ranges.push((start, end));
            start = end;
            while chars.get(start).is_some_and(|character| character.is_whitespace()) {
                start += 1;
            }
        }
    }
    if start < chars.len() {
        ranges.push((start, chars.len()));
    }
    ranges
}

fn is_abbreviation(chars: &[char], index: usize, next: Option<char>) -> bool {
    if next != Some(' ') {
        return false;
    }
    let start = (0..index)
        .rev()
        .find(|position| chars[*position].is_whitespace())
        .map_or(0, |position| position + 1);
    let token = chars[start..index].iter().collect::<String>();
    matches!(token.as_str(), "e.g" | "i.e" | "Dr" | "Mr" | "Ms" | "Prof" | "Fig" | "Sec" | "No" | "vs" | "etc")
        || (token.len() == 1 && token.chars().all(|character| character.is_ascii_uppercase()))
}

fn sha256_bytes(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}

fn sha256_slice(text: &str, start: usize, end: usize) -> String {
    let value = text.chars().skip(start).take(end - start).collect::<String>();
    sha256_bytes(value.as_bytes())
}
