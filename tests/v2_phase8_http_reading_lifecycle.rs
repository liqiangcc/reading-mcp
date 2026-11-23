//! V2-4b PR-2, §5: ports `tests/phase8_http_reading_lifecycle.rs` — the same reading journey over
//! streamable HTTP, including the restart resume — to the seven v2 tools. This stays the only HTTP
//! lifecycle test.
//!
//! The v1 journey was `list_documents` (two pages) → `open_document` → `get_document_structure`
//! (two pages) → `get_text_units` (one item, then its cursor) → `read_document` by locator →
//! `get_context` (neighbour window) → two section streams → `search_document` → read/context from
//! the hit → restart → reopen → anchored resume. The v2 journey is `browse` → `open` → `outline`
//! (paged by `next`) → `read` (one item, then `next`) → `cite` → section streams → `search` →
//! `read` from the hit → restart → `open` → `read` from the persisted `next`.
//!
//! Dropped, not silently: the `open` reading profile (§2.1 item 5); the `get_context` neighbour
//! windows and their tagged relations (§2.1 items 1 and 3); the v1 response fields with no v2 name
//! — `source`, `section://` ids, `stream` indexes and `body_order_version`, `coverage` statistics,
//! `complete`/`section_complete`, cursors and `resolved_target_locator`/`start_anchor_locator`
//! (§2.1 item 3). Every locator they carried is asserted through `cite`, and every sequence
//! through the v2 handles.

#[path = "support/v2_mcp.rs"]
mod v2_mcp;

use serde_json::json;

use v2_mcp::{
    call, flat_text, http_server_command, identity_of, locator_of, next_of, payload, reserve_port,
    section_items, section_of, start_http_client, v2_open, v2_read, wait_until_ready,
};

const ALPHA: &str = "section://reading-book/alpha";
const BETA: &str = "section://reading-book/beta";

#[tokio::test]
async fn v2_streamable_http_completes_the_reading_lifecycle_and_restart_resume() {
    let directory = tempfile::tempdir().expect("temporary fixture directory should be created");
    let state = tempfile::tempdir().expect("temporary state directory should be created");
    let document_path = directory.path().join("book.md");
    tokio::fs::write(
        &document_path,
        r#"# Reading Book

Introduction sentence one. Introduction sentence two.

## Alpha

Alpha sentence one. Alpha sentence two.

## Beta

Beta sentence one. Beta sentence two.
"#,
    )
    .await
    .expect("HTTP fixture should be written");
    tokio::fs::write(directory.path().join("appendix.md"), "Appendix body.")
        .await
        .expect("discovery fixture should be written");

    let port = reserve_port();
    let base = format!("http://127.0.0.1:{port}");
    let mut server = http_server_command(port, directory.path(), state.path())
        .spawn()
        .expect("HTTP server should start");
    wait_until_ready(&base, &mut server).await;

    let client = start_http_client(&base).await;
    let mut tool_names = client
        .list_all_tools()
        .await
        .expect("HTTP tools/list should succeed")
        .into_iter()
        .map(|tool| tool.name.into_owned())
        .collect::<Vec<_>>();
    tool_names.sort();
    assert_eq!(
        tool_names,
        vec![
            "browse",
            "cite",
            "open",
            "outline",
            "read",
            "search",
            "source_view"
        ]
    );

    // browse: the two v1 discovery pages in one answer.
    let listing = payload(
        &call(
            &client,
            "browse",
            json!({ "path": directory.path().to_string_lossy() }),
        )
        .await,
    );
    let entries = listing
        .as_array()
        .expect("browse answers with an array")
        .iter()
        .map(|entry| {
            (
                entry[0].as_str().expect("name").to_owned(),
                entry[1].as_str().expect("kind").to_owned(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        entries,
        vec![
            ("appendix.md".to_owned(), "doc".to_owned()),
            ("book.md".to_owned(), "doc".to_owned())
        ]
    );

    let opened = v2_open(&client, &document_path).await;
    assert_eq!(opened["sections"], 3, "three sections: {opened}");
    let doc = opened["doc"].as_str().expect("doc handle").to_owned();

    // outline: the v1 two-page structure cursor. Paging repeats nothing and covers exactly the
    // unbounded preorder.
    let full = payload(
        &call(
            &client,
            "outline",
            json!({ "at": doc, "depth": 8, "limit": 100 }),
        )
        .await,
    );
    let all = full["items"].as_array().expect("outline items");
    assert_eq!(all.len(), 3, "the three v1 sections: {full}");
    let first_page = payload(
        &call(
            &client,
            "outline",
            json!({ "at": doc, "depth": 8, "limit": 2 }),
        )
        .await,
    );
    assert_eq!(
        first_page["items"].as_array().expect("items").len(),
        2,
        "a two-node page: {first_page}"
    );
    let mut paged = first_page["items"].as_array().expect("items").clone();
    let mut cursor = next_of(&first_page);
    let mut pages = 1;
    while let Some(next) = cursor {
        let page = payload(
            &call(
                &client,
                "outline",
                json!({ "at": doc, "depth": 8, "limit": 2, "cursor": next }),
            )
            .await,
        );
        paged.extend(page["items"].as_array().expect("items").clone());
        cursor = next_of(&page);
        pages += 1;
        assert!(pages < 10, "outline paging must terminate");
    }
    assert_eq!(
        paged,
        all.to_vec(),
        "the pages rebuild the preorder: {paged:?}"
    );
    for row in &paged {
        assert!(
            row[0].as_str().expect("handle").starts_with(&doc),
            "every row is a section of this document: {row}"
        );
        assert!(!row[1].as_str().expect("title").is_empty());
    }
    let alpha = paged[1][0].as_str().expect("section handle").to_owned();
    let beta = paged[2][0].as_str().expect("section handle").to_owned();

    // read: the v1 first sentence page, then its continuation.
    let first = payload(&v2_read(&client, &alpha, json!({})).await);
    assert_eq!(flat_text(&first), "Alpha sentence one.", "{first}");
    assert_eq!(first["section"]["s"], alpha);
    assert_eq!(first["section"]["title"], "Alpha");
    let first_sentence = first["at"]
        .as_str()
        .expect("the item's own handle")
        .to_owned();
    let next = first["next"]
        .as_str()
        .expect("the page continues")
        .to_owned();
    let first_locator = locator_of(&client, &first_sentence).await;
    assert_eq!(first_locator["owner_section_id"], ALPHA);
    assert_eq!(first_locator["paragraph_index"], 1);
    assert_eq!(first_locator["sentence_index"], 1);
    let second = payload(&v2_read(&client, &next, json!({})).await);
    assert_eq!(flat_text(&second), "Alpha sentence two.", "{second}");

    // The v1 exact read by locator: `cite` returns exactly that locator for the handle, and the
    // handle reads exactly that sentence.
    assert_eq!(
        locator_of(&client, &first_sentence).await,
        first_locator,
        "the cited locator is the v1 resolved target"
    );
    let exact = payload(&v2_read(&client, &first_sentence, json!({})).await);
    assert_eq!(flat_text(&exact), "Alpha sentence one.");
    assert!(
        exact.get("items").is_none(),
        "the v1 one-item page is one flat item: {exact}"
    );

    // Each section stream is exactly its two sentences, and every item cites its own section.
    for (handle, section_id, sentences) in [
        (
            alpha.clone(),
            ALPHA,
            ["Alpha sentence one.", "Alpha sentence two."],
        ),
        (
            beta.clone(),
            BETA,
            ["Beta sentence one.", "Beta sentence two."],
        ),
    ] {
        let items = section_items(&client, &handle, 4000).await;
        let texts = items
            .iter()
            .map(|(_, text)| text.as_str())
            .collect::<Vec<_>>();
        assert_eq!(texts, sentences, "the section stream: {items:?}");
        for (sentence, _) in items {
            assert_eq!(
                locator_of(&client, &sentence).await["owner_section_id"],
                section_id,
                "{sentence} is owned by {section_id}"
            );
        }
    }

    // search: the Beta hit names the v1 section and reads at the match.
    let searched = payload(
        &call(
            &client,
            "search",
            json!({ "doc": doc, "query": "Beta sentence one", "limit": 10 }),
        )
        .await,
    );
    assert_eq!(searched["total"], 1, "{searched}");
    let hit = searched["hits"][0][0].as_str().expect("sentence handle");
    assert_eq!(section_of(hit), beta);
    assert_eq!(searched["sections"][section_of(hit)], "Beta");
    assert_eq!(locator_of(&client, hit).await["owner_section_id"], BETA);
    let search_read = payload(&v2_read(&client, hit, json!({})).await);
    assert_eq!(flat_text(&search_read), "Beta sentence one.");

    let identity_before = identity_of(&client, &doc).await;
    client
        .cancel()
        .await
        .expect("first HTTP client session should close cleanly");
    server.kill().await.expect("first HTTP server should stop");

    let mut restarted = http_server_command(port, directory.path(), state.path())
        .spawn()
        .expect("restarted HTTP server should start");
    wait_until_ready(&base, &mut restarted).await;
    let resumed_client = start_http_client(&base).await;

    // Reopening after the restart keeps the same document identity and the same handles.
    let reopened = v2_open(&resumed_client, &document_path).await;
    assert_eq!(
        reopened["doc"], doc,
        "the restart keeps the document handle"
    );
    let identity = identity_of(&resumed_client, &doc).await;
    assert_eq!(
        identity["normalized_document_hash"], identity_before["normalized_document_hash"],
        "the restart keeps the normalized identity"
    );

    // The v1 anchored resume: the persisted continuation reads the next sentence, exclusively.
    let resumed = payload(&v2_read(&resumed_client, &next, json!({})).await);
    assert!(
        resumed.get("items").is_none(),
        "the v1 one-item page is one flat item: {resumed}"
    );
    assert_eq!(
        flat_text(&resumed),
        "Alpha sentence two.",
        "the persisted next resumes the stream: {resumed}"
    );
    assert_eq!(
        locator_of(&resumed_client, &first_sentence).await,
        first_locator,
        "the persisted first sentence still cites the v1 locator"
    );

    resumed_client
        .cancel()
        .await
        .expect("resumed HTTP client session should close cleanly");
    restarted
        .kill()
        .await
        .expect("restarted HTTP server should stop");
}
