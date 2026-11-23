//! V2-4b PR-1, §2 row 19: re-covers `tests/structure_continuation_mcp.rs` — the structure cursor
//! continues a multi-page forest without repeating ancestors — as the v2 `outline` `next` cursor
//! across the same five-section forest.
//!
//! v2 `outline` is a flat preorder listing, so there are no repeated ancestor nodes to leak; the
//! equivalent assertions are that paging repeats no section, continues in the same preorder as one
//! unbounded listing, and that the forest's levels are the v1 ones (Root 1, One 2, Deep 3, Two 2,
//! Second 1).

#[path = "support/v2_mcp.rs"]
mod v2_mcp;

use std::collections::HashSet;

use serde_json::{Value, json};

use v2_mcp::{call, next_of, payload, start_client, v2_open};

#[tokio::test]
async fn outline_next_continues_a_multi_page_forest_without_repeating_a_section() {
    let directory = tempfile::tempdir().expect("temporary directory should be created");
    let path = directory.path().join("structure.md");
    tokio::fs::write(
        &path,
        r#"# Root

Root intro.

## One

One body.

### Deep

Deep body.

## Two

Two body.

# Second

Second body.
"#,
    )
    .await
    .expect("fixture should be written");
    let client = start_client(directory.path(), &directory.path().join("state")).await;

    let opened = v2_open(&client, &path).await;
    assert_eq!(
        opened["sections"].as_u64(),
        Some(5),
        "the forest has five sections: {opened}"
    );
    let doc = opened["doc"].as_str().expect("doc handle").to_owned();

    // One unbounded listing is the reference preorder and its levels.
    let full = payload(
        &call(
            &client,
            "outline",
            json!({ "at": doc, "depth": 4, "limit": 100 }),
        )
        .await,
    );
    let full_rows = rows(&full);
    let titles: Vec<&str> = full_rows.iter().map(|row| row.1.as_str()).collect();
    assert_eq!(titles, ["Root", "One", "Deep", "Two", "Second"]);
    let levels: Vec<u64> = full_rows.iter().map(|row| row.2).collect();
    assert_eq!(levels, [1, 2, 3, 2, 1], "the v1 forest shape");
    assert!(
        full.get("next").is_none(),
        "a complete listing has no cursor: {full}"
    );

    // The same forest in pages of two, following `next` until it stops.
    let mut paged = Vec::new();
    let mut cursor: Option<String> = None;
    let mut page_sizes = Vec::new();
    loop {
        let mut request = json!({ "at": doc, "depth": 4, "limit": 2 });
        if let Some(cursor) = &cursor {
            request["cursor"] = json!(cursor);
        }
        let page = payload(&call(&client, "outline", request).await);
        let page_rows = rows(&page);
        assert!(
            !page_rows.is_empty() && page_rows.len() <= 2,
            "a page carries one or two nodes: {page}"
        );
        page_sizes.push(page_rows.len());
        paged.extend(page_rows);
        match next_of(&page) {
            Some(next) => {
                assert!(
                    next.starts_with("o:"),
                    "outline next is an o: cursor: {next}"
                );
                assert_ne!(Some(next.clone()), cursor, "the cursor must advance");
                cursor = Some(next);
            }
            None => break,
        }
        assert!(page_sizes.len() < 100, "paging must terminate");
    }

    assert_eq!(page_sizes, [2, 2, 1], "the forest pages 2 + 2 + 1");
    assert_eq!(
        paged, full_rows,
        "paged outline equals the unbounded listing, in order, without gaps"
    );
    let unique: HashSet<&String> = paged.iter().map(|row| &row.0).collect();
    assert_eq!(
        unique.len(),
        paged.len(),
        "no section is listed twice across pages"
    );

    client.cancel().await.expect("client should close cleanly");
}

/// `(handle, title, level)` of each outline row.
fn rows(outline: &Value) -> Vec<(String, String, u64)> {
    outline["items"]
        .as_array()
        .unwrap_or_else(|| panic!("outline items: {outline}"))
        .iter()
        .map(|row| {
            let parts = row.as_array().expect("an outline row is an array");
            (
                parts[0].as_str().expect("handle").to_owned(),
                parts[1].as_str().expect("title").to_owned(),
                parts[2].as_u64().expect("level"),
            )
        })
        .collect()
}
