//! V2-4b PR-2, §5: ports `tests/read_continuation_mcp.rs` — a section-tree read continued in
//! deliberately small windows until the stream is complete — to v2 `read` with a small
//! `max_chars` batch budget.
//!
//! v1 continued the legacy section-tree stream by character offsets (`stream.start_char`,
//! `end_char`, `total_chars`, `truncated`) and rebuilt the whole content from the windows. v2 never
//! splits an item: `max_chars` is the batch budget and continuation is the `next` handle, so the
//! equivalent is the smallest budget that still carries whole items, followed until the walk ends,
//! and the pages must rebuild the full sequence exactly — same handles, same texts, same order.
//! The char-offset stream fields are v1 response fields with no v2 name (§2.1 item 3).

#[path = "support/v2_mcp.rs"]
mod v2_mcp;

use serde_json::json;

use v2_mcp::{call, next_of, payload, read_items, start_client, v2_open, v2_read, walk};

#[tokio::test]
async fn v2_stdio_client_continues_a_section_tree_until_complete() {
    let directory = tempfile::tempdir().expect("temporary directory should be created");
    let document_path = directory.path().join("continuation.md");
    tokio::fs::write(
        &document_path,
        r#"# Continuation

A long first paragraph with enough content to cross several deliberately small response windows. Unicode stays ordered: 系统调用、进程、内存、🙂.

A second paragraph follows the first paragraph exactly once.

## Child

Child section content is part of the legacy section-tree stream.
"#,
    )
    .await
    .expect("fixture should be written");

    let client = start_client(directory.path(), std::path::Path::new("memory")).await;

    let opened = v2_open(&client, &document_path).await;
    assert_eq!(opened["sections"], 2, "{opened}");
    let doc = opened["doc"].as_str().expect("doc handle").to_owned();
    let outline = payload(
        &call(
            &client,
            "outline",
            json!({ "at": doc, "depth": 8, "limit": 100 }),
        )
        .await,
    );
    let section = outline["items"][0][0]
        .as_str()
        .expect("the root section handle")
        .to_owned();

    // The v1 full read: the section tree in one answer, nothing truncated.
    let full_page = payload(&v2_read(&client, &section, json!({ "max_chars": 4000 })).await);
    assert!(
        full_page.get("next").is_none(),
        "the whole stream fits one answer: {full_page}"
    );
    let full = read_items(&full_page, &section);
    let full_texts = full
        .iter()
        .map(|(_, text)| text.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        full_texts,
        [
            "A long first paragraph with enough content to cross several deliberately small response windows.",
            "Unicode stays ordered: 系统调用、进程、内存、🙂.",
            "A second paragraph follows the first paragraph exactly once.",
            "Child section content is part of the legacy section-tree stream.",
        ],
        "the v1 section-tree stream: {full_page}"
    );
    // The child section is part of the stream, named by its boundary (the v1 stream's sections).
    assert_eq!(
        full_page["sections"][1]["title"], "Child",
        "the child section is in the tree stream: {full_page}"
    );
    assert_eq!(full_page["sections"][1]["at"], 3);

    // The smallest budget that still carries whole items: every page holds at least one item and
    // never exceeds the budget, and the pages rebuild the full sequence without a gap.
    let total_chars: usize = full.iter().map(|(_, text)| text.chars().count()).sum();
    let budget = full
        .iter()
        .map(|(_, text)| text.chars().count())
        .max()
        .expect("a non-empty stream");
    let mut rebuilt = Vec::new();
    let mut from = Some(section.clone());
    let mut pages = 0usize;
    while let Some(page_from) = from {
        let result = v2_read(&client, &page_from, json!({ "max_chars": budget })).await;
        let page = payload(&result);
        assert!(
            result.is_error != Some(true),
            "small-budget read from {page_from} failed: {page}"
        );
        let items = read_items(&page, &page_from);
        assert!(!items.is_empty(), "every page carries at least one item");
        let chars: usize = items.iter().map(|(_, text)| text.chars().count()).sum();
        assert!(
            chars <= budget,
            "a page stays within the budget: {chars} > {budget}: {page}"
        );
        // The v1 window is contiguous: each page is exactly the next slice of the full sequence.
        assert_eq!(
            items,
            full[rebuilt.len()..rebuilt.len() + items.len()].to_vec(),
            "page {pages} is the next slice of the stream: {page}"
        );
        rebuilt.extend(items);
        from = next_of(&page);
        pages += 1;
        assert!(pages < 100, "the continuation must make finite progress");
    }
    assert_eq!(
        rebuilt, full,
        "the small-window pages rebuild the full section tree"
    );
    assert!(
        pages > 1,
        "a budget as wide as one item must take several pages for this fixture"
    );
    assert_eq!(
        rebuilt
            .iter()
            .map(|(_, text)| text.chars().count())
            .sum::<usize>(),
        total_chars,
        "the rebuilt stream has the v1 total char count"
    );
    assert_eq!(
        rebuilt
            .iter()
            .map(|(_, text)| text.as_str())
            .collect::<String>(),
        full.iter()
            .map(|(_, text)| text.as_str())
            .collect::<String>(),
        "the rebuilt text is the full content"
    );

    // The whole-book walk from the document handle is the same sequence (the section tree is the
    // tail of the book).
    assert_eq!(walk(&client, &doc, json!({})).await, full);

    client
        .cancel()
        .await
        .expect("MCP process should close cleanly");
}
