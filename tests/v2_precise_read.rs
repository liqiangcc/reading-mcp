//! V2-4b PR-1, §2 row 12: re-covers `tests/precise_read_locator_stdio.rs` — a text locator hands
//! off to an exact small-budget read with continuation — as `read{"from":handle,"max_chars":n}`.
//!
//! v2 never splits an item: `max_chars` is the batch budget and continuation is the `next` handle,
//! so the equivalent of the v1 char-level exact-target continuation is a small budget that returns
//! one item per page, followed until the paragraph is rebuilt exactly. A budget below the first
//! item answers `too_long` with a retry at the item's own char count, which is also exercised.

#[path = "support/v2_mcp.rs"]
mod v2_mcp;

use serde_json::json;

use v2_mcp::{next_of, payload, read_items, start_client, tool_error, v2_open, v2_read, walk};

const PARAGRAPH: &str = "Exact locator reads preserve this entire paragraph across a deliberately small response budget. A second long sentence keeps rendered offsets away from source identity. A third sentence closes the paragraph with enough text to need several pages.";

#[tokio::test]
async fn read_from_a_handle_with_a_small_max_chars_continues_without_gaps() {
    let directory = tempfile::tempdir().expect("temporary directory should be created");
    let path = directory.path().join("precise.md");
    tokio::fs::write(
        &path,
        format!("# Book\n\n## Topic\n\n{PARAGRAPH}\n\nSecond paragraph.\n"),
    )
    .await
    .expect("fixture should be written");
    let client = start_client(directory.path(), &directory.path().join("state")).await;

    let doc = v2_open(&client, &path).await["doc"]
        .as_str()
        .expect("doc handle")
        .to_owned();

    // The reference sequence: one sentence per call, following `next`.
    let baseline = walk(&client, &doc, json!({})).await;
    assert!(
        baseline.len() >= 3,
        "the fixture must split into several sentences: {baseline:?}"
    );
    assert!(
        baseline
            .iter()
            .map(|(_, text)| text.as_str())
            .collect::<String>()
            .contains("Exact locator reads preserve"),
        "the long paragraph must be readable: {baseline:?}"
    );

    // A budget exactly as wide as the widest item: every page carries at least one whole item and
    // never exceeds the budget, and the pages rebuild the reference sequence without a gap.
    let budget = baseline
        .iter()
        .map(|(_, text)| text.chars().count())
        .max()
        .expect("a non-empty baseline");
    let mut rebuilt = Vec::new();
    let mut from = Some(doc.clone());
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
            "a page must stay within the budget: {chars} > {budget}: {page}"
        );
        rebuilt.extend(items);
        from = next_of(&page);
        pages += 1;
        assert!(pages < 10_000, "the continuation must terminate");
    }
    assert_eq!(
        rebuilt, baseline,
        "small-budget pages must rebuild the reference sequence"
    );
    assert!(
        pages > 1,
        "a budget as wide as one item must take several pages for this fixture"
    );

    // The same continuation from a mid-book handle starts at that handle's own item, not after it.
    let start = baseline[1].0.clone();
    let from_handle = walk(&client, &start, json!({ "max_chars": budget })).await;
    assert_eq!(
        from_handle,
        baseline[1..],
        "a handle-anchored small-budget read includes the anchor and continues to the end"
    );

    // A budget below the first item is `too_long`, and its retry is exactly the item's char count.
    let error = tool_error(&v2_read(&client, &doc, json!({ "max_chars": 1 })).await);
    assert_eq!(error["error"], "too_long");
    let retry_chars = error["retry"]["max_chars"]
        .as_u64()
        .expect("a retry with the item char count") as usize;
    assert_eq!(
        retry_chars,
        baseline[0].1.chars().count(),
        "the retry widens max_chars to the first item: {error}"
    );
    let retried = payload(&v2_read(&client, &doc, json!({ "max_chars": retry_chars })).await);
    assert_eq!(
        read_items(&retried, &doc),
        vec![baseline[0].clone()],
        "the suggested retry reads exactly the first item"
    );

    client.cancel().await.expect("client should close cleanly");
}
