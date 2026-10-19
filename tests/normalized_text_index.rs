use reading_mcp::domain::{NormalizedTextIndex, NormalizedTextRange};

fn mixed_text() -> String {
    "aé中😀 b\n".repeat(43)
}

#[test]
fn index_slice_matches_range_slice_for_every_range() {
    let text = mixed_text();
    let char_count = text.chars().count();
    let index = NormalizedTextIndex::new(&text);
    let mut checked = 0usize;
    for start in 0..=char_count + 2 {
        for end in start..=char_count + 2 {
            let range = NormalizedTextRange::new(start, end).unwrap();
            assert_eq!(
                index.slice(range),
                range.slice(&text),
                "range {start}..{end}"
            );
            checked += 1;
        }
    }
    assert!(checked > 0);
}

#[test]
fn empty_text_index() {
    let text = "";
    let index = NormalizedTextIndex::new(text);
    assert_eq!(index.char_len(), 0);
    let range = NormalizedTextRange::new(0, 0).unwrap();
    assert_eq!(index.slice(range), Ok(""));
}

#[test]
fn char_len_matches_char_count() {
    let text = mixed_text();
    assert!(text.chars().count() >= 300);
    let index = NormalizedTextIndex::new(&text);
    assert_eq!(index.char_len(), text.chars().count());
}
