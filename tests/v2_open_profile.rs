//! V2-4b PR-1, §2 row 8: re-covers `tests/open_reading_profile_stdio.rs` — the `open` response
//! carries the reading profile — against v2 `open`.
//!
//! The v1 test also pinned the nine-tool surface; the v2 equivalent (`tools/list` is exactly the
//! seven v2 tools) is already covered by `tests/v2_open_read.rs::the_default_tool_set_is_the_seven_v2_tools`,
//! so this file only covers the profile.
//!
//! v2 gap: `open` answers with `OpenView` (`doc`, `title`, `format`, `sections`, `chars`, `top`,
//! `more_top`) and no `reading_profile` field, although the plan (§2 row 8) says v2 `open` returns
//! it. Product code is not changed here; the test is ignored until v2 surfaces the profile.

#[path = "support/v2_mcp.rs"]
mod v2_mcp;

use serde_json::json;

use v2_mcp::{start_client, v2_open};

#[tokio::test]
#[ignore = "v2 gap: open returns OpenView with no reading_profile field"]
async fn open_returns_the_reading_profile() {
    let directory = tempfile::tempdir().expect("temporary directory should be created");
    let path = directory.path().join("profile.md");
    tokio::fs::write(&path, "# Profile\n\nFirst sentence. Second sentence.\n")
        .await
        .expect("fixture should be written");
    let client = start_client(directory.path(), &directory.path().join("state")).await;

    let opened = v2_open(&client, &path).await;
    let profile = &opened["reading_profile"];
    assert!(
        profile.is_object(),
        "open carries the reading profile: {opened}"
    );

    assert_eq!(profile["schema_version"], "reading-profile/v1");
    assert_eq!(
        profile["capabilities"]["paragraph_enumeration"]["availability"],
        "available"
    );
    assert_eq!(
        profile["capabilities"]["paragraph_enumeration"]["segmentation_version"],
        "text-segmentation/v3"
    );
    assert_eq!(
        profile["capabilities"]["lexical_search"]["availability"],
        "available"
    );
    assert_eq!(
        profile["capabilities"]["lexical_search"]["precise_candidates"],
        json!(true)
    );

    let coverage = &profile["canonical_text_coverage"];
    let owner = coverage["owner_chars"].as_u64().expect("owner chars");
    assert!(owner > 0, "the profile counts the owner text: {coverage}");
    assert_eq!(
        coverage["paragraph_chars"].as_u64().unwrap()
            + coverage["paragraph_separator_chars"].as_u64().unwrap(),
        owner,
        "paragraph chars plus separators cover the owner text: {coverage}"
    );
    assert_eq!(coverage["native_paragraph_chars"], 0);
    assert_eq!(coverage["native_structural_container_chars"], 0);
    assert_eq!(coverage["native_non_prose_chars"], 0);
    assert!(coverage["fallback_chars"].as_u64().unwrap() > 0);
    assert_eq!(coverage["coarse_paragraphs"], 0);
    assert_eq!(
        profile["capabilities"]["sentence_first_enumeration"]["source_preserving_coarse_regions"],
        json!(false)
    );

    let reliability = &profile["reliability"];
    assert_eq!(reliability["evidence"].as_array().unwrap().len(), 1);
    assert_eq!(reliability["evidence"][0]["integrity"], "not_applicable");
    assert!(reliability.get("publication_coverage").is_none());
    assert!(reliability.get("structure_provenance").is_none());
    assert!(reliability.get("navigation_resolution").is_none());

    client.cancel().await.expect("client should close cleanly");
}
