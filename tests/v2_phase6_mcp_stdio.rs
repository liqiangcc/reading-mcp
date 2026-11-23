//! V2-4b PR-2, §5: ports `tests/phase6_mcp_stdio.rs` — the end-to-end stdio reading flow and the
//! restart persistence of the same book — to the seven v2 tools.
//!
//! The v1 test drove `list_documents` → `open_document` → `get_document_structure` →
//! `get_text_units` (two pages) → `search_document` → `get_context` → `read_document`; the v2
//! journey is `browse` → `open` → `outline` → `read` (one item, then `next`) → `search` → `read`
//! from the section handle. v1 response fields with no v2 name (`source`, `section://` ids,
//! `parent_id`, stream indexes, `complete`, cursors, the section window) are dropped per §2.1
//! item 3; the v1 locators they implied are asserted through `cite`.

#[path = "support/v2_mcp.rs"]
mod v2_mcp;

use serde_json::json;

use v2_mcp::{
    call, cite_one, flat_text, identity_of, locator_of, payload, section_items, section_of,
    start_client, v2_open, v2_read,
};

#[tokio::test]
async fn v2_stdio_client_completes_the_real_reading_tool_flow() {
    let directory = tempfile::tempdir().expect("temporary document directory should be created");
    let state = tempfile::tempdir().expect("temporary state directory should be created");
    let document_path = directory.path().join("operating-systems.md");
    tokio::fs::write(
        &document_path,
        r#"# Operating Systems

A compact operating systems study guide.

## Virtual Memory

Address spaces give each process an isolated view of memory.

Page replacement algorithms decide which resident page should be evicted.

### Page Tables

Page table entries map virtual pages to physical frames.

## Processes

Processes own resources and execution state.
"#,
    )
    .await
    .expect("Markdown fixture should be written");
    tokio::fs::write(directory.path().join("appendix.md"), "Appendix")
        .await
        .expect("second Markdown fixture should be written");

    let client = start_client(directory.path(), state.path()).await;

    // tools/list: the seven v2 tools (the v1 test pinned the nine v1 names).
    let mut tool_names = client
        .list_all_tools()
        .await
        .expect("tools/list should succeed")
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

    // browse: the two documents in name order. v1 discovered them one page at a time; `browse`
    // answers the directory once, so the two v1 pages are one v2 listing.
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
            ("operating-systems.md".to_owned(), "doc".to_owned())
        ],
        "the v1 listing pages are the one browse answer: {listing}"
    );

    let opened = v2_open(&client, &document_path).await;
    assert_eq!(opened["title"], "Operating Systems");
    assert_eq!(opened["format"], "markdown");
    assert_eq!(opened["sections"], 4, "four sections: {opened}");
    let doc = opened["doc"].as_str().expect("doc handle").to_owned();
    assert!(doc.starts_with('d'), "the doc handle is short: {doc}");

    // v1 `content_hash` on the open response; v2 carries the identity on `cite`.
    let identity = identity_of(&client, &doc).await;
    assert!(
        identity["content_hash"]
            .as_str()
            .expect("content hash")
            .starts_with("sha256:")
    );

    // outline: the same four sections, in preorder, with the v1 levels. v2 carries the tree as
    // levels over a preorder listing, the shape of the v1 `parent_id`/`children` tree.
    let outline = payload(
        &call(
            &client,
            "outline",
            json!({ "at": doc, "depth": 4, "limit": 100 }),
        )
        .await,
    );
    let sections = outline["items"].as_array().expect("outline items");
    assert_eq!(sections.len(), 4, "one root, two children, one grandchild");
    let titles = sections
        .iter()
        .map(|section| section[1].as_str().expect("title"))
        .collect::<Vec<_>>();
    assert_eq!(
        titles,
        vec![
            "Operating Systems",
            "Virtual Memory",
            "Page Tables",
            "Processes"
        ]
    );
    let levels = sections
        .iter()
        .map(|section| section[2].as_u64().expect("level"))
        .collect::<Vec<_>>();
    assert_eq!(levels, vec![1, 2, 3, 2]);
    assert_eq!(
        sections[0][0],
        format!("{doc}.s0"),
        "the root section is the document's first section"
    );
    let section_handles = sections
        .iter()
        .map(|section| section[0].as_str().expect("handle").to_owned())
        .collect::<Vec<_>>();
    // v2 lists the tree in preorder with levels, so the parent of a row is the nearest preceding
    // row one level up: the v1 `children`/`parent_id` shape, with the same two children.
    assert_eq!(parent_title(sections, 0), None);
    assert_eq!(
        parent_title(sections, 1).as_deref(),
        Some("Operating Systems")
    );
    assert_eq!(parent_title(sections, 2).as_deref(), Some("Virtual Memory"));
    assert_eq!(
        parent_title(sections, 3).as_deref(),
        Some("Operating Systems")
    );
    let children = (0..sections.len())
        .filter(|index| parent_title(sections, *index).as_deref() == Some("Operating Systems"))
        .map(|index| titles[index])
        .collect::<Vec<_>>();
    assert_eq!(
        children,
        vec!["Virtual Memory", "Processes"],
        "the root has the same two children as v1"
    );

    // The v1 `section://` ids of the tree: `cite` names the same owners.
    assert_eq!(
        locator_of(&client, &section_handles[0]).await["owner_section_id"],
        "section://operating-systems"
    );
    assert_eq!(
        locator_of(&client, &section_handles[1]).await["owner_section_id"],
        "section://operating-systems/virtual-memory"
    );

    // read: the v1 first sentence page. A section handle reads at its own first sentence.
    let vm = section_handles[1].clone();
    let first = payload(&v2_read(&client, &vm, json!({})).await);
    assert!(
        first.get("items").is_none(),
        "the v1 one-item page is one flat item: {first}"
    );
    assert_eq!(
        first["text"], "Address spaces give each process an isolated view of memory.",
        "{first}"
    );
    assert_eq!(first["section"]["s"], vm, "the item starts its section");
    assert_eq!(first["section"]["title"], "Virtual Memory");
    let sentence = first["at"]
        .as_str()
        .expect("the item's own handle")
        .to_owned();
    let locator = locator_of(&client, &sentence).await;
    assert_eq!(locator["paragraph_index"], 1);
    assert_eq!(locator["sentence_index"], 1);
    assert_eq!(
        locator["owner_section_id"], "section://operating-systems/virtual-memory",
        "the v1 section id is the cited owner"
    );

    // The v1 second page: the continuation is the `next` handle, and it stays in the section.
    let next = first["next"]
        .as_str()
        .expect("the first page continues")
        .to_owned();
    let second = payload(&v2_read(&client, &next, json!({})).await);
    assert_eq!(
        second["text"], "Page replacement algorithms decide which resident page should be evicted.",
        "{second}"
    );
    assert!(
        second.get("section").is_none(),
        "the second sentence is not a section start: {second}"
    );

    // The section's sentence stream is exactly those two items: the batch names the next section
    // at index 2, which is the v1 `total_items` and the end of `section_complete`.
    let batch = payload(&v2_read(&client, &vm, json!({ "max_chars": 4000 })).await);
    let items = section_items(&client, &vm, 4000).await;
    assert_eq!(
        items,
        vec![
            (sentence.clone(), flat_text(&first)),
            (next.clone(), flat_text(&second))
        ],
        "the two v1 pages rebuild the section stream: {batch}"
    );
    assert!(
        batch.get("next").is_none(),
        "the whole section fits one page, so nothing is truncated: {batch}"
    );
    let boundaries = batch["sections"].as_array().expect("section boundaries");
    assert_eq!(boundaries[0]["at"], 0);
    assert_eq!(boundaries[0]["s"], vm);
    assert_eq!(boundaries[1]["at"], 2, "two items end the section: {batch}");
    let page_tables = boundaries[1]["s"].as_str().expect("section handle");
    assert_eq!(boundaries[1]["title"], "Page Tables");

    // search: the hit is the same sentence, named by the same v1 section.
    let searched = payload(
        &call(
            &client,
            "search",
            json!({ "doc": doc, "query": "replacement algorithms", "limit": 10 }),
        )
        .await,
    );
    let hits = searched["hits"].as_array().expect("hits");
    assert_eq!(searched["total"], 1, "one hit: {searched}");
    assert_eq!(hits.len(), 1);
    let hit = hits[0][0].as_str().expect("sentence handle");
    assert_eq!(hit, next, "the hit is the continued sentence");
    assert!(
        hits[0][1]
            .as_str()
            .expect("snippet")
            .contains("replacement algorithms")
    );
    assert_eq!(
        searched["sections"][section_of(hit)],
        "Virtual Memory",
        "the hit section is named: {searched}"
    );
    assert_eq!(
        locator_of(&client, hit).await["owner_section_id"],
        "section://operating-systems/virtual-memory"
    );

    // The v1 `get_context` section window (the owner plus the following section) is the section
    // batch's named boundary plus that boundary's first item.
    assert_eq!(boundaries[0]["title"], "Virtual Memory");
    assert_eq!(boundaries[1]["title"], "Page Tables");
    let page_tables_item = payload(&v2_read(&client, page_tables, json!({})).await);
    assert_eq!(
        page_tables_item["text"], "Page table entries map virtual pages to physical frames.",
        "{page_tables_item}"
    );

    // read: the v1 whole-section read. The heading "### Page Tables" is the v2 section title, and
    // the body it renders is the section stream plus the next section's first item.
    assert!(
        flat_text(&first).contains("Address spaces give each process"),
        "{first}"
    );
    assert!(
        flat_text(&second).contains("Page replacement algorithms"),
        "{second}"
    );
    assert!(
        flat_text(&page_tables_item).contains("physical frames"),
        "{page_tables_item}"
    );
    assert_eq!(
        boundaries[1]["title"], "Page Tables",
        "the heading is the title"
    );
    assert!(
        sections.iter().any(|section| section[1] == "Page Tables"),
        "outline lists the heading: {outline}"
    );

    client
        .cancel()
        .await
        .expect("MCP child process should close cleanly");
}

#[tokio::test]
async fn v2_persistent_stdio_runtime_survives_server_restart() {
    let directory = tempfile::tempdir().expect("temporary directory should be created");
    let state_dir = directory.path().join("state");
    let document_path = directory.path().join("persistent.md");
    tokio::fs::write(
        &document_path,
        "# Persistent Book\n\n## Memory\n\nReplacement algorithms survive restart.\n",
    )
    .await
    .expect("persistent fixture should be written");

    let first_client = start_client(directory.path(), &state_dir).await;
    let opened = v2_open(&first_client, &document_path).await;
    let doc = opened["doc"].as_str().expect("doc handle").to_owned();
    first_client
        .cancel()
        .await
        .expect("first MCP process should close cleanly");

    let second_client = start_client(directory.path(), &state_dir).await;
    let read = payload(&v2_read(&second_client, &doc, json!({})).await);
    assert_eq!(
        flat_text(&read),
        "Replacement algorithms survive restart.",
        "the persisted document reads without reopening: {read}"
    );

    let searched = payload(
        &call(
            &second_client,
            "search",
            json!({ "doc": doc, "query": "replacement algorithms", "limit": 10 }),
        )
        .await,
    );
    assert_eq!(searched["total"], 1, "persistent FTS survives restart");
    let hit = searched["hits"][0][0].as_str().expect("sentence handle");
    assert_eq!(
        cite_one(&second_client, hit).await["locator"]["owner_section_id"],
        "section://persistent-book/memory"
    );

    second_client
        .cancel()
        .await
        .expect("second MCP process should close cleanly");
}

/// V2-4b PR-5: ports `tests/whole_document_reading.rs`'s reopen leg — after a restart the same
/// document answers the stored open result, and reading from it still works.
#[tokio::test]
async fn v2_reopening_after_restart_returns_the_stored_open_result() {
    let directory = tempfile::tempdir().expect("temporary directory should be created");
    let state_dir = directory.path().join("state");
    let document_path = directory.path().join("reopen.md");
    tokio::fs::write(
        &document_path,
        "# Reopen\n\nFirst sentence. Second sentence.\n",
    )
    .await
    .expect("reopen fixture should be written");

    let first_client = start_client(directory.path(), &state_dir).await;
    let opened = v2_open(&first_client, &document_path).await;
    let doc = opened["doc"].as_str().expect("doc handle").to_owned();
    let first_read = payload(&v2_read(&first_client, &doc, json!({})).await);
    first_client
        .cancel()
        .await
        .expect("first MCP process should close cleanly");

    let second_client = start_client(directory.path(), &state_dir).await;
    let reopened = v2_open(&second_client, &document_path).await;
    assert_eq!(
        reopened, opened,
        "the restart must answer the stored open result"
    );

    let read = payload(&v2_read(&second_client, &doc, json!({})).await);
    assert_eq!(
        flat_text(&read),
        "First sentence.",
        "the stored document still reads after the restart: {read}"
    );
    assert_eq!(
        read, first_read,
        "the same handle answers the same item before and after the restart"
    );
    // A v2 read is a pure function of `from`: asking again does not advance.
    let again = payload(&v2_read(&second_client, &doc, json!({})).await);
    assert_eq!(again, read, "a repeated read must not advance the stream");

    second_client
        .cancel()
        .await
        .expect("second MCP process should close cleanly");
}

/// The title of the nearest preceding outline row one level up: the parent in the preorder tree.
fn parent_title(sections: &[serde_json::Value], index: usize) -> Option<String> {
    let level = sections[index][2].as_u64()?;
    sections[..index]
        .iter()
        .rev()
        .find(|row| row[2].as_u64() == Some(level - 1))
        .map(|row| row[1].as_str().expect("title").to_owned())
}
