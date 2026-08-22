use reading_mcp::domain::{NormalizedBlockKind, NormalizedBlockProvenance};

#[test]
fn normalized_block_wire_names_are_stable() {
    assert_eq!(
        serde_json::to_string(&NormalizedBlockKind::Paragraph).expect("serialize paragraph"),
        "\"paragraph\""
    );
    assert_eq!(
        serde_json::to_string(&NormalizedBlockKind::BlockQuote).expect("serialize blockquote"),
        "\"blockquote\""
    );
    assert_eq!(
        serde_json::to_string(&NormalizedBlockKind::ListItem).expect("serialize list item"),
        "\"list_item\""
    );
    assert_eq!(
        serde_json::to_string(&NormalizedBlockKind::Preformatted).expect("serialize preformatted"),
        "\"preformatted\""
    );
    assert_eq!(
        serde_json::to_string(&NormalizedBlockKind::Table).expect("serialize table"),
        "\"table\""
    );
    assert_eq!(
        serde_json::to_string(&NormalizedBlockProvenance::XhtmlNativeBlock)
            .expect("serialize provenance"),
        "\"xhtml_native_block\""
    );
}
