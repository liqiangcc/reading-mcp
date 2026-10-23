//! `json::to_string_exact` returns `serde_json::to_string`'s bytes in a `String` that is not
//! over-allocated (MEM-2b). The bytes are the contract: every stored blob and every tool response
//! is built from them.

use reading_mcp::domain::{
    NormalizedBlock, NormalizedBlockKind, NormalizedBlockMap, NormalizedBlockProvenance,
    NormalizedTextRange, SectionId,
};
use reading_mcp::json::to_string_exact;

fn block(
    section: &str,
    block_index: usize,
    source_order: usize,
    start: usize,
    end: usize,
    anchor: Option<&str>,
) -> NormalizedBlock {
    NormalizedBlock {
        owner_section_id: SectionId(section.to_string()),
        block_index,
        source_order,
        kind: NormalizedBlockKind::Paragraph,
        normalized_range: NormalizedTextRange::new(start, end).expect("ordered range"),
        native_anchor: anchor.map(str::to_string),
        native_location: anchor.map(|anchor| format!("html:#{anchor}")),
        provenance: NormalizedBlockProvenance::XhtmlNativeBlock,
    }
}

fn assert_same_json(map: &NormalizedBlockMap) -> String {
    let exact = to_string_exact(map).expect("exact JSON");
    let slack = serde_json::to_string(map).expect("serde_json JSON");
    assert_eq!(exact, slack, "the bytes must be serde_json::to_string's");
    assert_eq!(
        exact.capacity(),
        exact.len(),
        "the String must not carry capacity slack"
    );
    exact
}

#[test]
fn empty_map_is_byte_identical() {
    let json = assert_same_json(&NormalizedBlockMap::new(Vec::new()));
    assert_eq!(
        json,
        r#"{"schema_version":"normalized-block-model/v1","blocks":[]}"#
    );
}

#[test]
fn one_block_is_byte_identical() {
    let map = NormalizedBlockMap::new(vec![block("section://a", 1, 0, 0, 5, Some("p1"))]);
    let json = assert_same_json(&map);
    assert!(json.contains(r#""native_anchor":"p1""#), "{json}");
}

#[test]
fn non_ascii_anchors_are_byte_identical() {
    let map = NormalizedBlockMap::new(vec![
        block("section://卷一", 1, 0, 0, 7, Some("第一段")),
        block("section://卷一", 2, 1, 7, 14, Some("注「甲」\n乙")),
        block("section://émoji", 1, 2, 14, 21, Some("🙂anchor")),
    ]);
    let json = assert_same_json(&map);
    assert!(json.contains("第一段"), "{json}");
    assert!(
        json.contains("\\n"),
        "the escape stays serde_json's: {json}"
    );
}

#[test]
fn ten_thousand_blocks_are_byte_identical() {
    let blocks = (0..10_000)
        .map(|index| {
            let start = index * 10;
            block(
                &format!("section://{}", index % 97),
                index % 100 + 1,
                index,
                start,
                start + 9,
                Some(&format!("anchor-{index}")),
            )
        })
        .collect::<Vec<_>>();
    let map = NormalizedBlockMap::new(blocks);
    let json = assert_same_json(&map);
    assert!(
        json.len() > 1_000_000,
        "the fixture must be large enough to matter: {}",
        json.len()
    );
}
