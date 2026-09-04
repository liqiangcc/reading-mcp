use lopdf::Document as PdfDocument;

const MAX_PAGE_DECOMPRESSED_BYTES: usize = 16 * 1024 * 1024;

#[test]
#[ignore = "requires canonical Raft 2014 PDF evidence"]
fn raft_numbered_line_shape_is_reported_without_body_text() {
    let path = std::env::var("READING_MCP_RAFT_EVIDENCE_PDF")
        .expect("Raft diagnostic workflow must provide the PDF path");
    let bytes = std::fs::read(path).expect("Raft evidence PDF should be readable");
    let pdf = PdfDocument::load_mem(&bytes).expect("Raft evidence PDF should parse");

    for page_number in pdf.get_pages().keys().copied() {
        let text = pdf
            .extract_text_with_limit(&[page_number], MAX_PAGE_DECOMPRESSED_BYTES)
            .expect("Raft page text should extract");
        let lines = text.lines().collect::<Vec<_>>();
        for (index, line) in lines.iter().enumerate() {
            let trimmed = line.trim();
            let Some(first) = trimmed.split_whitespace().next() else {
                continue;
            };
            let digit_prefix_len = first.bytes().take_while(u8::is_ascii_digit).count();
            if digit_prefix_len == 0 {
                continue;
            }
            let pure_numeric = first
                .trim_end_matches('.')
                .split('.')
                .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()));
            let rest = trimmed.strip_prefix(first).unwrap_or_default().trim();
            let next = lines
                .get(index + 1)
                .map(|value| value.trim())
                .unwrap_or_default();
            println!(
                "HEADING_SHAPE page={} first_len={} digit_prefix_len={} pure_numeric={} first_has_dot={} line_chars={} line_words={} rest_chars={} rest_words={} next_chars={} next_words={} next_starts_alpha={}",
                page_number,
                first.chars().count(),
                digit_prefix_len,
                pure_numeric,
                first.contains('.'),
                trimmed.chars().count(),
                trimmed.split_whitespace().count(),
                rest.chars().count(),
                rest.split_whitespace().count(),
                next.chars().count(),
                next.split_whitespace().count(),
                next.chars().next().is_some_and(char::is_alphabetic),
            );
        }

        let chunks = pdf.extract_text_chunks_with_limit(
            &[page_number],
            MAX_PAGE_DECOMPRESSED_BYTES,
        );
        for (chunk_index, chunk) in chunks.into_iter().enumerate() {
            let Ok(chunk) = chunk else {
                println!("CHUNK_SHAPE page={} chunk={} decode_error=true", page_number, chunk_index);
                continue;
            };
            let trimmed = chunk.trim();
            if trimmed.is_empty() {
                continue;
            }
            let tokens = trimmed.split_whitespace().collect::<Vec<_>>();
            let first = tokens.first().copied().unwrap_or_default();
            let digit_prefix_len = first.bytes().take_while(u8::is_ascii_digit).count();
            let chars = trimmed.chars().count();
            let words = tokens.len();
            let short_structural = chars <= 180 && words <= 28;
            if digit_prefix_len > 0 || short_structural {
                println!(
                    "CHUNK_SHAPE page={} chunk={} chars={} words={} lines={} first_len={} digit_prefix_len={} first_starts_alpha={} short_structural={}",
                    page_number,
                    chunk_index,
                    chars,
                    words,
                    trimmed.lines().count(),
                    first.chars().count(),
                    digit_prefix_len,
                    first.chars().next().is_some_and(char::is_alphabetic),
                    short_structural,
                );
            }

            let numeric_first = first
                .trim_end_matches('.')
                .split('.')
                .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()));
            let intro_pos = tokens.iter().position(|token| {
                token.trim_matches(|c: char| !c.is_alphanumeric()) == "Introduction"
            });
            if (numeric_first && words <= 28) || intro_pos.is_some() {
                let start = intro_pos.map(|pos| pos.saturating_sub(2)).unwrap_or(0);
                let end = tokens.len().min(start + 10);
                println!(
                    "CHUNK_CANDIDATE page={} chunk={} context={}",
                    page_number,
                    chunk_index,
                    tokens[start..end].join(" ")
                );
            }
        }
    }
}
