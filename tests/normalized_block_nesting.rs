use reading_mcp::application::ports::{Parser, RetrievedResource};
use reading_mcp::domain::{DocumentSource, MediaType, NormalizedBlockKind};
use reading_mcp::parsing::HtmlParser;

#[tokio::test]
async fn nested_native_blocks_are_projected_once_without_duplicate_source_text() {
    let source = r#"<html><body>
<h1>Chapter</h1>
<blockquote id="quote"><p>Quoted <em>text</em>.</p><p>Second paragraph.</p></blockquote>
<p>After.</p>
</body></html>"#;
    let document = HtmlParser
        .parse(RetrievedResource {
            source: DocumentSource("memory:nested.html".into()),
            final_source: DocumentSource("memory:nested.html".into()),
            media_type: MediaType("text/html".into()),
            bytes: source.as_bytes().to_vec(),
            etag: None,
            last_modified: None,
            metadata: Default::default(),
        })
        .await
        .expect("HTML should parse");

    let map = document
        .normalized_block_map()
        .expect("block map should validate")
        .expect("block map should exist");
    assert_eq!(map.blocks.len(), 2);
    assert_eq!(map.blocks[0].kind, NormalizedBlockKind::BlockQuote);
    assert_eq!(map.blocks[1].kind, NormalizedBlockKind::Paragraph);

    let owner = document
        .find_section(&map.blocks[0].owner_section_id)
        .expect("owner Section should exist");
    assert_eq!(owner.content, "Quoted text.Second paragraph.\n\nAfter.");
    assert_eq!(owner.content.matches("Quoted text.").count(), 1);
    assert_eq!(
        owner
            .normalized_text_slice(map.blocks[0].normalized_range)
            .expect("blockquote range should resolve"),
        "Quoted text.Second paragraph."
    );
    assert_eq!(
        owner
            .normalized_text_slice(map.blocks[1].normalized_range)
            .expect("paragraph range should resolve"),
        "After."
    );
}
