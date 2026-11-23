//! V2-4b PR-1, §2 row 5: re-covers the kept slice of `tests/normalized_text_contract_mcp.rs` —
//! the normalized-document hash and its version fields — from the v2 payload. The v1 test read
//! them from the `open_document` response and compared the read stream's coordinate space; v2
//! `open` has no hash fields and no coordinate-space field (§2.1 item 3 drops both), so the kept
//! identity is the one `cite` returns: `doc.content_hash`, `doc.normalized_document_hash`,
//! `doc.norm`, `doc.ndh` and `doc.seg`. The second test is the normalized-hash slice: the same
//! bytes keep the identity across opens, changed bytes move it.

#[path = "support/v2_mcp.rs"]
mod v2_mcp;

use reading_mcp::domain::{
    NORMALIZATION_VERSION, NORMALIZED_DOCUMENT_HASH_VERSION, TEXT_SEGMENTATION_VERSION,
};
use serde_json::json;

use v2_mcp::{call, payload, start_client, v2_open};

#[tokio::test]
async fn cite_carries_the_document_identity_and_its_version_fields() {
    let directory = tempfile::tempdir().expect("temporary directory should be created");
    let path = directory.path().join("normalized.md");
    tokio::fs::write(&path, "# Normalized\n\nExact persisted text: A中🙂Z.\n")
        .await
        .expect("fixture should be written");
    let client = start_client(directory.path(), &directory.path().join("state")).await;

    let opened = v2_open(&client, &path).await;
    let doc = opened["doc"].as_str().expect("doc handle").to_owned();

    // A document handle cites the document identity, without a unit locator.
    let cited = payload(&call(&client, "cite", json!({ "at": [doc.clone()] })).await);
    let item = &cited["items"][0];
    assert!(
        item.get("locator").is_none(),
        "a document handle has no unit locator: {cited}"
    );
    let identity = &item["doc"];
    assert!(
        identity["id"].as_str().is_some(),
        "the identity names the document: {cited}"
    );
    let content_hash = identity["content_hash"].as_str().expect("content hash");
    let normalized_hash = identity["normalized_document_hash"]
        .as_str()
        .expect("normalized document hash");
    assert!(
        content_hash.starts_with("sha256:"),
        "the content hash is a sha256: {content_hash}"
    );
    assert!(
        normalized_hash.starts_with("sha256:"),
        "the normalized document hash is a sha256: {normalized_hash}"
    );
    assert_ne!(
        content_hash, normalized_hash,
        "raw and normalized identity stay distinct"
    );
    assert_eq!(identity["norm"], NORMALIZATION_VERSION);
    assert_eq!(identity["ndh"], NORMALIZED_DOCUMENT_HASH_VERSION);
    assert_eq!(identity["seg"], TEXT_SEGMENTATION_VERSION);

    // A sentence locator is bound to exactly the document identity the document handle cited.
    let read = payload(&call(&client, "read", json!({ "from": doc })).await);
    let sentence = read["at"]
        .as_str()
        .expect("the first sentence's own handle")
        .to_owned();
    let cited = payload(&call(&client, "cite", json!({ "at": [sentence] })).await);
    let item = &cited["items"][0];
    let locator = &item["locator"];
    assert_eq!(locator["document_id"], identity["id"], "{cited}");
    assert_eq!(locator["content_hash"], content_hash, "{cited}");
    assert_eq!(
        locator["normalized_document_hash"], normalized_hash,
        "the locator carries the document's normalized hash: {cited}"
    );
    assert_eq!(
        locator["segmentation_version"], TEXT_SEGMENTATION_VERSION,
        "{cited}"
    );
    assert!(
        locator["normalized_range"]["end"].as_u64().is_some(),
        "the locator carries its normalized range: {cited}"
    );
    assert_eq!(
        item["doc"], *identity,
        "the cited item repeats its identity"
    );

    client.cancel().await.expect("client should close cleanly");
}

#[tokio::test]
async fn normalized_identity_is_stable_across_opens_and_moves_with_the_bytes() {
    let directory = tempfile::tempdir().expect("temporary directory should be created");
    let path = directory.path().join("normalized.md");
    tokio::fs::write(&path, "# Normalized\n\nStable normalized text.\n")
        .await
        .expect("fixture should be written");
    let client = start_client(directory.path(), &directory.path().join("state")).await;

    let first = identity_of(&client, &path).await;
    let again = identity_of(&client, &path).await;
    assert_eq!(
        again, first,
        "the same bytes must keep the same handle and identity"
    );

    // Different bytes: a new document identity and a different normalized hash.
    tokio::fs::write(&path, "# Normalized\n\nChanged normalized text, longer.\n")
        .await
        .expect("fixture should be rewritten");
    let changed = identity_of(&client, &path).await;
    assert_ne!(
        changed.0, first.0,
        "changed bytes must mint a different document handle"
    );
    assert_ne!(
        changed.1, first.1,
        "changed bytes must change the raw content hash"
    );
    assert_ne!(
        changed.2, first.2,
        "changed bytes must change the normalized document hash"
    );

    client.cancel().await.expect("client should close cleanly");
}

/// `(handle, content_hash, normalized_document_hash)` from `open` + `cite`.
async fn identity_of(client: &v2_mcp::Client, path: &std::path::Path) -> (String, String, String) {
    let doc = v2_open(client, path).await["doc"]
        .as_str()
        .expect("doc handle")
        .to_owned();
    let cited = payload(&call(client, "cite", json!({ "at": [doc.clone()] })).await);
    let identity = &cited["items"][0]["doc"];
    (
        doc,
        identity["content_hash"]
            .as_str()
            .expect("content hash")
            .to_owned(),
        identity["normalized_document_hash"]
            .as_str()
            .expect("normalized document hash")
            .to_owned(),
    )
}
