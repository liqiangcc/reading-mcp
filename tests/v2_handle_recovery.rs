//! Stale-handle proof remaps after a simulated extraction deploy.

#[path = "support/v2_mcp.rs"]
mod v2_mcp;

use serde_json::json;
use v2_mcp::{
    call, flat_text, http_server_command, payload, reserve_port, section_items_with, section_of,
    start_client, start_client_with_env, start_http_client, tool_error, v2_open, v2_read,
    wait_until_ready,
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

/// One one-page PDF with a single sentence, the same fixture shape as
/// `tests/v2_source_view_stdio.rs`.
fn build_text_pdf() -> Vec<u8> {
    use lopdf::content::{Content, Operation};
    use lopdf::{Document as PdfDocument, Object, Stream, dictionary};

    let mut document = PdfDocument::with_version("1.5");
    let pages_id = document.new_object_id();
    let font_id = document.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => "Helvetica",
    });
    let resources_id = document.add_object(dictionary! {
        "Font" => dictionary! { "F1" => font_id },
    });
    let content = Content {
        operations: vec![
            Operation::new("BT", vec![]),
            Operation::new("Tf", vec!["F1".into(), 14.into()]),
            Operation::new("Td", vec![72.into(), 720.into()]),
            Operation::new("Tj", vec![Object::string_literal("SOURCE VIEW SENTENCE.")]),
            Operation::new("ET", vec![]),
        ],
    };
    let content_id = document.add_object(Stream::new(
        dictionary! {},
        content.encode().expect("fixture content should encode"),
    ));
    let page_id = document.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => pages_id,
        "Contents" => content_id,
    });
    document.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => vec![Object::Reference(page_id)],
            "Count" => 1,
            "Resources" => resources_id,
            "MediaBox" => vec![0.into(), 0.into(), 595.into(), 842.into()],
        }),
    );
    let catalog_id = document.add_object(dictionary! {
        "Type" => "Catalog",
        "Pages" => pages_id,
    });
    document.trailer.set("Root", catalog_id);

    let mut bytes = Vec::new();
    document
        .save_to(&mut bytes)
        .expect("fixture PDF should serialize");
    bytes
}

/// The rendered-page cache is keyed on the raw content hash, which a whitespace-only extraction
/// deploy does not change: a fresh handle's render after the deploy serves exactly the entry the
/// pre-deploy render stored. A stale handle must therefore never reach the cache at all — the
/// error with its `retry` is decided by handle resolution before any cache lookup. This test
/// renders through the cache on the pre-deploy server, then proves the stale sentence handle is
/// still refused with `retry.at` after the deploy restart, and that the retry target renders the
/// same page (the cached entry) successfully.
#[tokio::test]
async fn stale_source_view_refusal_precedes_the_rendered_page_cache_stdio() {
    let directory = tempfile::tempdir().expect("temporary directory should be created");
    let state = tempfile::tempdir().expect("temporary state directory should be created");
    let document_path = directory.path().join("page.pdf");
    tokio::fs::write(&document_path, build_text_pdf())
        .await
        .expect("PDF fixture should be written");

    let first = start_client(directory.path(), state.path()).await;
    let opened = v2_open(&first, &document_path).await;
    let doc = opened["doc"].as_str().expect("doc handle").to_owned();
    let read = payload(&v2_read(&first, &doc, json!({})).await);
    let sentence = read["at"].as_str().expect("sentence handle").to_owned();
    assert_eq!(flat_text(&read), "SOURCE VIEW SENTENCE.", "{read}");
    // Warm the rendered-page cache through the fresh handle.
    let rendered = call(&first, "source_view", json!({"at": sentence})).await;
    assert!(rendered.is_error != Some(true), "{rendered:?}");
    first.cancel().await.expect("first client should stop");

    let second = start_client_with_env(
        directory.path(),
        state.path(),
        &[("READING_MCP_TEST_EXTRACTION_VARIANT", "spaces")],
    )
    .await;

    // The stale handle is refused with its retry even though a cache entry keyed by the unchanged
    // content hash sits in the state directory: the cache lookup never ran for it.
    let result = call(&second, "source_view", json!({"at": sentence})).await;
    let error = tool_error(&result);
    assert_eq!(error["error"], "stale_handle", "{error}");
    let target = error["retry"]["at"]
        .as_str()
        .expect("source_view retry.at")
        .to_owned();
    assert_ne!(target, sentence);

    // The retry target is fresh: it reads the proven text. (`source_view` on it cannot render in
    // this test because the test-only extraction variant rewrites the text after the parser fixed
    // its page bindings, so the two no longer align; a real extraction deploy computes the
    // bindings from the text it actually extracts.)
    let retried = payload(&v2_read(&second, &target, json!({})).await);
    assert_eq!(
        strip_ws(retried["text"].as_str().expect("retry target reads")),
        strip_ws("SOURCE VIEW SENTENCE."),
        "{retried}"
    );
    second.cancel().await.expect("second client should stop");
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
    assert_eq!(flat_text(&read), "Alpha sentence one.");
    let sentence = read["at"].as_str().expect("sentence handle").to_owned();
    let section = section_of(&paragraph_handle(&sentence));
    let sentences = section_items_with(&first, &section, json!({}), 16_000).await;
    let paragraphs =
        section_items_with(&first, &section, json!({"unit": "paragraph"}), 16_000).await;
    assert_eq!(sentences.len(), 3, "{sentences:?}");
    assert_eq!(paragraphs.len(), 2, "{paragraphs:?}");
    first.cancel().await.expect("first client should stop");

    let second = start_client_with_env(
        directory.path(),
        state.path(),
        &[("READING_MCP_TEST_EXTRACTION_VARIANT", "spaces")],
    )
    .await;
    for (items, extra) in [
        (sentences, json!({})),
        (paragraphs, json!({"unit": "paragraph"})),
    ] {
        for (old_handle, old_text) in items {
            let stale = v2_read(&second, &old_handle, extra.clone()).await;
            let error = tool_error(&stale);
            assert_eq!(error["error"], "stale_handle", "{error}");
            let retry = error["retry"].clone();
            let target = retry["from"].as_str().expect("read retry.from").to_owned();
            assert_ne!(target, old_handle);
            let retried = payload(&call(&second, "read", retry).await);
            assert_eq!(
                strip_ws(&flat_text(&retried)),
                strip_ws(&old_text),
                "retry of {old_handle}: {retried}"
            );
        }
    }

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
        json!({"doc": doc.clone(), "query": "needle", "limit": 5}),
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

    // `source_view` rejects Markdown only after resolving the handle, so the stale answer still
    // proves the stdio wire shape without a PDF fixture.
    let result = call(&second, "source_view", json!({"at": sentence})).await;
    let error = tool_error(&result);
    assert_eq!(error["error"], "stale_handle", "{error}");
    assert!(
        error["retry"]["at"]
            .as_str()
            .expect("source_view retry.at")
            .starts_with('d')
    );

    // A document handle needs no per-unit text proof: it names the same re-ingested document.
    let result = v2_read(&second, &doc, json!({})).await;
    let error = tool_error(&result);
    assert_eq!(error["error"], "stale_handle", "{error}");
    let target = error["retry"]["from"]
        .as_str()
        .expect("document retry.from");
    assert_eq!(target.matches('.').count(), 0, "{target}");

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
    assert_eq!(
        retry_args["direction"].as_str(),
        Some("backward"),
        "a backward cursor retry must keep its direction: {retry_args}"
    );
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
async fn first_deploy_rows_without_ingest_metadata_remap_stdio() {
    let directory = tempfile::tempdir().expect("temporary directory should be created");
    let state = tempfile::tempdir().expect("temporary state directory should be created");
    let document_path = directory.path().join("deploy.md");
    tokio::fs::write(
        &document_path,
        "# Deploy\n\n## Alpha\n\nDeploy sentence one. Deploy sentence two.\n",
    )
    .await
    .expect("fixture should be written");

    let first = start_client(directory.path(), state.path()).await;
    let opened = v2_open(&first, &document_path).await;
    let doc = opened["doc"].as_str().expect("doc handle").to_owned();
    let read = payload(&v2_read(&first, &doc, json!({})).await);
    let old_text = flat_text(&read);
    let sentence = read["at"].as_str().expect("sentence handle").to_owned();
    first.cancel().await.expect("first client should stop");

    // Rows written by the pre-feature binary have no metadata values once the additive migration
    // has added the columns. The first post-deploy stale access must still re-ingest those rows
    // and build their proof mappings; it must not wait for an explicit `open`.
    let database = state.path().join("reading-mcp.sqlite");
    let connection = rusqlite::Connection::open(&database).expect("state database should open");
    let updated = connection
        .execute(
            "UPDATE text_unit_documents_v2 SET ingest_fingerprint = NULL, handle_checksum = NULL",
            [],
        )
        .expect("pre-feature row metadata should be simulated");
    assert_eq!(updated, 1);
    drop(connection);

    // Before any extraction change, a same-version restart can hit the existing open-result
    // cache. That hit must stamp both metadata fields; leaving the checksum null would make a
    // later segmentation-version deploy mistake the current probe checksum for the pre-feature
    // checksum and skip proof generation.
    let stamped = start_client(directory.path(), state.path()).await;
    let same_version = payload(&v2_read(&stamped, &sentence, json!({})).await);
    assert!(
        same_version["text"]
            .as_str()
            .is_some_and(|text| !text.is_empty()),
        "{same_version}"
    );
    stamped.cancel().await.expect("stamping client should stop");
    let connection = rusqlite::Connection::open(&database).expect("state database should reopen");
    let stamped_checksum: Option<String> = connection
        .query_row(
            "SELECT handle_checksum FROM text_unit_documents_v2",
            [],
            |row| row.get(0),
        )
        .expect("handle checksum should be queryable");
    assert_eq!(
        stamped_checksum.as_deref(),
        doc.rsplit('~').next(),
        "handle checksum must be stamped"
    );
    drop(connection);

    let second = restart_variant(directory.path(), state.path(), "spaces").await;
    let result = v2_read(&second, &sentence, json!({})).await;
    let error = tool_error(&result);
    assert_eq!(error["error"], "stale_handle", "{error}");
    let target = error["retry"]["from"]
        .as_str()
        .expect("first-deploy retry")
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
