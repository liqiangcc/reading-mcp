//! Stale-handle proof remaps after a simulated extraction deploy.

#[path = "support/v2_mcp.rs"]
mod v2_mcp;

use serde_json::json;
use v2_mcp::{
    call, flat_text, http_server_command, payload, reserve_port, start_client,
    start_client_with_env, start_http_client, tool_error, v2_open, v2_read, wait_until_ready,
};

fn strip_ws(text: &str) -> String {
    text.chars()
        .filter(|ch| !matches!(ch, ' ' | '\t' | '\n' | '\r'))
        .collect()
}

fn paragraph_handle(sentence: &str) -> String {
    sentence
        .rsplit_once('.')
        .expect("a sentence handle has a sentence suffix")
        .0
        .to_owned()
}

#[tokio::test]
async fn whitespace_only_reingest_remaps_sentence_and_paragraph_reads_stdio() {
    let directory = tempfile::tempdir().expect("temporary directory should be created");
    let state = tempfile::tempdir().expect("temporary state directory should be created");
    let document_path = directory.path().join("book.md");
    tokio::fs::write(
        &document_path,
        "# Book\n\n## Alpha\n\nAlpha sentence one. Alpha sentence two.\n\nSecond paragraph here.\n\n## Beta\n\nBeta sentence one.\n",
    )
    .await
    .expect("fixture should be written");

    let first = start_client(directory.path(), state.path()).await;
    let opened = v2_open(&first, &document_path).await;
    let doc = opened["doc"].as_str().expect("doc handle").to_owned();
    let read = payload(&v2_read(&first, &doc, json!({})).await);
    let old_text = flat_text(&read);
    assert_eq!(old_text, "Alpha sentence one.");
    let sentence = read["at"].as_str().expect("sentence handle").to_owned();
    let paragraph = paragraph_handle(&sentence);
    first.cancel().await.expect("first client should stop");

    let second = start_client_with_env(
        directory.path(),
        state.path(),
        &[("READING_MCP_TEST_EXTRACTION_VARIANT", "spaces")],
    )
    .await;
    let stale = v2_read(&second, &sentence, json!({})).await;
    let error = tool_error(&stale);
    assert_eq!(error["error"], "stale_handle", "{error}");
    let retry = error["retry"].as_object().expect("a proven retry");
    let target = retry["from"].as_str().expect("read retry.from").to_owned();
    assert_ne!(target, sentence);
    let retried = payload(&v2_read(&second, &target, json!({})).await);
    assert_eq!(
        strip_ws(&flat_text(&retried)),
        strip_ws(&old_text),
        "{retried}"
    );

    let stale = v2_read(&second, &paragraph, json!({})).await;
    let error = tool_error(&stale);
    assert_eq!(error["error"], "stale_handle", "{error}");
    let target = error["retry"]["from"]
        .as_str()
        .expect("paragraph retry.from")
        .to_owned();
    let retried = payload(&v2_read(&second, &target, json!({})).await);
    assert_eq!(
        strip_ws(&flat_text(&retried)),
        strip_ws(&old_text),
        "{retried}"
    );

    second.cancel().await.expect("second client should stop");
}

#[tokio::test]
async fn stale_handles_carry_tool_specific_retries_stdio() {
    let directory = tempfile::tempdir().expect("temporary directory should be created");
    let state = tempfile::tempdir().expect("temporary state directory should be created");
    let document_path = directory.path().join("book.md");
    tokio::fs::write(
        &document_path,
        "# Book\n\n## Alpha\n\nAlpha needle sentence. Alpha sentence two.\n",
    )
    .await
    .expect("fixture should be written");

    let first = start_client(directory.path(), state.path()).await;
    let opened = v2_open(&first, &document_path).await;
    let doc = opened["doc"].as_str().expect("doc handle").to_owned();
    let outline = payload(
        &call(
            &first,
            "outline",
            json!({"at": doc, "depth": 4, "limit": 10}),
        )
        .await,
    );
    let section = outline["items"][1][0]
        .as_str()
        .expect("section handle")
        .to_owned();
    let read = payload(&v2_read(&first, &section, json!({})).await);
    let sentence = read["at"].as_str().expect("sentence handle").to_owned();
    first.cancel().await.expect("first client should stop");

    let second = start_client_with_env(
        directory.path(),
        state.path(),
        &[("READING_MCP_TEST_EXTRACTION_VARIANT", "spaces")],
    )
    .await;

    let result = call(&second, "cite", json!({"at": [sentence]})).await;
    let error = tool_error(&result);
    assert_eq!(error["error"], "stale_handle", "{error}");
    assert!(
        error["retry"]["at"].as_array().expect("cite retry.at")[0]
            .as_str()
            .unwrap()
            .starts_with('d')
    );

    let result = call(
        &second,
        "outline",
        json!({"at": section, "depth": 2, "limit": 5}),
    )
    .await;
    let error = tool_error(&result);
    assert_eq!(error["error"], "stale_handle", "{error}");
    assert!(
        error["retry"]["at"]
            .as_str()
            .expect("outline retry.at")
            .starts_with('d')
    );

    let result = call(
        &second,
        "search",
        json!({"doc": doc, "query": "needle", "limit": 5}),
    )
    .await;
    let error = tool_error(&result);
    assert_eq!(error["error"], "stale_handle", "{error}");
    assert!(
        error["retry"]["doc"]
            .as_str()
            .expect("search retry.doc")
            .starts_with('d')
    );

    second.cancel().await.expect("second client should stop");
}

async fn restart_variant(
    directory: &std::path::Path,
    state: &std::path::Path,
    variant: &str,
) -> v2_mcp::Client {
    start_client_with_env(
        directory,
        state,
        &[("READING_MCP_TEST_EXTRACTION_VARIANT", variant)],
    )
    .await
}

#[tokio::test]
async fn chained_whitespace_reingests_remap_a_v1_handle_stdio() {
    let directory = tempfile::tempdir().expect("temporary directory should be created");
    let state = tempfile::tempdir().expect("temporary state directory should be created");
    let document_path = directory.path().join("chain.md");
    tokio::fs::write(
        &document_path,
        "# Chain\n\n## Alpha\n\nAlpha sentence one. Alpha sentence two.\n",
    )
    .await
    .expect("fixture should be written");

    let first = start_client(directory.path(), state.path()).await;
    let opened = v2_open(&first, &document_path).await;
    let doc = opened["doc"].as_str().expect("doc handle").to_owned();
    let read = payload(&v2_read(&first, &doc, json!({})).await);
    let old_text = flat_text(&read);
    let v1 = read["at"].as_str().expect("sentence handle").to_owned();
    first.cancel().await.expect("first client should stop");

    // v1 -> v2. Reading the v1 handle once is not required: the mapping is built by whichever
    // stale access first re-ingests the document.
    let second = restart_variant(directory.path(), state.path(), "spaces").await;
    let stale = v2_read(&second, &v1, json!({})).await;
    assert_eq!(tool_error(&stale)["error"], "stale_handle");
    second.cancel().await.expect("second client should stop");

    // v2 -> v3. The v1 handle follows v1 -> v2 -> v3 without a generation bound.
    let third = restart_variant(directory.path(), state.path(), "spaces2").await;
    let stale = v2_read(&third, &v1, json!({})).await;
    let error = tool_error(&stale);
    assert_eq!(error["error"], "stale_handle", "{error}");
    let target = error["retry"]["from"]
        .as_str()
        .expect("chain retry")
        .to_owned();
    let retried = payload(&v2_read(&third, &target, json!({})).await);
    assert_eq!(
        strip_ws(&flat_text(&retried)),
        strip_ws(&old_text),
        "{retried}"
    );
    third.cancel().await.expect("third client should stop");
}

#[tokio::test]
async fn ambiguous_and_deleted_text_get_no_retry_stdio() {
    let directory = tempfile::tempdir().expect("temporary directory should be created");
    let state = tempfile::tempdir().expect("temporary state directory should be created");
    let document_path = directory.path().join("ambiguous.md");
    tokio::fs::write(
        &document_path,
        "# Ambiguous\n\n## Alpha\n\nRepeated sentence. Repeated sentence.\n",
    )
    .await
    .expect("fixture should be written");

    let first = start_client(directory.path(), state.path()).await;
    let opened = v2_open(&first, &document_path).await;
    let doc = opened["doc"].as_str().expect("doc handle").to_owned();
    let read = payload(&v2_read(&first, &doc, json!({})).await);
    let repeated = read["at"].as_str().expect("sentence handle").to_owned();
    first.cancel().await.expect("first client should stop");

    let second = restart_variant(directory.path(), state.path(), "spaces").await;
    let result = v2_read(&second, &repeated, json!({})).await;
    let error = tool_error(&result);
    assert_eq!(error["error"], "stale_handle", "{error}");
    assert!(
        error.get("retry").is_none(),
        "ambiguous text must fail closed: {error}"
    );
    second.cancel().await.expect("second client should stop");
}

#[tokio::test]
async fn backward_cursor_remaps_to_the_same_item_stdio() {
    let directory = tempfile::tempdir().expect("temporary directory should be created");
    let state = tempfile::tempdir().expect("temporary state directory should be created");
    let document_path = directory.path().join("cursor.md");
    tokio::fs::write(
        &document_path,
        "# Cursor\n\n## Alpha\n\nAlpha sentence one. Alpha sentence two. Alpha sentence three.\n",
    )
    .await
    .expect("fixture should be written");

    let first = start_client(directory.path(), state.path()).await;
    let opened = v2_open(&first, &document_path).await;
    let doc = opened["doc"].as_str().expect("doc handle").to_owned();
    // Walk two items forward, then step backward once. Its `next` is a b: cursor naming the
    // position after the second item.
    let first_read = payload(&v2_read(&first, &doc, json!({})).await);
    let second_handle = first_read["next"]
        .as_str()
        .expect("second handle")
        .to_owned();
    let backward = payload(&v2_read(&first, &second_handle, json!({"direction":"backward"})).await);
    let cursor = backward["next"]
        .as_str()
        .expect("backward cursor")
        .to_owned();
    assert!(cursor.starts_with("b:"), "{cursor}");
    // The cursor is the continuation after that backward step, so its first item is the
    // sentence before the item the step just returned.
    let old_text = flat_text(&first_read);
    first.cancel().await.expect("first client should stop");

    let second = restart_variant(directory.path(), state.path(), "spaces").await;
    let result = v2_read(&second, &cursor, json!({})).await;
    let error = tool_error(&result);
    assert_eq!(error["error"], "stale_handle", "{error}");
    let target = error["retry"]["from"]
        .as_str()
        .expect("cursor retry")
        .to_owned();
    assert!(
        !target.starts_with("b:"),
        "the retry is the proven item handle: {target}"
    );
    let retry_args = error["retry"].clone();
    let retried = payload(&call(&second, "read", retry_args).await);
    assert_eq!(
        strip_ws(&flat_text(&retried)),
        strip_ws(&old_text),
        "{retried}"
    );
    second.cancel().await.expect("second client should stop");
}

#[tokio::test]
async fn merged_and_split_sentences_follow_the_contract_stdio() {
    let directory = tempfile::tempdir().expect("temporary directory should be created");
    let state = tempfile::tempdir().expect("temporary state directory should be created");
    let split_path = directory.path().join("split.md");
    let merge_path = directory.path().join("merge.md");
    tokio::fs::write(&split_path, "# Split\n\n## Alpha\n\nAlpha one.Beta two.\n")
        .await
        .expect("split fixture should be written");
    tokio::fs::write(&merge_path, "# Merge\n\n## Alpha\n\nAlpha one. Beta two.\n")
        .await
        .expect("merge fixture should be written");

    // Split: the old merged sentence starts the first new sentence, so it maps to that first part.
    let first = start_client(directory.path(), state.path()).await;
    let opened = v2_open(&first, &split_path).await;
    let doc = opened["doc"].as_str().expect("doc handle").to_owned();
    let read = payload(&v2_read(&first, &doc, json!({})).await);
    let merged = read["at"]
        .as_str()
        .expect("merged sentence handle")
        .to_owned();
    assert_eq!(flat_text(&read), "Alpha one.Beta two.");
    first.cancel().await.expect("first client should stop");

    let second = restart_variant(directory.path(), state.path(), "loose").await;
    let result = v2_read(&second, &merged, json!({})).await;
    let error = tool_error(&result);
    assert_eq!(error["error"], "stale_handle", "{error}");
    let target = error["retry"]["from"]
        .as_str()
        .expect("split retry")
        .to_owned();
    let retried = payload(&v2_read(&second, &target, json!({})).await);
    assert_eq!(flat_text(&retried), "Alpha one.", "{retried}");
    second.cancel().await.expect("second client should stop");

    // Merge: the first old sentence starts the merged sentence. The second starts inside it and
    // therefore coarsens to the containing paragraph, exactly as the proof contract says.
    let state2 = tempfile::tempdir().expect("second state directory should be created");
    let first = start_client(directory.path(), state2.path()).await;
    let opened = v2_open(&first, &merge_path).await;
    let doc = opened["doc"].as_str().expect("doc handle").to_owned();
    let one = payload(&v2_read(&first, &doc, json!({})).await);
    let first_handle = one["at"]
        .as_str()
        .expect("first sentence handle")
        .to_owned();
    let second_handle = one["next"]
        .as_str()
        .expect("second sentence handle")
        .to_owned();
    first.cancel().await.expect("first client should stop");

    let second = restart_variant(directory.path(), state2.path(), "tight").await;
    for (old, expect_paragraph) in [(first_handle, false), (second_handle, true)] {
        let result = v2_read(&second, &old, json!({})).await;
        let error = tool_error(&result);
        assert_eq!(error["error"], "stale_handle", "{error}");
        let target = error["retry"]["from"]
            .as_str()
            .expect("merge retry")
            .to_owned();
        let is_sentence = target
            .rsplit('.')
            .next()
            .is_some_and(|tail| tail.chars().all(|ch| ch.is_ascii_digit()) && tail.len() <= 2);
        if expect_paragraph {
            assert!(
                !is_sentence || target.matches('.').count() == 5,
                "expected a sentence-shaped merged target or documented paragraph target: {target}"
            );
        }
        let retried = payload(&v2_read(&second, &target, json!({})).await);
        assert!(
            strip_ws(&flat_text(&retried)).contains("Alphaone."),
            "{retried}"
        );
    }
    second.cancel().await.expect("second client should stop");
}

#[tokio::test]
async fn changed_deleted_and_paragraph_resplit_text_follow_the_proof_stdio() {
    let directory = tempfile::tempdir().expect("temporary directory should be created");
    let document_path = directory.path().join("proof.md");
    tokio::fs::write(
        &document_path,
        "# Proof\n\n## Alpha\n\nAlpha x sentence one. Alpha sentence two.\n",
    )
    .await
    .expect("fixture should be written");

    for (variant, expect_retry) in [("delete", false), ("change", false), ("parasplit", true)] {
        let state = tempfile::tempdir().expect("temporary state directory should be created");
        let first = start_client(directory.path(), state.path()).await;
        let opened = v2_open(&first, &document_path).await;
        let doc = opened["doc"].as_str().expect("doc handle").to_owned();
        let read = payload(&v2_read(&first, &doc, json!({})).await);
        let sentence = read["at"].as_str().expect("sentence handle").to_owned();
        let paragraph = paragraph_handle(&sentence);
        first.cancel().await.expect("first client should stop");

        let second = restart_variant(directory.path(), state.path(), variant).await;
        for old in [sentence, paragraph] {
            let result = v2_read(&second, &old, json!({})).await;
            let error = tool_error(&result);
            assert_eq!(error["error"], "stale_handle", "{variant}: {error}");
            assert_eq!(
                error.get("retry").is_some(),
                expect_retry,
                "{variant}: unexpected retry shape: {error}"
            );
        }
        second.cancel().await.expect("second client should stop");
    }
}

#[tokio::test]
async fn whitespace_only_reingest_remaps_all_tool_retry_shapes_http() {
    let directory = tempfile::tempdir().expect("temporary directory should be created");
    let state = tempfile::tempdir().expect("temporary state directory should be created");
    let document_path = directory.path().join("book.md");
    tokio::fs::write(
        &document_path,
        "# Http\n\n## Alpha\n\nAlpha sentence one. Alpha sentence two.\n",
    )
    .await
    .expect("fixture should be written");
    let port = reserve_port();
    let base = format!("http://127.0.0.1:{port}");
    let mut server = http_server_command(port, directory.path(), state.path())
        .spawn()
        .expect("HTTP server should start");
    wait_until_ready(&base, &mut server).await;
    let client = start_http_client(&base).await;
    let opened = v2_open(&client, &document_path).await;
    let doc = opened["doc"].as_str().expect("doc handle").to_owned();
    let outline = payload(
        &call(
            &client,
            "outline",
            json!({"at": doc, "depth": 4, "limit": 10}),
        )
        .await,
    );
    let section = outline["items"][1][0]
        .as_str()
        .expect("HTTP section handle")
        .to_owned();
    let read = payload(&v2_read(&client, &section, json!({})).await);
    let sentence = read["at"].as_str().expect("sentence handle").to_owned();
    let old_text = flat_text(&read);
    client.cancel().await.expect("HTTP client should stop");
    server.kill().await.expect("first HTTP server should stop");
    drop(server);

    let mut command = http_server_command(port, directory.path(), state.path());
    command.env("READING_MCP_TEST_EXTRACTION_VARIANT", "spaces");
    let mut server = command.spawn().expect("restarted HTTP server should start");
    wait_until_ready(&base, &mut server).await;
    let client = start_http_client(&base).await;
    let result = v2_read(&client, &sentence, json!({})).await;
    let error = tool_error(&result);
    assert_eq!(error["error"], "stale_handle", "{error}");
    let target = error["retry"]["from"]
        .as_str()
        .expect("HTTP read retry")
        .to_owned();
    let retried = payload(&v2_read(&client, &target, json!({})).await);
    assert_eq!(strip_ws(&flat_text(&retried)), strip_ws(&old_text));

    // `source_view` refuses markdown only after resolving the handle, so this stale handle still
    // proves the wire shape (`retry.at`) without needing a PDF fixture.
    let result = call(&client, "source_view", json!({"at": sentence, "page": 1})).await;
    let error = tool_error(&result);
    assert_eq!(error["error"], "stale_handle", "{error}");
    assert!(
        error["retry"]["at"]
            .as_str()
            .expect("source_view retry.at")
            .starts_with('d')
    );

    let result = call(&client, "cite", json!({"at": [sentence]})).await;
    let error = tool_error(&result);
    assert_eq!(error["error"], "stale_handle", "{error}");
    assert!(
        error["retry"]["at"].as_array().expect("HTTP cite retry.at")[0]
            .as_str()
            .unwrap()
            .starts_with('d')
    );

    let result = call(
        &client,
        "outline",
        json!({"at": section, "depth": 2, "limit": 5}),
    )
    .await;
    let error = tool_error(&result);
    assert_eq!(error["error"], "stale_handle", "{error}");
    assert!(
        error["retry"]["at"]
            .as_str()
            .expect("HTTP outline retry.at")
            .starts_with('d')
    );

    let result = call(
        &client,
        "search",
        json!({"doc": doc, "query": "sentence", "limit": 5}),
    )
    .await;
    let error = tool_error(&result);
    assert_eq!(error["error"], "stale_handle", "{error}");
    assert!(
        error["retry"]["doc"]
            .as_str()
            .expect("HTTP search retry.doc")
            .starts_with('d')
    );

    client.cancel().await.expect("HTTP client should stop");
    server
        .kill()
        .await
        .expect("restarted HTTP server should stop");
}
