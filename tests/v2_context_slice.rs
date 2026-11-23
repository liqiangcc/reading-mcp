//! V2-4b PR-1, §2 row 2: re-covers the kept slice of `tests/context_granularity_stdio.rs` —
//! `get_context` consuming a text locator — as `cite` → `read{"from":<cited handle>}` against the
//! same locator. The tagged parent/children/ancestor/neighbour relations are dropped by §2.1
//! item 1: no v2 tool returns structural relations, so this test does not ask for them. The
//! paragraph container is still reachable through the paragraph handle that `read` accepts.

#[path = "support/v2_mcp.rs"]
mod v2_mcp;

use serde_json::json;

use v2_mcp::{call, flat_text, payload, read_items, start_client, v2_open};

#[tokio::test]
async fn cite_then_read_at_the_cited_locator_keeps_the_context_slice() {
    let directory = tempfile::tempdir().expect("temporary directory should be created");
    let path = directory.path().join("context.md");
    tokio::fs::write(
        &path,
        "# Book\n\n## Topic\n\nFirst sentence. Second sentence.\n\nThird paragraph sentence.\n",
    )
    .await
    .expect("fixture should be written");
    let client = start_client(directory.path(), &directory.path().join("state")).await;

    let doc = v2_open(&client, &path).await["doc"]
        .as_str()
        .expect("doc handle")
        .to_owned();
    let outline = payload(
        &call(
            &client,
            "outline",
            json!({ "at": doc, "depth": 3, "limit": 100 }),
        )
        .await,
    );
    let topic = outline["items"]
        .as_array()
        .expect("outline items")
        .iter()
        .find(|item| item[1] == "Topic")
        .expect("the Topic section is listed")[0]
        .as_str()
        .expect("section handle")
        .to_owned();

    // The v1 `get_text_units` sentence enumeration of the Topic section: three sentences.
    let units = payload(&call(&client, "read", json!({ "from": topic, "max_chars": 4000 })).await);
    let sentences = read_items(&units, &topic);
    let texts: Vec<&str> = sentences.iter().map(|(_, text)| text.as_str()).collect();
    assert_eq!(
        texts,
        [
            "First sentence.",
            "Second sentence.",
            "Third paragraph sentence."
        ],
        "the section reads as its v1 sentence stream: {units}"
    );

    // The anchor is the second sentence; `cite` returns the v1 locator of that exact unit.
    let anchor = sentences[1].0.clone();
    let cited = payload(&call(&client, "cite", json!({ "at": [anchor.clone()] })).await);
    let item = &cited["items"][0];
    assert_eq!(item["at"], anchor, "cite echoes the cited handle");
    let locator = &item["locator"];
    assert_eq!(
        locator["owner_section_id"], "section://book/topic",
        "the locator names the v1 section: {locator}"
    );
    assert_eq!(
        locator["paragraph_index"], 1,
        "the locator names paragraph 1"
    );
    assert_eq!(locator["sentence_index"], 2, "the locator names sentence 2");
    assert!(
        locator["normalized_range"]["start"].as_u64().is_some(),
        "the locator carries its normalized range: {locator}"
    );

    // The kept slice: `read` consumes the cited locator's handle and reads that exact sentence.
    let at = item["at"].as_str().expect("the cited handle");
    let read = payload(&call(&client, "read", json!({ "from": at })).await);
    assert_eq!(flat_text(&read), "Second sentence.");
    assert!(
        read.get("at").is_none(),
        "the cited handle is the item's own handle, so `at` stays absent: {read}"
    );

    // The container slice: the paragraph handle is the sentence handle without its sentence part.
    let paragraph = anchor
        .rsplit_once('.')
        .map(|(head, _)| head)
        .expect("a sentence handle has a sentence part")
        .to_owned();
    let container = payload(
        &call(
            &client,
            "read",
            json!({ "from": paragraph, "unit": "paragraph" }),
        )
        .await,
    );
    assert_eq!(
        flat_text(&container),
        "First sentence. Second sentence.",
        "the paragraph container is still readable"
    );
    let paragraph_cite = payload(&call(&client, "cite", json!({ "at": [paragraph] })).await);
    assert_eq!(
        paragraph_cite["items"][0]["locator"]["paragraph_index"], 1,
        "the paragraph handle cites paragraph 1: {paragraph_cite}"
    );
    assert!(
        paragraph_cite["items"][0]["locator"]["sentence_index"].is_null(),
        "a paragraph locator names no sentence: {paragraph_cite}"
    );

    // The section locator is the v1 section locator, and the section handle reads its first item.
    let section_cite = payload(&call(&client, "cite", json!({ "at": [topic] })).await);
    assert!(
        section_cite["items"][0]["locator"]["paragraph_index"].is_null(),
        "a section locator names no paragraph: {section_cite}"
    );
    let from_section = payload(&call(&client, "read", json!({ "from": topic })).await);
    assert_eq!(
        flat_text(&from_section),
        "First sentence.",
        "a section handle reads at its first sentence"
    );

    client.cancel().await.expect("client should close cleanly");
}
